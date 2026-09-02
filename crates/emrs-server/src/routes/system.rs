//! System 域：服务器信息 + 存活探测。
//!
//! - [`public`]：`/System/Info/Public`（匿名发现第一跳）、`/System/Ping`（存活探测）。
//! - [`authenticated`]：`/System/Info` + `/System/Info/Query`（完整能力声明，需认证）。
//!
//! 转码相关一律"不支持"，防客户端探测崩溃。成型走 [`crate::emby::SystemInfoDto`]。

use axum::Router;
use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;

use crate::emby::{SystemInfoDto, SystemInfoPublicDto};
use crate::state::AppState;

/// 公开组：Info/Public + Ping。
pub fn public() -> Router<AppState> {
    Router::new()
        .route("/System/Info/Public", get(info_public))
        .route("/System/Ping", get(ping).head(ping))
}

/// 认证组：Info + Info/Query。
pub fn authenticated() -> Router<AppState> {
    Router::new()
        .route("/System/Info", get(info))
        .route("/System/Info/Query", get(info))
}

/// GET /System/Info/Public：匿名探测（Infuse/Senplayer 发现第一跳）。
async fn info_public(State(state): State<AppState>) -> impl IntoResponse {
    axum::Json(SystemInfoPublicDto::new(
        &state.cfg.emby.server_name,
        &state.cfg.emby.server_id,
    ))
}

/// GET|HEAD /System/Ping：文本应答。
async fn ping() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "emrs Server",
    )
}

/// GET /System/Info：完整服务器能力声明。
async fn info(State(state): State<AppState>) -> impl IntoResponse {
    let port = state.cfg.server.port.to_string();
    axum::Json(SystemInfoDto::new(
        &state.cfg.emby.server_name,
        &state.cfg.emby.server_id,
        &port,
    ))
}
