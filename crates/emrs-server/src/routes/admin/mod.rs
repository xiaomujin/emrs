//! Admin 仪表盘路由：登录 + 库管理 + 媒体管理 + 扫描/监听 job。
//!
//! - 登录端点 `/admin/login` 不走 authGuard（签发 admin_session token）。
//! - 其余端点挂载在 `authenticated_routes()` 内，依赖 authGuard 做管理员认证。
//!
//! 模块拆分：
//! - [`libraries`]：库 CRUD + 媒体列表 + 人工裁决/手动识别
//! - [`jobs`]：扫描 / 刮削 / 探测 / 监听 job
//! - [`settings`]：app_setting 读写

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use serde::Deserialize;
use serde_json::json;

use emrs_core::auth::{random_token, verify_password};

use crate::state::AppState;

mod jobs;
mod libraries;
mod settings;

// handler 实现按域拆分到子模块，这里按域 glob 引入供 admin_routes 注册。
use jobs::*;
use libraries::*;
use settings::*;

/// Admin 路由组。
pub fn admin_routes() -> Router<AppState> {
    Router::new()
        // 库管理
        .route("/admin/libraries", get(list_libraries).post(create_library))
        .route(
            "/admin/libraries/{id}",
            get(get_library).put(update_library).delete(delete_library),
        )
        // 媒体管理
        .route("/admin/media", get(list_media))
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

// ---------------------------------------------------------------------------
// Admin 登录
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct AdminLoginReq {
    pub(crate) username: String,
    pub(crate) password: String,
}

/// POST /admin/login：管理员登录，签发 auth_token(kind=admin)。
pub(crate) async fn admin_login(
    State(st): State<AppState>,
    axum::extract::Json(body): axum::extract::Json<AdminLoginReq>,
) -> Response {
    if body.username.is_empty() || body.password.is_empty() {
        return (StatusCode::BAD_REQUEST, "用户名和密码不能为空").into_response();
    }

    // 查 user 表（role=admin）
    match emrs_core::auth::AuthStore::find_user(&st.db, &body.username).await {
        Ok(Some(user)) if user.is_admin => {
            if !verify_password(&user.password_hash, &body.password) {
                let _ = emrs_core::auth::AuthStore::log_login_event(
                    &st.db,
                    &emrs_core::auth::LoginEvent {
                        user_id: Some(user.id),
                        username: body.username.clone(),
                        login_type: "admin".to_string(),
                        success: false,
                        reason: "password mismatch".to_string(),
                        ..Default::default()
                    },
                )
                .await;
                return (StatusCode::UNAUTHORIZED, "用户名或密码错误").into_response();
            }
            // 签发 admin auth_token
            let token = random_token(16);
            let device = emrs_core::auth::DeviceInfo::default();
            if let Err(e) =
                emrs_core::auth::AuthStore::insert_token(&st.db, &token, user.id, "admin", &device)
                    .await
            {
                tracing::error!(error = %e, "admin login: insert token failed");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
            let _ = emrs_core::auth::AuthStore::touch_last_login(&st.db, user.id).await;
            let _ = emrs_core::auth::AuthStore::log_login_event(
                &st.db,
                &emrs_core::auth::LoginEvent {
                    user_id: Some(user.id),
                    username: body.username.clone(),
                    login_type: "admin".to_string(),
                    success: true,
                    ..Default::default()
                },
            )
            .await;
            axum::Json(json!({
                "token": token,
                "username": body.username,
            }))
            .into_response()
        }
        Ok(Some(_)) => {
            // 非 admin 用户试图登录管理面板
            (StatusCode::UNAUTHORIZED, "无管理员权限").into_response()
        }
        Ok(None) => (StatusCode::UNAUTHORIZED, "用户名或密码错误").into_response(),
        Err(e) => {
            tracing::error!(error = %e, "admin login: db error");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
