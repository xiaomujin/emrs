//! media_source / external_subtitle 读写、get_playback_info 组装。
//!
//! 新表 `media_source` 替代旧 `video_media`；`external_subtitle` 替代旧
//! `video_subtitle`（只存外部字幕，内嵌流走 `media_source.metadata` JSON）。
//! PlaybackInfo 输出 [`super::MediaSourceRow`]，路由层 `media_sources_json` /
//! `media_streams_json` 直接消费。

use anyhow::{Context, Result};

use super::MediaSourceRow;
use crate::db::Db;
use crate::importer::probe::StreamInfo;

use std::collections::HashMap;

/// 批量查询多个 item 的视频分辨率（width/height），列表端点顶层输出用。
///
/// 从 `media_source.metadata`（ffprobe JSONB）解析首个 `Video` 流的宽高；
/// 一个 item 多源时取首源（`ORDER BY id` + map insert-if-absent）。空输入返回空 map。
/// 与 [`crate::emby::dto`] 内 `media_streams_json` 同源解析逻辑，不在 SQL 里
/// 做 JSON 提取（SQLite/MySQL/PG 函数分歧）。
pub async fn video_dims_batch(
    db: &Db,
    item_ids: &[i64],
) -> Result<HashMap<i64, (Option<i64>, Option<i64>)>> {
    let mut out: HashMap<i64, (Option<i64>, Option<i64>)> = HashMap::new();
    if item_ids.is_empty() {
        return Ok(out);
    }
    let placeholders = std::iter::repeat_n("?", item_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT item_id, metadata FROM media_source \
         WHERE item_id IN ({placeholders}) ORDER BY id"
    );
    let mut q = sqlx::query_as::<_, (i64, Option<String>)>(&sql);
    for id in item_ids {
        q = q.bind(id);
    }
    let rows = q.fetch_all(db.pool()).await.context("video_dims_batch")?;
    for (item_id, metadata) in rows {
        if out.contains_key(&item_id) {
            continue; // 首源胜出
        }
        let dims = metadata
            .as_deref()
            .filter(|s| !s.is_empty())
            .and_then(|s| serde_json::from_str::<Vec<StreamInfo>>(s).ok())
            .and_then(|list| list.into_iter().find(|s| s.stream_type == "Video"))
            .map(|v| (v.width, v.height));
        if let Some(d) = dims {
            out.insert(item_id, d);
        }
    }
    Ok(out)
}

/// 查询 PlaybackInfo（媒体源信息，单版本）。
///
/// 新表 `media_source` 统一挂 `item_id`（不再区分 video_list_id / video_episode_id）；
/// 旧 `file_metadata` → `media_source.metadata`，`file_chapters` → `media_source.chapters`。
/// 返回该 item 的首条 media_source 行；多版本用 [`list_media_sources`]。
pub async fn get_playback_info(db: &Db, item_id: i64) -> Result<Option<MediaSourceRow>> {
    let row = sqlx::query_as::<_, MediaSourceRow>(
        "SELECT ms.uuid, ms.name, ms.file_size, ms.file_duration AS file_second, \
                ms.container AS file_container, \
                CASE ms.protocol WHEN 'file' THEN 'local' WHEN 'strm' THEN 'strm' \
                                 ELSE ms.protocol END AS path_type, \
                COALESCE(ms.path, ms.remote_path) AS path_url, \
                ms.metadata AS file_metadata, ms.chapters AS file_chapters, \
                ms.item_id, ms.id AS media_id \
         FROM media_source ms \
         WHERE ms.item_id = ? \
         ORDER BY ms.id LIMIT 1",
    )
    .bind(item_id)
    .fetch_optional(db.pool())
    .await
    .context("get_playback_info")?;
    Ok(row)
}

/// 查询 item 的所有 media_source 行（多版本 PlaybackInfo 用）。
pub async fn list_media_sources(db: &Db, item_id: i64) -> Result<Vec<MediaSourceRow>> {
    let rows = sqlx::query_as::<_, MediaSourceRow>(
        "SELECT ms.uuid, ms.name, ms.file_size, ms.file_duration AS file_second, \
                ms.container AS file_container, \
                CASE ms.protocol WHEN 'file' THEN 'local' WHEN 'strm' THEN 'strm' \
                                 ELSE ms.protocol END AS path_type, \
                COALESCE(ms.path, ms.remote_path) AS path_url, \
                ms.metadata AS file_metadata, ms.chapters AS file_chapters, \
                ms.item_id, ms.id AS media_id \
         FROM media_source ms \
         WHERE ms.item_id = ? \
         ORDER BY ms.id",
    )
    .bind(item_id)
    .fetch_all(db.pool())
    .await
    .context("list_media_sources")?;
    Ok(rows)
}
