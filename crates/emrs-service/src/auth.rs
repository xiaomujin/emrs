//! 认证编排（Phase 3）：登录验密 → 签发 token → 写登录事件的流程收拢。
//!
//! 自 server `routes/admin/login.rs` 平移；HTTP 状态码与文案的映射留在 server。

use tracing::error;

use emrs_core::auth::{DeviceInfo, LoginEvent, random_token, verify_password};
use emrs_infra::auth_store::AuthStore;
use emrs_infra::db::Db;

/// 管理员登录成功结果（token 为明文，仅此一次返回）。
#[derive(Debug, Clone)]
pub struct AdminLoginSuccess {
    pub token: String,
    pub username: String,
}

/// 管理员登录失败（server 按变体映射 HTTP 状态与文案）。
#[derive(Debug, thiserror::Error)]
pub enum AdminLoginError {
    #[error("用户名和密码不能为空")]
    EmptyInput,
    /// 用户不存在或密码错误（同一文案，防用户名枚举；密码错时已写 failed 登录事件）。
    #[error("用户名或密码错误")]
    InvalidCredentials,
    /// 非 admin 用户试图登录管理面板。
    #[error("无管理员权限")]
    NotAdmin,
    /// DB / 写 token 失败（server 统一 500）。
    #[error(transparent)]
    Db(#[from] anyhow::Error),
}

/// 管理员登录编排：验空 → 查 user（role=admin）→ 验密 → 签发
/// `auth_token(kind=admin)` → touch_last_login → 写登录事件。
///
/// 成功/失败的登录事件均落库（best-effort，与迁移前逐字一致）。
pub async fn admin_login(
    db: &Db,
    username: &str,
    password: &str,
) -> Result<AdminLoginSuccess, AdminLoginError> {
    if username.is_empty() || password.is_empty() {
        return Err(AdminLoginError::EmptyInput);
    }

    // 查 user 表（role=admin）
    let user = match AuthStore::find_user(db, username).await {
        Ok(Some(user)) if user.is_admin => user,
        Ok(Some(_)) => return Err(AdminLoginError::NotAdmin),
        Ok(None) => return Err(AdminLoginError::InvalidCredentials),
        Err(e) => return Err(AdminLoginError::Db(e)),
    };

    if !verify_password(&user.password_hash, password) {
        let _ = AuthStore::log_login_event(
            db,
            &LoginEvent {
                user_id: Some(user.id),
                username: username.to_string(),
                login_type: "admin".to_string(),
                success: false,
                reason: "password mismatch".to_string(),
                ..Default::default()
            },
        )
        .await;
        return Err(AdminLoginError::InvalidCredentials);
    }

    // 签发 admin auth_token
    let token = random_token(16);
    let device = DeviceInfo::default();
    if let Err(e) = AuthStore::insert_token(db, &token, user.id, "admin", &device).await {
        error!(error = %e, "admin login: insert token failed");
        return Err(AdminLoginError::Db(e));
    }
    let _ = AuthStore::touch_last_login(db, user.id).await;
    let _ = AuthStore::log_login_event(
        db,
        &LoginEvent {
            user_id: Some(user.id),
            username: username.to_string(),
            login_type: "admin".to_string(),
            success: true,
            ..Default::default()
        },
    )
    .await;

    Ok(AdminLoginSuccess {
        token,
        username: username.to_string(),
    })
}
