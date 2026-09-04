//! Resume 卡片（`/Users/{uid}/Items/Resume` 专用）。
//!
//! 图片**只返回该 item 自身的 Primary**（无上级回退，episode 自身无图 → `ImageTags` 为空 `{}`）；
//! 与浏览列表卡片（走 `build_card_images` 回退）语义不同，故独立成模块。

use serde::Serialize;

use super::super::dto::provider_ids_map;
use emby_proto::{BaseItemDto, ImageTagsDto, ViewsUserData, image_tag, item_id};
use emrs_infra::stores::ResumeEntry;

/// Resume 卡片 JSON。
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

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::shared::test_resume_entry;

    #[test]
    fn resume_card_images_are_own_primary_only() {
        // episode 自身主图 → ImageTags.Primary 为自身图；BackdropImageTags 恒空数组；不输出上级剧集图片字段。
        let out = serde_json::to_value(ResumeCardJson::from_row(
            "srv",
            &test_resume_entry("Episode"),
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
            &test_resume_entry("Episode"),
            None,
        ))
        .unwrap();
        assert_eq!(out["ImageTags"], serde_json::json!({}));
        assert_eq!(out["BackdropImageTags"], serde_json::json!([]));
    }
}