//! Emby `/Users/{id}/Items/Latest` 响应 DTO：轻量卡片条目成型。
//!
//! 对齐参考 `emby_json/Latest_emos.json`：Latest 是卡片轮播，只返回
//! 核心字段（Name/Id/海报/年份/UserData），不携带媒体源 / 详情字段。
//! 与 `views.rs`（CollectionFolderView）同构：类型化结构体 + `Serialize`，
//! 图片存在标志由路由层批量预取（[`ItemImageFlags`]），本层纯函数成型、零 DB 查询。

use serde::Serialize;

use super::dto::{GenreItemDto, ItemImageFlags, PersonItemDto, item_user_data, provider_ids};
use super::{BaseItemDto, ImageTagsDto, genre_id, image_tag, item_id};
use emrs_infra::stores::{ItemRow, taxonomy_store::ItemTaxonomy};

/// 单个 Latest 条目（轻量卡片，对齐 emos 参考字段集）。
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct LatestItemJson {
    #[serde(flatten)]
    base: BaseItemDto,
    production_year: Option<i64>,
    genres: Vec<String>,
    people: Vec<PersonItemDto>,
    genre_items: Vec<GenreItemDto>,
    media_type: String,
    can_delete: bool,
    can_download: bool,
}

impl LatestItemJson {
    /// 由 `ItemRow` + 批量预取的图片标志 + taxonomy 成型（纯函数，无 DB 查询）。
    pub fn from_row(
        server_id: &str,
        item: &ItemRow,
        flags: &ItemImageFlags,
        tax: &ItemTaxonomy,
    ) -> Self {
        let id = item_id(item.id);

        // 图片标记：自身有 Primary 才输出 ImageTags（与 item_to_json 一致）。
        // tag 值为图片表行 id（`img-{id}`），即 Emby 的图片唯一标记。
        let image_tags = flags
            .own_primary
            .map(|img_id| ImageTagsDto {
                primary: Some(image_tag(img_id)),
                ..Default::default()
            })
            .unwrap_or_default();
        let backdrop_image_tags: Vec<String> = flags
            .own_backdrops
            .iter()
            .map(|img_id| image_tag(*img_id))
            .collect();

        let production_year = item
            .date_air
            .as_deref()
            .and_then(|d| d.get(0..4))
            .and_then(|y| y.parse::<i64>().ok());

        // 分类 / 演职员（与 item_to_json 的 attach_taxonomy 同构）
        let genres: Vec<String> = tax.genres.iter().map(|(_, n)| n.clone()).collect();
        let genre_items: Vec<GenreItemDto> = tax
            .genres
            .iter()
            .map(|(id, name)| GenreItemDto {
                name: name.clone(),
                id: genre_id(*id),
            })
            .collect();
        let people: Vec<PersonItemDto> =
            tax.people.iter().map(PersonItemDto::from_person).collect();

        Self {
            base: BaseItemDto {
                name: item.title.clone(),
                server_id: server_id.to_string(),
                id,
                item_type: item.item_type.clone(),
                is_folder: matches!(item.item_type.as_str(), "Movie" | "Series" | "Season"),
                date_created: item.created_at.clone(),
                user_data: item_user_data(item),
                primary_image_aspect_ratio: if item.item_type == "Episode" {
                    1.777778
                } else {
                    0.666667
                },
                image_tags,
                backdrop_image_tags,
                provider_ids: provider_ids(item),
            },
            production_year,
            genres,
            people,
            genre_items,
            media_type: "Video".to_string(),
            can_delete: false,
            can_download: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emrs_infra::stores::ItemRow;
    use emrs_infra::stores::taxonomy_store::{ItemTaxonomy, PersonBrief};

    fn row() -> ItemRow {
        ItemRow {
            id: 1,
            library_id: None,
            item_type: "Series".into(),
            title: "石纪元".into(),
            description: None,
            date_air: Some("2019-01-01".into()),
            created_at: "2026-01-20T00:17:47.0000000Z".into(),
            updated_at: String::new(),
            container: None,
            file_second: None,
            uuid: None,
            name: None,
            path_type: None,
            path_url: None,
            play_ms: 0,
            is_complete: 0,
            play_count: 0,
            is_favorite: 0,
            season_number: None,
            episode_number: None,
            series_id: None,
            series_name: None,
            season_id: None,
            season_name: None,
            is_virtual: 0,
            tmdb_id: Some("86031".into()),
            imdb_id: None,
            tvdb_id: None,
            community_rating: Some(7.8),
            official_rating: None,
            tagline: None,
            sort_title: None,
            end_date: None,
            status: None,
            production_year: None,
        }
    }

    fn tax() -> ItemTaxonomy {
        ItemTaxonomy {
            genres: vec![(28, "动作".into()), (18, "剧情".into())],
            people: vec![PersonBrief {
                id: 100,
                name: "石纪元 主演".into(),
                role: "Actor".into(),
                character_name: Some("石神千空".into()),
                primary_image_id: Some(101),
            }],
            studios: vec![],
            tags: vec![],
        }
    }

    #[test]
    fn latest_json_matches_emos_reference_keys() {
        let flags = ItemImageFlags {
            own_primary: Some(1),
            own_backdrops: vec![],
            ..Default::default()
        };
        let v = LatestItemJson::from_row("emya", &row(), &flags, &tax());
        let out = serde_json::to_value(v).unwrap();
        let obj = out.as_object().unwrap();

        // 对齐 Latest_emos.json 字段集（精简卡片字段）。
        let mut expected: Vec<&str> = vec![
            "Name",
            "ServerId",
            "Id",
            "DateCreated",
            "ProductionYear",
            "ProviderIds",
            "Genres",
            "People",
            "GenreItems",
            "IsFolder",
            "Type",
            "MediaType",
            "UserData",
            "PrimaryImageAspectRatio",
            "ImageTags",
            "BackdropImageTags",
            "CanDelete",
            "CanDownload",
        ];
        expected.sort_unstable();
        let mut actual: Vec<&str> = obj.keys().map(String::as_str).collect();
        actual.sort_unstable();
        assert_eq!(actual, expected, "key 集合/大小写与 emos 参考不一致");

        assert_eq!(obj["Type"], "Series");
        assert_eq!(obj["MediaType"], "Video");
        assert_eq!(obj["IsFolder"], true);
        assert_eq!(obj["ProductionYear"], 2019);
        assert_eq!(obj["CanDelete"], false);
        assert_eq!(obj["CanDownload"], false);
        assert_eq!(obj["ProviderIds"]["Tmdb"], "86031");
        assert_eq!(obj["ImageTags"]["Primary"], "img-1");
        assert_eq!(obj["BackdropImageTags"], serde_json::json!([]));
        assert_eq!(obj["Genres"], serde_json::json!(["动作", "剧情"]));
        assert_eq!(obj["GenreItems"][0]["Name"], "动作");
        assert_eq!(obj["People"][0]["Name"], "石纪元 主演");
        assert_eq!(obj["People"][0]["Id"], "p-100");
        assert_eq!(obj["People"][0]["Role"], "Actor");
        assert_eq!(obj["People"][0]["Character"], "石神千空");
        assert_eq!(obj["People"][0]["PrimaryImageTag"], "img-101");
    }

    #[test]
    fn latest_no_image_omits_image_tags() {
        let v = LatestItemJson::from_row("emya", &row(), &ItemImageFlags::default(), &tax());
        let out = serde_json::to_value(v).unwrap();
        assert_eq!(out["ImageTags"], serde_json::json!({}));
        assert_eq!(out["BackdropImageTags"], serde_json::json!([]));
    }
}
