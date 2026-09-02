//! 路由按**业务域**组织（system / users / items / images / playback / sessions /
//! taxonomy / admin / web）；**安全分区**（公开 / 认证+超时 JSON / 认证+流式 / 根级）
//! 的装配集中在 [`crate::app`]，两条轴各自一处，互不穿插。
//!
//! 各域按贡献到的分区暴露构造函数，命名统一：
//! - `public()`：不走 authGuard；
//! - `authenticated()`：authGuard + 30s Timeout（JSON API）；
//! - `stream()`：authGuard、无 Timeout（长播放，见 [`playback`]）；
//! - `root()`：不参与三重前缀（[`web`]、[`admin`]）。
//!
//! 跨域共享的查询参数结构与 ID 解析在 [`params`]。

pub mod admin;
pub mod images;
pub mod items;
pub mod params;
pub mod playback;
pub mod sessions;
pub mod system;
pub mod taxonomy;
pub mod users;
pub mod web;
