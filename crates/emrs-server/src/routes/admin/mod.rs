//! Admin 仪表盘域：登录 + 管理 API + 后台页面，跨三个 zone。
//!
//! - [`public`]：`POST /admin/login`（不走 authGuard，签发 admin_session token）。
//! - [`authenticated`]：`/admin/*` 管理 API，依赖 authGuard 做管理员认证。
//! - [`root`]：`/admin` 后台页面（单文件 HTML，不参与三重前缀，登录后调用管理 API）。
//!
//! handler 实现按子域拆分：
//! - [`libraries`]：库 CRUD + 媒体列表 + 人工裁决/手动识别
//! - [`jobs`]：扫描 / 刮削 / 探测 / 监听 job
//! - [`settings`]：app_setting 读写
//! - [`login`]：管理员登录

use axum::Router;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::{get, post, put};

use crate::state::AppState;

mod jobs;
mod libraries;
mod login;
mod settings;

// handler 实现按域拆分到子模块，这里按域 glob 引入供路由注册。
use jobs::*;
use libraries::*;
use settings::*;

/// 公开组：管理员登录。
pub fn public() -> Router<AppState> {
    Router::new().route("/admin/login", post(login::admin_login))
}

/// 认证组：Admin 管理 API（挂载于 authGuard 内）。
pub fn authenticated() -> Router<AppState> {
    Router::new()
        // 库管理
        .route("/admin/libraries", get(list_libraries).post(create_library))
        .route(
            "/admin/libraries/{id}",
            get(get_library).put(update_library).delete(delete_library),
        )
        // 媒体树（库→剧→季→集，单层懒加载）
        .route("/admin/tree/children", get(list_tree_children))
        // 扫描 job（异步化 + 轮询）
        .route("/admin/library/scan/start", post(start_scan))
        .route(
            "/admin/library/scan/{id}",
            get(get_scan_job).delete(cancel_scan_job),
        )
        // 元数据刮削 job（TMDB）
        .route("/admin/library/scrape/start", post(start_scrape))
        .route("/admin/library/scrape/{id}", get(get_scrape_job))
        // 流信息回填 job（ffprobe：对缺失 file_metadata 的本地视频）
        .route("/admin/library/probe/start", post(start_probe))
        .route("/admin/library/probe/{id}", get(get_probe_job))
        // 人工裁决 + 手动识别
        .route("/admin/library/items", get(list_items_by_scrape_status))
        .route(
            "/admin/library/items/{id}/identify",
            post(manual_identify_item),
        )
        // 目录监听（notify + 增量扫描）
        .route("/admin/library/watch/start", post(start_watch))
        .route("/admin/library/watch", get(watch_status).delete(stop_watch))
        .route("/admin/settings", get(get_settings))
        .route("/admin/settings", put(set_setting))
}

/// 根级（不参与三重前缀）：管理后台页面。
pub fn root() -> Router<AppState> {
    Router::new()
        .route("/admin", get(admin_page))
        .route("/admin/", get(admin_page))
        .route("/admin/index.html", get(admin_page))
}

/// GET /admin：管理后台单文件页面（编译期内联自 assets/admin.html）。
async fn admin_page() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        include_str!("../../../assets/admin.html"),
    )
}
