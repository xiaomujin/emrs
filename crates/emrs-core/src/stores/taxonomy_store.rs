//! genre / people / studio / tag + item_genre / item_people / item_studio / item_tag 查询。
//!
//! `/Genres`、`/Persons`、`/Studios`、`/Tags` 端点数据源。旧版为空 stub，新表落地真数据。

use anyhow::{Context, Result};

use std::collections::{HashMap, HashSet};

use super::{ItemRow, ItemsResult};
use crate::db::Db;

/// 规范命名表（genre / studio / tag）按 `tmdb_id` 幂等 upsert，返回行 id。
///
/// 三表结构一致：`(id, tmdb_id, name, created_at, updated_at)`。命中已存在则同步
/// `name`/`updated_at`；否则插入后取回自增 id。任一步失败返回 `0`（调用方按 0 跳过关联），
/// 语义与迁移前 scanner 内的 `upsert_genre/studio/tag` 逐字一致。
///
/// `table` 为调用方传入的固定字面量（`"genre"|"studio"|"tag"`），非用户输入。
/// 本表写路径的唯一属主（I1）。
pub async fn upsert_named(
    db: &Db,
    table: &'static str,
    tmdb_id: &str,
    name: &str,
    now: &str,
) -> i64 {
    let existing: Option<i64> =
        sqlx::query_scalar(&format!("SELECT id FROM {table} WHERE tmdb_id = ? LIMIT 1"))
            .bind(tmdb_id)
            .fetch_optional(db.pool())
            .await
            .ok()
            .flatten();
    if let Some(id) = existing {
        let _ = sqlx::query(&format!(
            "UPDATE {table} SET name = ?, updated_at = ? WHERE id = ?"
        ))
        .bind(name)
        .bind(now)
        .bind(id)
        .execute(db.pool())
        .await;
        return id;
    }
    let _ = sqlx::query(&format!(
        "INSERT INTO {table} (tmdb_id, name, created_at, updated_at) VALUES (?, ?, ?, ?)"
    ))
    .bind(tmdb_id)
    .bind(name)
    .bind(now)
    .bind(now)
    .execute(db.pool())
    .await;
    sqlx::query_scalar::<_, i64>(&format!("SELECT id FROM {table} ORDER BY id DESC LIMIT 1"))
        .fetch_one(db.pool())
        .await
        .unwrap_or(0)
}

/// 规范命名表（genre / studio / tag）按 `name` 幂等 upsert，返回行 id。
///
/// 命中同名直接返回其 id（不覆盖），否则插入 `(name, created_at, updated_at)` 后取回自增 id。
/// 用于无 tmdb_id 的按名归类（NFO / 手动识别）。`table` 为固定字面量。
pub async fn upsert_by_name(db: &Db, table: &'static str, name: &str) -> i64 {
    let now = crate::emby::format_time_now();
    let existing: Option<i64> =
        sqlx::query_scalar(&format!("SELECT id FROM {table} WHERE name = ? LIMIT 1"))
            .bind(name)
            .fetch_optional(db.pool())
            .await
            .ok()
            .flatten();
    if let Some(id) = existing {
        return id;
    }
    let _ = sqlx::query(&format!(
        "INSERT INTO {table} (name, created_at, updated_at) VALUES (?, ?, ?)"
    ))
    .bind(name)
    .bind(&now)
    .bind(&now)
    .execute(db.pool())
    .await;
    sqlx::query_scalar::<_, i64>(&format!("SELECT id FROM {table} ORDER BY id DESC LIMIT 1"))
        .fetch_one(db.pool())
        .await
        .unwrap_or(0)
}

/// 单个人物 brief（Emby `People` 数组元素）。
#[derive(Debug, Clone)]
pub struct PersonBrief {
    pub id: i64,
    pub name: String,
    pub role: String,
    pub character_name: Option<String>,
    /// 头像图片行 id（`item_image.parent_type='people'`，primary，无则 None）；
    /// 供 `People[].PrimaryImageTag`（`img-{id}`，tag 标识图片本身）。
    pub primary_image_id: Option<i64>,
}

/// people 表行（人员详情端点数据源）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PersonRow {
    pub id: i64,
    pub tmdb_id: Option<String>,
    pub name: String,
    pub original_name: Option<String>,
    pub gender: i64,
    pub description: Option<String>,
    pub birthday: Option<String>,
    pub deathday: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 单个 item 的分类 / 演职员聚合（批量预取，供 `item_to_json` / Latest 附加字段）。
#[derive(Debug, Clone, Default)]
pub struct ItemTaxonomy {
    /// `(genre_id, name)`，按名排序；同时供 `Genres`（仅名）与 `GenreItems`（名 + Id）。
    pub genres: Vec<(i64, String)>,
    /// 演职员（按 `item_people.sort_order` 排序）。
    pub people: Vec<PersonBrief>,
    /// `(studio_id, name)`，按 `item_studio.sort_order` 排序；Emby `Studios` 数组。
    pub studios: Vec<(i64, String)>,
    /// 标签名（按 `item_tag.sort_order` 排序）；Emby `Tags` 数组（仅名字符串）。
    pub tags: Vec<String>,
}

/// 批量查询多个 item 的 genres + people（`item_genre` / `item_people` 关联表）。
/// 空数组直接返回空 map。避免列表端点对每条 item 单独查库（N+1）。
pub async fn taxonomy_batch(
    db: &Db,
    item_ids: &[i64],
) -> Result<std::collections::HashMap<i64, ItemTaxonomy>> {
    let mut out: std::collections::HashMap<i64, ItemTaxonomy> = std::collections::HashMap::new();
    if item_ids.is_empty() {
        return Ok(out);
    }
    let placeholders = std::iter::repeat_n("?", item_ids.len())
        .collect::<Vec<_>>()
        .join(", ");

    // genres：item_genre → genre
    let genre_sql = format!(
        "SELECT ig.item_id, g.id, g.name FROM item_genre ig \
         JOIN genre g ON g.id = ig.genre_id \
         WHERE ig.item_id IN ({placeholders}) ORDER BY g.name"
    );
    let mut gq = sqlx::query_as::<_, (i64, i64, String)>(&genre_sql);
    for id in item_ids {
        gq = gq.bind(id);
    }
    for (item_id, genre_id, name) in gq
        .fetch_all(db.pool())
        .await
        .context("query item genres batch")?
    {
        out.entry(item_id)
            .or_default()
            .genres
            .push((genre_id, name));
    }

    // people：item_people → people
    let people_sql = format!(
        "SELECT ip.item_id, p.id, p.name, ip.role, ip.character_name \
         FROM item_people ip \
         JOIN people p ON p.id = ip.people_id \
         WHERE ip.item_id IN ({placeholders}) ORDER BY ip.sort_order"
    );
    let mut pq = sqlx::query_as::<_, (i64, i64, String, String, Option<String>)>(&people_sql);
    for id in item_ids {
        pq = pq.bind(id);
    }
    let people_rows: Vec<(i64, i64, String, String, Option<String>)> = pq
        .fetch_all(db.pool())
        .await
        .context("query item people batch")?;

    // 收集所有 people_id，批量查头像图片行 id（每人取 id 最小一张）
    let all_people_ids: HashSet<i64> = people_rows.iter().map(|(_, pid, _, _, _)| *pid).collect();
    let people_ids_vec: Vec<i64> = all_people_ids.into_iter().collect();
    let mut people_image_id: HashMap<i64, i64> = HashMap::new();
    if !people_ids_vec.is_empty() {
        let ph = std::iter::repeat_n("?", people_ids_vec.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT parent_id, id FROM item_image \
             WHERE parent_type = 'people' AND parent_id IN ({ph}) \
               AND image_type = 'primary' AND path_url IS NOT NULL \
             ORDER BY id ASC"
        );
        let mut q = sqlx::query_as::<_, (i64, i64)>(&sql);
        for pid in &people_ids_vec {
            q = q.bind(pid);
        }
        let rows = q
            .fetch_all(db.pool())
            .await
            .context("query people image flags")?;
        for (pid, img_id) in rows {
            people_image_id.entry(pid).or_insert(img_id);
        }
    }

    for (item_id, pid, name, role, character_name) in people_rows {
        out.entry(item_id).or_default().people.push(PersonBrief {
            id: pid,
            name,
            role,
            character_name,
            primary_image_id: people_image_id.get(&pid).copied(),
        });
    }

    // studios：item_studio → studio
    let studio_sql = format!(
        "SELECT ist.item_id, s.id, s.name FROM item_studio ist \
         JOIN studio s ON s.id = ist.studio_id \
         WHERE ist.item_id IN ({placeholders}) ORDER BY ist.sort_order"
    );
    let mut sq = sqlx::query_as::<_, (i64, i64, String)>(&studio_sql);
    for id in item_ids {
        sq = sq.bind(id);
    }
    for (item_id, studio_id, name) in sq
        .fetch_all(db.pool())
        .await
        .context("query item studios batch")?
    {
        out.entry(item_id)
            .or_default()
            .studios
            .push((studio_id, name));
    }

    // tags：item_tag → tag（仅名字，Emby Tags 是 string 数组）
    let tag_sql = format!(
        "SELECT itt.item_id, t.name FROM item_tag itt \
         JOIN tag t ON t.id = itt.tag_id \
         WHERE itt.item_id IN ({placeholders}) ORDER BY itt.sort_order"
    );
    let mut tq = sqlx::query_as::<_, (i64, String)>(&tag_sql);
    for id in item_ids {
        tq = tq.bind(id);
    }
    for (item_id, name) in tq
        .fetch_all(db.pool())
        .await
        .context("query item tags batch")?
    {
        out.entry(item_id).or_default().tags.push(name);
    }

    Ok(out)
}

/// 查询所有 Genres（`/Genres` 端点）。
/// `library_id` 非空时只返回该库有 item 的 genre（分类页按库过滤）。
pub async fn list_genres(
    db: &Db,
    library_id: Option<i64>,
    limit: i64,
    start: i64,
) -> Result<ItemsResult> {
    // EXISTS 而非 JOIN：避免同库多 item 把同一 genre 复制成多行
    let (lib_cond, lib_bind): (String, Vec<i64>) = if let Some(lib) = library_id {
        (
            " WHERE EXISTS (SELECT 1 FROM item_genre ig \
             JOIN item i ON i.id = ig.item_id \
             WHERE ig.genre_id = g.id AND i.library_id = ?)"
                .to_string(),
            vec![lib],
        )
    } else {
        (String::new(), vec![])
    };
    let total_sql = format!("SELECT COUNT(*) FROM genre g{lib_cond}");
    let mut total_q = sqlx::query_scalar::<_, i64>(&total_sql);
    for b in &lib_bind {
        total_q = total_q.bind(b);
    }
    let total: i64 = total_q
        .fetch_one(db.pool())
        .await
        .context("count list_genres")?;

    let item_sql = format!(
        "SELECT g.id, NULL AS library_id, 'Genre' AS item_type, g.name AS title, \
                NULL AS description, NULL AS date_air, g.created_at, g.updated_at, \
                NULL AS container, NULL AS file_second, \
                NULL AS uuid, NULL AS name, NULL AS path_type, NULL AS path_url, \
                0 AS play_ms, 0 AS is_complete, 0 AS play_count, 0 AS is_favorite, \
                NULL AS season_number, NULL AS episode_number, \
                0 AS is_virtual, \
                NULL AS series_id, NULL AS series_name, NULL AS season_id, NULL AS season_name, \
                g.id AS parent_id, \
                NULL AS tmdb_id, NULL AS imdb_id, NULL AS tvdb_id, \
                NULL AS community_rating, NULL AS official_rating, NULL AS tagline, \
                NULL AS sort_title, \
                NULL AS end_date, NULL AS status, NULL AS production_year \
         FROM genre g{lib_cond} \
         ORDER BY g.name LIMIT ? OFFSET ?"
    );
    let mut q = sqlx::query_as::<_, ItemRow>(&item_sql);
    for b in &lib_bind {
        q = q.bind(b);
    }
    q = q.bind(limit).bind(start);
    let items = q.fetch_all(db.pool()).await.context("query list_genres")?;

    Ok(ItemsResult { items, total })
}

/// 查询单个 People（`/Users/{uid}/Items/p-{id}` 详情端点数据源）。
pub async fn get_person(db: &Db, id: i64) -> Result<Option<PersonRow>> {
    let row = sqlx::query_as::<_, PersonRow>(
        "SELECT id, tmdb_id, name, original_name, gender, description, birthday, deathday, \
                created_at, updated_at \
         FROM people p \
         WHERE p.id = ? LIMIT 1",
    )
    .bind(id)
    .fetch_optional(db.pool())
    .await
    .context("query get_person")?;
    Ok(row)
}

/// 查询所有 People（`/Persons` 端点）。
/// `library_id` 非空时只返回该库有 item 的 person（分类页按库过滤）。
pub async fn list_persons(
    db: &Db,
    library_id: Option<i64>,
    limit: i64,
    start: i64,
) -> Result<ItemsResult> {
    // EXISTS 而非 JOIN：避免同库多 item 把同一 person 复制成多行
    let (lib_cond, lib_bind): (String, Vec<i64>) = if let Some(lib) = library_id {
        (
            " WHERE EXISTS (SELECT 1 FROM item_people ip \
             JOIN item i ON i.id = ip.item_id \
             WHERE ip.people_id = p.id AND i.library_id = ?)"
                .to_string(),
            vec![lib],
        )
    } else {
        (String::new(), vec![])
    };
    let total_sql = format!("SELECT COUNT(*) FROM people p{lib_cond}");
    let mut total_q = sqlx::query_scalar::<_, i64>(&total_sql);
    for b in &lib_bind {
        total_q = total_q.bind(b);
    }
    let total: i64 = total_q
        .fetch_one(db.pool())
        .await
        .context("count list_persons")?;

    let item_sql = format!(
        "SELECT p.id, NULL AS library_id, 'Person' AS item_type, p.name AS title, \
                p.description, NULL AS date_air, p.created_at, p.updated_at, \
                NULL AS container, NULL AS file_second, \
                NULL AS uuid, NULL AS name, NULL AS path_type, NULL AS path_url, \
                0 AS play_ms, 0 AS is_complete, 0 AS play_count, 0 AS is_favorite, \
                NULL AS season_number, NULL AS episode_number, \
                0 AS is_virtual, \
                NULL AS series_id, NULL AS series_name, NULL AS season_id, NULL AS season_name, \
                p.id AS parent_id, \
                NULL AS tmdb_id, NULL AS imdb_id, NULL AS tvdb_id, \
                NULL AS community_rating, NULL AS official_rating, NULL AS tagline, \
                NULL AS sort_title, \
                NULL AS end_date, NULL AS status, NULL AS production_year \
         FROM people p{lib_cond} \
         ORDER BY p.name LIMIT ? OFFSET ?"
    );
    let mut q = sqlx::query_as::<_, ItemRow>(&item_sql);
    for b in &lib_bind {
        q = q.bind(b);
    }
    q = q.bind(limit).bind(start);
    let items = q.fetch_all(db.pool()).await.context("query list_persons")?;

    Ok(ItemsResult { items, total })
}

/// 查询所有 Tags（`/Tags` 端点）。
/// 从 `tag` 规范表读取（刮削时 TMDB keywords 写入）。
pub async fn list_tags(db: &Db) -> Result<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as("SELECT name FROM tag ORDER BY name")
        .fetch_all(db.pool())
        .await
        .context("query list_tags")?;
    Ok(rows.into_iter().map(|(n,)| n).collect())
}

/// 查询所有 Studios（`/Studios` 端点数据源）。
/// 从 `studio` 规范表读取（刮削时 TMDB production_companies 写入）。
pub async fn list_studios(
    db: &Db,
    library_id: Option<i64>,
    limit: i64,
    start: i64,
) -> Result<ItemsResult> {
    // EXISTS 而非 JOIN：避免同库多 item 把同一 studio 复制成多行
    let (lib_cond, lib_bind): (String, Vec<i64>) = if let Some(lib) = library_id {
        (
            " WHERE EXISTS (SELECT 1 FROM item_studio ist \
             JOIN item i ON i.id = ist.item_id \
             WHERE ist.studio_id = s.id AND i.library_id = ?)"
                .to_string(),
            vec![lib],
        )
    } else {
        (String::new(), vec![])
    };
    let total_sql = format!("SELECT COUNT(*) FROM studio s{lib_cond}");
    let mut total_q = sqlx::query_scalar::<_, i64>(&total_sql);
    for b in &lib_bind {
        total_q = total_q.bind(b);
    }
    let total: i64 = total_q
        .fetch_one(db.pool())
        .await
        .context("count list_studios")?;

    let item_sql = format!(
        "SELECT s.id, NULL AS library_id, 'Studio' AS item_type, s.name AS title, \
                NULL AS description, NULL AS date_air, s.created_at, s.updated_at, \
                NULL AS container, NULL AS file_second, \
                NULL AS uuid, NULL AS name, NULL AS path_type, NULL AS path_url, \
                0 AS play_ms, 0 AS is_complete, 0 AS play_count, 0 AS is_favorite, \
                NULL AS season_number, NULL AS episode_number, \
                0 AS is_virtual, \
                NULL AS series_id, NULL AS series_name, NULL AS season_id, NULL AS season_name, \
                s.id AS parent_id, \
                NULL AS tmdb_id, NULL AS imdb_id, NULL AS tvdb_id, \
                NULL AS community_rating, NULL AS official_rating, NULL AS tagline, \
                NULL AS sort_title, \
                NULL AS end_date, NULL AS status, NULL AS production_year \
         FROM studio s{lib_cond} \
         ORDER BY s.name LIMIT ? OFFSET ?",
    );
    let mut q = sqlx::query_as::<_, ItemRow>(&item_sql);
    for b in &lib_bind {
        q = q.bind(b);
    }
    q = q.bind(limit).bind(start);
    let items = q.fetch_all(db.pool()).await.context("query list_studios")?;

    Ok(ItemsResult { items, total })
}

/// 从 `item` 某个逗号分隔 TEXT 列去重聚合（official_rating 分级列表用）。
async fn aggregate_csv_column(
    db: &Db,
    column: &str,
    ctx: &str,
    library_id: Option<i64>,
) -> Result<Vec<String>> {
    let (lib_cond, lib_bind): (String, Vec<i64>) = if let Some(lib) = library_id {
        (" AND library_id = ?".to_string(), vec![lib])
    } else {
        (String::new(), vec![])
    };
    let sql = format!(
        "SELECT DISTINCT {column} FROM item \
         WHERE {column} IS NOT NULL AND {column} != ''{lib_cond}"
    );
    let mut q = sqlx::query_as::<_, (Option<String>,)>(&sql);
    for b in &lib_bind {
        q = q.bind(*b);
    }
    let rows = q
        .fetch_all(db.pool())
        .await
        .with_context(|| format!("{ctx}: {column}"))?;

    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for (val,) in rows {
        if let Some(s) = val {
            for item in s.split(',').map(|t| t.trim()).filter(|t| !t.is_empty()) {
                if seen.insert(item.to_string()) {
                    result.push(item.to_string());
                }
            }
        }
    }
    result.sort();
    Ok(result)
}

/// 查询所有年份（`/Years` 端点）：从 `item.date_air` 前 4 位提取、去重排序。
pub async fn list_years(
    db: &Db,
    library_id: Option<i64>,
    limit: i64,
    start: i64,
) -> Result<ItemsResult> {
    let (lib_cond, lib_bind): (String, Vec<i64>) = if let Some(lib) = library_id {
        (" AND library_id = ?".to_string(), vec![lib])
    } else {
        (String::new(), vec![])
    };
    let sql = format!(
        "SELECT date_air FROM item \
         WHERE date_air IS NOT NULL AND date_air != ''{lib_cond}"
    );
    let mut q = sqlx::query_as::<_, (Option<String>,)>(&sql);
    for b in &lib_bind {
        q = q.bind(*b);
    }
    let rows = q.fetch_all(db.pool()).await.context("query years")?;

    let mut years: Vec<i64> = Vec::new();
    for (d,) in rows {
        // 前 4 字符须为数字年份（get(..4) 越界安全，非数字 parse 失败自然过滤，跨方言可移植）
        if let Some(s) = d
            && let Some(head) = s.get(..4)
            && let Ok(y) = head.parse::<i64>()
        {
            years.push(y);
        }
    }
    years.sort_unstable();
    years.dedup();

    let total = years.len() as i64;
    let items: Vec<ItemRow> = years
        .into_iter()
        .skip(start as usize)
        .take(limit as usize)
        .map(|year| {
            // 复用 ItemRow 承载：Name=年份、Type=Year、Id=年份数字
            let y = year.to_string();
            ItemRow {
                id: year,
                library_id: None,
                item_type: "Year".into(),
                title: y.clone(),
                description: None,
                date_air: Some(y),
                created_at: String::new(),
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
                tmdb_id: None,
                imdb_id: None,
                tvdb_id: None,
                community_rating: None,
                official_rating: None,
                tagline: None,
                sort_title: None,
                end_date: None,
                status: None,
                production_year: Some(year),
            }
        })
        .collect();

    Ok(ItemsResult { items, total })
}

/// 查询所有分级（`/OfficialRatings` 端点）：`item.official_rating` 去重排序。
pub async fn list_official_ratings(
    db: &Db,
    library_id: Option<i64>,
    limit: i64,
    start: i64,
) -> Result<ItemsResult> {
    let mut ratings =
        aggregate_csv_column(db, "official_rating", "query official ratings", library_id).await?;
    ratings.sort();
    let total = ratings.len() as i64;
    let items: Vec<ItemRow> = ratings
        .into_iter()
        .skip(start as usize)
        .take(limit as usize)
        .map(|r| {
            let rid = emrs_core_rating_id(&r);
            ItemRow {
                id: rid,
                library_id: None,
                item_type: "Rating".into(),
                title: r.clone(),
                description: None,
                date_air: None,
                created_at: String::new(),
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
                tmdb_id: None,
                imdb_id: None,
                tvdb_id: None,
                community_rating: None,
                official_rating: Some(r),
                tagline: None,
                sort_title: None,
                end_date: None,
                status: None,
                production_year: None,
            }
        })
        .collect();

    Ok(ItemsResult { items, total })
}

/// `/Items/Counts` 数据源：各类型计数。
pub async fn item_counts(db: &Db) -> Result<(i64, i64, i64)> {
    let row = sqlx::query_as::<_, (i64, i64, i64)>(
        "SELECT \
            (SELECT COUNT(*) FROM item WHERE type = 'movie'), \
            (SELECT COUNT(*) FROM item WHERE type = 'series'), \
            (SELECT COUNT(*) FROM item WHERE type = 'episode')",
    )
    .fetch_one(db.pool())
    .await
    .context("query item counts")?;
    Ok(row)
}

/// 稳定 id：rating 字符串的确定性散列映射为 i64（无外键关联，仅作 DTO Id）。
fn emrs_core_rating_id(rating: &str) -> i64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    rating.hash(&mut h);
    (h.finish() as i64).max(1)
}
