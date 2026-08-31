//! 认证身份与用户/登录事件行类型。

/// 设备上下文（X-Emby-Authorization 解析结果）。
#[derive(Debug, Clone, Default)]
pub struct DeviceInfo {
    pub client: String,
    pub device: String,
    pub device_id: String,
    pub version: String,
}

/// 认证身份：由 guard 解析、注入 request extensions。
#[derive(Debug, Clone)]
pub struct AuthContext {
    /// 0 表示 API key/管理会话（非真实用户）
    pub user_id: i64,
    pub username: String,
    pub is_admin: bool,
    pub token: String,
    pub device: DeviceInfo,
}

/// 用户行（登录查询）。
#[derive(Debug, Clone)]
pub struct UserRow {
    pub id: i64,
    pub username: String,
    pub password_hash: String,
    pub is_admin: bool,
    pub is_disable: bool,
}

/// 登录事件记录参数。
#[derive(Debug, Clone, Default)]
pub struct LoginEvent {
    pub user_id: Option<i64>,
    pub username: String,
    /// "user" | "admin"
    pub login_type: String,
    pub success: bool,
    pub ip: String,
    pub device_id: String,
    pub device_name: String,
    pub device_client: String,
    pub user_agent: String,
    pub reason: String,
}
