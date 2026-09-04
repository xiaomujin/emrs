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
        r##"<!doctype html>
<html lang="zh-CN">
<head><meta charset="utf-8"><title>EMRS</title>
<meta name="viewport" content="width=device-width,initial-scale=1"></head>
<body style="margin:0;background:#0f1420;min-height:100vh;display:flex;align-items:center;justify-content:center">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 200" width="220" height="220">
  <defs>
    <linearGradient id="g" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" style="stop-color:#f74c00"/>
      <stop offset="100%" style="stop-color:#ce4200"/>
    </linearGradient>
    <filter id="glow">
      <feGaussianBlur stdDeviation="2" result="b"/>
      <feMerge><feMergeNode in="b"/><feMergeNode in="SourceGraphic"/></feMerge>
    </filter>
  </defs>
  <style>
    @keyframes breathe { 0%,100% { transform: scale(1); opacity: .35 } 50% { transform: scale(1.06); opacity: 1 } }
    .logo { animation: breathe 4s ease-in-out infinite; transform-origin: 100px 95px }
  </style>
  <g transform="translate(0 -14)">
    <g class="logo">
      <g transform="translate(100 74) scale(0.7) translate(-100 -74)">
      <polygon points="100,18 162,52 162,120 100,154 38,120 38,52"
               fill="none" stroke="url(#g)" stroke-width="2"/>
      <g stroke="url(#g)" stroke-width="2" fill="none">
        <line x1="92" y1="18" x2="108" y2="18"/>
        <line x1="100" y1="12" x2="100" y2="24"/>
        <line x1="162" y1="44" x2="170" y2="44"/>
        <line x1="166" y1="52" x2="170" y2="48"/>
        <line x1="162" y1="128" x2="170" y2="128"/>
        <line x1="166" y1="120" x2="170" y2="124"/>
        <line x1="54" y1="154" x2="70" y2="154"/>
        <line x1="62" y1="148" x2="62" y2="160"/>
        <line x1="38" y1="44" x2="30" y2="44"/>
        <line x1="34" y1="52" x2="30" y2="48"/>
        <line x1="38" y1="128" x2="30" y2="128"/>
        <line x1="34" y1="120" x2="30" y2="124"/>
      </g>
      <g filter="url(#glow)">
        <polygon points="82,60 82,112 128,86" fill="url(#g)"/>
      </g>
    </g>
    <text x="100" y="170"
          font-family="'Segoe UI', Helvetica, Arial, sans-serif"
          font-size="30" font-weight="800" letter-spacing="6"
          fill="url(#g)" text-anchor="middle">EMRS</text>
  </g>
</svg></body></html>"##,
    )
}
