//! PlaybackInfo 端点 + strm 直链缺流信息时的后台 ffprobe 回填。
//!
//! 图片端点（`/Items/{id}/Images/*`）属独立域，见 [`crate::routes::images`]。

use std::collections::HashSet;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::emby::PlaybackInfoResponseDto;
use emrs_core::auth::AuthContext;
use emrs_core::stores::{ItemsStore, MediaSourceRow};

use super::parse_id;
use crate::state::AppState;

/// 在途 strm 后台回填去重（media_source.id 集合）：并发播放请求命中同一源
/// 只探测一次，防止 ffprobe 风暴。锁内只做 check+insert/remove、不跨 await。
static STRM_PROBING: LazyLock<Mutex<HashSet<i64>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// GET|POST /Items/{id}/PlaybackInfo：播放信息。
pub(super) async fn playback_info(
    State(st): State<AppState>,
    axum::Extension(ctx): axum::Extension<AuthContext>,
    Path(id): Path<String>,
) -> Response {
    playback_info_core(&st, ctx, id).await
}

/// GET|POST /Users/{user_id}/Items/{item_id}/PlaybackInfo：播放信息（Emby 带 userId 别名路径）。
pub(super) async fn playback_info_by_user(
    State(st): State<AppState>,
    axum::Extension(ctx): axum::Extension<AuthContext>,
    Path((_user_id, item_id)): Path<(i64, String)>,
) -> Response {
    playback_info_core(&st, ctx, item_id).await
}

/// PlaybackInfo 核心逻辑（两个路径别名共用）。
async fn playback_info_core(st: &AppState, ctx: AuthContext, id: String) -> Response {
    let Some(num_id) = parse_id(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // 多版本 PlaybackInfo：查询 item 的所有 media_source 行
    match ItemsStore::list_media_sources(&st.db, num_id).await {
        Ok(sources) if !sources.is_empty() => {
            // strm 直链缺流信息 → 后台 ffprobe 回填（当前请求不阻塞，见 spawn_strm_probe）
            for media in &sources {
                spawn_strm_probe(st, media);
            }
            let mut media_sources = Vec::with_capacity(sources.len());
            let mut play_session_id = String::new();
            for (idx, media) in sources.iter().enumerate() {
                match crate::emby::media_sources_json(
                    &st.db,
                    st.cfg.playback.signing_key.as_deref(),
                    ctx.user_id,
                    media,
                    idx == 0,
                )
                .await
                {
                    Ok(ms) => {
                        if play_session_id.is_empty() {
                            play_session_id = media.uuid.as_deref().unwrap_or("").to_string();
                        }
                        media_sources.extend(ms);
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "playback_info: cannot issue ticket");
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "未配置播放票据签名密钥 playback.signing_key",
                        )
                            .into_response();
                    }
                }
            }
            axum::Json(PlaybackInfoResponseDto::new(media_sources, play_session_id)).into_response()
        }
        Ok(_) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(error = %e, "playback_info failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "查询失败").into_response()
        }
    }
}

/// strm 直链是否缺流信息、需后台回填。
///
/// - `path_type`：`MediaSourceRow.path_type` 为 `url`/`strm` 表示 strm 直链
///   （本地 `file://` 源属 `local`，由 Probe 阶段批量探测，不在此列）；
/// - `file_metadata`：ffprobe 结果 JSON，空（NULL/`""`/`"[]"`）即从未探测过。
///   探测成功会写入非空流列表，之后自然不再触发；探测无果的坏链则每次
///   被播放重试一次（内存去重只防并发重复，不持久化）。
fn should_backfill(path_type: Option<&str>, file_metadata: Option<&str>) -> bool {
    let remote = matches!(path_type, Some("url") | Some("strm"));
    let empty = matches!(file_metadata, None | Some("") | Some("[]"));
    remote && empty
}

/// strm 播放请求命中缺流信息源时，后台异步 ffprobe 回填（当前请求不阻塞）。
///
/// 复用 `Scanner::probe_media_source`：ffprobe 原生支持 http(s) URL，
/// 内部时长先走 MP4/MKV 头部解析（对 URL 打开失败自动回落 `format.duration`），
/// 结果经 UPDATE 写回 `media_source.metadata`/`chapters`/`file_duration`/
/// `container` 并置 `status='ok'/'failed'`（播放链路不读 status，不受影响）。
/// 同条目下次请求或列表/详情即有完整流信息与时长。
///
/// 并发去重：`STRM_PROBING` 持在途 media_source.id，命中即跳过；完成后移除。
/// 探测经 `strm_probe_timeout_secs` 超时兜底（`probe_media_checked` 已加
/// `kill_on_drop`，超时能真正回收 ffprobe 子进程）。开关 `strm_probe_backfill`
/// 关闭时直接跳过。
fn spawn_strm_probe(st: &AppState, media: &MediaSourceRow) {
    if !st.cfg.playback.strm_probe_backfill {
        return;
    }
    let Some(media_id) = media.media_id else {
        return;
    };
    let Some(url) = media.path_url.as_deref().filter(|u| !u.is_empty()) else {
        return;
    };
    if !should_backfill(media.path_type.as_deref(), media.file_metadata.as_deref()) {
        return;
    }
    {
        let mut in_flight = STRM_PROBING.lock().unwrap_or_else(|e| e.into_inner());
        if !in_flight.insert(media_id) {
            return; // 已在途，跳过
        }
    }

    let scanner = emrs_core::importer::Scanner::new(st.db.clone(), String::new());
    let timeout = Duration::from_secs(st.cfg.playback.strm_probe_timeout_secs.max(1));
    let url = url.to_string();
    tokio::spawn(async move {
        let _ = tokio::time::timeout(timeout, scanner.probe_media_source(media_id, &url)).await;
        // 探测完成（成功/失败/超时）都释放去重位：坏链下次播放可再试
        STRM_PROBING
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&media_id);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strm_should_backfill_when_no_streams() {
        // url 直链 + 从未探测（None / 空串 / 空数组）→ 需回填
        assert!(should_backfill(Some("url"), None));
        assert!(should_backfill(Some("url"), Some("")));
        assert!(should_backfill(Some("url"), Some("[]")));
        // strm 同 url 直链
        assert!(should_backfill(Some("strm"), Some("[]")));
        // 已探测出流信息（非空）→ 不回填
        assert!(!should_backfill(
            Some("url"),
            Some(r#"[{"stream_type":"Video"}]"#)
        ));
        assert!(!should_backfill(Some("url"), Some("{}")));
        // 本地 file 源由 Probe 阶段批量处理，不在此列
        assert!(!should_backfill(Some("local"), None));
        assert!(!should_backfill(Some("local"), Some("[]")));
        // 未知类型 / 缺类型 → 不回填
        assert!(!should_backfill(None, None));
        assert!(!should_backfill(Some("webdav"), None));
    }
}
