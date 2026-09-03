//! Emby 协议层（core 侧门面）。
//!
//! 协议原语（时间 / ID / 共享 DTO / 响应壳）在 [`emby_proto`] crate，本模块
//! 平铺 re-export，core 内 `crate::emby::format_time_now` 等路径保持不变。
//! 响应**成型**（item/列表/会话 → Emby JSON）在 emrs-server 的 `emby` 模块。

pub use emby_proto::*;
