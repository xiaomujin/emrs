//! Items 域：Item 本体浏览 / 详情 / 用户视图 / 续播 / 最新 / 剧集三件套 / 相似推荐 /
//! PlaybackInfo / 用户状态（收藏·已看·隐藏续看）。
//!
//! 全部走认证 + Timeout（authed JSON API zone）。`/Users/{uid}/Items*` 虽在 Users 前缀下，
//! 但语义是 item 查询、与列表渲染助手共享，故归本域（Users 域只留登录/发现/用户详情）。
//!
//! ItemId 带类型前缀（`i-`/`l-`/`p-`/`g-`/`s-`），DB 存裸 i64、前缀纯传输层；
//! 格式化/解析统一走 [`crate::emby::id`]（裸数字不再兼容，一律视为非法）。
//!
//! 子模块：
//! - [`list`]：Items 列表 / 详情 / 用户视图 / 续播 / 最新 / 剧集 / 相似 + 共享渲染助手
//! - [`playback_info`]：PlaybackInfo + strm 后台回填
//! - [`user_data`]：收藏 / 已看 / 隐藏续看
//!
//! 注：视频流式与短票据播放见 [`crate::routes::playback`]；图片端点见 [`crate::routes::images`]；
//! 播放进度上报见 [`crate::routes::sessions`]。

use axum::Router;
use axum::routing::{get, post};

// 共享内核：向子模块再导出（子模块经 `use super::{...}` 引用）。
pub(crate) use crate::routes::params::{
    ItemsQuery, SeasonQuery, parse_generic_id, parse_genre_ids, parse_id, parse_item_ids,
    parse_person_ids, parse_studio_ids, resolve_item_id,
};
use crate::state::AppState;

mod list;
mod playback_info;
mod user_data;

use list::*;
use playback_info::{playback_info, playback_info_by_user};
use user_data::{
    delete_favorite, hide_from_resume, mark_played, mark_played_delete, toggle_favorite,
};

/// 认证组：Items 系列端点（受 Timeout 层约束）。
pub fn authenticated() -> Router<AppState> {
    Router::new()
        // Items 列表
        // .route("/Items", get(items_list))
        // .route("/Items/{id}", get(item_by_id))
        .route(
            "/Items/{id}/PlaybackInfo",
            get(playback_info).post(playback_info),
        )
        // Emby 带 userId 的 PlaybackInfo 别名（部分客户端用此路径）
        .route(
            "/Users/{user_id}/Items/{item_id}/PlaybackInfo",
            get(playback_info_by_user).post(playback_info_by_user),
        )
        // 用户 Items
        .route("/Users/{user_id}/Views", get(users_views))
        .route("/Users/{user_id}/Items", get(users_items))
        .route("/Users/{user_id}/Items/Resume", get(users_resume))
        .route("/Users/{user_id}/Items/Latest", get(users_latest))
        .route("/Users/{user_id}/Items/{item_id}", get(users_item_by_id))
        // 收藏
        .route(
            "/Users/{user_id}/FavoriteItems/{item_id}",
            post(toggle_favorite).delete(toggle_favorite),
        )
        .route(
            "/Users/{user_id}/FavoriteItems/{item_id}/Delete",
            post(delete_favorite),
        )
        // 已看
        .route(
            "/Users/{user_id}/PlayedItems/{item_id}",
            post(mark_played).delete(mark_played),
        )
        .route(
            "/Users/{user_id}/PlayedItems/{item_id}/Delete",
            post(mark_played_delete),
        )
        .route(
            "/Users/{user_id}/HideFromResume/{item_id}",
            post(hide_from_resume),
        )
        // 剧集
        .route("/Shows/NextUp", get(shows_next_up))
        .route("/Shows/{id}/Seasons", get(shows_seasons))
        .route("/Shows/{id}/Episodes", get(shows_episodes))
        // 相似推荐（Hills 详情页）
        .route("/Items/{id}/Similar", get(item_similar))
}
