//! 认证领域：bcrypt 密码、随机 token、认证上下文类型。
//!
//! 统一 `user` 表（role 列区分 admin/user/managed），
//! token 存 sha256 到 `auth_token`（kind=admin|user），
//! 登录事件写 `auth_login_event`。
//! 命中顺序：master API key（config）→ `auth_token(admin)` → `auth_token(user)`。
//!
//! 模块拆分：
//! - [`password`]：bcrypt 哈希/校验
//! - [`token`]：随机 token 生成 + sha256 哈希 + 前缀
//! - [`context`]：DeviceInfo / AuthContext / UserRow / LoginEvent 类型
//!
//! DB 侧认证查询（AuthStore）在 emrs-infra `auth_store` 模块。

mod context;
mod password;
mod token;

pub use context::{AuthContext, DeviceInfo, LoginEvent, UserRow};
pub use password::{hash_password, verify_password};
pub use token::{random_token, token_hash, token_prefix};
