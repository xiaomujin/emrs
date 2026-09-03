//! Sessions 域：`/Sessions` 命名空间统一归口。
//!
//! - `GET /Sessions`：当前用户进行中播放会话（EMRS 无客户端会话注册表，从 user_item_data 派生）。
//! - `POST /Sessions/Capabilities[/Full]`、`/Sessions/Playing/Ping`：Emby 兼容空应答 stub。
//! - `POST /Sessions/Playing`、`/Progress`、`/Stopped`：播放开始 / 进度 / 终态上报。
//!
//! 全部走认证 + Timeout（authed JSON API zone）；匿名兼容读（无 AuthContext）时 `GET /Sessions` 返回空列表。

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};

use emrs_core::auth::AuthContext;
use emrs_infra::stores::{ItemsStore, PlaybackStore};

use crate::emby::SessionListEntryDto;
use crate::routes::params::resolve_item_id;
use crate::state::AppState;

/// 认证组：Sessions 列表 + 能力/Ping stub + 播放进度上报。
pub fn authenticated() -> Router<AppState> {
    Router::new()
        // Sessions 列表（从 user_item_data 派生进行中会话）
        .route("/Sessions", get(sessions))
        // 能力/Ping stub：Emby 客户端上报兼容，空应答 204
        .route("/Sessions/Capabilities/Full", post(no_content))
        .route("/Sessions/Capabilities", post(no_content))
        .route("/Sessions/Playing/Ping", post(no_content))
        // 播放进度上报（真实实现）
        .route("/Sessions/Playing", post(report_playing))
        .route("/Sessions/Playing/Progress", post(report_progress))
        .route("/Sessions/Playing/Stopped", post(report_stopped))
}

/// 204 空应答。
async fn no_content() -> impl IntoResponse {
    StatusCode::NO_CONTENT
}

/// GET /Sessions：当前用户的进行中播放会话（NowPlayingItem + PlaybackPositionTicks）。
/// EMRS 无客户端会话注册表，从 user_item_data 派生"正在播放"条目作为会话列表。
/// 匿名兼容读（GET/HEAD 无 token）时无 AuthContext，返回空列表。
async fn sessions(
    State(st): State<AppState>,
    ctx: Option<axum::Extension<AuthContext>>,
) -> Response {
    let Some(axum::Extension(ctx)) = ctx else {
        return axum::Json(Vec::<SessionListEntryDto>::new()).into_response();
    };
    let items = match ItemsStore::list_active_sessions(&st.db, ctx.user_id).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "sessions query failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    // 批量预取图片行 id，组装 NowPlayingItem DTO
    let mut ids: Vec<i64> = Vec::with_capacity(items.len() * 2);
    for i in &items {
        ids.push(i.id);
        if let Some(sid) = i.series_id {
            ids.push(sid);
        }
    }
    let flags = ItemsStore::image_ids_batch(&st.db, &ids)
        .await
        .unwrap_or_default();
    let sessions: Vec<SessionListEntryDto> = items
        .iter()
        .map(|item| {
            let now_playing_item = crate::emby::item_to_json(
                &st.cfg.emby.server_id,
                item,
                &crate::emby::ItemImageFlags::from_batch(&flags, item),
                None,
                None,
                None,
            );
            let user_id = ctx.user_id.to_string();
            SessionListEntryDto::new(
                now_playing_item,
                format!("session-{}", item.id),
                &user_id,
                &ctx.username,
                &ctx.device,
                item.play_ms * 10_000,
            )
        })
        .collect();
    axum::Json(sessions).into_response()
}

/// POST /Sessions/Playing：开始播放（`play_count` +1，作为 Resume 标记）。
async fn report_playing(
    State(st): State<AppState>,
    axum::Extension(ctx): axum::Extension<AuthContext>,
    body: axum::body::Bytes,
) -> Response {
    let body = parse_json_body(&body);
    let Some(item_id) = item_id_from_body(&body) else {
        return StatusCode::NO_CONTENT.into_response();
    };
    let Some(target_id) = resolve_item_id(&st, &item_id).await else {
        return StatusCode::NO_CONTENT.into_response();
    };
    if let Err(e) = PlaybackStore::mark_started(&st.db, ctx.user_id, target_id).await {
        tracing::error!(error = %e, "mark_started failed");
    }
    StatusCode::NO_CONTENT.into_response()
}

/// 宽松解析请求体：Hills 等客户端 POST 时可能不带 `application/json` Content-Type，
/// 用 `Bytes` 提取后自行解析，空体/坏 JSON 一律回退为空对象。
fn parse_json_body(bytes: &axum::body::Bytes) -> serde_json::Value {
    serde_json::from_slice(bytes).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "进度上报 body 解析失败，按空对象处理");
        serde_json::json!({})
    })
}

/// 从进度上报 body 提取 ItemId 字符串。
fn item_id_from_body(body: &serde_json::Value) -> Option<String> {
    body.get("ItemId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// POST /Sessions/Playing/Progress：播放进度。
async fn report_progress(
    State(st): State<AppState>,
    axum::Extension(ctx): axum::Extension<AuthContext>,
    body: axum::body::Bytes,
) -> Response {
    let body = parse_json_body(&body);
    let Some(item_id) = item_id_from_body(&body) else {
        return StatusCode::NO_CONTENT.into_response();
    };
    let Some(target_id) = resolve_item_id(&st, &item_id).await else {
        return StatusCode::NO_CONTENT.into_response();
    };

    let position_ticks = body
        .get("PositionTicks")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let play_ms = position_ticks / 10_000; // Emby Ticks = 100ns

    if let Err(e) =
        PlaybackStore::upsert_progress(&st.db, ctx.user_id, target_id, play_ms, false).await
    {
        tracing::error!(error = %e, "report_progress failed");
    }
    StatusCode::NO_CONTENT.into_response()
}

/// POST /Sessions/Playing/Stopped：停止播放（终态进度）。
async fn report_stopped(
    State(st): State<AppState>,
    axum::Extension(ctx): axum::Extension<AuthContext>,
    body: axum::body::Bytes,
) -> Response {
    let body = parse_json_body(&body);
    let Some(item_id) = item_id_from_body(&body) else {
        return StatusCode::NO_CONTENT.into_response();
    };
    let Some(target_id) = resolve_item_id(&st, &item_id).await else {
        return StatusCode::NO_CONTENT.into_response();
    };

    let position_ticks = body
        .get("PositionTicks")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let failed = body
        .get("Failed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    // 总时长（Ticks）：顶层 RunTimeTicks，缺失时兼容客户端嵌套的 Item.RunTimeTicks；
    // 都拿不到则回库查 media_source.file_duration（秒 → 100ns ticks）。
    let mut runtime_ticks = body
        .get("RunTimeTicks")
        .and_then(|v| v.as_i64())
        .or_else(|| {
            body.get("Item")
                .and_then(|v| v.get("RunTimeTicks"))
                .and_then(|v| v.as_i64())
        })
        .unwrap_or(0);
    if runtime_ticks == 0
        && let Ok(Some(media)) = ItemsStore::get_playback_info(&st.db, target_id).await
        && let Some(secs) = media.file_second
    {
        runtime_ticks = secs * 10_000_000;
    }
    let play_ms = position_ticks / 10_000;
    // 已看完判定：未失败且播放进度 ≥ 总时长的 80%
    let is_complete = !failed && runtime_ticks > 0 && position_ticks >= runtime_ticks * 8 / 10;

    if let Err(e) =
        PlaybackStore::upsert_progress(&st.db, ctx.user_id, target_id, play_ms, is_complete).await
    {
        tracing::error!(error = %e, "report_stopped failed");
    }
    StatusCode::NO_CONTENT.into_response()
}
