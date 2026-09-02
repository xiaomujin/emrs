//! 路由组装：**安全分区** × 三重前缀 + 中间件栈。
//!
//! 本文件是唯一读得懂"哪些端点走哪条安全边界"的地方；各端点的实现按业务域
//! 归在 [`crate::routes`] 下，按其贡献的分区暴露 `public()` / `authenticated()` /
//! `stream()` / `root()`，此处只做装配。
//!
//! 分区（zone）：
//! - **公开**：不走 authGuard（发现 / 登录 / 图片 / 票据播放 / admin 登录）。
//! - **认证 JSON**：authGuard + 30s Timeout。
//! - **认证流式**：authGuard、**无 Timeout**（长播放防掐断，故置于 Timeout 之外）。
//! - **根级**：`/`、`/web`、`/admin` 页，不参与三重前缀。
//!
//! 中间件顺序（外 → 内）：CatchPanic → requestLogger → securityHeaders → lowercaseQuery
//! → [各分区自身层]。

use axum::Router;
use axum::middleware::{from_fn, from_fn_with_state};
use tower_http::catch_panic::CatchPanicLayer;

use crate::middleware::{
    api_timeout, auth_guard, lowercase_query, request_logger, security_headers,
};
use crate::routes::{admin, images, items, playback, sessions, system, taxonomy, users, web};
use crate::state::AppState;

pub fn router(state: AppState) -> Router {
    // 认证层共享：JSON 组与流式组都要过 authGuard
    let auth = from_fn_with_state(state.clone(), auth_guard);

    // 公开组（无 auth）
    let public = Router::new()
        .merge(system::public())
        .merge(users::public())
        .merge(images::public())
        .merge(playback::public())
        .merge(admin::public());

    // 认证 JSON API 组：auth + 30s Timeout
    let authed_json = Router::new()
        .merge(system::authenticated())
        .merge(users::authenticated())
        .merge(taxonomy::authenticated())
        .merge(sessions::authenticated())
        .merge(items::authenticated())
        .merge(admin::authenticated())
        .layer(auth.clone())
        .layer(from_fn(api_timeout));

    // 认证流式组（/Videos/... 302 直链与代理长流）：auth 但不加 Timeout，防长播掐断
    let authed_stream = playback::stream().layer(auth);

    // Emby 路由（无前缀基座）
    let emby = Router::new()
        .merge(public)
        .merge(authed_json)
        .merge(authed_stream)
        .with_state(state.clone());

    Router::new()
        // 三重前缀（MoviePilot 等集成方实测需要）
        .nest("/emby/emby", emby.clone())
        .nest("/emby", emby.clone())
        .merge(emby) // 无前缀版
        // 根级（不参与前缀）：/ 与 /web stub、/admin 管理页
        .merge(web::root())
        .merge(admin::root())
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
