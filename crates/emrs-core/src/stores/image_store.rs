//! item_image 查询：primary / backdrop 图片 URL。
//!
//! `item_image` 表用 `parent_type`（'item' / 'people'）+ `parent_id` 定位图片；
//! video 各类型（movie/series/season/episode）统一 `parent_type='item'`，
//! 季/集无自有图片时回退到上级剧集。

use std::collections::HashMap;

use anyhow::{Context, Result};

use crate::db::Db;

/// 某对象各类型图片的行 id 集合（来自 `item_image.id`）。
///
/// Primary / Logo / Thumb / Banner 各取 id 最小的一行（Primary 有唯一约束，
/// 其余类型实际也按单图处理）；Backdrop 可多张，按 id 升序收集，
/// 与图片路由 `OFFSET = index` 的取图顺序一一对应。
#[derive(Debug, Clone, Default)]
pub struct ImageTypeIds {
    pub primary: Option<i64>,
    pub backdrops: Vec<i64>,
    pub logo: Option<i64>,
    pub thumb: Option<i64>,
    pub banner: Option<i64>,
}

/// 查询某对象的指定类型图片（`item_image` 表），返回 `(图片行 id, path_url)`。
/// `parent_type`：`item`（video 各类型）/ `people`（人物）。
/// `relation_id`：对应 `item_image.parent_id`（即 item.id / people.id）。
/// `image_type`：`Primary` / `Backdrop`（路由层首字母大写；表存小写）。
/// `index`：同类型多行时取第 index 行（按 id 升序，0 起）；单图类型恒传 0。
pub async fn get_image_path(
    db: &Db,
    parent_type: &str,
    relation_id: i64,
    image_type: &str,
    index: i64,
) -> Result<Option<(i64, String)>> {
    let img_type_lower = image_type.to_ascii_lowercase();
    let row: Option<(i64, String)> = sqlx::query_as(
        "SELECT id, path_url FROM item_image \
         WHERE parent_type = ? AND parent_id = ? AND image_type = ? \
           AND path_url IS NOT NULL \
         ORDER BY id ASC \
         LIMIT 1 OFFSET ?",
    )
    .bind(parent_type)
    .bind(relation_id)
    .bind(&img_type_lower)
    .bind(index.max(0))
    .fetch_optional(db.pool())
    .await
    .context("query image path")?;
    Ok(row)
}

/// 批量查询多个 item 各类型图片的行 id（Primary / Backdrop / Logo / Thumb / Banner），避免列表端点 N+1。
///
/// 一次 `IN (...)` 查询拿回 `parent_id → ImageTypeIds`（仅统计 `path_url` 非空的图片，
/// 按 id 升序）。空数组直接返回空 map。
/// Logo/Thumb 供 Episode 输出 `ParentLogoImageTag` / `ParentThumbImageTag`（回退上级剧集）。
pub async fn image_ids_batch(db: &Db, relation_ids: &[i64]) -> Result<HashMap<i64, ImageTypeIds>> {
    let mut ids: HashMap<i64, ImageTypeIds> = HashMap::new();
    if relation_ids.is_empty() {
        return Ok(ids);
    }
    let placeholders = std::iter::repeat_n("?", relation_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT parent_id, image_type, id FROM item_image \
         WHERE parent_type = 'item' AND parent_id IN ({placeholders}) \
           AND path_url IS NOT NULL \
         ORDER BY id ASC"
    );
    let mut q = sqlx::query_as::<_, (i64, String, i64)>(&sql);
    for id in relation_ids {
        q = q.bind(id);
    }
    let rows = q
        .fetch_all(db.pool())
        .await
        .context("query image ids batch")?;
    for (parent_id, image_type, image_id) in rows {
        let e = ids.entry(parent_id).or_default();
        match image_type.to_ascii_lowercase().as_str() {
            // 单图类型取 id 最小首行（已按 id 升序，首个即最小）；Backdrop 全量收集。
            "primary" => {
                e.primary.get_or_insert(image_id);
            }
            "backdrop" => e.backdrops.push(image_id),
            "logo" => {
                e.logo.get_or_insert(image_id);
            }
            "thumb" => {
                e.thumb.get_or_insert(image_id);
            }
            "banner" => {
                e.banner.get_or_insert(image_id);
            }
            _ => {}
        }
    }
    Ok(ids)
}

/// 批量查询多个 item 的 Primary 图片行 id（一次 `IN (...)`，避免 N+1）。
///
/// 仅命中 `image_type='primary'`（每个 parent 有唯一约束，恒一行），返回
/// `parent_id → 图片行 id`。Resume 等只需主图的列表用它，避免拉回 backdrop/logo 等无用类型。
/// 空数组直接返回空 map。
pub async fn image_primary_batch(db: &Db, relation_ids: &[i64]) -> Result<HashMap<i64, i64>> {
    let mut out: HashMap<i64, i64> = HashMap::new();
    if relation_ids.is_empty() {
        return Ok(out);
    }
    let placeholders = std::iter::repeat_n("?", relation_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT parent_id, id FROM item_image \
         WHERE parent_type = 'item' AND image_type = 'primary' AND path_url IS NOT NULL \
           AND parent_id IN ({placeholders}) \
         ORDER BY id ASC"
    );
    let mut q = sqlx::query_as::<_, (i64, i64)>(&sql);
    for id in relation_ids {
        q = q.bind(id);
    }
    let rows = q
        .fetch_all(db.pool())
        .await
        .context("query primary image ids batch")?;
    for (parent_id, image_id) in rows {
        // primary 有唯一约束，理论一对一；保留首个（id 最小）。
        out.entry(parent_id).or_insert(image_id);
    }
    Ok(out)
}

/// 查询 Season/Episode 的上级 item 指定类型图片 URL。
/// `item_type`：`season` / `episode`（DB 小写）；其余返回 None。
/// 季/集无自有图片时回退到上级 series item。
/// `index`：透传给 [`get_image_path`]，上级同类型多行时取第 index 行。
pub async fn get_parent_image_path(
    db: &Db,
    item_type: &str,
    id: i64,
    image_type: &str,
    index: i64,
) -> Result<Option<String>> {
    // 新表 item 自引用：season.parent_id = series_id，episode.parent_id = season_id。
    // 季/集无自有图片时回退到上级 series item。
    let sql = match item_type {
        "season" => "SELECT parent_id FROM item WHERE id = ? AND type = 'season' LIMIT 1",
        "episode" => {
            "SELECT season_item.id \
             FROM item ep JOIN item season_item ON season_item.id = ep.parent_id \
             WHERE ep.id = ? AND ep.type = 'episode' LIMIT 1"
        }
        _ => return Ok(None),
    };
    let parent_id: Option<i64> = sqlx::query_scalar(sql)
        .bind(id)
        .fetch_optional(db.pool())
        .await
        .context("query parent item")?;
    match parent_id {
        Some(pid) if pid > 0 => {
            let row = get_image_path(db, "item", pid, image_type, index).await?;
            Ok(row.map(|(_, path)| path))
        }
        _ => Ok(None),
    }
}
