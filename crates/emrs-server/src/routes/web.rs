//! Web 域：根级静态入口（不参与三重前缀）。
//!
//! `/` 重定向 `/web`；`/web` stub 是客户端判定"这是 Emby 服务器"的特征路径。
//! 管理后台页面属 Admin 域（见 [`crate::routes::admin::root`]）。

use axum::Router;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;

use crate::state::AppState;

/// 根级路由（不参与三重前缀）：/ 重定向 /web，/web stub。
pub fn root() -> Router<AppState> {
    Router::new()
        .route("/", get(|| async { axum::response::Redirect::to("/web") }))
        .route("/web", get(web_stub))
        .route("/web/", get(web_stub))
        .route("/web/index.html", get(web_stub))
}

/// GET /web：HTML stub（客户端判定"这是 Emby 服务器"的特征路径）。
async fn web_stub() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        r#"<!doctype html>
<html lang="zh-CN">
<head><meta charset="utf-8"><title>emrs</title>
<meta name="viewport" content="width=device-width,initial-scale=1"></head>
<body style="margin:0;font-family:Segoe UI,Roboto,sans-serif;background:#101828;color:#e6e9ef;min-height:100vh;display:flex;align-items:center;justify-content:center">
<div style="max-width:520px;padding:32px;background:#1d2435;border-radius:12px">
<h1 style="margin:0 0 12px;font-size:20px">emrs</h1>
<p style="color:#aab2c5;line-height:1.5">Emby 兼容媒体服务器（Rust）。请使用 Emby / Infuse 等客户端连接本地址。</p>
</div></body></html>"#,
    )
}
