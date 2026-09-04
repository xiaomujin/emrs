//! ItemDto 详情成型：`ItemRow` → Emby item JSON（依赖 `shared` / `person` / `media_source`）。
//!
//! [`item_to_json`] 为纯函数，图片存在标志由调用方批量预取（[`shared::ItemImageFlags`]）；
//! 需要 DB 的媒体源成型（[`attach_media_sources`]）才接收 `&Db`。

use serde::Serialize;

use super::media_source::{MediaSourceDto, media_sources_json};
use super::person::person_item_dto;
use super::shared::{
    GenreItemDto, ItemImageFlags, StudioDto, emby_date, item_user_data, provider_ids,
};
use emby_proto::{
    BaseItemDto, ExternalUrlDto, ImageTagsDto, PersonItemDto, genre_id, image_tag, item_id,
    library_id, studio_id,
};
use emrs_infra::db::Db;
use emrs_infra::stores::{ItemRow, ItemsStore};
use emrs_infra::stores::taxonomy_store::ItemTaxonomy;

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
            let people: Vec<PersonItemDto> = t.people.iter().map(person_item_dto).collect();
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

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::shared::test_row;

    use serde_json::json;

    /// 类型化子对象（GenreItems / People / Studios / ImageTags）序列化形状
    /// 必须与旧 `json!` 版一致（PascalCase key、前缀 Id、Character 省略语义）。
    #[test]
    fn typed_taxonomy_subobjects_shape() {
        use emrs_infra::stores::taxonomy_store::{ItemTaxonomy, PersonBrief};
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
            &test_row(),
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
            &test_row(),
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
            &test_row(),
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
            &test_row(),
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
        let mut season = test_row();
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
        let mut series = test_row();
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
            &test_row(),
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
            &test_row(),
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
            &test_row(),
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
            &test_row(),
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
            &test_row(),
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
}