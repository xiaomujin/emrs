//! 列表端点精简 DTO：每个列表端点只返回真实客户端（`emby_json/` 抓包）需要的字段，
//! 不再复用详情页的胖 [`super::dto::ItemDto`]。
//!
//! 裁剪基准：**以精简抓包为基线 + 零成本字段保留**——判据是 DB 成本而非严格必要性。
//! 凡随 [`ItemRow`] 载入、或由 `preload_image_flags` / `preload_child_counts` 预取、
//! 或可由 provider id 直接算出的字段（`ProviderIds` / `RunTimeTicks` / `ProductionYear`
//! / `DateCreated` / 图片标记）保留；需 `taxonomy_batch` 这一次额外查询才有的重字段
//! （`People` / `Genres` / `GenreItems` / `Studios` / `Tags`）从除 NextUp（需 People）
//! 与详情页外的所有列表移除。
//!
//! 与 [`super::latest::LatestItemJson`] 同构：`#[serde(flatten)] base: BaseItemDto` +
//! 端点专属字段、`from_row` 纯函数、零 DB 查询。字段集完全相同的端点共用一个结构体。
//!
//! 结构体 ↔ 端点：
//! - [`EpisodeCardJson`]：Shows/Episodes / Items 进入 season 分支。
//! - [`ResumeCardJson`]：`/Users/{uid}/Items/Resume`（图片各自独立、无回退）。
//! - [`SeasonCardJson`]：Shows/Seasons / Items 进入 series 分支。
//! - [`MovieSeriesCardJson`]：Items 根·库分支（Movie+Series 混合）/ Similar。
//! - [`NextUpJson`]：Shows/NextUp（唯一在列表里保留 People 的端点）。

use serde::Serialize;

use super::dto::{ItemImageFlags, PersonItemDto, item_user_data, provider_ids, provider_ids_map};
use super::{BaseItemDto, ImageTagsDto, ViewsUserData, image_tag, item_id, library_id};
use emrs_infra::stores::taxonomy_store::ItemTaxonomy;
use emrs_infra::stores::{ItemRow, ResumeEntry};

/// 由 `ItemRow` + 预取图片标志计算 Episode 顶层显示比例（横图 vs 竖海报）。
fn aspect(item: &ItemRow) -> f64 {
    if item.item_type == "Episode" {
        1.777778
    } else {
        0.666667
    }
}

/// 生产年份：刮削列优先，否则 `date_air` 前 4 位（对齐 [`super::dto::item_to_json`]）。
fn production_year(item: &ItemRow) -> Option<i64> {
    item.production_year.or_else(|| {
        item.date_air
            .as_deref()
            .and_then(|d| d.get(0..4))
            .and_then(|y| y.parse::<i64>().ok())
    })
}

/// 计算 [`ViewsUserData`]：进度百分比（`play_ms / file_second`）+ folder 未播集数（`unplayed`）。
/// 其余字段复用 [`item_user_data`]。
fn user_data(item: &ItemRow, unplayed: Option<i64>) -> ViewsUserData {
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

/// 卡片图片集（复刻 [`super::dto::item_to_json`] 的图片逻辑，供各列表结构体共用）：
/// - `image_tags`：自身 Primary；季/集无自身 Primary 时回退上级剧集 Primary；
///   自身 Logo/Thumb/Banner 存在才发。
/// - `backdrop_image_tags`：自身 Backdrop（按 id 升序）。
/// - `series_primary_image_tag` / `primary_image_item_id`：季/集回退上级剧集主图时填。
/// - `parent_backdrop_*`：季/集无自身 Backdrop 且上级剧集有 Backdrop 时指向 series。
/// - `parent_logo_*` / `parent_thumb_*`：Episode 专属，上级剧集有图时指向 series。
struct CardImages {
    image_tags: ImageTagsDto,
    backdrop_image_tags: Vec<String>,
    series_primary_image_tag: Option<String>,
    parent_backdrop_item_id: Option<String>,
    parent_backdrop_image_tags: Option<Vec<String>>,
    parent_logo_item_id: Option<String>,
    parent_logo_image_tag: Option<String>,
    parent_thumb_item_id: Option<String>,
    parent_thumb_image_tag: Option<String>,
}

fn build_card_images(item: &ItemRow, flags: &ItemImageFlags) -> CardImages {
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

/// Resume 卡片（`/Users/{uid}/Items/Resume` 专用）。
///
/// 图片**只返回该 item 自身的 Primary**（无上级回退，episode 自身无图 → `ImageTags` 为空 `{}`）；
/// `BackdropImageTags` 恒为空数组 `[]`；**不输出任何上级剧集图片字段**
/// （SeriesPrimaryImageTag / ParentBackdrop* / ParentLogo* / ParentThumb*）。
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ResumeCardJson {
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
    media_type: Option<String>,
}

impl ResumeCardJson {
    /// `primary_image_id` = 该 item 自身 Primary 图片行 id（`image_primary_batch` 预取；无图 → None）。
    pub fn from_row(server_id: &str, item: &ResumeEntry, primary_image_id: Option<i64>) -> Self {
        // ImageTags：仅自身 Primary，无回退。
        let mut image_tags = ImageTagsDto::default();
        if let Some(img_id) = primary_image_id {
            image_tags.primary = Some(image_tag(img_id));
        }
        // Resume 只查 Primary，不查 backdrop → BackdropImageTags 恒空数组。
        let backdrop_image_tags: Vec<String> = Vec::new();

        // UserData：进度（play_ms → ticks）+ 播放百分比（play_ms / file_second）。
        let mut user_data = ViewsUserData {
            played: item.is_complete != 0,
            playback_position_ticks: item.play_ms * 10_000,
            play_count: item.play_count,
            is_favorite: item.is_favorite != 0,
            ..Default::default()
        };
        if let Some(secs) = item.file_second.filter(|s| *s > 0) {
            let pct = (item.play_ms as f64 / 1000.0 / secs as f64) * 100.0;
            if pct > 0.0 {
                user_data.played_percentage = Some(pct.min(100.0));
            }
        }
        // 生产年份：刮削列优先，否则 date_air 前 4 位。
        let production_year = item.production_year.or_else(|| {
            item.date_air
                .as_deref()
                .and_then(|d| d.get(0..4))
                .and_then(|y| y.parse::<i64>().ok())
        });

        Self {
            base: BaseItemDto {
                name: item.title.clone(),
                server_id: server_id.to_string(),
                id: item_id(item.id),
                item_type: item.item_type.clone(),
                is_folder: false,
                date_created: item.created_at.clone(),
                user_data,
                primary_image_aspect_ratio: if item.item_type == "Episode" {
                    1.777778
                } else {
                    0.666667
                },
                image_tags,
                backdrop_image_tags,
                provider_ids: provider_ids_map(
                    item.tmdb_id.as_deref(),
                    item.imdb_id.as_deref(),
                    item.tvdb_id.as_deref(),
                ),
            },
            run_time_ticks: item.file_second.map(|s| s * 10_000_000),
            production_year,
            index_number: item.episode_number,
            parent_index_number: item.season_number,
            series_name: item.series_name.clone(),
            series_id: item.series_id.map(item_id),
            season_id: item.season_id.map(item_id),
            season_name: item.season_name.clone(),
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

/// NextUp 条目（Shows/NextUp 专用）。
/// 对齐 `emby_json/NextUp_yuchu.json`：Episode 卡片字段 + PremiereDate / Overview /
/// ParentId / **People**（唯一在列表保留 taxonomy 的端点）/ 空的 Chapters。
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct NextUpJson {
    #[serde(flatten)]
    base: BaseItemDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    premiere_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    overview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_time_ticks: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    index_number: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_index_number: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_id: Option<String>,
    people: Vec<PersonItemDto>,
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
    // 抓包恒为 []（无章节数据通路，与 item_to_json 一致留空数组）。
    chapters: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    media_type: Option<String>,
}

impl NextUpJson {
    pub fn from_row(
        server_id: &str,
        item: &ItemRow,
        flags: &ItemImageFlags,
        tax: &ItemTaxonomy,
    ) -> Self {
        let img = build_card_images(item, flags);
        let is_folder = matches!(item.item_type.as_str(), "Series" | "Season");
        let people: Vec<PersonItemDto> =
            tax.people.iter().map(PersonItemDto::from_person).collect();
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
            premiere_date: item.date_air.as_deref().map(super::dto::emby_date),
            overview: item.description.clone(),
            run_time_ticks: item.file_second.map(|s| s * 10_000_000),
            index_number: item.episode_number,
            parent_index_number: item.season_number,
            parent_id: item.season_id.map(item_id),
            people,
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
            chapters: Vec::new(),
            media_type: Some("Video".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 全字段填充的测试行（description / tagline 等刻意给值，验证卡片端点省略它们）。
    fn row(ty: &str) -> ItemRow {
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
    fn resume_entry(ty: &str) -> ResumeEntry {
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
    fn series_fallback_flags() -> ItemImageFlags {
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

    fn keys(v: &serde_json::Value) -> Vec<String> {
        v.as_object().unwrap().keys().cloned().collect()
    }

    /// 所有列表卡片都应省略的重字段（仅详情页 item_to_json 才有）。
    const HEAVY_FORBIDDEN: &[&str] = &[
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

    #[test]
    fn episode_card_omits_heavy_and_carries_core() {
        let out = serde_json::to_value(EpisodeCardJson::from_row(
            "srv",
            &row("Episode"),
            &series_fallback_flags(),
        ))
        .unwrap();
        let ks = keys(&out);
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
    fn resume_card_images_are_own_primary_only() {
        // episode 自身主图 → ImageTags.Primary 为自身图；BackdropImageTags 恒空数组；不输出上级剧集图片字段。
        let out = serde_json::to_value(ResumeCardJson::from_row(
            "srv",
            &resume_entry("Episode"),
            Some(500),
        ))
        .unwrap();
        assert_eq!(out["ImageTags"]["Primary"], "img-500");
        assert_eq!(out["ProviderIds"]["Tmdb"], "86031");
        assert_eq!(out["RunTimeTicks"], 12_000_000_000i64);
        assert_eq!(out["BackdropImageTags"], serde_json::json!([]));
        // ImageTags 只含 Primary（无 Logo/Thumb/Banner）。
        assert!(out["ImageTags"].get("Logo").is_none());
        assert!(out["ImageTags"].get("Thumb").is_none());
        // 已移除的上级剧集图片字段一律不出现。
        for f in [
            "SeriesPrimaryImageTag",
            "ParentBackdropItemId",
            "ParentBackdropImageTags",
            "ParentLogoItemId",
            "ParentLogoImageTag",
            "ParentThumbItemId",
            "ParentThumbImageTag",
        ] {
            assert!(
                !out.as_object().unwrap().contains_key(f),
                "Resume 卡片不应含上级剧集图片字段 {f}"
            );
        }
    }

    #[test]
    fn resume_card_no_own_primary_image_tags_empty() {
        // episode 无自身主图 → ImageTags 为空 {}（不回退上级剧集）；BackdropImageTags 仍空数组。
        let out = serde_json::to_value(ResumeCardJson::from_row(
            "srv",
            &resume_entry("Episode"),
            None,
        ))
        .unwrap();
        assert_eq!(out["ImageTags"], serde_json::json!({}));
        assert_eq!(out["BackdropImageTags"], serde_json::json!([]));
    }

    #[test]
    fn season_card_omits_heavy_and_carries_counts() {
        let out = serde_json::to_value(SeasonCardJson::from_row(
            "srv",
            &row("Season"),
            &ItemImageFlags {
                own_primary: Some(500),
                own_backdrops: vec![],
                series_backdrops: vec![901],
                ..Default::default()
            },
            Some((30, 25, None)),
        ))
        .unwrap();
        let ks = keys(&out);
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
            &row("Movie"),
            &ItemImageFlags {
                own_primary: Some(500),
                ..Default::default()
            },
        ))
        .unwrap();
        let ks = keys(&out);
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

    #[test]
    fn next_up_keeps_people_but_no_other_taxonomy() {
        let tax = ItemTaxonomy {
            genres: vec![(28, "动作".into())],
            people: vec![emrs_infra::stores::taxonomy_store::PersonBrief {
                id: 100,
                name: "演员".into(),
                role: "Actor".into(),
                character_name: Some("角色".into()),
                primary_image_id: Some(101),
            }],
            studios: vec![(5, "工作室".into())],
            tags: vec!["标签".into()],
        };
        let out = serde_json::to_value(NextUpJson::from_row(
            "srv",
            &row("Episode"),
            &series_fallback_flags(),
            &tax,
        ))
        .unwrap();
        let ks = keys(&out);
        // NextUp 唯一保留 People；其余 taxonomy（Genres/GenreItems/Studios/Tags）仍省略。
        assert!(ks.iter().any(|k| k == "People"));
        for f in [
            "Genres",
            "GenreItems",
            "Studios",
            "TagItems",
            "Tags",
            "MediaSources",
            "Width",
            "Height",
        ] {
            assert!(!ks.iter().any(|k| k == f), "NextUp 不应含 {f}");
        }
        // Episode 卡片字段 + NextUp 专属。
        for f in [
            "Overview",
            "PremiereDate",
            "ParentId",
            "Chapters",
            "SeriesPrimaryImageTag",
            "MediaType",
        ] {
            assert!(ks.iter().any(|k| k == f), "NextUp 应含 {f}");
        }
        assert_eq!(out["People"][0]["Name"], "演员");
        assert_eq!(out["People"][0]["PrimaryImageTag"], "img-101");
        assert_eq!(out["Chapters"], serde_json::json!([]));
        assert_eq!(out["ParentId"], "i-200"); // episode 父级 = season
    }
}
