//! 流式播放路由：本地 Range 服务 / HTTP 直链 302 / 字幕。

use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tokio::io::AsyncSeekExt;

use emrs_core::auth::AuthContext;
use emrs_core::cloud::CloudRef;
use emrs_core::playback::{PlayRequest, PlaybackRouter};

use crate::state::AppState;

/// 流式播放路由（认证但不加 Timeout，防长播被 30s 掐断；挂在 [`crate::app`] 的认证层内）。
pub fn streaming_routes() -> Router<AppState> {
    Router::new()
        // 视频播放（本地 Range 服务 / http 直链 302）
        // `/Videos/{uuid}/{name}` 为 Emby 协议标准路径；小写 `/videos` 别名兼容
        // 部分客户端（如某些 Senplayer 版本）按小写请求的现状，二者指向同一 handler。
        .route("/Videos/{uuid}/{name}", get(play_video).head(play_video))
        .route("/videos/{uuid}/{name}", get(play_video).head(play_video))
        // 字幕
        .route("/Videos/{uuid}/Subtitles/{index}", get(play_subtitle))
}

/// GET|HEAD /Videos/{uuid}/{name}：视频播放。
///
/// - 本地源（path_type='local'）：Range 流式服务（206/200）
/// - 其余（http/https 直链）：302 到直链（结果写 Cache，TTL 10 分钟）
async fn play_video(
    State(st): State<AppState>,
    axum::Extension(ctx): axum::Extension<AuthContext>,
    Path((uuid, _name)): Path<(String, String)>,
    req: Request,
) -> Response {
    let range = req
        .headers()
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let media = sqlx::query_as::<_, (i64, Option<String>, Option<String>)>(
        "SELECT id, COALESCE(path, remote_path) AS path_url, \
                CASE protocol WHEN 'file' THEN 'local' WHEN 'strm' THEN 'strm' ELSE protocol END AS path_type \
         FROM media_source \
         WHERE uuid = ? LIMIT 1",
    )
    .bind(&uuid)
    .fetch_optional(st.db.pool())
    .await;

    match media {
        Ok(Some((_, Some(url), Some(typ)))) if typ == "local" => {
            // 本地视频源（直扫入库）：Range 流式服务（206/200）
            serve_local_file(&url, range.as_deref()).await
        }
        Ok(Some((_, Some(url), Some(typ)))) => {
            let cloud_ref = CloudRef {
                path_type: typ,
                path_url: url,
            };
            let play_req = PlayRequest {
                cloud_ref,
                user_id: ctx.user_id,
                device_id: Some(ctx.device.device_id.clone()),
            };
            let router = PlaybackRouter::new(st.drivers.clone(), st.cache.clone());
            match router.resolve_direct(&play_req).await {
                Ok(Some(direct_url)) => {
                    axum::response::Redirect::temporary(&direct_url).into_response()
                }
                Ok(None) => {
                    tracing::warn!(uuid, "play_video: driver returned no direct url");
                    StatusCode::NOT_FOUND.into_response()
                }
                Err(e) => {
                    tracing::error!(uuid, error = %e, "play_video: resolve failed");
                    StatusCode::INTERNAL_SERVER_ERROR.into_response()
                }
            }
        }
        Ok(Some((_, Some(url), None))) => {
            // path_type 为 NULL：无协议标识，按 http 直链 302（webdav 等已不支持）
            axum::response::Redirect::temporary(&url).into_response()
        }
        Ok(Some((_, None, _))) | Ok(None) => {
            tracing::warn!(uuid, "play_video: media not found or no url");
            StatusCode::NOT_FOUND.into_response()
        }
        Err(e) => {
            tracing::error!(uuid, error = %e, "play_video: db error");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// 本地文件流式服务（`path_type='local'`）：Range 206/200，支持拖动进度。
///
/// `path_url` 可能带 `file://` 前缀（STRM 相对路径转换而来），此处剥离后按绝对路径打开。
pub(crate) async fn serve_local_file(path: &str, range: Option<&str>) -> Response {
    let local = path.strip_prefix("file://").unwrap_or(path);
    let mut file = match tokio::fs::File::open(local).await {
        Ok(f) => f,
        Err(e) => {
            tracing::error!(path = local, error = %e, "serve_local_file: open failed");
            return StatusCode::NOT_FOUND.into_response();
        }
    };
    let total = match file.metadata().await {
        Ok(m) => m.len(),
        Err(e) => {
            tracing::error!(path = local, error = %e, "serve_local_file: metadata failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let mime = mime_for(local);

    match parse_range(range, total) {
        RangeResult::Unsat => Response::builder()
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header(header::CONTENT_RANGE, format!("bytes */{total}"))
            .body(axum::body::Body::empty())
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        RangeResult::Partial(start, end) => {
            if let Err(e) = file.seek(tokio::io::SeekFrom::Start(start)).await {
                tracing::error!(path = local, error = %e, "serve_local_file: seek failed");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
            let length = end - start + 1;
            Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_TYPE, mime)
                .header(header::CONTENT_LENGTH, length)
                .header(
                    header::CONTENT_RANGE,
                    format!("bytes {start}-{end}/{total}"),
                )
                .header(header::ACCEPT_RANGES, "bytes")
                .body(axum::body::Body::from_stream(
                    tokio_util::io::ReaderStream::with_capacity(file, 64 * 1024),
                ))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        RangeResult::Full => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime)
            .header(header::CONTENT_LENGTH, total)
            .header(header::ACCEPT_RANGES, "bytes")
            .body(axum::body::Body::from_stream(
                tokio_util::io::ReaderStream::with_capacity(file, 64 * 1024),
            ))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
    }
}

/// Range 解析结果。
enum RangeResult {
    /// 无 Range 头：整文件 200。
    Full,
    /// Range 非法 / 超出范围：416。
    Unsat,
    /// 可满足范围（含端点）：206。
    Partial(u64, u64),
}

/// 解析 `Range: bytes=...`，支持 `start-`、`start-end`、`-suffix` 三种形态。
fn parse_range(range: Option<&str>, total: u64) -> RangeResult {
    let Some(spec) = range else {
        return RangeResult::Full;
    };
    let spec = spec.strip_prefix("bytes=").unwrap_or(spec).trim();
    let Some((start_s, end_s)) = spec.split_once('-') else {
        return RangeResult::Unsat;
    };
    let start_s = start_s.trim();
    let end_s = end_s.trim();

    // 后缀形式 bytes=-N：取最后 N 字节
    if start_s.is_empty() {
        if total == 0 {
            return RangeResult::Unsat;
        }
        let suffix = match end_s.parse::<u64>() {
            Ok(v) if v > 0 => v,
            _ => return RangeResult::Unsat,
        };
        let start = total.saturating_sub(suffix);
        return RangeResult::Partial(start, total - 1);
    }

    let start = match start_s.parse::<u64>() {
        Ok(v) => v,
        Err(_) => return RangeResult::Unsat,
    };
    if start >= total {
        return RangeResult::Unsat;
    }
    let end = if end_s.is_empty() {
        total - 1
    } else {
        match end_s.parse::<u64>() {
            Ok(e) if e >= start => e.min(total - 1),
            _ => return RangeResult::Unsat,
        }
    };
    RangeResult::Partial(start, end)
}

/// 扩展名 → MIME（与 `probe::container_for` 对应）。
fn mime_for(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "mp4" | "m4v" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "ts" | "m2ts" => "video/mp2t",
        "mov" => "video/quicktime",
        "wmv" => "video/x-ms-wmv",
        "flv" => "video/x-flv",
        "mpg" | "mpeg" => "video/mpeg",
        "3gp" => "video/3gpp",
        "ogv" => "video/ogg",
        _ => "application/octet-stream",
    }
}

/// GET /Videos/{uuid}/Subtitles/{index}：外部字幕字节。
///
/// 从 `external_subtitle` 表按 `media_source.uuid` + 内部序号查外部字幕行，
/// 取其 `path`（本地字幕文件），返回内容。
async fn play_subtitle(
    State(st): State<AppState>,
    Path((uuid, index)): Path<(String, i32)>,
) -> Response {
    // 1. uuid → media_source 自增 id
    let media_id: Option<i64> =
        sqlx::query_scalar("SELECT id FROM media_source WHERE uuid = ? LIMIT 1")
            .bind(&uuid)
            .fetch_optional(st.db.pool())
            .await
            .ok()
            .flatten();
    let Some(media_id) = media_id else {
        return StatusCode::NOT_FOUND.into_response();
    };

    // 2. 按外部字幕内部序号取行（按 id 升序，0 基；dto.rs 的 DeliveryUrl
    //    `/Videos/{uuid}/Subtitles/{i}` 用同一序号，index 参数即内部序号）。
    let path = sqlx::query_scalar::<_, String>(
        "SELECT path FROM external_subtitle \
         WHERE media_source_id = ? ORDER BY id LIMIT 1 OFFSET ?",
    )
    .bind(media_id)
    .bind(index.max(0))
    .fetch_optional(st.db.pool())
    .await;

    match path {
        Ok(Some(path)) if !path.is_empty() => {
            // 本地字幕文件：Range 流式服务
            serve_local_file(&path, None).await
        }
        Ok(_) => {
            tracing::warn!(uuid, index, "subtitle stream not found");
            StatusCode::NOT_FOUND.into_response()
        }
        Err(e) => {
            tracing::error!(uuid, error = %e, "subtitle lookup failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
