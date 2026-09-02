//! emrs-server：HTTP 层（axum + tower）+ Emby 协议成型（[`emby`] 模块）。
#![recursion_limit = "512"]
//!
//! 公开发现层 + 认证矩阵 + Sessions/空目录 stub + Items/PlaybackInfo/播放
//! + importer + admin 面。

pub mod app;
pub mod emby;
pub mod log;
pub mod middleware;
pub mod routes;
pub mod state;

pub use app::router;
pub use state::AppState;
