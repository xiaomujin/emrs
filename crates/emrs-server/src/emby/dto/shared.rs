//! 跨 DTO 复用的叶子辅助（无 `dto` 内部依赖，只依赖 `emby_proto` / `emrs_infra`）。
//!
//! - [`ItemImageFlags`]：预取的图片行 id（Primary/Backdrop/Logo/Thumb/Banner，含 Series 回退）
//! - [`provider_ids`] / [`provider_ids_map`]：ItemRow / 精简行 → Emby `ProviderIds`
//! - [`item_user_data`]：ItemRow → `ViewsUserData`（布尔 i64 → bool，play_ms → ticks）
//! - [`emby_date`]：日期字符串补全时间戳尾部
//! - [`GenreItemDto`] / [`StudioDto`]：`NameIdDto` 别名

use std::collections::HashMap;

use serde_json::json;

use emby_proto::{NameIdDto, ViewsUserData};
use emrs_infra::stores::ItemRow;
use emrs_infra::stores::image_store::ImageTypeIds;

/// Emby `GenreItems` 元素 `{Name, Id}`（= [`NameIdDto`]，alias 复用）。
pub type GenreItemDto = NameIdDto;

/// Emby `Studios` 元素 `{Id, Name}`（= [`NameIdDto`]，alias 复用）。
pub type StudioDto = NameIdDto;

/// ItemRow 的 provider 列 → Emby `ProviderIds` 字典（tmdb/imdb/tvdb，PascalCase key）。
pub(crate) fn provider_ids(item: &ItemRow) -> serde_json::Map<String, serde_json::Value> {
    provider_ids_map(
        item.tmdb_id.as_deref(),
        item.imdb_id.as_deref(),
        item.tvdb_id.as_deref(),
    )
}

/// 由 tmdb/imdb/tvdb 三个可选外部 id 构建 Emby `ProviderIds` map（空值省略）。
/// 供 `provider_ids`（`&ItemRow`）与精简行结构（如 `ResumeEntry`）共用。
pub(crate) fn provider_ids_map(
    tmdb: Option<&str>,
    imdb: Option<&str>,
    tvdb: Option<&str>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut map = serde_json::Map::new();
    if let Some(v) = tmdb.filter(|s| !s.is_empty()) {
        map.insert("Tmdb".into(), json!(v));
    }
    if let Some(v) = imdb.filter(|s| !s.is_empty()) {
        map.insert("Imdb".into(), json!(v));
    }
    if let Some(v) = tvdb.filter(|s| !s.is_empty()) {
        map.insert("Tvdb".into(), json!(v));
    }
    map
}

/// 预取的图片行 id（Primary / Backdrop / Logo / Thumb / Banner，含 Series 回退及自身 Logo/Thumb）。
///
/// 列表端点批量预取后逐 item 传入，避免 `item_to_json` 内部逐条查库（N+1）。
/// 存的是 `item_image.id`（图片表主键）：Emby 的 ImageTags/BackdropImageTags 值即图片自身的
/// 唯一标记，一部剧可有多张 Backdrop，故 `*_backdrops` 为按 id 升序的列表。
/// `series_*` 仅对 Season/Episode 有意义（回退上级剧集图片）。
#[derive(Debug, Clone, Default)]
pub struct ItemImageFlags {
    pub own_primary: Option<i64>,
    pub own_backdrops: Vec<i64>,
    pub own_logo: Option<i64>,
    pub own_thumb: Option<i64>,
    pub own_banner: Option<i64>,
    pub series_primary: Option<i64>,
    pub series_backdrops: Vec<i64>,
    pub series_logo: Option<i64>,
    pub series_thumb: Option<i64>,
}

impl ItemImageFlags {
    /// 从批量查询结果（`image_ids_batch`）组装单个 item 的图片行 id。
    pub fn from_batch(flags: &HashMap<i64, ImageTypeIds>, item: &ItemRow) -> Self {
        let own = flags.get(&item.id).cloned().unwrap_or_default();
        let series = item
            .series_id
            .and_then(|sid| flags.get(&sid).cloned())
            .unwrap_or_default();
        Self {
            own_primary: own.primary,
            own_backdrops: own.backdrops,
            own_logo: own.logo,
            own_thumb: own.thumb,
            own_banner: own.banner,
            series_primary: series.primary,
            series_backdrops: series.backdrops,
            series_logo: series.logo,
            series_thumb: series.thumb,
        }
    }
}

/// ItemRow → ViewsUserData（布尔 i64 → bool，play_ms → ticks）。
/// 供 `item_to_json` / Latest 等 DTO 复用。
pub(crate) fn item_user_data(item: &ItemRow) -> ViewsUserData {
    ViewsUserData {
        played: item.is_complete != 0,
        playback_position_ticks: item.play_ms * 10_000,
        play_count: item.play_count,
        is_favorite: item.is_favorite != 0,
        ..Default::default()
    }
}

/// 日期字符串补全时间戳尾部：`"2026-01-04"` → `"2026-01-04T00:00:00.0000000Z"`。
/// 已有完整时间戳（含 `T`）保持原样；空串原样返回（不补）。
pub(crate) fn emby_date(d: &str) -> String {
    let t = d.trim();
    if t.is_empty() {
        return d.to_string();
    }
    if t.contains('T') {
        t.to_string()
    } else {
        format!("{}T00:00:00.0000000Z", t)
    }
}

/// 测试用：构造一个 Episode ItemRow（id=1、library_id=3、series_id=9、season_id=8）。
/// 供 shared（provider_ids）与 item（item_to_json）测试复用，避免重复样板。
#[cfg(test)]
pub(crate) fn test_row() -> ItemRow {
    ItemRow {
        id: 1,
        library_id: Some(3),
        item_type: "Episode".into(),
        title: "第 5 集".into(),
        description: None,
        date_air: Some("2026-01-01".into()),
        created_at: "2026-01-01T00:00:00.0000000Z".into(),
        updated_at: "2026-03-01T00:00:00.0000000Z".into(),
        container: Some("mp4".into()),
        file_second: Some(1187),
        uuid: None,
        name: None,
        path_type: None,
        path_url: None,
        play_ms: 0,
        is_complete: 0,
        play_count: 3,
        is_favorite: 0,
        season_number: Some(1),
        episode_number: Some(5),
        series_id: Some(9),
        series_name: Some("成也萧河".into()),
        season_id: Some(8),
        season_name: Some("第 1 季".into()),
        is_virtual: 0,
        tmdb_id: Some("12345".into()),
        imdb_id: None,
        tvdb_id: Some("11864467".into()),
        community_rating: Some(8.1),
        official_rating: None,
        tagline: None,
        sort_title: None,
        end_date: None,
        status: None,
        production_year: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_ids_from_columns() {
        let m = provider_ids(&test_row());
        // 主 provider 列 PascalCase
        assert_eq!(m["Tmdb"], "12345");
        assert_eq!(m["Tvdb"], "11864467");
        assert!(!m.contains_key("Imdb"), "空列不应输出");
    }

    #[test]
    fn provider_ids_omits_empty_columns() {
        let mut r = test_row();
        r.tmdb_id = None;
        r.tvdb_id = None;
        let m = provider_ids(&r);
        assert!(m.is_empty(), "全空时输出空字典");
    }
}