//! 用户状态端点：收藏 / 已看 / 隐藏续看（user_item_data 写操作）。

use axum::extract::{Path, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};

use emrs_core::db::Db;
use emrs_core::emby::ViewsUserData;
use emrs_core::stores::PlaybackStore;

use super::{parse_id, resolve_item_id};
use crate::state::AppState;

/// POST|DELETE /Users/{user_id}/FavoriteItems/{item_id}：收藏/取消收藏。
/// 返回 UserData 实际字段（回读 user_item_data；Hills 等客户端解析响应体为对象，不依赖完整 Item）。
pub(super) async fn toggle_favorite(
    State(st): State<AppState>,
    method: Method,
    Path((user_id, item_id)): Path<(i64, String)>,
) -> Response {
    let Some(num_id) = parse_id(&item_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let is_favorite = method == Method::POST;

    match PlaybackStore::toggle_favorite(&st.db, user_id, num_id, is_favorite).await {
        Ok(()) => favorite_response(&st.db, user_id, num_id).await,
        Err(e) => {
            tracing::error!(error = %e, "toggle_favorite failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// POST /Users/{user_id}/FavoriteItems/{item_id}/Delete：取消收藏。
pub(super) async fn delete_favorite(
    State(st): State<AppState>,
    Path((user_id, item_id)): Path<(i64, String)>,
) -> Response {
    let Some(num_id) = parse_id(&item_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match PlaybackStore::toggle_favorite(&st.db, user_id, num_id, false).await {
        Ok(()) => favorite_response(&st.db, user_id, num_id).await,
        Err(e) => {
            tracing::error!(error = %e, "delete_favorite failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// 收藏操作的 UserData 响应体：回读 user_item_data 实际字段（对齐 Emby UserDataDto）。
/// 回读失败或无行时退回默认体，保证始终返回对象（Hills 等客户端解析 body 为对象）。
async fn favorite_response(db: &Db, user_id: i64, item_id: i64) -> Response {
    let body = match PlaybackStore::get_user_data(db, user_id, item_id).await {
        Ok(Some(d)) => ViewsUserData::from(d),
        _ => ViewsUserData::default(),
    };
    axum::Json(body).into_response()
}

/// POST|DELETE /Users/{user_id}/PlayedItems/{item_id}：标记已看/未看。
pub(super) async fn mark_played(
    State(st): State<AppState>,
    method: Method,
    Path((user_id, item_id)): Path<(i64, String)>,
) -> Response {
    let Some(target) = resolve_item_id(&st, &item_id).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let played = method == Method::POST;
    match PlaybackStore::mark_played(&st.db, user_id, target, played).await {
        Ok(()) => played_response(&st.db, user_id, target).await,
        Err(e) => {
            tracing::error!(error = %e, "mark_played failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// POST /Users/{user_id}/PlayedItems/{item_id}/Delete：标记未看。
pub(super) async fn mark_played_delete(
    State(st): State<AppState>,
    Path((user_id, item_id)): Path<(i64, String)>,
) -> Response {
    let Some(target) = resolve_item_id(&st, &item_id).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match PlaybackStore::mark_played(&st.db, user_id, target, false).await {
        Ok(()) => played_response(&st.db, user_id, target).await,
        Err(e) => {
            tracing::error!(error = %e, "mark_played_delete failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// POST /Users/{user_id}/HideFromResume/{item_id}：隐藏续看（=标记已看）。
pub(super) async fn hide_from_resume(
    State(st): State<AppState>,
    Path((user_id, item_id)): Path<(i64, String)>,
) -> Response {
    let Some(target) = resolve_item_id(&st, &item_id).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match PlaybackStore::mark_played(&st.db, user_id, target, true).await {
        Ok(()) => played_response(&st.db, user_id, target).await,
        Err(e) => {
            tracing::error!(error = %e, "hide_from_resume failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// 标记已看/未看的 JSON 响应：回读 user_item_data 实际字段（对齐 Emby UserDataDto）。
/// `item_id` 为路径原始 id（响应 ItemId 字段），`target` 为写操作落库的 item id（回读键）。
/// 回读失败或无行时退回默认体，保证始终返回对象（Hills 等客户端解析 body 为对象）。
async fn played_response(db: &Db, user_id: i64, target: i64) -> Response {
    let user_data = match PlaybackStore::get_user_data(db, user_id, target).await {
        Ok(Some(d)) => ViewsUserData::from(d),
        _ => ViewsUserData::default(),
    };
    (StatusCode::OK, axum::Json(user_data)).into_response()
}
