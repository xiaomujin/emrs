//! 列表卡片共享成型助手（`list_dto` 内部复用，不向上泄漏到 emby 其余部分）。
//!
//! - [`aspect`] / [`production_year`] / [`user_data`]：ItemRow → 海报比例 / 年份 / UserData
//! - [`CardImages`] / [`build_card_images`]：复刻 [`super::super::dto::item_to_json`] 的图片回退逻辑
//! - 测试样板（[`test_row`] / [`test_resume_entry`] / [`test_series_fallback_flags`] /
//!   [`test_keys`] / [`HEAVY_FORBIDDEN`]）由各卡片测试子模块复用，`#[cfg(test)]` 下供内部使用。

use super::super::dto::{ItemImageFlags, item_user_data};
use emby_proto::{ImageTagsDto, ViewsUserData, image_tag, item_id};
use emrs_infra::stores::ItemRow;

/// 由 `ItemRow` + 预取图片标志计算顶层显示比例（横图 vs 竖海报）。
pub(super) fn aspect(item: &ItemRow) -> f64 {
    if item.item_type == "Episode" {
        1.777778
    } else {
        0.666667
    }
}

/// 生产年份：刮削列优先，否则 `date_air` 前 4 位（对齐 [`super::super::dto::item_to_json`]）。
pub(super) fn production_year(item: &ItemRow) -> Option<i64> {
    item.production_year.or_else(|| {
        item.date_air
            .as_deref()
            .and_then(|d| d.get(0..4))
            .and_then(|y| y.parse::<i64>().ok())
    })
}

/// 计算 [`ViewsUserData`]：进度百分比（`play_ms / file_second`）+ folder 未播集数（`unplayed`）。
/// 其余字段复用 [`item_user_data`]。
pub(super) fn user_data(item: &ItemRow, unplayed: Option<i64>) -> ViewsUserData {
    let mut ud = item_user_data(item);
    if let Some(secs) = item.file_second.filter(|s| *s > 0) {
        let pct = (item.play_ms as f64 / 1000.0 / secs as f64) * 100.0;
        if pct > 0.0 {
            ud.played_percentage = Some(pct.min(100.0));
        }
    }
    ud.unplayed_item_count = unplayed;
    ud
}

/// 卡片图片集（复刻 [`super::super::dto::item_to_json`] 的图片逻辑，供各列表结构体共用）：
/// - `image_tags`：自身 Primary；季/集无自身 Primary 时回退上级剧集 Primary；
///   自身 Logo/Thumb/Banner 存在才发。
/// - `backdrop_image_tags`：自身 Backdrop（按 id 升序）。
/// - `series_primary_image_tag` / `parent_backdrop_*`：季/集回退上级剧集主图 / 背板时填。
/// - `parent_logo_*` / `parent_thumb_*`：Episode 专属，上级剧集有图时指向 series。
pub(super) struct CardImages {
    pub(super) image_tags: ImageTagsDto,
    pub(super) backdrop_image_tags: Vec<String>,
    pub(super) series_primary_image_tag: Option<String>,
    pub(super) parent_backdrop_item_id: Option<String>,
    pub(super) parent_backdrop_image_tags: Option<Vec<String>>,
    pub(super) parent_logo_item_id: Option<String>,
    pub(super) parent_logo_image_tag: Option<String>,
    pub(super) parent_thumb_item_id: Option<String>,
    pub(super) parent_thumb_image_tag: Option<String>,
}

/// 组装卡片图片集合：复刻详情页 `item_to_json` 的图片回退规则（仅列表卡片需要的子集）。
pub(super) fn build_card_images(item: &ItemRow, flags: &ItemImageFlags) -> CardImages {
    let ty = item.item_type.as_str();
    let is_episode = ty == "Episode";
    let is_season = ty == "Season";
    let series_id_str = item.series_id.map(item_id);

    let mut image_tags = ImageTagsDto::default();
    let mut series_primary_image_tag = None;
    if let Some(img_id) = flags.own_primary {
        image_tags.primary = Some(image_tag(img_id));
    } else if (is_season || is_episode)
        && let Some(img_id) = flags.series_primary
    {
        image_tags.primary = Some(image_tag(img_id));
        series_primary_image_tag = image_tags.primary.clone();
    }
    if let Some(img_id) = flags.own_logo {
        image_tags.logo = Some(image_tag(img_id));
    }
    if let Some(img_id) = flags.own_thumb {
        image_tags.thumb = Some(image_tag(img_id));
    }
    if let Some(img_id) = flags.own_banner {
        image_tags.banner = Some(image_tag(img_id));
    }

    let backdrop_image_tags: Vec<String> = flags
        .own_backdrops
        .iter()
        .map(|id| image_tag(*id))
        .collect();

    // Parent Backdrop 回退：季/集自身无 Backdrop、上级剧集有 → 指向 series。
    let parent_backdrop_item_id = if flags.own_backdrops.is_empty()
        && (is_season || is_episode)
        && series_id_str.is_some()
        && !flags.series_backdrops.is_empty()
    {
        series_id_str.clone()
    } else {
        None
    };
    let parent_backdrop_image_tags = parent_backdrop_item_id.as_ref().map(|_| {
        flags
            .series_backdrops
            .iter()
            .map(|id| image_tag(*id))
            .collect()
    });

    // Parent Logo / Thumb：Episode 专属，tag 为 series 图片行 id。
    let parent_logo_item_id = if is_episode && flags.series_logo.is_some() {
        series_id_str.clone()
    } else {
        None
    };
    let parent_thumb_item_id = if is_episode && flags.series_thumb.is_some() {
        series_id_str.clone()
    } else {
        None
    };
    let parent_logo_image_tag = if is_episode {
        flags.series_logo.map(image_tag)
    } else {
        None
    };
    let parent_thumb_image_tag = if is_episode {
        flags.series_thumb.map(image_tag)
    } else {
        None
    };

    CardImages {
        image_tags,
        backdrop_image_tags,
        series_primary_image_tag,
        parent_backdrop_item_id,
        parent_backdrop_image_tags,
        parent_logo_item_id,
        parent_logo_image_tag,
        parent_thumb_item_id,
        parent_thumb_image_tag,
    }
}

#[cfg(test)]
use emrs_infra::stores::ResumeEntry;

/// 全字段填充的测试行（description / tagline 等刻意给值，验证卡片端点省略它们）。
#[cfg(test)]
pub(crate) fn test_row(ty: &str) -> ItemRow {
    ItemRow {
        id: 42,
        library_id: Some(7),
        item_type: ty.into(),
        title: "标题".into(),
        description: Some("这是一段很长的简介".into()),
        date_air: Some("2019-01-01".into()),
        created_at: "2026-01-20T00:17:47.0000000Z".into(),
        updated_at: "2026-01-21T00:00:00.0000000Z".into(),
        container: Some("mkv".into()),
        file_second: Some(1200),
        uuid: Some("u-1".into()),
        name: None,
        path_type: None,
        path_url: None,
        play_ms: 60_000,
        is_complete: 0,
        play_count: 1,
        is_favorite: 0,
        season_number: Some(2),
        episode_number: Some(6),
        series_id: Some(100),
        series_name: Some("剧名".into()),
        season_id: Some(200),
        season_name: Some("第 2 季".into()),
        is_virtual: 0,
        tmdb_id: Some("86031".into()),
        imdb_id: Some("tt123".into()),
        tvdb_id: None,
        community_rating: Some(7.8),
        official_rating: Some("PG-13".into()),
        tagline: Some("标语".into()),
        sort_title: Some("排序".into()),
        end_date: Some("2024-09-21".into()),
        status: Some("Continuing".into()),
        production_year: Some(2019),
    }
}

/// Resume 精简行构造（Episode/Movie），对齐 `resume_entry` 消费字段。
#[cfg(test)]
pub(crate) fn test_resume_entry(ty: &str) -> ResumeEntry {
    ResumeEntry {
        id: 42,
        item_type: ty.into(),
        title: "标题".into(),
        created_at: "2026-01-20T00:17:47.0000000Z".into(),
        play_ms: 60_000,
        is_complete: 0,
        play_count: 1,
        is_favorite: 0,
        file_second: Some(1200),
        production_year: Some(2019),
        date_air: Some("2019-01-01".into()),
        season_number: Some(2),
        episode_number: Some(6),
        series_id: Some(100),
        series_name: Some("剧名".into()),
        season_id: Some(200),
        season_name: Some("第 2 季".into()),
        tmdb_id: Some("86031".into()),
        imdb_id: Some("tt123".into()),
        tvdb_id: None,
    }
}

/// 季/集回退上级剧集图片：自身无主图，series 有 primary/backdrop/logo/thumb。
#[cfg(test)]
pub(crate) fn test_series_fallback_flags() -> ItemImageFlags {
    ItemImageFlags {
        own_primary: None,
        own_backdrops: vec![],
        own_logo: None,
        own_thumb: None,
        own_banner: None,
        series_primary: Some(900),
        series_backdrops: vec![901],
        series_logo: Some(902),
        series_thumb: Some(903),
    }
}

#[cfg(test)]
pub(crate) fn test_keys(v: &serde_json::Value) -> Vec<String> {
    v.as_object().unwrap().keys().cloned().collect()
}

/// 所有列表卡片都应省略的重字段（仅详情页 item_to_json 才有）。
#[cfg(test)]
pub(crate) const HEAVY_FORBIDDEN: &[&str] = &[
    "People",
    "Genres",
    "GenreItems",
    "Studios",
    "TagItems",
    "Tags",
    "Taglines",
    "Path",
    "FileName",
    "MediaSources",
    "Width",
    "Height",
    "LocationType",
    "DateModified",
    "PresentationUniqueKey",
    "DisplayPreferencesId",
    "LockedFields",
    "LockData",
    "CanDelete",
    "CanDownload",
    "OriginalTitle",
];