//! Admin 登录：`POST /admin/login`（不走 authGuard，签发 kind=admin 的 auth_token）。
//!
//! 登录成功/失败均写 login_event；非 admin 用户尝试登录管理面板返回 401。
//! 编排逻辑在 `emrs_service::auth::admin_login`，本路由只做 HTTP 映射。

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use emrs_service::auth::{AdminLoginError, admin_login as login_flow};

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
    match login_flow(&st.db, &body.username, &body.password).await {
        Ok(ok) => axum::Json(json!({
            "token": ok.token,
            "username": ok.username,
        }))
        .into_response(),
        Err(AdminLoginError::EmptyInput) => {
            (StatusCode::BAD_REQUEST, "用户名和密码不能为空").into_response()
        }
        Err(AdminLoginError::InvalidCredentials) => {
            (StatusCode::UNAUTHORIZED, "用户名或密码错误").into_response()
        }
        Err(AdminLoginError::NotAdmin) => {
            (StatusCode::UNAUTHORIZED, "无管理员权限").into_response()
        }
        Err(AdminLoginError::Db(e)) => {
            tracing::error!(error = %e, "admin login: db error");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
