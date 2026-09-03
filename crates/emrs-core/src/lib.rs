//! emrs-core：纯领域层（配置 / trait 契约 / 纯逻辑类型），不含任何 IO 实现。
//!
//! 模块：
//! - [`config`]：emrs.yml（缺失自动生成）+ 内嵌默认配置加载
//! - [`cache`]：`Cache` trait + 错误类型（实现在 emrs-infra）
//! - [`auth`]：bcrypt 密码 / token 签发校验 / 认证上下文类型（DB 查询在 emrs-infra）
//! - [`emby`]：[`emby_proto`] 门面 re-export
//! - [`cloud`]：CloudDriver trait + DriverRegistry（内置驱动实现在 emrs-infra）
//! - [`playback`]：PlaybackRouter（直链解析 + 缓存）/ ticket / block_cache

pub mod auth;
pub mod cache;
pub mod cloud;
pub mod config;
pub mod emby;
pub mod playback;
pub mod scan;
