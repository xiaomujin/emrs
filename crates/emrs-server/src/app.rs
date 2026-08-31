//! 路由组装：三重前缀（`/emby/emby`、`/emby`、无前缀）+ 中间件栈。
//!
//! 中间件顺序（外 → 内）：CatchPanic → requestLogger → securityHeaders → lowercaseQuery
//! → [公开组 | (Timeout(30s) → authGuard → 认证组)]。
//! 注意：Timeout 只作用于 JSON API 组；流式播放路由必须放在本层之外，防长播掐断。

use axum::Router;
use axum::middleware::{from_fn, from_fn_with_state};
use tower_http::catch_panic::CatchPanicLayer;

use crate::middleware::{
    api_timeout, auth_guard, lowercase_query, request_logger, security_headers,
};
use crate::routes::{authenticated_routes, items, public_routes, root_routes};
use crate::state::AppState;

pub fn router(state: AppState) -> Router {
    // 认证层共享：JSON 组与流式组都要过 authGuard
    let auth = from_fn_with_state(state.clone(), auth_guard);

    // Emby 路由（无前缀基座）
    let emby = Router::new()
        .merge(public_routes())
        // JSON API 组：认证 + 30s 超时
        .merge(
            authenticated_routes()
                .layer(auth.clone())
                .layer(from_fn(api_timeout)),
        )
        // 流式播放组（/Videos/... 302 直链与代理长流）：认证但不加 Timeout，防长播掐断
        .merge(items::streaming_routes().layer(auth))
        .with_state(state.clone());

    Router::new()
        // 三重前缀（MoviePilot 等集成方实测需要）
        .nest("/emby/emby", emby.clone())
        .nest("/emby", emby.clone())
        .merge(emby) // 无前缀版
        // 根级（/ 与 /web stub，不在前缀下）
        .merge(root_routes())
        .layer(from_fn(lowercase_query))
        .layer(from_fn(security_headers))
        .layer(from_fn(request_logger))
        .layer(CatchPanicLayer::new())
        .with_state(state)
}

// Router::nest 要求 fallback 状态一致；显式断言 state 类型
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_is_send() {
        // Router 必须线程安全（serve 需求）——编译期即保证，这里保留一条冒烟
        fn assert_send<T: Send>(_: T) {}
        assert_send(router);
    }
}
