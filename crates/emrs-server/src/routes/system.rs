//! /System/Info（需认证）：完整服务器能力声明。
//! 转码相关一律"不支持"，防客户端探测崩溃。成型走 [`crate::emby::SystemInfoDto`]。

use axum::extract::State;
use axum::response::IntoResponse;

use crate::emby::SystemInfoDto;

use crate::state::AppState;

pub async fn info(State(state): State<AppState>) -> impl IntoResponse {
    let port = state.cfg.server.port.to_string();
    axum::Json(SystemInfoDto::new(
        &state.cfg.emby.server_name,
        &state.cfg.emby.server_id,
        &port,
    ))
}
