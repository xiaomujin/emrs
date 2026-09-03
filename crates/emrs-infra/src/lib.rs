//! emrs-infra：IO 实现层（数据库 / stores / 缓存实现 / 出网 / 驱动 / 扫描 / 监听）。
//!
//! 模块：
//! - [`db`]：sqlx Any 统一池 + 三方言迁移（migrations/ 编译期内嵌）
//! - [`stores`]：媒体库 Store 层（行类型 + SQL 读写唯一属主）
//! - [`auth_store`]：AuthStore（DB 侧 user/auth_token/auth_login_event 查询）
//! - [`cache`]：`Cache` trait 的 memory/redis/valkey 实现 + 双层门面 + 工厂
//! - [`cloud`]：内置 http 直链驱动 + 默认注册表构造
//! - [`http_client`]：统一出网（`Outbound` / `HttpClient`）
//! - [`scanner`]：目录扫描器（fs 遍历 + 幂等落库）+ TMDB 刮削落库面
//! - [`filename`] / [`nfo`] / [`strm`] / [`probe`] / [`tmdb`]：扫描期解析与探测
//! - [`watcher`]：库目录监听（notify + debounce，经 `ScanWaker` trait 反向唤醒 service）
//! - [`block_cache`]：磁盘分块缓存（热点区间加速，filetime 淘汰）

pub mod auth_store;
pub mod block_cache;
pub mod cache;
pub mod cloud;
pub mod db;
pub mod filename;
pub mod http_client;
pub mod nfo;
pub mod probe;
pub mod scanner;
pub mod stores;
pub mod strm;
pub mod tmdb;
pub mod watcher;
