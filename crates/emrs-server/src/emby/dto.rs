//! Emby 协议 DTO 成型：`ItemRow` / `MediaSourceRow` → Emby JSON。
//!
//! 从 HTTP 层下沉到 core：协议成型属领域层职责，不依赖 axum / `AppState`。
//! 调用方（`emrs-server`）传入 `server_id` + `signing_key`。
//! `item_to_json` 为纯函数，图片存在标志由调用方批量预取（[`ItemImageFlags`]）；
//! 需要 DB 的媒体源成型（`attach_media_sources` 等）才接收 `&Db`。

use std::collections::HashMap;

use serde::Serialize;
use serde_json::json;

use super::{
    BaseItemDto, ImageTagsDto, NameIdDto, NameIdTypeDto, ViewsUserData, genre_id, image_tag,
    item_id, library_id, person_id, studio_id,
};
use emrs_core::db::Db;
use emrs_core::playback::ticket::{TicketClaims, issue_ticket};
use emrs_core::stores::{
    ItemRow, ItemsStore, MediaSourceRow, StreamInfo,
    image_store::ImageTypeIds,
    taxonomy_store::{ItemTaxonomy, PersonBrief, PersonRow},
};

/// Emby Person 详情 DTO（`/Users/{uid}/Items/p-{id}`）。
///
/// flatten [`NameIdTypeDto`]（Name/Id/Type）。`primary_image_id`（头像图片行 id）
/// 存在时 `ImageTags.Primary` = `img-{图片行 id}`；`PremiereDate`/`ProductionYear`
/// 仅 birthday 存在时发（`ProductionYear` 取 birthday 前 4 位 parse，失败→0）；
/// `Overview` 仅 description 存在时发。
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct PersonDetailDto {
    #[serde(flatten)]
    pub name_id_type: NameIdTypeDto,
    pub server_id: String,
    pub production_locations: Vec<String>,
    pub provider_ids: serde_json::Map<String, serde_json::Value>,
    pub image_tags: ImageTagsDto,
    pub backdrop_image_tags: Vec<String>,
    pub primary_image_aspect_ratio: f64,
    pub date_created: String,
    pub date_modified: String,
    pub external_urls: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub premiere_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub production_year: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overview: Option<String>,
}

/// PersonRow → Emby Person 详情 DTO。
///
/// Id 用 `p-{id}` 前缀，与 `attach_taxonomy` 的 People 数组、`/Persons` 列表、
/// `/Items/{id}/Images/p-{id}` 图片路由保持一致。`primary_image_id` 为人员头像
/// 图片行 id（`item_image.parent_type='people'`，无则 None）；tag 标识图片本身。
pub fn person_to_json(
    server_id: &str,
    person: &PersonRow,
    primary_image_id: Option<i64>,
) -> PersonDetailDto {
    let id = person_id(person.id);

    let image_tags = match primary_image_id {
        Some(img_id) => ImageTagsDto {
            primary: Some(image_tag(img_id)),
            ..Default::default()
        },
        None => ImageTagsDto::default(),
    };

    let mut provider_ids = serde_json::Map::new();
    if let Some(t) = person.tmdb_id.as_deref().filter(|s| !s.is_empty()) {
        provider_ids.insert("Tmdb".into(), json!(t));
    }

    let (premiere_date, production_year) = match &person.birthday {
        Some(birthday) => {
            let year = birthday.get(0..4).map(|y| y.parse::<i64>().unwrap_or(0));
            (Some(birthday.clone()), year)
        }
        None => (None, None),
    };

    PersonDetailDto {
        name_id_type: NameIdTypeDto {
            name_id: NameIdDto {
                name: person.name.clone(),
                id,
            },
            item_type: "Person".into(),
        },
        server_id: server_id.to_string(),
        production_locations: Vec::new(),
        provider_ids,
        image_tags,
        backdrop_image_tags: Vec::new(),
        primary_image_aspect_ratio: 0.666667,
        date_created: person.created_at.clone(),
        date_modified: person.updated_at.clone(),
        external_urls: Vec::new(),
        premiere_date,
        production_year,
        overview: person.description.clone(),
    }
}

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
pub(super) fn item_user_data(item: &ItemRow) -> ViewsUserData {
    ViewsUserData {
        played: item.is_complete != 0,
        playback_position_ticks: item.play_ms * 10_000,
        play_count: item.play_count,
        is_favorite: item.is_favorite != 0,
        ..Default::default()
    }
}

/// Emby `GenreItems` 元素 `{Name, Id}`（= [`NameIdDto`]，alias 复用）。
pub type GenreItemDto = NameIdDto;

/// Emby `Studios` 元素 `{Id, Name}`（= [`NameIdDto`]，alias 复用）。
pub type StudioDto = NameIdDto;

/// Emby `ExternalUrls` 元素 `{Name, Url}`（Movie/Series 外链）。
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ExternalUrlDto {
    pub name: String,
    pub url: String,
}

/// Emby `People` 元素（演职员）。`Character` / `PrimaryImageTag` 仅当存在时输出。
/// flatten [`NameIdDto`]（Name/Id）+ `Role` + `Type` + 可选 `Character` / `PrimaryImageTag`。
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct PersonItemDto {
    #[serde(flatten)]
    name_id: NameIdDto,
    role: String,
    #[serde(rename = "Type")]
    item_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    character: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    primary_image_tag: Option<String>,
}

impl PersonItemDto {
    /// `PersonBrief` → `People` 数组元素（Name/Id/Role/Type + 可选 Character/PrimaryImageTag）。
    /// 供 `item_to_json` / `LatestItemJson` 复用，两处 taxonomy 折入同构。
    pub(crate) fn from_person(p: &PersonBrief) -> Self {
        Self {
            name_id: NameIdDto {
                name: p.name.clone(),
                id: person_id(p.id),
            },
            role: p.role.clone(),
            item_type: "Person".into(),
            character: p.character_name.clone(),
            primary_image_tag: p.primary_image_id.map(image_tag),
        }
    }
}

/// Emby `MediaSources` 元素（详情 PlaybackInfo / item 详情）。
///
/// `Chapters` 留 `Vec<Value>`（`file_chapters` JSONB 任意结构，无法静态类型化）；
/// `RequiredHttpHeaders` 恒空对象；`Bitrate` / `DefaultAudioStreamIndex` /
/// `DefaultSubtitleStreamIndex` 为 `Option` 且 **skip**（无值时省略，对齐真实 Emby；
/// 部分客户端对 `null` 数值字段解析会报 SerializationException）。
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct MediaSourceDto {
    protocol: String,
    id: String,
    path: String,
    #[serde(rename = "Type")]
    source_type: String,
    container: String,
    name: String,
    is_remote: bool,
    has_mixed_protocols: bool,
    size: i64,
    run_time_ticks: i64,
    supports_transcoding: bool,
    supports_direct_stream: bool,
    supports_direct_play: bool,
    is_infinite_stream: bool,
    requires_opening: bool,
    requires_closing: bool,
    requires_looping: bool,
    supports_probing: bool,
    media_streams: Vec<MediaStreamDto>,
    formats: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bitrate: Option<i64>,
    required_http_headers: RequiredHttpHeaders,
    direct_stream_url: String,
    add_api_key_to_direct_stream_url: bool,
    read_at_native_framerate: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_audio_stream_index: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_subtitle_stream_index: Option<i64>,
    item_id: String,
    chapters: Vec<serde_json::Value>,
}

/// `RequiredHttpHeaders`（恒空对象 `{}`）。
#[derive(Serialize, Default)]
pub struct RequiredHttpHeaders {}

/// Emby `MediaStreams` 元素（扁平化：Video/Audio/Subtitle 变体字段一律 `Option`+skip，
/// 仅本类型设置的字段出现）。
///
/// 与旧 `json!` 版的差异：类型内 `Option` 字段为空时旧版发 `null`、新版省略
/// （更贴近真实 Emby 省略风格；客户端 null==absent）。`NalLengthSize` 仅 h264/AVC
/// 时输出；其余变体字段按流类型设置。
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct MediaStreamDto {
    // base（恒发）
    index: i64,
    #[serde(rename = "Type")]
    stream_type: String,
    codec: String,
    display_title: String,
    time_base: String,
    is_default: bool,
    is_forced: bool,
    is_external: bool,
    is_hearing_impaired: bool,
    is_interlaced: bool,
    is_text_subtitle_stream: bool,
    supports_external_stream: bool,
    protocol: String,
    extended_video_type: String,
    extended_video_sub_type: String,
    extended_video_sub_type_description: String,
    attachment_size: i64,
    // 变体字段（仅本类型设置时 Some，否则 None→省略）
    #[serde(skip_serializing_if = "Option::is_none")]
    color_transfer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    color_primaries: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    color_space: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    video_range: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bit_rate: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bit_depth: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ref_frames: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    height: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    width: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    aspect_ratio: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    codec_tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    nal_length_size: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_anamorphic: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pixel_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    level: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    average_frame_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    real_frame_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    display_language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    channel_layout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    channels: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sample_rate: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delivery_method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subtitle_location_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delivery_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
}

/// `MediaStreamDto` 默认值：协议 `File`、扩展视频三字段 `None`、附件大小 0，
/// 其余 bool=false / String 空串 / Option=None。供 `stream_json` 与外部字幕构造器
/// `..Default::default()` 复用，省逐字段 `None` 枚举。
impl Default for MediaStreamDto {
    fn default() -> Self {
        Self {
            index: 0,
            stream_type: String::new(),
            codec: String::new(),
            display_title: String::new(),
            time_base: String::new(),
            is_default: false,
            is_forced: false,
            is_external: false,
            is_hearing_impaired: false,
            is_interlaced: false,
            is_text_subtitle_stream: false,
            supports_external_stream: false,
            protocol: "File".into(),
            extended_video_type: "None".into(),
            extended_video_sub_type: "None".into(),
            extended_video_sub_type_description: "None".into(),
            attachment_size: 0,
            color_transfer: None,
            color_primaries: None,
            color_space: None,
            video_range: None,
            bit_rate: None,
            bit_depth: None,
            ref_frames: None,
            height: None,
            width: None,
            profile: None,
            aspect_ratio: None,
            codec_tag: None,
            nal_length_size: None,
            is_anamorphic: None,
            pixel_format: None,
            level: None,
            average_frame_rate: None,
            real_frame_rate: None,
            language: None,
            display_language: None,
            channel_layout: None,
            channels: None,
            sample_rate: None,
            title: None,
            delivery_method: None,
            subtitle_location_type: None,
            delivery_url: None,
            path: None,
        }
    }
}

/// 类型化 Emby item DTO：`ItemRow` → `ItemDto`，serde 直接序列化到字节，
/// 跳过 `serde_json::Value` 树（省每 item ~30 次 String-key 分配）。
///
/// 可选顶层字段一律 `skip_serializing_if = "Option::is_none"`，保留旧 Value 版
/// 的"省略"语义；`UserData` 复用 [`ViewsUserData`]（其 Option 不 skip，发 null，
/// 与旧行为一致）。`tax` 由构造器折入（替代旧 `attach_taxonomy`）。
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ItemDto {
    #[serde(flatten)]
    base: BaseItemDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    location_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    date_modified: Option<String>,
    sort_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    media_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_time_ticks: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    primary_image_tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    primary_image_item_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    series_primary_image_tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    overview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    premiere_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    production_year: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    community_rating: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    official_rating: Option<String>,
    /// Emby `Taglines`（复数数组；无标语时为空数组）。
    taglines: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    series_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    series_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    season_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    season_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    index_number: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_index_number: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    series_presentation_unique_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    container: Option<String>,
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
    genres: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    genre_items: Option<Vec<GenreItemDto>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    people: Option<Vec<PersonItemDto>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    studios: Option<Vec<StudioDto>>,
    // folder 计数（Season/Series），由 child_counts_batch 批量预取。
    #[serde(skip_serializing_if = "Option::is_none")]
    recursive_item_count: Option<i64>,
    // Season 直接子集数（= 递归数；Series 不发）。
    #[serde(skip_serializing_if = "Option::is_none")]
    child_count: Option<i64>,
    // Episode 视频分辨率（首 Video 流，由 video_dims_batch 预取）。
    #[serde(skip_serializing_if = "Option::is_none")]
    width: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    height: Option<i64>,
    // Episode 显示父级（= SeasonId）；Season/Series 不发。
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_id: Option<String>,
    // 外链（folder 项发空数组，对齐 Emby Season 输出；Series 也发）。
    #[serde(skip_serializing_if = "Option::is_none")]
    external_urls: Option<Vec<ExternalUrlDto>>,
    // 播出日（Series 发空数组，对齐 Emby Series 输出）。
    #[serde(skip_serializing_if = "Option::is_none")]
    air_days: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    media_sources: Option<Vec<MediaSourceDto>>,
}

/// 日期字符串补全时间戳尾部：`"2026-01-04"` → `"2026-01-04T00:00:00.0000000Z"`。
/// 已有完整时间戳（含 `T`）保持原样；空串原样返回（不补）。
pub(super) fn emby_date(d: &str) -> String {
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

/// 从 ExternalUrls 内置列表 + imdb/tmdb/tvdb 列构建 Emby `ExternalUrls` 数组。
/// 当前仅包含 IMDb / TheMovieDb / TheTVDB（按 name 排序确定顺序）。
fn external_urls_for(item: &ItemRow) -> Vec<ExternalUrlDto> {
    let mut urls = Vec::new();
    if let Some(v) = item.imdb_id.as_deref().filter(|s| !s.is_empty()) {
        urls.push(ExternalUrlDto {
            name: "IMDb".into(),
            url: format!("https://www.imdb.com/title/{v}"),
        });
    }
    if let Some(v) = item.tmdb_id.as_deref().filter(|s| !s.is_empty()) {
        urls.push(ExternalUrlDto {
            name: "TheMovieDb".into(),
            url: format!(
                "https://www.themoviedb.org/{}/{}",
                if item.item_type == "Movie" {
                    "movie"
                } else {
                    "tv"
                },
                v
            ),
        });
    }
    if let Some(v) = item.tvdb_id.as_deref().filter(|s| !s.is_empty()) {
        urls.push(ExternalUrlDto {
            name: "TheTVDB".into(),
            url: format!("https://thetvdb.com/?tab=series&id={v}"),
        });
    }
    urls
}

/// ItemRow → 类型化 Emby item DTO。
///
/// 图片存在标志由调用方预先批量查询（[`ItemImageFlags`]），本函数不再触碰 DB。
/// `tax` 折入构造器：`Some` 时附加 Genres / GenreItems / People / Studios / Tags，
/// `None`（如 sessions）时省略。`media_sources` 由详情路径的
/// [`attach_media_sources`] 单独填入，列表路径保持 `None`。
///
/// `counts`：folder 项（Season/Series）的 `(recursive_total, unplayed, season_count)`，由
/// [`ItemsStore::child_counts_batch`] 预取；`Some` 且本项是 folder 时填
/// `RecursiveItemCount` / `ChildCount`(Season=总集数、Series=季数) / `UserData.UnplayedItemCount`。
/// `dims`：Episode 的 `(width, height)`，由 [`ItemsStore::video_dims_batch`]
/// 预取；`Some` 且本项是 Episode 时填顶层 `Width` / `Height`。列表路径未预取时
/// 传 `None`，对应字段省略。
pub fn item_to_json(
    server_id: &str,
    item: &ItemRow,
    flags: &ItemImageFlags,
    tax: Option<&ItemTaxonomy>,
    counts: Option<(i64, i64, Option<i64>)>,
    dims: Option<(Option<i64>, Option<i64>)>,
) -> ItemDto {
    let id = item_id(item.id);
    // 一次性预取 series 前缀字符串：Episode/Season 的 SeriesId /
    // SeriesPresentationUniqueKey / ParentLogo / ParentThumb 共用，
    // 避免重复 format! 与 Value clone。
    let series_id_str: Option<String> = item.series_id.map(item_id);
    // 类型分类一次：后续多处分支共用，避免重复 as_str/比较。
    let ty = item.item_type.as_str();
    let is_episode = ty == "Episode";
    let is_season = ty == "Season";
    let is_folder = is_season || ty == "Series";
    let has_media = matches!(ty, "Movie" | "Episode");

    // folder 计数（Season/Series）：recursive_total → RecursiveItemCount /
    // UserData.UnplayedItemCount；ChildCount 季=总集数、Series=直接季数（season_count）。
    let (recursive_item_count, child_count, unplayed_item_count) = match counts {
        Some((total, unplayed, season_count)) if is_folder => {
            let child = if is_season {
                Some(total)
            } else if ty == "Series" {
                season_count
            } else {
                None
            };
            (Some(total), child, Some(unplayed))
        }
        _ => (None, None, None),
    };
    // Episode 分辨率：首 Video 流 width/height（video_dims_batch 预取）。
    let (width, height) = match dims {
        Some((w, h)) if is_episode => (w, h),
        _ => (None, None),
    };
    // ParentId：Episode → season；Movie/Series → 所属库（l-{library_id}）。
    let parent_id = if is_episode {
        item.season_id.map(item_id)
    } else if matches!(ty, "Movie" | "Series") {
        item.library_id.map(library_id)
    } else {
        None
    };
    // folder 空数组/外链：ExternalUrls Movie/Series 从 provider 构建、Season 空数组；
    // AirDays(Series) 空数组，对齐 Emby 输出。
    let external_urls = match ty {
        "Movie" | "Series" => Some(external_urls_for(item)),
        "Season" => Some(Vec::new()),
        _ => None,
    };
    let air_days = if ty == "Series" {
        Some(Vec::new())
    } else {
        None
    };

    // 图片标记：tag 值为图片表行 id（`img-{id}`，Emby 的图片唯一标记）；
    // 季/集无自有图 → 回退上级剧集（PrimaryImageItemId 指向 series item，tag 仍为图片行 id）。
    let mut image_tags = ImageTagsDto::default();
    let mut primary_image_tag: Option<String> = None;
    let mut primary_image_item_id: Option<String> = None;
    let mut series_primary_image_tag: Option<String> = None;

    if let Some(img_id) = flags.own_primary {
        let tag = image_tag(img_id);
        image_tags.primary = Some(tag.clone());
        primary_image_tag = Some(tag);
    } else if (is_season || is_episode)
        && let Some(sid) = item.series_id
        && let Some(img_id) = flags.series_primary
    {
        let tag = image_tag(img_id);
        primary_image_item_id = Some(item_id(sid));
        image_tags.primary = Some(tag.clone());
        series_primary_image_tag = Some(tag);
    }

    // Logo / Thumb / Banner：仅自身拥有时发 ImageTags 内的对应 tag；季/集无自有则不发
    // （Episode 的 parent logo/thumb 由 ParentLogoImageTag / ParentThumbImageTag 指向 series）。
    if let Some(img_id) = flags.own_logo {
        image_tags.logo = Some(image_tag(img_id));
    }
    if let Some(img_id) = flags.own_thumb {
        image_tags.thumb = Some(image_tag(img_id));
    }
    if let Some(img_id) = flags.own_banner {
        image_tags.banner = Some(image_tag(img_id));
    }

    // Backdrop：自身拥有才发 tag（每张 backdrop 一个 tag，按 id 升序，与图片路由
    // `/N` 索引一致）；无图发空数组（对齐真实 Emby——占位 tag 会让客户端去请求
    // 不存在的图）；季/集回退上级剧集只走 ParentBackdropItemId / ParentBackdropImageTags。
    let backdrop_image_tags: Vec<String> = flags
        .own_backdrops
        .iter()
        .map(|img_id| image_tag(*img_id))
        .collect();

    let primary_image_aspect_ratio = if is_episode { 1.777778 } else { 0.666667 };

    // UserData：played_percentage 算好直接塞进（替代旧事后 mutate）
    let mut user_data = item_user_data(item);
    if let Some(secs) = item.file_second.filter(|s| *s > 0) {
        let pct = (item.play_ms as f64 / 1000.0 / secs as f64) * 100.0;
        if pct > 0.0 {
            user_data.played_percentage = Some(pct.min(100.0));
        }
    }
    // folder 项的未播集数（child_counts_batch 预取；非 folder 此处为 None → 省略）。
    user_data.unplayed_item_count = unplayed_item_count;

    // ProductionYear：刮削列优先，否则 date_air 前 4 位 parse（失败→0，保留旧语义）
    let production_year = item.production_year.or_else(|| {
        item.date_air
            .as_deref()
            .and_then(|d| d.get(0..4))
            .map(|y| y.parse::<i64>().unwrap_or(0))
    });

    // IndexNumber / ParentIndexNumber：Season→季号；Episode→集号 + 季号
    let (index_number, parent_index_number) = if is_season {
        (item.season_number, None)
    } else if is_episode {
        (item.episode_number, item.season_number)
    } else {
        (None, None)
    };

    // Parent Backdrop 回退
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
            .map(|img_id| image_tag(*img_id))
            .collect()
    });
    // Episode 专属：SeriesPresentationUniqueKey / Container / ParentLogo / ParentThumb
    let series_presentation_unique_key = if is_episode {
        series_id_str.clone()
    } else {
        None
    };
    let container = if is_episode {
        item.container.clone()
    } else {
        None
    };
    // Parent Logo/Thumb：ItemId 指向 series item，tag 为 series 图片行 id（`img-{id}`，
    // 与 ImageTags 值同语义——tag 标识图片本身，Episode 无自有 logo/thumb 时回退）。
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

    // taxonomy 折入（替代旧 attach_taxonomy）
    let (genres, genre_items, people, studios, tags) = match tax {
        Some(t) => {
            let genres: Vec<String> = t.genres.iter().map(|(_, n)| n.clone()).collect();
            let genre_items: Vec<GenreItemDto> = t
                .genres
                .iter()
                .map(|(gid, n)| GenreItemDto {
                    name: n.clone(),
                    id: genre_id(*gid),
                })
                .collect();
            let people: Vec<PersonItemDto> =
                t.people.iter().map(PersonItemDto::from_person).collect();
            let studios = if t.studios.is_empty() {
                None
            } else {
                Some(
                    t.studios
                        .iter()
                        .map(|(sid, n)| StudioDto {
                            id: studio_id(*sid),
                            name: n.clone(),
                        })
                        .collect::<Vec<_>>(),
                )
            };
            let tags = if t.tags.is_empty() {
                None
            } else {
                Some(t.tags.clone())
            };
            (Some(genres), Some(genre_items), Some(people), studios, tags)
        }
        None => (None, None, None, None, None),
    };

    ItemDto {
        base: BaseItemDto {
            name: item.title.clone(),
            server_id: server_id.to_string(),
            id,
            item_type: item.item_type.clone(),
            is_folder,
            date_created: item.created_at.clone(),
            user_data,
            primary_image_aspect_ratio,
            image_tags,
            backdrop_image_tags,
            provider_ids: provider_ids(item),
        },
        location_type: if item.is_virtual != 0 {
            Some("Virtual".into())
        } else {
            Some("FileSystem".into())
        },
        date_modified: Some(item.updated_at.clone()),
        sort_name: item
            .sort_title
            .as_deref()
            .unwrap_or(&item.title)
            .to_string(),
        media_type: has_media.then(|| "Video".to_string()),
        run_time_ticks: item.file_second.map(|s| s * 10_000_000),
        primary_image_tag,
        primary_image_item_id,
        series_primary_image_tag,
        overview: item.description.clone(),
        premiere_date: item.date_air.as_deref().map(emby_date),
        production_year,
        end_date: item.end_date.as_deref().map(emby_date),
        status: item.status.clone(),
        community_rating: item.community_rating,
        official_rating: item.official_rating.clone(),
        taglines: item
            .tagline
            .as_deref()
            .filter(|t| !t.trim().is_empty())
            .map(|t| vec![t.to_string()])
            .unwrap_or_default(),
        tags,
        series_id: series_id_str.clone(),
        series_name: item.series_name.clone(),
        season_id: item.season_id.map(item_id),
        season_name: item.season_name.clone(),
        index_number,
        parent_index_number,
        series_presentation_unique_key,
        container,
        parent_backdrop_item_id,
        parent_backdrop_image_tags,
        parent_logo_item_id,
        parent_logo_image_tag,
        parent_thumb_item_id,
        parent_thumb_image_tag,
        genres,
        genre_items,
        people,
        studios,
        recursive_item_count,
        child_count,
        width,
        height,
        parent_id,
        external_urls,
        air_days,
        media_sources: None,
    }
}

/// 为详情 item 附加 MediaSources（文件大小/时长等媒体信息）。
/// 仅 Movie/Episode（有 media 的项）会附加；Season/Series 等 folder 项保持不附加。
pub async fn attach_media_sources(
    db: &Db,
    signing_key: Option<&str>,
    user_id: i64,
    item: &ItemRow,
    dto: &mut ItemDto,
) {
    if !matches!(item.item_type.as_str(), "Movie" | "Episode") {
        return;
    }
    if let Ok(Some(media)) = ItemsStore::get_playback_info(db, item.id).await
        && let Ok(sources) = media_sources_json(db, signing_key, user_id, &media, true).await
    {
        dto.media_sources = Some(sources);
    }
}

/// 签发播放短票据：绑定 (uuid, user_id)，TTL = max(PLAYBACK_TICKET_TTL, 时长+1h)。
/// 已知时长时按「时长+1h 缓冲」伸缩（覆盖拖动/暂停续播），未知或过短兜底 6h。
/// 未配置 `playback.signing_key` 时返回 Err——调用方据此 500，不降级为 `/Videos/` 直链。
fn issue_playback_ticket(
    signing_key: Option<&str>,
    user_id: i64,
    uuid: &str,
    duration_secs: Option<i64>,
) -> anyhow::Result<String> {
    let key = signing_key
        .ok_or_else(|| anyhow::anyhow!("playback.signing_key 未配置，无法签发播放票据"))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let ttl = duration_secs
        .filter(|&s| s > 0)
        .map(|s| (s as u64).saturating_add(3600))
        .unwrap_or(0)
        .max(emrs_core::playback::PLAYBACK_TICKET_TTL.as_secs());
    let claims = TicketClaims {
        uuid: uuid.to_string(),
        user_id,
        exp: now + ttl,
    };
    issue_ticket(&claims, key.as_bytes())
}

/// MediaSourceRow → Emby MediaSource JSON。
/// `DirectStreamUrl` 恒为签名短票据 `/s/{ticket}`，
/// 客户端直连播放无需再带 token；票据过期即失效。
/// 未配置签名密钥时返回错误（不降级为 `/Videos/` 直链）。
/// `is_first`：多版本时第一个为 `Default`，其余为 `Grouping`（对齐官方）。
pub async fn media_sources_json(
    db: &Db,
    signing_key: Option<&str>,
    user_id: i64,
    media: &MediaSourceRow,
    is_first: bool,
) -> anyhow::Result<Vec<MediaSourceDto>> {
    let uuid = media.uuid.as_deref().unwrap_or("");
    let name = media.name.as_deref().unwrap_or("Stream");
    let container = media.file_container.as_deref().unwrap_or("mp4");

    // DirectStreamUrl 用短时效票据 /s/{ticket}：不泄露用户主 token，
    // 票据按时长伸缩（≥6h）且仅限本 uuid 播放。
    let direct_url = format!(
        "/s/{}",
        issue_playback_ticket(signing_key, user_id, uuid, media.file_second)?
    );

    // 流信息（ffprobe 解析，扫描时写入 media_source.metadata；外部字幕从 external_subtitle 表合并）
    let mut streams = media_streams_json(db, media).await;
    // 从流列表提取 Bitrate（首视频流）/默认音频/默认字幕索引
    let (mut bitrate, default_audio, default_subtitle) = defaults_from_streams(&streams);
    // 视频流无码率时，用文件大小/时长估算平均码率，保证 Bitrate 不为空
    if bitrate.is_none() {
        bitrate = estimate_bitrate(media.file_size, media.file_second);
    }
    // 视频流自身的 BitRate 为空时回填，保证 MediaStream 内码率也不为空
    for s in streams.iter_mut() {
        if s.stream_type == "Video" && s.bit_rate.is_none() {
            s.bit_rate = bitrate;
        }
    }
    // 章节：file_chapters（media_source.chapters）由 ffprobe 写入，缺失时兜底空数组
    let chapters = media
        .file_chapters
        .as_deref()
        .and_then(|s| serde_json::from_str::<Vec<serde_json::Value>>(s).ok())
        .unwrap_or_default();

    Ok(vec![MediaSourceDto {
        protocol: "File".into(),
        id: uuid.to_string(),
        path: media.path_url.as_deref().unwrap_or("").to_string(),
        source_type: (if is_first { "Default" } else { "Grouping" }).into(),
        container: container.to_string(),
        name: name.to_string(),
        is_remote: false,
        has_mixed_protocols: false,
        size: media.file_size.unwrap_or(0),
        run_time_ticks: media.file_second.unwrap_or(0) * 10_000_000,
        supports_transcoding: false,
        supports_direct_stream: true,
        supports_direct_play: true,
        is_infinite_stream: false,
        requires_opening: false,
        requires_closing: false,
        requires_looping: false,
        supports_probing: true,
        media_streams: streams,
        formats: Vec::new(),
        bitrate,
        required_http_headers: RequiredHttpHeaders {},
        direct_stream_url: direct_url,
        add_api_key_to_direct_stream_url: false,
        read_at_native_framerate: false,
        default_audio_stream_index: default_audio,
        default_subtitle_stream_index: default_subtitle,
        item_id: item_id(media.item_id),
        chapters,
    }])
}

/// 从 MediaStreams JSON 提取：Bitrate（首视频流）、DefaultAudioStreamIndex（首音频流）、
/// DefaultSubtitleStreamIndex（首个默认字幕流，无则首个字幕流）。
fn defaults_from_streams(streams: &[MediaStreamDto]) -> (Option<i64>, Option<i64>, Option<i64>) {
    let mut bitrate = None;
    let mut audio = None;
    let mut subtitle = None;
    let mut subtitle_default = None;
    for s in streams {
        match s.stream_type.as_str() {
            "Video" if bitrate.is_none() => bitrate = s.bit_rate,
            "Audio" if audio.is_none() => audio = Some(s.index),
            "Subtitle" => {
                if subtitle.is_none() {
                    subtitle = Some(s.index);
                }
                if subtitle_default.is_none() && s.is_default {
                    subtitle_default = Some(s.index);
                }
            }
            _ => {}
        }
    }
    (bitrate, audio, subtitle_default.or(subtitle))
}

/// 平均码率估算（bps）：文件大小 × 8 / 时长（秒）。缺任一项返回 None。
fn estimate_bitrate(size: Option<i64>, seconds: Option<i64>) -> Option<i64> {
    let size = size?;
    let secs = seconds?;
    if secs > 0 {
        Some(size.saturating_mul(8) / secs)
    } else {
        None
    }
}

/// 外部字幕查询返回行（display_title, codec, path, is_forced）。
/// is_forced 以 0/1 整数承载（sqlx Any 下 bool 解码失败，见 stores/mod.rs 约定）。
type ExternalSubtitleRow = (Option<String>, Option<String>, Option<String>, i64);

/// 从 `media_source.metadata` 反序列化流信息并转换为 Emby `MediaStreams`，
/// 再合并 `external_subtitle` 表中的外部字幕（IsExternal=true）。
/// 无数据或解析失败时仅输出外部字幕。
async fn media_streams_json(db: &Db, media: &MediaSourceRow) -> Vec<MediaStreamDto> {
    let mut streams: Vec<MediaStreamDto> = Vec::new();
    if let Some(meta) = media.file_metadata.as_deref().filter(|s| !s.is_empty()) {
        match serde_json::from_str::<Vec<StreamInfo>>(meta) {
            Ok(list) => {
                let container = media.file_container.as_deref();
                for (i, s) in list.iter().enumerate() {
                    streams.push(stream_json(s, i, container));
                }
            }
            Err(e) => tracing::debug!(error = %e, "file_metadata 反序列化失败"),
        }
    }

    // 外部字幕（接在内嵌流之后，Index 顺延）
    // 新表 external_subtitle：只存外部字幕（外挂附件）
    if let Some(media_id) = media.media_id {
        let rows: Vec<ExternalSubtitleRow> = match sqlx::query_as(
            "SELECT display_title, codec, path, is_forced FROM external_subtitle \
             WHERE media_source_id = ? ORDER BY id",
        )
        .bind(media_id)
        .fetch_all(db.pool())
        .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, media_id, "外部字幕查询失败，跳过外部字幕");
                Vec::new()
            }
        };
        let next_index = streams.iter().map(|s| s.index).max().unwrap_or(-1) + 1;
        for (i, (title, codec, path_url, is_forced_raw)) in rows.into_iter().enumerate() {
            let title = title.unwrap_or_default();
            let codec = codec.unwrap_or_default().to_ascii_lowercase();
            let language = external_subtitle_language(&title);
            let index = next_index + i as i64;
            let is_forced = is_forced_raw != 0;
            // DeliveryUrl 用外部字幕内部序号（0 基），与 /Videos/{uuid}/Subtitles/
            // 路由的 OFFSET 一致；Index 字段则顺延内嵌流，供客户端区分流。
            let delivery_url = media
                .uuid
                .as_deref()
                .map(|u| format!("/Videos/{u}/Subtitles/{i}"));
            streams.push(MediaStreamDto {
                index,
                stream_type: "Subtitle".into(),
                codec: subtitle_codec(&codec),
                display_title: external_subtitle_title(&title, &codec, is_forced),
                is_forced,
                is_external: true,
                is_text_subtitle_stream: true,
                supports_external_stream: true,
                delivery_method: Some("External".into()),
                delivery_url,
                display_language: language
                    .as_deref()
                    .and_then(language_name)
                    .map(str::to_string),
                language,
                title: Some(title),
                path: Some(path_url.unwrap_or_default()),
                ..Default::default()
            });
        }
    }

    streams
}

/// 单个流 → 类型化 Emby MediaStream DTO（对齐参考 Emby 输出字段集）。
/// `container`：媒体源容器（mp4/mkv 等），决定是否输出 `CodecTag`
/// （仅 mp4 家族输出 avc1/mp4a，mkv 参考 Emby 不发 CodecTag）。
fn stream_json(s: &StreamInfo, fallback_index: usize, container: Option<&str>) -> MediaStreamDto {
    let index = s.index.unwrap_or(fallback_index as i64);
    let is_default = s.is_default.unwrap_or(false);
    let is_forced = s.is_forced.unwrap_or(false);
    let time_base = s.time_base.as_deref().unwrap_or("").to_string();
    let ty = s.stream_type.as_str();
    // 仅 mp4/m4v/mov 输出 CodecTag（ISO BMFF 带 sample entry tag；mkv 无）。
    let has_codec_tag = matches!(container, Some("mp4" | "m4v" | "mov"));

    let mut dto = MediaStreamDto {
        index,
        stream_type: s.stream_type.clone(),
        codec: subtitle_codec_if_sub(ty, &s.codec),
        display_title: stream_display_title(s),
        time_base,
        is_default,
        is_forced,
        is_interlaced: s.is_interlaced.unwrap_or(false),
        ..Default::default()
    };
    match ty {
        "Video" => {
            dto.color_transfer = s.color_transfer.clone();
            dto.color_primaries = s.color_primaries.clone();
            dto.color_space = s.color_space.clone();
            dto.video_range = Some(video_range(s));
            dto.bit_rate = s.bit_rate;
            dto.bit_depth = s.bit_depth;
            dto.ref_frames = s.refs;
            dto.height = s.height;
            dto.width = s.width;
            dto.profile = s.profile.clone();
            dto.aspect_ratio = s.display_aspect_ratio.clone();
            dto.codec_tag = if has_codec_tag {
                codec_tag(&s.codec).map(str::to_string)
            } else {
                None
            };
            // NalLengthSize 仅 h264/AVC 输出 "4"
            dto.nal_length_size = is_h264_avc(s).then(|| "4".to_string());
            dto.is_anamorphic = Some(is_anamorphic(s));
            dto.pixel_format = s.pixel_format.clone();
            dto.level = s.level;
            // 帧率输出原始值（对齐参考 Emby：23.976025 不四舍五入）
            dto.average_frame_rate = s.frame_rate;
            dto.real_frame_rate = s.frame_rate;
        }
        "Audio" => {
            dto.language = s.language.clone();
            dto.display_language = display_language(s);
            dto.codec_tag = if has_codec_tag {
                codec_tag(&s.codec).map(str::to_string)
            } else {
                None
            };
            dto.channel_layout = s.channel_layout.clone();
            dto.bit_rate = s.bit_rate;
            dto.channels = s.channels;
            dto.sample_rate = s.sample_rate;
            dto.profile = s.profile.clone();
        }
        "Subtitle" => {
            dto.language = s.language.clone();
            dto.display_language = display_language(s);
            dto.title = s.title.clone();
            dto.delivery_method = Some("Embed".into());
            dto.subtitle_location_type = Some("InternalStream".into());
            dto.is_text_subtitle_stream = true;
            dto.supports_external_stream = true;
        }
        _ => {}
    }
    dto
}

/// h264 且 AVC 封装 → 输出 NalLengthSize="4"（对齐参考 Emby）。
fn is_h264_avc(s: &StreamInfo) -> bool {
    s.codec.eq_ignore_ascii_case("h264") && s.is_avc.unwrap_or(false)
}

/// 像素宽高比非 1:1 → 变形画面 IsAnamorphic。
fn is_anamorphic(s: &StreamInfo) -> bool {
    matches!(
        s.sample_aspect_ratio.as_deref(),
        Some(sar) if !sar.is_empty() && sar != "1:1" && sar != "0:1"
    )
}

/// 视频动态范围（由色彩转换推断；SDR/HDR10/HLG）。
fn video_range(s: &StreamInfo) -> String {
    match s.color_transfer.as_deref() {
        Some("smpte2084" | "smpte2086") => "HDR10".to_string(),
        Some("arib-std-b67") => "HLG".to_string(),
        _ => "SDR".to_string(),
    }
}

/// 语言代码 → 英文名（用于 DisplayLanguage）；未知代码原样返回。
fn display_language(s: &StreamInfo) -> Option<String> {
    s.language.as_deref().map(|code| {
        language_name(code)
            .map(|n| n.to_string())
            .unwrap_or_else(|| code.to_string())
    })
}

/// 语言代码 → 英文名。
fn language_name(code: &str) -> Option<&'static str> {
    match code.to_ascii_lowercase().as_str() {
        "eng" | "en" => Some("English"),
        "jpn" | "ja" => Some("Japanese"),
        "chi" | "zho" | "cmn" | "zh" => Some("Chinese"),
        "spa" | "es" => Some("Spanish"),
        "por" | "pt" => Some("Portuguese"),
        "fre" | "fra" | "fr" => Some("French"),
        "ger" | "deu" | "de" => Some("German"),
        "ara" | "ar" => Some("Arabic"),
        "ita" | "it" => Some("Italian"),
        "rus" | "ru" => Some("Russian"),
        "tha" | "th" => Some("Thai"),
        "vie" | "vi" => Some("Vietnamese"),
        "ind" | "id" => Some("Indonesian"),
        "may" | "msa" | "ms" => Some("Malay"),
        "kor" | "ko" => Some("Korean"),
        "nld" | "dut" | "nl" => Some("Dutch"),
        "pol" | "pl" => Some("Polish"),
        "tur" | "tr" => Some("Turkish"),
        "swe" | "sv" => Some("Swedish"),
        "nor" | "no" => Some("Norwegian"),
        "dan" | "da" => Some("Danish"),
        "fin" | "fi" => Some("Finnish"),
        "heb" | "he" => Some("Hebrew"),
        "hin" | "hi" => Some("Hindi"),
        "ben" | "bn" => Some("Bengali"),
        "tam" | "ta" => Some("Tamil"),
        "tel" | "te" => Some("Telugu"),
        "ukr" | "uk" => Some("Ukrainian"),
        "ces" | "cze" | "cs" => Some("Czech"),
        "hun" | "hu" => Some("Hungarian"),
        "ell" | "gre" | "el" => Some("Greek"),
        "cat" | "ca" => Some("Catalan"),
        "yue" => Some("Chinese (Cantonese)"),
        _ => None,
    }
}

/// 仅对字幕流做编码名映射（hdmv_pgs_subtitle→pgs / dvd_subtitle→dvdsub / webvtt→vtt）；
/// subrip 保留原始名（参考 Emby `Codec` 输出 `subrip`）。
fn subtitle_codec_if_sub(stream_type: &str, codec: &str) -> String {
    if stream_type == "Subtitle" {
        subtitle_codec(codec)
    } else {
        codec.to_string()
    }
}

/// 字幕编码名归一化（保留 ffprobe 原始名，对齐参考 Emby 输出的 `Codec` 字段）。
fn subtitle_codec(codec: &str) -> String {
    match codec.to_ascii_lowercase().as_str() {
        "hdmv_pgs_subtitle" => "pgs".to_string(),
        "dvd_subtitle" => "dvdsub".to_string(),
        "webvtt" => "vtt".to_string(),
        other => other.to_string(),
    }
}

/// 媒体流 CodecTag（mp4 box tag 近似，对齐参考 Emby 输出）：
/// h264→avc1、hevc→hvc1、aac→mp4a、ac3→ac-3、其余返回 null。
fn codec_tag(codec: &str) -> Option<&'static str> {
    match codec.to_ascii_lowercase().as_str() {
        "h264" => Some("avc1"),
        "hevc" | "h265" => Some("hvc1"),
        "aac" => Some("mp4a"),
        "ac3" => Some("ac-3"),
        _ => None,
    }
}

/// 流显示标题（Emby DisplayTitle，对齐参考 Emby）。
/// 视频：`{height}p {codec}`（编码大写，如 "1080p H264"）；
/// 音频：`{语言} {codec} {声道}`，默认流追加 "(默认)"（如 "Japanese AAC stereo (默认)"）；
/// 字幕：`{标题|语言} ({默认 }编码)`（编码大写，如 "Chinese (默认 ASS)"）。
fn stream_display_title(s: &StreamInfo) -> String {
    match s.stream_type.as_str() {
        "Video" => match s.height {
            Some(h) if h > 0 => format!("{}p {}", h, s.codec.to_uppercase()),
            _ => s.codec.to_uppercase(),
        },
        "Audio" => {
            let mut parts: Vec<String> = Vec::new();
            if let Some(lang) = display_language(s) {
                parts.push(lang);
            }
            parts.push(s.codec.to_uppercase());
            if let Some(layout) = s.channel_layout.as_deref().filter(|l| !l.is_empty()) {
                parts.push(layout.to_string());
            } else if let Some(label) = s.channels.and_then(channel_label) {
                parts.push(label.to_string());
            }
            let mut title = parts.join(" ");
            if s.is_default.unwrap_or(false) {
                title.push_str(" (默认)");
            }
            title
        }
        _ => {
            // 字幕：显示语言名优先（对齐参考 Emby，"Chinese Simplified (默认 SUBRIP)" 用语言名
            // 而非轨道 Title 作基底）。但语言码 `chi` 分不出简繁（参考能给出
            // Simplified/Traditional，我们拿不到该信息），此时退回轨道 Title（"简体"/"繁体"）保区分。
            let lang = display_language(s);
            let base = match lang.as_deref() {
                Some("Chinese") => s
                    .title
                    .as_deref()
                    .filter(|t| !t.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| "Chinese".to_string()),
                Some(name) => name.to_string(),
                None => s
                    .title
                    .as_deref()
                    .filter(|t| !t.is_empty())
                    .map(str::to_string)
                    .or_else(|| s.language.clone())
                    .unwrap_or_else(|| s.codec.clone()),
            };
            let codec = subtitle_codec(&s.codec).to_uppercase();
            if s.is_default.unwrap_or(false) {
                format!("{} (默认 {})", base, codec)
            } else {
                format!("{} ({})", base, codec)
            }
        }
    }
}

/// 声道数 → 显示标签（2→stereo、1→mono、6→5.1、8→7.1）。
fn channel_label(channels: i64) -> Option<&'static str> {
    match channels {
        1 => Some("mono"),
        2 => Some("stereo"),
        6 => Some("5.1"),
        8 => Some("7.1"),
        _ => None,
    }
}

/// 外部字幕显示标题：`{文件名} ({codec})`，如 "JPSC.ass (ass)" → "JPSC.ass (ass)"；
/// 强制字幕追加 "(强制)" 标注。若文件名仅由语言标签组成则简化标题。
fn external_subtitle_title(title: &str, codec: &str, is_forced: bool) -> String {
    if title.is_empty() {
        return if is_forced {
            format!("{codec} (强制)")
        } else {
            codec.to_string()
        };
    }
    if is_forced {
        format!("{} ({}) (强制)", title, codec)
    } else {
        format!("{} ({})", title, codec)
    }
}

/// 从外部字幕文件名推断语言（尽力而为，未识别返回 null）。
fn external_subtitle_language(title: &str) -> Option<String> {
    let lower = title.to_ascii_lowercase();
    let table = [
        (
            "chi",
            ["zh", "chi", "chs", "cht", "sc", "tc", "cn", "yue"].as_slice(),
        ),
        ("jpn", ["jp", "jpn", "ja", "jpsc", "jptc"].as_slice()),
        ("eng", ["eng", "en", "gb", "us"].as_slice()),
        ("kor", ["kor", "ko", "kr"].as_slice()),
        ("fre", ["fre", "fr", "fra"].as_slice()),
        ("ger", ["ger", "de", "deu"].as_slice()),
        ("spa", ["spa", "es", "esp"].as_slice()),
        ("ita", ["ita", "it"].as_slice()),
        ("rus", ["rus", "ru"].as_slice()),
        ("tha", ["tha", "th"].as_slice()),
        ("vie", ["vie", "vi"].as_slice()),
        ("por", ["por", "pt"].as_slice()),
        ("ara", ["ara", "ar"].as_slice()),
    ];
    for (lang, toks) in table {
        if toks.iter().any(|t| {
            let pat = format!(".{t}.");
            lower.contains(&pat)
        }) {
            return Some(lang.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use emrs_core::stores::ItemRow;

    fn row() -> ItemRow {
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

    #[test]
    fn provider_ids_from_columns() {
        let m = provider_ids(&row());
        // 主 provider 列 PascalCase
        assert_eq!(m["Tmdb"], "12345");
        assert_eq!(m["Tvdb"], "11864467");
        assert!(!m.contains_key("Imdb"), "空列不应输出");
    }

    #[test]
    fn provider_ids_omits_empty_columns() {
        let mut r = row();
        r.tmdb_id = None;
        r.tvdb_id = None;
        let m = provider_ids(&r);
        assert!(m.is_empty(), "全空时输出空字典");
    }

    #[test]
    fn external_subtitle_title_marks_forced() {
        assert_eq!(
            external_subtitle_title("JPSC.ass", "ass", false),
            "JPSC.ass (ass)"
        );
        assert_eq!(
            external_subtitle_title("Movie.eng.forced.srt", "srt", true),
            "Movie.eng.forced.srt (srt) (强制)"
        );
        // 空文件名退化为仅编码（强制时带标注）
        assert_eq!(external_subtitle_title("", "ass", false), "ass");
        assert_eq!(external_subtitle_title("", "ass", true), "ass (强制)");
    }

    /// 类型化子对象（GenreItems / People / Studios / ImageTags）序列化形状
    /// 必须与旧 `json!` 版一致（PascalCase key、前缀 Id、Character 省略语义）。
    #[test]
    fn typed_taxonomy_subobjects_shape() {
        use emrs_core::stores::taxonomy_store::{ItemTaxonomy, PersonBrief};
        let tax = ItemTaxonomy {
            genres: vec![(1, "动作".into()), (2, "科幻".into())],
            people: vec![PersonBrief {
                id: 10,
                name: "演员甲".into(),
                role: "Actor".into(),
                character_name: Some("角色".into()),
                primary_image_id: Some(11),
            }],
            studios: vec![(5, "工作室X".into())],
            tags: vec!["关键词X".into()],
        };
        // own primary（row id=1 → img-1）
        let v = serde_json::to_value(item_to_json(
            "srv",
            &row(),
            &ItemImageFlags {
                own_primary: Some(1),
                ..Default::default()
            },
            Some(&tax),
            None,
            None,
        ))
        .unwrap();
        assert_eq!(v["Genres"], json!(["动作", "科幻"]));
        assert_eq!(
            v["GenreItems"],
            json!([{ "Name": "动作", "Id": "g-1" }, { "Name": "科幻", "Id": "g-2" }])
        );
        assert_eq!(
            v["People"],
            json!([{ "Name": "演员甲", "Id": "p-10", "Role": "Actor", "Type": "Person", "Character": "角色", "PrimaryImageTag": "img-11" }])
        );
        assert_eq!(v["Studios"], json!([{ "Id": "s-5", "Name": "工作室X" }]));
        assert_eq!(v["Tags"], json!(["关键词X"]));
        assert_eq!(v["ImageTags"], json!({ "Primary": "img-1" }));

        // 无 primary → ImageTags={}
        let v2 = serde_json::to_value(item_to_json(
            "srv",
            &row(),
            &ItemImageFlags::default(),
            Some(&tax),
            None,
            None,
        ))
        .unwrap();
        assert_eq!(v2["ImageTags"], json!({}));

        // 无 character → People 元素不带 Character 键；无 primary_image → 不带 PrimaryImageTag
        let mut tax_no_char = tax.clone();
        tax_no_char.people[0].character_name = None;
        tax_no_char.people[0].primary_image_id = None;
        let v3 = serde_json::to_value(item_to_json(
            "srv",
            &row(),
            &ItemImageFlags {
                own_primary: Some(1),
                ..Default::default()
            },
            Some(&tax_no_char),
            None,
            None,
        ))
        .unwrap();
        assert!(
            !v3["People"][0]
                .as_object()
                .unwrap()
                .contains_key("Character"),
            "无 character 应省略 Character 键"
        );
        assert!(
            !v3["People"][0]
                .as_object()
                .unwrap()
                .contains_key("PrimaryImageTag"),
            "无 primary_image 应省略 PrimaryImageTag 键"
        );
    }

    /// 新增协议字段形状：
    /// - Episode：dims + season_id → ParentId==SeasonId、顶层 Width/Height；无 counts。
    /// - Season：counts=(30,25,None) → RecursiveItemCount=30、ChildCount=30、
    ///   UserData.UnplayedItemCount=25、ExternalUrls=[]。
    /// - Series：counts=(43,43,Some(2)) → RecursiveItemCount=43、ChildCount=2（直接季数）、
    ///   ParentId=l-{库}、ExternalUrls 从 provider 构建、AirDays=[]、Width 不发。
    /// - 通用：DateModified / Taglines 恒发、PremiereDate 补时间戳、LocationType 仍发。
    #[test]
    fn item_dto_protocol_fields_shape() {
        // Episode：dims=(Some(1920),Some(1080))，season_id=8 → ParentId==SeasonId
        let v = serde_json::to_value(item_to_json(
            "srv",
            &row(),
            &ItemImageFlags::default(),
            None,
            None,
            Some((Some(1920), Some(1080))),
        ))
        .unwrap();
        assert_eq!(v["ParentId"], "i-8", "Episode ParentId == SeasonId");
        assert_eq!(v["SeasonId"], "i-8");
        assert_eq!(v["Width"], 1920);
        assert_eq!(v["Height"], 1080);
        assert!(!v.as_object().unwrap().contains_key("RecursiveItemCount"));
        assert!(!v.as_object().unwrap().contains_key("ChildCount"));
        assert!(!v.as_object().unwrap().contains_key("AirDays"));
        assert!(!v.as_object().unwrap().contains_key("ExternalUrls"));
        assert_eq!(v["DateModified"], "2026-03-01T00:00:00.0000000Z");
        assert_eq!(
            v["PremiereDate"], "2026-01-01T00:00:00.0000000Z",
            "date_air 补时间戳"
        );
        assert_eq!(v["Taglines"], json!([]), "Taglines 复数数组恒发");
        assert_eq!(v["LocationType"], "FileSystem");

        // Season：counts=(30,25)
        let mut season = row();
        season.item_type = "Season".into();
        season.season_id = None;
        season.episode_number = None;
        let vs = serde_json::to_value(item_to_json(
            "srv",
            &season,
            &ItemImageFlags::default(),
            None,
            Some((30, 25, None)),
            None,
        ))
        .unwrap();
        assert_eq!(vs["RecursiveItemCount"], 30);
        assert_eq!(vs["ChildCount"], 30);
        assert_eq!(vs["UserData"]["UnplayedItemCount"], 25);
        assert_eq!(vs["ExternalUrls"], json!([]));
        assert!(!vs.as_object().unwrap().contains_key("ParentId"));
        assert!(!vs.as_object().unwrap().contains_key("Width"));
        assert!(!vs.as_object().unwrap().contains_key("AirDays"));

        // Series：counts=(43,43)
        let mut series = row();
        series.item_type = "Series".into();
        series.season_id = None;
        series.series_id = None;
        series.season_number = None;
        series.episode_number = None;
        let vser = serde_json::to_value(item_to_json(
            "srv",
            &series,
            &ItemImageFlags::default(),
            None,
            Some((43, 43, Some(2))),
            None,
        ))
        .unwrap();
        assert_eq!(vser["RecursiveItemCount"], 43);
        assert_eq!(vser["ChildCount"], 2, "Series ChildCount = 直接季数");
        assert_eq!(vser["UserData"]["UnplayedItemCount"], 43);
        assert_eq!(vser["AirDays"], json!([]));
        assert_eq!(vser["ParentId"], "l-3", "Series ParentId = 所属库");
        assert_eq!(
            vser["ExternalUrls"],
            json!([
                { "Name": "TheMovieDb", "Url": "https://www.themoviedb.org/tv/12345" },
                { "Name": "TheTVDB", "Url": "https://thetvdb.com/?tab=series&id=11864467" }
            ]),
            "Series ExternalUrls 从 provider ids 构建"
        );
        assert!(!vser.as_object().unwrap().contains_key("Width"));
    }

    /// BackdropImageTags 仅在自身有 Backdrop 图时发 tag；无图发空数组（不用自身 id
    /// 占位——客户端会拿着 tag 去请求不存在的图）。季/集回退上级剧集走
    /// ParentBackdropItemId / ParentBackdropImageTags（对齐真实 Emby Seasons 样本）。
    #[test]
    fn backdrop_image_tags_only_with_image() {
        // 无任何图 → 空数组，且不发 ParentBackdrop*
        let v = serde_json::to_value(item_to_json(
            "srv",
            &row(),
            &ItemImageFlags::default(),
            None,
            None,
            None,
        ))
        .unwrap();
        assert_eq!(v["BackdropImageTags"], json!([]));
        assert!(!v.as_object().unwrap().contains_key("ParentBackdropItemId"));
        assert!(
            !v.as_object()
                .unwrap()
                .contains_key("ParentBackdropImageTags")
        );

        // 季/集无自有图、上级剧集有 backdrop → 空数组 + Parent 回退
        let v2 = serde_json::to_value(item_to_json(
            "srv",
            &row(),
            &ItemImageFlags {
                series_backdrops: vec![7],
                ..Default::default()
            },
            None,
            None,
            None,
        ))
        .unwrap();
        assert_eq!(v2["BackdropImageTags"], json!([]));
        assert_eq!(v2["ParentBackdropItemId"], "i-9");
        assert_eq!(
            v2["ParentBackdropImageTags"],
            json!(["img-7"]),
            "Parent 回退的 tag 应为 series backdrop 图片行 id（img-{{id}}），而非 item id"
        );

        // 自身有 backdrop → 图片行 id tag（img-{id}），不发 ParentBackdrop*
        let v3 = serde_json::to_value(item_to_json(
            "srv",
            &row(),
            &ItemImageFlags {
                own_backdrops: vec![3, 5],
                ..Default::default()
            },
            None,
            None,
            None,
        ))
        .unwrap();
        assert_eq!(v3["BackdropImageTags"], json!(["img-3", "img-5"]));
        assert!(!v3.as_object().unwrap().contains_key("ParentBackdropItemId"));
    }

    /// Episode 的 ParentLogo/ParentThumb：ItemId 指向 series（i-{series_id}），
    /// Tag 为 series 图片行 id（img-{id}，tag 标识图片本身）——与 Primary/Backdrop
    /// 同语义，不能复用 ItemId 值（对齐真实 Emby Resume_emos 样本）。
    #[test]
    fn parent_logo_thumb_tag_is_image_id() {
        // Episode + series 有 logo/thumb：ParentLogoItemId=series item id，
        // ParentLogoImageTag=series logo 图片行 id（img-{id}），二者不同。
        let v = serde_json::to_value(item_to_json(
            "srv",
            &row(),
            &ItemImageFlags {
                series_logo: Some(11),
                series_thumb: Some(12),
                ..Default::default()
            },
            None,
            None,
            None,
        ))
        .unwrap();
        assert_eq!(v["ParentLogoItemId"], "i-9");
        assert_eq!(v["ParentLogoImageTag"], "img-11");
        assert_eq!(v["ParentThumbItemId"], "i-9");
        assert_eq!(v["ParentThumbImageTag"], "img-12");

        // series 无 logo/thumb → ParentLogo* / ParentThumb* 全省略
        let v2 = serde_json::to_value(item_to_json(
            "srv",
            &row(),
            &ItemImageFlags::default(),
            None,
            None,
            None,
        ))
        .unwrap();
        assert!(!v2.as_object().unwrap().contains_key("ParentLogoItemId"));
        assert!(!v2.as_object().unwrap().contains_key("ParentLogoImageTag"));
        assert!(!v2.as_object().unwrap().contains_key("ParentThumbItemId"));
        assert!(!v2.as_object().unwrap().contains_key("ParentThumbImageTag"));
    }

    /// MediaStreamDto / MediaSourceDto 序列化形状：
    /// RequiredHttpHeaders 恒 `{}`；流类型字段按 Video/Audio 设置、其余 skip 省略；
    /// NalLengthSize 仅 h264/AVC 出现。
    #[test]
    fn media_stream_dto_shape() {
        use emrs_core::stores::StreamInfo;

        // RequiredHttpHeaders 恒空对象（非 null）
        assert_eq!(
            serde_json::to_value(RequiredHttpHeaders {}).unwrap(),
            json!({})
        );

        // Video 流：h264/AVC → NalLengthSize="4"；无 Language/Channels（skip）
        let v = StreamInfo {
            stream_type: "Video".into(),
            codec: "h264".into(),
            index: Some(0),
            width: Some(1920),
            height: Some(1080),
            frame_rate: Some(23.976),
            bit_rate: Some(7541653),
            color_transfer: Some("bt709".into()),
            color_primaries: Some("bt709".into()),
            color_space: Some("bt709".into()),
            is_avc: Some(true),
            is_default: Some(true),
            time_base: Some("1/1000".into()),
            pixel_format: Some("yuv420p".into()),
            profile: Some("High".into()),
            level: Some(40),
            title: None,
            language: None,
            channels: None,
            sample_rate: None,
            bit_depth: None,
            display_aspect_ratio: None,
            is_forced: None,
            refs: None,
            is_interlaced: None,
            channel_layout: None,
            sample_aspect_ratio: None,
        };
        let s = serde_json::to_value(stream_json(&v, 0, Some("mp4"))).unwrap();
        assert_eq!(s["Type"], "Video");
        assert_eq!(s["Codec"], "h264");
        assert_eq!(s["VideoRange"], "SDR");
        assert_eq!(s["BitRate"], 7541653);
        assert_eq!(s["Height"], 1080);
        assert_eq!(s["NalLengthSize"], "4", "h264/AVC 应输出 NalLengthSize=4");
        assert!(
            !s.as_object().unwrap().contains_key("Language"),
            "Video 不应带 Language"
        );
        assert!(
            !s.as_object().unwrap().contains_key("Channels"),
            "Video 不应带 Channels"
        );

        // 非 h264 → 无 NalLengthSize
        let mut v2 = v.clone();
        v2.codec = "hevc".into();
        v2.is_avc = Some(false);
        let s2 = serde_json::to_value(stream_json(&v2, 0, Some("mkv"))).unwrap();
        assert!(
            !s2.as_object().unwrap().contains_key("NalLengthSize"),
            "非 h264/AVC 不应带 NalLengthSize"
        );

        // Audio 流：带 Language/Channels；无 Height
        let a = StreamInfo {
            stream_type: "Audio".into(),
            codec: "flac".into(),
            index: Some(1),
            language: Some("jpn".into()),
            channels: Some(2),
            sample_rate: Some(48000),
            channel_layout: Some("stereo".into()),
            is_default: Some(true),
            time_base: Some("1/1000".into()),
            profile: None,
            title: None,
            width: None,
            height: None,
            frame_rate: None,
            bit_rate: None,
            pixel_format: None,
            bit_depth: None,
            color_primaries: None,
            color_space: None,
            color_transfer: None,
            display_aspect_ratio: None,
            is_forced: None,
            level: None,
            refs: None,
            is_interlaced: None,
            sample_aspect_ratio: None,
            is_avc: None,
        };
        let sa = serde_json::to_value(stream_json(&a, 1, Some("mp4"))).unwrap();
        assert_eq!(sa["Type"], "Audio");
        assert_eq!(sa["Language"], "jpn");
        assert_eq!(sa["Channels"], 2);
        assert!(
            !sa.as_object().unwrap().contains_key("Height"),
            "Audio 不应带 Height"
        );
    }

    /// PersonDetailDto 形状：primary_image_id → `ImageTags.Primary`（`img-{图片行 id}`）；
    /// birthday → `PremiereDate` + `ProductionYear`（前 4 位 parse）；description → `Overview`；
    /// tmdb → `ProviderIds.Tmdb`；无则省略（保旧 `json!` 形状）。
    #[test]
    fn person_detail_dto_shape() {
        use emrs_core::stores::taxonomy_store::PersonRow;
        let person = PersonRow {
            id: 7,
            tmdb_id: Some("12345".into()),
            name: "演员甲".into(),
            original_name: None,
            gender: 0,
            description: Some("简介".into()),
            birthday: Some("1990-05-01".into()),
            deathday: None,
            created_at: "2026-01-01T00:00:00.0000000Z".into(),
            updated_at: "2026-01-02T00:00:00.0000000Z".into(),
        };
        // 有头像图片行 id=77 → tag img-77
        let v = serde_json::to_value(person_to_json("srv", &person, Some(77))).unwrap();
        assert_eq!(v["Type"], "Person");
        assert_eq!(v["Name"], "演员甲");
        assert_eq!(v["Id"], "p-7");
        assert_eq!(v["ServerId"], "srv");
        assert_eq!(v["ProviderIds"]["Tmdb"], "12345");
        assert_eq!(v["ImageTags"], json!({ "Primary": "img-77" }));
        assert_eq!(v["BackdropImageTags"], json!([]));
        assert_eq!(v["ProductionLocations"], json!([]));
        assert_eq!(v["ExternalUrls"], json!([]));
        assert_eq!(v["PrimaryImageAspectRatio"], 0.666667);
        assert_eq!(v["DateCreated"], "2026-01-01T00:00:00.0000000Z");
        assert_eq!(v["DateModified"], "2026-01-02T00:00:00.0000000Z");
        assert_eq!(v["PremiereDate"], "1990-05-01");
        assert_eq!(v["ProductionYear"], 1990);
        assert_eq!(v["Overview"], "简介");

        // 无头像 → ImageTags={}
        let v2 = serde_json::to_value(person_to_json("srv", &person, None)).unwrap();
        assert_eq!(v2["ImageTags"], json!({}));

        // 无 birthday/description → PremiereDate/ProductionYear/Overview 省略
        let mut p2 = person.clone();
        p2.birthday = None;
        p2.description = None;
        let v3 = serde_json::to_value(person_to_json("srv", &p2, None)).unwrap();
        assert!(!v3.as_object().unwrap().contains_key("PremiereDate"));
        assert!(!v3.as_object().unwrap().contains_key("ProductionYear"));
        assert!(!v3.as_object().unwrap().contains_key("Overview"));

        // 无 tmdb_id → ProviderIds={}
        let mut p3 = person.clone();
        p3.tmdb_id = None;
        let v4 = serde_json::to_value(person_to_json("srv", &p3, None)).unwrap();
        assert_eq!(v4["ProviderIds"], json!({}));
    }
}
