//! Emby 列表端点通用响应壳。

use serde::Serialize;

/// Emby 分页 Items 响应壳：`{ Items, TotalRecordCount }`。
///
/// 列表端点（Resume / NextUp / Seasons / Episodes / Genres / Persons / Tags 等）
/// 统一用它；`T` 为条目类型（通用 `serde_json::Value` 或类型化 DTO）。
#[derive(Serialize)]
pub struct ItemsResponse<T> {
    #[serde(rename = "Items")]
    pub items: Vec<T>,
    #[serde(rename = "TotalRecordCount")]
    pub total_record_count: usize,
}

/// `GET /Items/Counts` 响应：各类型条目计数。
///
/// 对齐参考 Emby 字段集；当前为 stub（全零），待计数查询落地后由
/// 路由层填真实值。类型化结构体 + `Default` 全零，序列化走 serde。
#[derive(Serialize, Default)]
#[serde(rename_all = "PascalCase")]
pub struct ItemsCounts {
    pub movie_count: i64,
    pub series_count: i64,
    pub episode_count: i64,
    pub artist_count: i64,
    pub program_count: i64,
    pub trailer_count: i64,
    pub song_count: i64,
    pub album_count: i64,
    pub music_video_count: i64,
    pub box_set_count: i64,
    pub book_count: i64,
    pub item_count: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_response_wraps_items_and_count() {
        let resp = ItemsResponse {
            items: vec![serde_json::json!({ "Id": "42", "Type": "Episode" })],
            total_record_count: 1,
        };
        let out = serde_json::to_value(resp).unwrap();
        // 对齐 ViewsResponse：Items / TotalRecordCount PascalCase 壳。
        assert_eq!(out["TotalRecordCount"], 1);
        assert_eq!(out["Items"].as_array().unwrap().len(), 1);
        assert_eq!(out["Items"][0]["Id"], "42");
    }
}
