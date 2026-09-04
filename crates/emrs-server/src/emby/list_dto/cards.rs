//! Items 浏览列表的三张卡片：Episode / Season / Movie+Series。
//!
//! 三者共享 [`super::shared::build_card_images`] 的图片回退逻辑，`from_row` 纯函数零 DB 查询。

use serde::Serialize;

use super::super::dto::{ItemImageFlags, provider_ids};
use super::shared::{aspect, build_card_images, production_year, user_data};
use emby_proto::{BaseItemDto, item_id, library_id};
use emrs_infra::stores::ItemRow;

/// Episode 卡片（Shows/Episodes / Items 进入 season 分支共用）。
/// 对齐抓包：核心 + 集号/时长/上级 series·season 引用/Parent* 图，
/// 无 People / Overview / DateCreated 顶层专属 / Chapters。
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct EpisodeCardJson {
    #[serde(flatten)]
    base: BaseItemDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_time_ticks: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    production_year: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    index_number: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_index_number: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    series_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    series_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    season_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    season_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    series_primary_image_tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_backdrop_item_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_backdrop_image_tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_logo_item_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_logo_image_tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_thumb_item_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_thumb_image_tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    media_type: Option<String>,
}

impl EpisodeCardJson {
    /// `ItemRow` + 预取图片标志 → Episode 卡片（无 People/Overview 等重字段）。
    pub fn from_row(server_id: &str, item: &ItemRow, flags: &ItemImageFlags) -> Self {
        let img = build_card_images(item, flags);
        let is_folder = matches!(item.item_type.as_str(), "Series" | "Season");
        Self {
            base: BaseItemDto {
                name: item.title.clone(),
                server_id: server_id.to_string(),
                id: item_id(item.id),
                item_type: item.item_type.clone(),
                is_folder,
                date_created: item.created_at.clone(),
                user_data: user_data(item, None),
                primary_image_aspect_ratio: aspect(item),
                image_tags: img.image_tags,
                backdrop_image_tags: img.backdrop_image_tags,
                provider_ids: provider_ids(item),
            },
            run_time_ticks: item.file_second.map(|s| s * 10_000_000),
            production_year: production_year(item),
            index_number: item.episode_number,
            parent_index_number: item.season_number,
            series_name: item.series_name.clone(),
            series_id: item.series_id.map(item_id),
            season_id: item.season_id.map(item_id),
            season_name: item.season_name.clone(),
            series_primary_image_tag: img.series_primary_image_tag,
            parent_backdrop_item_id: img.parent_backdrop_item_id,
            parent_backdrop_image_tags: img.parent_backdrop_image_tags,
            parent_logo_item_id: img.parent_logo_item_id,
            parent_logo_image_tag: img.parent_logo_image_tag,
            parent_thumb_item_id: img.parent_thumb_item_id,
            parent_thumb_image_tag: img.parent_thumb_image_tag,
            media_type: Some("Video".into()),
        }
    }
}

/// Season 卡片（Shows/Seasons / Items 进入 series 分支共用）。
/// 对齐 `emby_json/Seasons_yuchu.json`：季号 / ParentBackdrop* / 子集计数，无 People / Overview。
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct SeasonCardJson {
    #[serde(flatten)]
    base: BaseItemDto,
    sort_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_id: Option<String>,
    external_urls: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    index_number: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    series_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    series_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    series_primary_image_tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_backdrop_item_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_backdrop_image_tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recursive_item_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    child_count: Option<i64>,
}

impl SeasonCardJson {
    /// `counts`：`(递归子项总数, 未播数, 直接季数)`，由 `child_counts_batch` 预取；
    /// 季：`RecursiveItemCount` = `ChildCount` = 总集数，`UserData.UnplayedItemCount` = 未播数。
    pub fn from_row(
        server_id: &str,
        item: &ItemRow,
        flags: &ItemImageFlags,
        counts: Option<(i64, i64, Option<i64>)>,
    ) -> Self {
        let img = build_card_images(item, flags);
        let (recursive_item_count, child_count, unplayed) = match counts {
            Some((total, unplayed, _)) => (Some(total), Some(total), Some(unplayed)),
            None => (None, None, None),
        };
        Self {
            base: BaseItemDto {
                name: item.title.clone(),
                server_id: server_id.to_string(),
                id: item_id(item.id),
                item_type: item.item_type.clone(),
                is_folder: true,
                date_created: item.created_at.clone(),
                user_data: user_data(item, unplayed),
                primary_image_aspect_ratio: aspect(item),
                image_tags: img.image_tags,
                backdrop_image_tags: img.backdrop_image_tags,
                provider_ids: provider_ids(item),
            },
            sort_name: item
                .sort_title
                .as_deref()
                .unwrap_or(&item.title)
                .to_string(),
            // Season 的父级是其所属 Series（Episode→season，Movie/Series→library；Season→series）。
            parent_id: item.series_id.map(item_id),
            // 季无自有外链，恒发空数组（对齐 Seasons_yuchu 抓包）。
            external_urls: Vec::new(),
            index_number: item.season_number,
            series_name: item.series_name.clone(),
            series_id: item.series_id.map(item_id),
            series_primary_image_tag: img.series_primary_image_tag,
            parent_backdrop_item_id: img.parent_backdrop_item_id,
            parent_backdrop_image_tags: img.parent_backdrop_image_tags,
            recursive_item_count,
            child_count,
        }
    }
}

/// Movie / Series 卡片（Items 根·库分支 / Similar 共用）。
/// 精简海报卡字段：年份 / 时长 / 评分 / 状态 / 子项数 / 播出季，无 People / Overview /
/// Studios / Taglines / ExternalUrls / Path。Movie 侧仅填充自身有的字段（Series 专属字段
/// 经 `skip_serializing_if` 省略）。
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct MovieSeriesCardJson {
    #[serde(flatten)]
    base: BaseItemDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    production_year: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_time_ticks: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    community_rating: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    official_rating: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    media_type: Option<String>,
}

impl MovieSeriesCardJson {
    /// `ItemRow` + 预取图片标志 → Movie/Series 海报卡（无 Overview/外链/标语）。
    pub fn from_row(server_id: &str, item: &ItemRow, flags: &ItemImageFlags) -> Self {
        let img = build_card_images(item, flags);
        let ty = item.item_type.as_str();
        let is_folder = matches!(ty, "Series" | "Season");
        // Movie/Series 的 ParentId = 所属库（l-{id}）。
        let parent_id = item.library_id.map(library_id);
        Self {
            base: BaseItemDto {
                name: item.title.clone(),
                server_id: server_id.to_string(),
                id: item_id(item.id),
                item_type: item.item_type.clone(),
                is_folder,
                date_created: item.created_at.clone(),
                user_data: user_data(item, None),
                primary_image_aspect_ratio: aspect(item),
                image_tags: img.image_tags,
                backdrop_image_tags: img.backdrop_image_tags,
                provider_ids: provider_ids(item),
            },
            production_year: production_year(item),
            run_time_ticks: item.file_second.map(|s| s * 10_000_000),
            community_rating: item.community_rating,
            official_rating: item.official_rating.clone(),
            status: item.status.clone(),
            end_date: item.end_date.clone(),
            parent_id,
            media_type: Some("Video".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::shared::{
        HEAVY_FORBIDDEN, test_keys, test_row, test_series_fallback_flags,
    };

    #[test]
    fn episode_card_omits_heavy_and_carries_core() {
        let out = serde_json::to_value(EpisodeCardJson::from_row(
            "srv",
            &test_row("Episode"),
            &test_series_fallback_flags(),
        ))
        .unwrap();
        let ks = test_keys(&out);
        for f in HEAVY_FORBIDDEN {
            assert!(!ks.iter().any(|k| k == f), "Episode 卡片不应含重字段 {f}");
        }
        // 列表抓包额外不含的字段。
        for f in [
            "Overview",
            "ExternalUrls",
            "Chapters",
            "SortName",
            "Status",
            "EndDate",
            "CommunityRating",
        ] {
            assert!(!ks.iter().any(|k| k == f), "Episode 卡片不应含 {f}");
        }
        // 核心 + TV 引用 + Parent* 图。
        for f in [
            "Name",
            "Id",
            "Type",
            "ImageTags",
            "SeriesId",
            "SeriesName",
            "SeasonId",
            "SeasonName",
            "IndexNumber",
            "ParentIndexNumber",
            "SeriesPrimaryImageTag",
            "ParentBackdropItemId",
            "ParentLogoItemId",
            "ParentThumbItemId",
            "RunTimeTicks",
            "MediaType",
            "ProviderIds",
        ] {
            assert!(ks.iter().any(|k| k == f), "Episode 卡片应含 {f}");
        }
        assert_eq!(out["Type"], "Episode");
        assert_eq!(out["ImageTags"]["Primary"], "img-900");
        assert_eq!(out["ProviderIds"]["Tmdb"], "86031");
        assert_eq!(out["RunTimeTicks"], 12_000_000_000i64);
        // 自身无主图 → 不直接暴露顶层 PrimaryImageTag（走 SeriesPrimaryImageTag）。
        assert!(!ks.iter().any(|k| k == "PrimaryImageTag"));
    }

    #[test]
    fn season_card_omits_heavy_and_carries_counts() {
        let out = serde_json::to_value(SeasonCardJson::from_row(
            "srv",
            &test_row("Season"),
            &ItemImageFlags {
                own_primary: Some(500),
                own_backdrops: vec![],
                series_backdrops: vec![901],
                ..Default::default()
            },
            Some((30, 25, None)),
        ))
        .unwrap();
        let ks = test_keys(&out);
        for f in HEAVY_FORBIDDEN {
            assert!(!ks.iter().any(|k| k == f), "Season 卡片不应含重字段 {f}");
        }
        for f in [
            "Overview",
            "Chapters",
            "RunTimeTicks",
            "CommunityRating",
            "EndDate",
            "Status",
        ] {
            assert!(!ks.iter().any(|k| k == f), "Season 卡片不应含 {f}");
        }
        for f in [
            "IndexNumber",
            "ChildCount",
            "RecursiveItemCount",
            "SeriesId",
            "SeriesName",
            "ParentId",
            "SortName",
            "ExternalUrls",
            "ImageTags",
        ] {
            assert!(ks.iter().any(|k| k == f), "Season 卡片应含 {f}");
        }
        assert_eq!(out["Type"], "Season");
        assert_eq!(out["IsFolder"], true);
        assert_eq!(out["ChildCount"], 30);
        assert_eq!(out["RecursiveItemCount"], 30);
        assert_eq!(out["ParentId"], "i-100"); // season 父级 = series
        assert_eq!(out["UserData"]["UnplayedItemCount"], 25);
        assert_eq!(out["ExternalUrls"], serde_json::json!([]));
    }

    #[test]
    fn movie_series_card_omits_heavy() {
        let out = serde_json::to_value(MovieSeriesCardJson::from_row(
            "srv",
            &test_row("Movie"),
            &ItemImageFlags {
                own_primary: Some(500),
                ..Default::default()
            },
        ))
        .unwrap();
        let ks = test_keys(&out);
        for f in HEAVY_FORBIDDEN {
            assert!(!ks.iter().any(|k| k == f), "Movie 卡片不应含重字段 {f}");
        }
        // 列表海报卡不含 Overview / 外链 / 集号 / 标语。
        for f in [
            "Overview",
            "ExternalUrls",
            "Taglines",
            "IndexNumber",
            "People",
        ] {
            assert!(!ks.iter().any(|k| k == f), "Movie 卡片不应含 {f}");
        }
        for f in [
            "Name",
            "Id",
            "Type",
            "ImageTags",
            "ProductionYear",
            "CommunityRating",
            "Status",
            "MediaType",
            "ParentId",
        ] {
            assert!(ks.iter().any(|k| k == f), "Movie 卡片应含 {f}");
        }
        assert_eq!(out["Type"], "Movie");
        assert_eq!(out["IsFolder"], false);
        assert_eq!(out["ParentId"], "l-7"); // movie 父级 = 库
    }
}