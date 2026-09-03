//! Admin 登录：`POST /admin/login`（不走 authGuard，签发 kind=admin 的 auth_token）。
//!
//! 登录成功/失败均写 login_event；非 admin 用户尝试登录管理面板返回 401。

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use emrs_core::auth::{random_token, verify_password};

use crate::state::AppState;

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
    match emrs_infra::auth_store::AuthStore::find_user(&st.db, &body.username).await {
        Ok(Some(user)) if user.is_admin => {
            if !verify_password(&user.password_hash, &body.password) {
                let _ = emrs_infra::auth_store::AuthStore::log_login_event(
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
            if let Err(e) = emrs_infra::auth_store::AuthStore::insert_token(
                &st.db, &token, user.id, "admin", &device,
            )
            .await
            {
                tracing::error!(error = %e, "admin login: insert token failed");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
            let _ = emrs_infra::auth_store::AuthStore::touch_last_login(&st.db, user.id).await;
            let _ = emrs_infra::auth_store::AuthStore::log_login_event(
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
