//! `UserItemData` → Emby `ViewsUserData` 的成型转换（裁定 C3：DTO 映射属地）。
//!
//! 原 core/emby.rs 的固有方法随行类型归属 emrs-infra 后，受孤儿规则限制
//! （固有 impl 只能落在类型所在 crate）改为 server 侧自由函数——转换逻辑
//! 仍在 DTO 成型层（server），不进 proto、不进 infra。

use emby_proto::ViewsUserData;
use emrs_infra::stores::UserItemData;

/// 成型为 Emby `ViewsUserData` 响应（`i64` 布尔 → `bool`）。
///
/// 字段语义：`played_percentage` / `unplayed_item_count` 不在此填
/// （恒 None → 省略），由列表端点按场景在 server 侧组装。
pub fn to_views_user_data(d: &UserItemData) -> ViewsUserData {
    ViewsUserData {
        played: d.played != 0,
        play_count: d.play_count,
        playback_position_ticks: d.playback_position_ticks.unwrap_or(0),
        last_played_date: d.last_played_date.clone(),
        is_favorite: d.is_favorite != 0,
        played_percentage: None,
        unplayed_item_count: None,
    }
}
