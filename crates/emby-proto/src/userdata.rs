//! Emby UserData 响应类型。
//!
//! [`ViewsUserData`] 是统一返回类型（库视图 / item 详情 / 收藏·已看端点共用）；
//! 由 DB 实体到它的 `From` 转换依赖具体存储行类型，在业务侧（emrs-core）实现。

use serde::Serialize;

/// Emby UserData DTO（统一返回类型）：库视图 / item 详情 / 收藏·已看端点共用。
///
/// 字段语义分两类：`last_played_date` / `played_percentage` 保留旧「恒发 null」
/// 语义（无 `skip`，与 Emby 客户端 null==absent 容忍一致）；`unplayed_item_count`
/// 仅 folder 项（Season/Series）有值，其余**省略**（`skip_serializing_if`）——
/// 真实 Emby Episode 的 UserData 不带 `UnplayedItemCount` 键。
#[derive(Serialize, Default)]
#[serde(rename_all = "PascalCase")]
pub struct ViewsUserData {
    pub playback_position_ticks: i64,
    pub is_favorite: bool,
    pub played: bool,
    pub play_count: i64,
    pub last_played_date: Option<String>,
    pub played_percentage: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unplayed_item_count: Option<i64>,
}
