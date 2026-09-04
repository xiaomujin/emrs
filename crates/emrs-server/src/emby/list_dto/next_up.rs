//! NextUp 条目（Shows/NextUp 专用）——唯一在列表保留 taxonomy（People）的端点。

use serde::Serialize;

use super::super::dto::{ItemImageFlags, emby_date, person_item_dto, provider_ids};
use super::shared::{aspect, build_card_images, user_data};
use emby_proto::{BaseItemDto, item_id};
use emrs_infra::stores::taxonomy_store::ItemTaxonomy;
use emrs_infra::stores::ItemRow;

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
    people: Vec<emby_proto::PersonItemDto>,
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
    /// `ItemRow` + 预取图片标志 + `taxonomy`（含 People）→ NextUp 条目。
    pub fn from_row(
        server_id: &str,
        item: &ItemRow,
        flags: &ItemImageFlags,
        tax: &ItemTaxonomy,
    ) -> Self {
        let img = build_card_images(item, flags);
        let is_folder = matches!(item.item_type.as_str(), "Series" | "Season");
        let people: Vec<_> = tax.people.iter().map(person_item_dto).collect();
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
            premiere_date: item.date_air.as_deref().map(emby_date),
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
    use super::super::shared::{test_keys, test_row, test_series_fallback_flags};

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
            &test_row("Episode"),
            &test_series_fallback_flags(),
            &tax,
        ))
        .unwrap();
        let ks = test_keys(&out);
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