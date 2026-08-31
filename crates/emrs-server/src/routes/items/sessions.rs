//! 播放会话进度上报端点：/Sessions/Playing / Progress / Stopped。

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use emrs_core::auth::AuthContext;
use emrs_core::stores::{ItemsStore, PlaybackStore};

use crate::state::AppState;

use super::resolve_item_id;

/// POST /Sessions/Playing：开始播放（`play_count` +1，作为 Resume 标记）。
pub(super) async fn report_playing(
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
pub(super) async fn report_progress(
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
pub(super) async fn report_stopped(
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
