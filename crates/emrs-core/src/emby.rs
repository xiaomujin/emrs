//! Emby 协议层（core 侧门面）。
//!
//! 协议原语（时间 / ID / 共享 DTO / 响应壳）在 [`emby_proto`] crate，本模块
//! 平铺 re-export，core 内 `crate::emby::format_time_now` 等路径保持不变。
//! 响应**成型**（item/列表/会话 → Emby JSON）在 emrs-server 的 `emby` 模块。
//!
//! 这里只保留必须落在领域层的转换实现：[`UserItemData`] → [`ViewsUserData`]
//! （self 为本地类型的固有方法；`From` 实现因孤儿规则无法放进 emby-proto）。

pub use emby_proto::*;

use crate::stores::UserItemData;

impl UserItemData {
    /// 成型为 Emby `ViewsUserData` 响应（`i64` 布尔 → `bool`）。
    ///
    /// 字段语义：`played_percentage` / `unplayed_item_count` 不在此填
    /// （恒 None → 省略），由列表端点按场景在 server 侧组装。
    pub fn to_views_user_data(&self) -> ViewsUserData {
        ViewsUserData {
            played: self.played != 0,
            play_count: self.play_count,
            playback_position_ticks: self.playback_position_ticks.unwrap_or(0),
            last_played_date: self.last_played_date.clone(),
            is_favorite: self.is_favorite != 0,
            played_percentage: None,
            unplayed_item_count: None,
        }
    }
}
