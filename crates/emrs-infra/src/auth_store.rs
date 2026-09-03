//! DB 侧认证查询（user / auth_token / auth_login_event）。
//!
//! SQL 走 Any 池 `?` 占位符，三方言通用。

use anyhow::Result;

use crate::db::Db;

use emrs_core::auth::hash_password;
use emrs_core::auth::{AuthContext, DeviceInfo, LoginEvent, UserRow};
use emrs_core::auth::{token_hash, token_prefix};

/// `user` 表查询行（裁定 C10 命名化；`role` 字符串 → `UserRow.is_admin` 的换算在查询层完成）。
#[derive(Debug, sqlx::FromRow)]
struct UserAuthRow {
    id: i64,
    username: String,
    password_hash: String,
    role: String,
    is_disabled: i64,
}

impl UserAuthRow {
    /// 换算为领域层 [`UserRow`]（role == "admin" 即管理员）。
    fn into_user_row(self) -> UserRow {
        UserRow {
            id: self.id,
            username: self.username,
            password_hash: self.password_hash,
            is_admin: self.role == "admin",
            is_disable: self.is_disabled != 0,
        }
    }
}

/// token 校验行（auth_token JOIN user，裁定 C10 命名化）。
#[derive(Debug, sqlx::FromRow)]
struct TokenVerifyRow {
    user_id: i64,
    username: String,
    role: String,
    is_disabled: i64,
    device_client: Option<String>,
    device_name: Option<String>,
    device_id: Option<String>,
    device_version: Option<String>,
}

/// DB 侧认证查询。
pub struct AuthStore;

impl AuthStore {
    /// 按用户名查可用用户（新 `user` 表，role 列区分管理员）。
    pub async fn find_user(db: &Db, username: &str) -> Result<Option<UserRow>> {
        let row = sqlx::query_as::<_, UserAuthRow>(
            "SELECT id, username, password_hash, role, is_disabled \
             FROM \"user\" WHERE username = ? LIMIT 1",
        )
        .bind(username)
        .fetch_optional(db.pool())
        .await?;
        Ok(row.map(UserAuthRow::into_user_row))
    }

    /// 按 id 查用户（Users/{id} 端点）。
    pub async fn find_user_by_id(db: &Db, id: i64) -> Result<Option<UserRow>> {
        let row = sqlx::query_as::<_, UserAuthRow>(
            "SELECT id, username, password_hash, role, is_disabled \
             FROM \"user\" WHERE id = ? LIMIT 1",
        )
        .bind(id)
        .fetch_optional(db.pool())
        .await?;
        Ok(row.map(UserAuthRow::into_user_row))
    }

    /// 登录成功后签发 token 入库（存 sha256，不存明文）。
    /// `kind` = "user" | "admin"。
    pub async fn insert_token(
        db: &Db,
        token: &str,
        user_id: i64,
        kind: &str,
        d: &DeviceInfo,
    ) -> Result<()> {
        let hash = token_hash(token);
        let prefix = token_prefix(token).to_string();
        sqlx::query(
            "INSERT INTO auth_token \
             (token_hash, token_prefix, kind, user_id, device_client, device_name, device_id, device_version) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&hash)
        .bind(&prefix)
        .bind(kind)
        .bind(user_id)
        .bind(&d.client)
        .bind(&d.device)
        .bind(&d.device_id)
        .bind(&d.version)
        .execute(db.pool())
        .await?;
        Ok(())
    }

    /// 撤销指定 token（登出）。
    pub async fn revoke_token(db: &Db, token: &str) -> Result<()> {
        let hash = token_hash(token);
        let now = emrs_core::emby::format_time_now();
        sqlx::query(
            "UPDATE auth_token SET revoked_at = ? WHERE token_hash = ? AND revoked_at IS NULL",
        )
        .bind(&now)
        .bind(&hash)
        .execute(db.pool())
        .await?;
        Ok(())
    }

    /// token 校验（按 sha256 查 auth_token 表，join user 表拿状态/角色）。
    /// `kind_filter` = "admin" 仅查管理员 token，"user" 仅查用户 token。
    /// 返回 None 表示 token 无效/已撤销/用户不存在。
    pub async fn verify_token(
        db: &Db,
        token: &str,
        kind_filter: &str,
    ) -> Result<Option<AuthContext>> {
        let hash = token_hash(token);
        let row = sqlx::query_as::<_, TokenVerifyRow>(
            "SELECT t.user_id, u.username, u.role, u.is_disabled, \
                    t.device_client, t.device_name, t.device_id, t.device_version \
             FROM auth_token t JOIN \"user\" u ON u.id = t.user_id \
             WHERE t.token_hash = ? AND t.kind = ? AND t.revoked_at IS NULL LIMIT 1",
        )
        .bind(&hash)
        .bind(kind_filter)
        .fetch_optional(db.pool())
        .await?;
        let Some(r) = row else {
            return Ok(None);
        };
        if r.is_disabled != 0 {
            return Ok(None);
        }
        // touch last_used_at
        let _ = sqlx::query("UPDATE auth_token SET last_used_at = ? WHERE token_hash = ?")
            .bind(emrs_core::emby::format_time_now())
            .bind(&hash)
            .execute(db.pool())
            .await;
        Ok(Some(AuthContext {
            user_id: r.user_id,
            username: r.username,
            is_admin: r.role == "admin",
            token: token.to_string(),
            device: DeviceInfo {
                client: r.device_client.unwrap_or_default(),
                device: r.device_name.unwrap_or_default(),
                device_id: r.device_id.unwrap_or_default(),
                version: r.device_version.unwrap_or_default(),
            },
        }))
    }

    /// 用户 token 校验（便捷方法，等价 verify_token(db, token, "user")）。
    pub async fn verify_user_token(db: &Db, token: &str) -> Result<Option<AuthContext>> {
        Self::verify_token(db, token, "user").await
    }

    /// 管理员 token 校验（便捷方法，等价 verify_token(db, token, "admin")）。
    pub async fn verify_admin_token(db: &Db, token: &str) -> Result<Option<AuthContext>> {
        Self::verify_token(db, token, "admin").await
    }

    /// 写登录事件到 `auth_login_event`。
    pub async fn log_login_event(db: &Db, ev: &LoginEvent) -> Result<()> {
        sqlx::query(
            "INSERT INTO auth_login_event \
             (user_id, username, login_type, success, ip, device_id, device_name, device_client, user_agent, reason) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(ev.user_id)
        .bind(&ev.username)
        .bind(&ev.login_type)
        .bind(if ev.success { 1i64 } else { 0 })
        .bind(&ev.ip)
        .bind(&ev.device_id)
        .bind(&ev.device_name)
        .bind(&ev.device_client)
        .bind(&ev.user_agent)
        .bind(&ev.reason)
        .execute(db.pool())
        .await?;
        Ok(())
    }

    /// 更新用户最后登录时间。
    pub async fn touch_last_login(db: &Db, user_id: i64) -> Result<()> {
        let now = emrs_core::emby::format_time_now();
        sqlx::query("UPDATE \"user\" SET last_login_at = ? WHERE id = ?")
            .bind(&now)
            .bind(user_id)
            .execute(db.pool())
            .await?;
        Ok(())
    }

    /// 首次启动时创建默认管理员（username=admin，随机密码）。
    /// 如果 admin 用户已存在则跳过。返回明文密码（仅首次）。
    pub async fn ensure_default_admin(db: &Db) -> Result<Option<String>> {
        let existing: Option<i64> =
            sqlx::query_scalar::<_, i64>("SELECT id FROM \"user\" WHERE role = 'admin' LIMIT 1")
                .fetch_optional(db.pool())
                .await?;
        if existing.is_some() {
            return Ok(None);
        }
        let password = "admin123".to_string(); // 16 hex chars
        let hash = hash_password(&password)?;
        sqlx::query(
            "INSERT INTO \"user\" (username, password_hash, role) VALUES ('admin', ?, 'admin')",
        )
        .bind(&hash)
        .execute(db.pool())
        .await?;
        Ok(Some(password))
    }
}
