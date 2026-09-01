//! PlaybackInfo + 图片代理端点。

use std::collections::HashSet;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

use emrs_core::auth::AuthContext;
use emrs_core::emby::{IdKind, PlaybackInfoResponseDto, media_sources_json};
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
                match media_sources_json(
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

/// GET /Items/{id}/Images/{image_path}：图片代理（下载上游 URL 并返回，Hills 等客户端展示用）。
/// 注册在公开组（匿名可访问）；Season/Episode 无自有图片时回退到上级剧集海报。
///
/// `{id}` 命名空间按类型前缀分流：`i-{id}`/裸数字 = item.id（movie/series/season/episode）；
/// `p-{id}` = people.id（演员头像）。item.id 与 people.id 各表独立自增、数值会撞，
/// 必须前缀区分。`l-/g-/s-` 库/类型/工作室暂无图片 → 404。
/// `{image_path}` 通配：兼容 `Primary` / `Primary/0` / `primary.jpg` / `Primary/0.jpg`
/// （Emby 客户端带索引或扩展名），内部取首段、剥 `.jpg`/`.png` 等扩展名后归一化类型；
/// `/0` 索引取第 index 行（按 id 升序），与 `BackdropImageTags` 中 tag 顺序一一对应。
/// query 支持 `maxWidth`/`maxHeight`/`quality`（Emby 客户端缩放请求）。
pub(crate) async fn item_image(
    State(st): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<ResizeQuery>,
    // Path((id, image_path)): Path<(String, String)>,
) -> Response {
    // tag=img-{id} 直查图片行（精确匹配，无需偏移）
    if let Some(url) = resolve_by_tag(&st, q.tag.as_deref()).await {
        return proxy_image(&st, Some(url), q).await;
    }
    StatusCode::NOT_FOUND.into_response()

    // // 归一化 image_type：取首段（去掉 /0 索引），再剥扩展名（Primary.jpg → Primary）
    // let image_type = normalize_image_type(&image_path);
    // // `/0` 索引（Backdrop 第 N 张，0 起）
    // let index = normalize_image_index(&image_path);
    // // 支持 Primary / Backdrop / Logo / Thumb；其他类型暂未入库
    // if !matches!(
    //     image_type.to_ascii_lowercase().as_str(),
    //     "primary" | "backdrop" | "logo" | "thumb"
    // ) {
    //     return StatusCode::NOT_FOUND.into_response();
    // }
    //
    // // 命名空间分流：p-{id} → people（演员头像）；i-{id}/裸数字 → item；其余 404
    // match emrs_core::emby::parse_id(&id) {
    //     Some((IdKind::People, rid)) => {
    //         let url = find_image_url(&st, "people", rid, image_type, index).await;
    //         proxy_image(&st, url, q).await
    //     }
    //     Some((IdKind::Item, rid)) => {
    //         let ty = ItemsStore::get_item_type(&st.db, rid)
    //             .await
    //             .ok()
    //             .flatten()
    //             .unwrap_or_default();
    //         let url = resolve_image_url(&st, &ty, rid, image_type, index).await;
    //         proxy_image(&st, url, q).await
    //     }
    //     _ => StatusCode::NOT_FOUND.into_response(),
    // }
}

/// 图片缩放 query 参数（宽松解析，Emby 客户端带 maxWidth/maxHeight/quality/tag）。
#[derive(serde::Deserialize, Default)]
pub(crate) struct ResizeQuery {
    #[serde(
        default,
        deserialize_with = "crate::routes::items::deserialize_lenient_i64"
    )]
    maxwidth: Option<i64>,
    #[serde(
        default,
        deserialize_with = "crate::routes::items::deserialize_lenient_i64"
    )]
    maxheight: Option<i64>,
    #[serde(
        default,
        deserialize_with = "crate::routes::items::deserialize_lenient_i64"
    )]
    quality: Option<i64>,
    #[serde(default)]
    tag: Option<String>,
}

// /// 归一化图片类型：取首段（去掉 `/0` 索引），再剥 `.jpg`/`.png` 等扩展名。
// /// `Primary/0.jpg` → `Primary`；`primary.jpg` → `primary`。
// fn normalize_image_type(image_path: &str) -> &str {
//     let first = image_path.split('/').next().unwrap_or(image_path);
//     first
//         .rsplit_once('.')
//         .map(|(base, _)| base)
//         .unwrap_or(first)
// }

// /// 提取 `/N` 索引段（Backdrop 第 N 张，0 起）；无索引段返回 0。
// /// `Backdrop/2.jpg` → 2；`Primary` → 0。非数字索引按 0 处理。
// fn normalize_image_index(image_path: &str) -> i64 {
//     let mut parts = image_path.split('/');
//     let _type_seg = parts.next();
//     match parts.next() {
//         Some(seg) => seg
//             .rsplit_once('.')
//             .map(|(base, _)| base)
//             .unwrap_or(seg)
//             .trim()
//             .parse::<i64>()
//             .unwrap_or(0)
//             .max(0),
//         None => 0,
//     }
// }

/// 从 `tag=img-{id}` 解析图片行 id，直接查 `item_image.path_url`。
async fn resolve_by_tag(st: &AppState, tag: Option<&str>) -> Option<String> {
    let tag = tag?;
    let Some((IdKind::Image, img_id)) = emrs_core::emby::parse_id(tag) else {
        return None;
    };
    sqlx::query_scalar::<_, Option<String>>("SELECT path_url FROM item_image WHERE id = ?")
        .bind(img_id)
        .fetch_optional(st.db.pool())
        .await
        .ok()
        .flatten()
        .flatten()
        .filter(|u| !u.is_empty())
}

// /// 查 `item_image` 表取图片 URL；错误 / 缺失 / 空串统一归一为 None。
// /// `parent_type`：`item`（video 各类型）/ `people`（人物）。
// /// `index`：同类型多行时取第 index 行（按 id 升序，0 起），与 tag `img-{id}` 排序一致。
// async fn find_image_url(
//     st: &AppState,
//     parent_type: &str,
//     rid: i64,
//     image_type: &str,
//     index: i64,
// ) -> Option<String> {
//     ItemsStore::get_image_path(&st.db, parent_type, rid, image_type, index)
//         .await
//         .ok()
//         .flatten()
//         .map(|(_, url)| url)
//         .filter(|u| !u.is_empty())
// }

// /// Season/Episode 无自有图片时回退到上级剧集，查其图片 URL。
// async fn resolve_image_url(
//     st: &AppState,
//     _item_type: &str,
//     rid: i64,
//     image_type: &str,
//     index: i64,
// ) -> Option<String> {
//     find_image_url(st, "item", rid, image_type, index).await
// }

/// 下载图片字节（上游 image.tmdb.org 等）并返回；URL 为空返回 404。
/// `q.maxwidth`/`q.maxheight` 非空时按比例缩放后重新编码（jpg/webp），
/// `q.quality` 控制 JPEG 质量（1-100，默认 90）。
async fn proxy_image(st: &AppState, url: Option<String>, q: ResizeQuery) -> Response {
    let Some(url) = url.filter(|u| !u.is_empty()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match st.proxy.fetch_image(&url).await {
        Ok((bytes, content_type)) => {
            // 有缩放参数才处理；否则原样转发
            let needs_resize = q.maxwidth.is_some() || q.maxheight.is_some();
            let (final_bytes, final_ct) = if needs_resize {
                resize_image(&bytes, &q).unwrap_or_else(|_| (bytes, content_type.clone()))
            } else {
                (bytes, content_type)
            };
            let mut resp = Response::new(final_bytes.into());
            let ct = header::HeaderValue::from_str(&final_ct)
                .unwrap_or(header::HeaderValue::from_static("application/octet-stream"));
            resp.headers_mut().insert(header::CONTENT_TYPE, ct);
            resp.headers_mut().insert(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("public, max-age=86400"),
            );
            resp
        }
        Err(e) => {
            tracing::warn!(error = %e, url, "image fetch failed");
            StatusCode::BAD_GATEWAY.into_response()
        }
    }
}

/// 按 maxWidth/maxHeight 等比缩放图片（jpg/webp/png 输入）。
/// 带 alpha 通道的图（logo 等）输出 PNG 保透明；无 alpha 输出 JPEG。
/// 仅当上游为这些格式时可解；失败原样返回。
fn resize_image(bytes: &[u8], q: &ResizeQuery) -> Result<(Vec<u8>, String), image::ImageError> {
    use image::ImageReader;
    let img = ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(image::ImageError::IoError)?
        .decode()?;
    let (w, h) = (img.width(), img.height());
    // 计算目标尺寸（等比缩放，同时满足 maxWidth/maxHeight 上限）
    let mw = q.maxwidth.unwrap_or(w as i64).max(1) as u32;
    let mh = q.maxheight.unwrap_or(h as i64).max(1) as u32;
    let scale = ((mw as f64 / w as f64).min(mh as f64 / h as f64)).min(1.0);
    let target_w = ((w as f64 * scale).round() as u32).max(1);
    let target_h = ((h as f64 * scale).round() as u32).max(1);
    let quality = q.quality.unwrap_or(90).clamp(1, 100) as u8;

    // 缩放：Lanczos3 保质量；无缩放时直接用原图，避免二次编码损失。
    let resized = if target_w == w && target_h == h {
        img
    } else {
        img.resize(target_w, target_h, image::imageops::FilterType::Lanczos3)
    };

    // 保透明：logo 等带 alpha 通道的图（PNG/WebP）输出 PNG，避免 JPEG 抹掉透明层
    // 导致客户端渲染成黑底。无 alpha（海报/背景/缩略，JPEG 输入）仍走 JPEG 压缩。
    if resized.color().has_alpha() {
        let mut out = Vec::new();
        resized.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)?;
        return Ok((out, "image/png".to_string()));
    }

    let mut out = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
    resized.write_with_encoder(encoder)?;
    Ok((out, "image/jpeg".to_string()))
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

    /// 左列透明、右列不透明白的 PNG（模拟 logo）。
    fn make_alpha_png(w: u32, h: u32) -> Vec<u8> {
        let mut img = image::RgbaImage::new(w, h);
        for (x, _y, p) in img.enumerate_pixels_mut() {
            *p = if x < w / 2 {
                image::Rgba([0, 0, 0, 0])
            } else {
                image::Rgba([255, 255, 255, 255])
            };
        }
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    fn make_opaque_jpeg(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::new(w, h);
        let mut out = Vec::new();
        let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90);
        image::DynamicImage::ImageRgb8(img)
            .write_with_encoder(enc)
            .unwrap();
        out
    }

    #[test]
    fn resize_alpha_png_outputs_png_with_alpha() {
        let bytes = make_alpha_png(20, 20);
        let q = ResizeQuery {
            maxwidth: Some(10),
            maxheight: None,
            quality: None,
            tag: None,
        };
        let (out, ct) = resize_image(&bytes, &q).unwrap();
        assert_eq!(ct, "image/png", "带 alpha 的图必须输出 PNG 保透明");
        let decoded = image::ImageReader::new(std::io::Cursor::new(&out))
            .with_guessed_format()
            .unwrap()
            .decode()
            .unwrap();
        assert!(decoded.color().has_alpha(), "输出应保留 alpha 通道");
    }

    #[test]
    fn resize_opaque_jpeg_outputs_jpeg() {
        let bytes = make_opaque_jpeg(16, 16);
        let q = ResizeQuery {
            maxwidth: Some(8),
            maxheight: None,
            quality: None,
            tag: None,
        };
        let (_, ct) = resize_image(&bytes, &q).unwrap();
        assert_eq!(ct, "image/jpeg", "无 alpha 的图仍走 JPEG 压缩");
    }
}
