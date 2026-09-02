//! emrs-core：领域层（配置 / 数据库 / 缓存 / 认证 / Emby 工具），不依赖任何 HTTP 框架。
//!
//! 模块：
//! - [`config`]：emrs.yml（缺失自动生成）+ 内嵌默认配置加载
//! - [`db`]：sqlx Any 统一池 + 三方言（sqlite/mysql/postgres）迁移
//! - [`cache`]：Cache trait + 内存(moka)/Redis/Valkey 三实现
//! - [`auth`]：bcrypt 密码 + token 签发/校验（user/auth_token 表）
//! - [`emby`]：[`emby_proto`] 门面 re-export + 领域层转换（`UserItemData` → `ViewsUserData`）
//! - [`stores`]：媒体库 Store 层（按领域拆分：library/item/media/image/ref/user_data）

pub mod auth;
pub mod cache;
pub mod cloud;
pub mod config;
pub mod db;
pub mod emby;
pub mod http_client;
pub mod importer;
pub mod job;
pub mod playback;
pub mod stores;
pub mod watcher;
