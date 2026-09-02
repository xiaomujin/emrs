//! 图片域：`/Items/{id}/Images/{*image_path}`（匿名可访问）。
//!
//! 独立成域：注册点在公开组、实现曾寄居 Items 并靠 `pub(crate)` 回露，此处归位消除反向依赖。
//! 默认（`http.image_proxy = false`）301 重定向到图片原始 URL，客户端直连上游省本机带宽；
//! 开启 `http.image_proxy` 后改为本机下载上游并返回（可按 maxWidth/maxHeight 缩放）。

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;

use crate::emby::IdKind;
use crate::state::AppState;

/// 公开组：图片端点（客户端 `<img>` 不带 token，匿名可访问）。
pub fn public() -> Router<AppState> {
    Router::new().route("/Items/{id}/Images/{*image_path}", get(item_image))
}

/// GET /Items/{id}/Images/{image_path}：图片端点。
///
/// `{id}` 命名空间按类型前缀分流：`i-{id}`/裸数字 = item.id（movie/series/season/episode）；
/// `p-{id}` = people.id（演员头像）。item.id 与 people.id 各表独立自增、数值会撞，
/// 必须前缀区分。`l-/g-/s-` 库/类型/工作室暂无图片 → 404。
/// `{image_path}` 通配：兼容 `Primary` / `Primary/0` / `primary.jpg` / `Primary/0.jpg`
/// （Emby 客户端带索引或扩展名），内部取首段、剥 `.jpg`/`.png` 等扩展名后归一化类型；
/// `/0` 索引取第 index 行（按 id 升序），与 `BackdropImageTags` 中 tag 顺序一一对应。
/// query 支持 `maxWidth`/`maxHeight`/`quality`（Emby 客户端缩放请求）。
async fn item_image(
    State(st): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<ResizeQuery>,
) -> Response {
    // tag=img-{id} 直查图片行（精确匹配，无需偏移）
    if let Some(url) = resolve_by_tag(&st, q.tag.as_deref()).await {
        return proxy_image(&st, Some(url), q).await;
    }
    StatusCode::NOT_FOUND.into_response()
}

/// 图片缩放 query 参数（宽松解析，Emby 客户端带 maxWidth/maxHeight/quality/tag）。
#[derive(serde::Deserialize, Default)]
pub(crate) struct ResizeQuery {
    #[serde(
        default,
        deserialize_with = "crate::routes::params::deserialize_lenient_i64"
    )]
    maxwidth: Option<i64>,
    #[serde(
        default,
        deserialize_with = "crate::routes::params::deserialize_lenient_i64"
    )]
    maxheight: Option<i64>,
    #[serde(
        default,
        deserialize_with = "crate::routes::params::deserialize_lenient_i64"
    )]
    quality: Option<i64>,
    #[serde(default)]
    tag: Option<String>,
}

/// 从 `tag=img-{id}` 解析图片行 id，直接查 `item_image.path_url`。
async fn resolve_by_tag(st: &AppState, tag: Option<&str>) -> Option<String> {
    let tag = tag?;
    let Some((IdKind::Image, img_id)) = crate::emby::parse_id(tag) else {
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

/// 返回图片：`http.image_proxy = false`（默认）301 重定向到原始 URL；
/// 开启时下载字节流并返回。URL 为空返回 404。
/// `q.maxwidth`/`q.maxheight` 非空时按比例缩放后重新编码（jpg/webp），
/// `q.quality` 控制 JPEG 质量（1-100，默认 90）。
/// 注意：缩放发生在本机，仅代理模式生效；重定向模式下 query 参数被忽略。
async fn proxy_image(st: &AppState, url: Option<String>, q: ResizeQuery) -> Response {
    let Some(url) = url.filter(|u| !u.is_empty()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !st.cfg.http.image_proxy {
        return Redirect::permanent(&url).into_response();
    }
    match st.http.fetch_image(&url).await {
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
