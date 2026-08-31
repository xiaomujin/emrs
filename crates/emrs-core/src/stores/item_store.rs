//! item 多态查询：Items / Resume / Latest / NextUp / Seasons / Episodes / Favorites。
//!
//! 新表 `item` 以 `type`（movie/series/season/episode）+ `parent_id` 自引用统一
//! 旧 `video_list` / `video_season` / `video_episode` 三表；用户数据统一到
//! `user_item_data`（`played` / `play_count` / `playback_position_ticks` /
//! `is_favorite`）。所有 SQL 输出 [`super::ItemRow`]，路由层 `item_to_json` 不变。

use anyhow::{Context, Result};

use super::{ItemRow, ItemsResult, ResumeEntry};
use crate::db::Db;

use std::collections::HashMap;

/// Movie/Series 列（无 media_source JOIN，folder 项不带媒体源）。
const FOLDER_COLS: &str = "i.id, i.library_id AS library_id, \
    CASE i.type WHEN 'movie' THEN 'Movie' WHEN 'series' THEN 'Series' \
                WHEN 'season' THEN 'Season' WHEN 'episode' THEN 'Episode' \
                ELSE i.type END AS item_type, \
    i.title, i.description, i.date_air, i.created_at, i.updated_at, \
    NULL AS container, NULL AS file_second, \
    NULL AS uuid, NULL AS name, NULL AS path_type, NULL AS path_url, \
    COALESCE(uid.playback_position_ticks, 0) / 10000 AS play_ms, \
    COALESCE(uid.played, 0) AS is_complete, \
    COALESCE(uid.play_count, 0) AS play_count, \
    COALESCE(uid.is_favorite, 0) AS is_favorite, \
    i.season_number, i.episode_number, \
    i.is_virtual AS is_virtual, \
    NULL AS series_id, NULL AS series_name, NULL AS season_id, NULL AS season_name, \
    i.tmdb_id, i.imdb_id, i.tvdb_id, \
    i.community_rating, i.official_rating, i.tagline, i.sort_title, \
    i.end_date, i.status, i.production_year";

/// Season 列（`list_seasons` 专用）：folder 形状 + series JOIN 回溯，无 media_source。
/// `title` / `season_name` 用 `COALESCE(i.title, 'Season N')` 兜底；UserData 字段恒 0
/// （季文件夹无自身播放数据，未播数由 `child_counts_batch` 单独算）。
const SEASON_COLS: &str = "i.id, i.library_id AS library_id, 'Season' AS item_type, \
    COALESCE(i.title, 'Season ' || CAST(i.season_number AS TEXT)) AS title, \
    i.description, i.date_air, i.created_at, i.updated_at, \
    NULL AS container, NULL AS file_second, \
    NULL AS uuid, NULL AS name, NULL AS path_type, NULL AS path_url, \
    0 AS play_ms, 0 AS is_complete, 0 AS play_count, 0 AS is_favorite, \
    i.season_number, NULL AS episode_number, \
    i.is_virtual AS is_virtual, \
    series_item.id AS series_id, series_item.title AS series_name, \
    NULL AS season_id, COALESCE(i.title, 'Season ' || CAST(i.season_number AS TEXT)) AS season_name, \
    i.tmdb_id, i.imdb_id, i.tvdb_id, \
    i.community_rating, i.official_rating, i.tagline, i.sort_title, \
    i.end_date, i.status, i.production_year";

/// 查询所有 Movies。
pub async fn list_movies(db: &Db, user_id: i64, limit: i64, start: i64) -> Result<ItemsResult> {
    list_movies_by_library(db, user_id, None, limit, start).await
}

/// 按库 ID 查询 Movies。
pub async fn list_movies_by_library(
    db: &Db,
    user_id: i64,
    library_id: Option<i64>,
    limit: i64,
    start: i64,
) -> Result<ItemsResult> {
    let (total_sql, item_sql, tail): (&str, String, Vec<i64>) = if let Some(lib_id) = library_id {
        (
            "SELECT COUNT(*) FROM item WHERE type = 'movie' AND library_id = ?",
            format!(
                "SELECT {FOLDER_COLS} FROM item i \
                 LEFT JOIN user_item_data uid ON uid.item_id = i.id AND uid.user_id = ? \
                 WHERE i.type = 'movie' AND i.library_id = ? \
                 ORDER BY i.title LIMIT ? OFFSET ?"
            ),
            vec![lib_id],
        )
    } else {
        (
            "SELECT COUNT(*) FROM item WHERE type = 'movie'",
            format!(
                "SELECT {FOLDER_COLS} FROM item i \
                 LEFT JOIN user_item_data uid ON uid.item_id = i.id AND uid.user_id = ? \
                 WHERE i.type = 'movie' \
                 ORDER BY i.title LIMIT ? OFFSET ?"
            ),
            vec![],
        )
    };
    let total: i64 = sqlx::query_scalar(total_sql)
        .fetch_one(db.pool())
        .await
        .context("count movies")?;
    let mut q = sqlx::query_as::<_, ItemRow>(&item_sql).bind(user_id);
    for v in &tail {
        q = q.bind(v);
    }
    q = q.bind(limit).bind(start);
    let items = q.fetch_all(db.pool()).await.context("query movies")?;
    Ok(ItemsResult { items, total })
}

/// 查询所有 Series。
pub async fn list_series(db: &Db, user_id: i64, limit: i64, start: i64) -> Result<ItemsResult> {
    list_series_by_library(db, user_id, None, limit, start).await
}

/// 按库 ID 查询 Series。
pub async fn list_series_by_library(
    db: &Db,
    user_id: i64,
    library_id: Option<i64>,
    limit: i64,
    start: i64,
) -> Result<ItemsResult> {
    let (total_sql, item_sql, tail): (&str, String, Vec<i64>) = if let Some(lib_id) = library_id {
        (
            "SELECT COUNT(*) FROM item WHERE type = 'series' AND library_id = ?",
            format!(
                "SELECT {FOLDER_COLS} FROM item i \
                 LEFT JOIN user_item_data uid ON uid.item_id = i.id AND uid.user_id = ? \
                 WHERE i.type = 'series' AND i.library_id = ? \
                 ORDER BY i.title LIMIT ? OFFSET ?"
            ),
            vec![lib_id],
        )
    } else {
        (
            "SELECT COUNT(*) FROM item WHERE type = 'series'",
            format!(
                "SELECT {FOLDER_COLS} FROM item i \
                 LEFT JOIN user_item_data uid ON uid.item_id = i.id AND uid.user_id = ? \
                 WHERE i.type = 'series' \
                 ORDER BY i.title LIMIT ? OFFSET ?"
            ),
            vec![],
        )
    };
    let total: i64 = sqlx::query_scalar(total_sql)
        .fetch_one(db.pool())
        .await
        .context("count series")?;
    let mut q = sqlx::query_as::<_, ItemRow>(&item_sql).bind(user_id);
    for v in &tail {
        q = q.bind(v);
    }
    q = q.bind(limit).bind(start);
    let items = q.fetch_all(db.pool()).await.context("query series")?;
    Ok(ItemsResult { items, total })
}

/// Movie/Series 统一查询（搜索 + 排序 + 库过滤）。
///
/// `search_term` 非空时按 title LIKE 过滤（大小写不敏感，用 `LOWER` 兼容三方言）；
/// `sort_by` 支持 `SortName`/`Name`/`DateCreated`/`PremiereDate`/`CommunityRating`
/// （其他值回退 `title`），`sort_order` 支持 `Ascending`/`Descending`（其他回退 ASC）。
/// `item_types` 为 DB 小写白名单（movie/series）；空切片表示两者都要。
#[allow(clippy::too_many_arguments)]
pub async fn list_movies_series(
    db: &Db,
    user_id: i64,
    library_id: Option<i64>,
    search_term: Option<&str>,
    item_types: &[&str],
    is_played: Option<bool>,
    tags: Option<&str>,
    sort_by: Option<&str>,
    sort_order: Option<&str>,
    limit: i64,
    start: i64,
) -> Result<ItemsResult> {
    // 类型白名单：空切片默认 movie+series
    let type_clause = if item_types.is_empty() {
        " AND i.type IN ('movie','series')".to_string()
    } else {
        let ph = std::iter::repeat_n("?", item_types.len())
            .collect::<Vec<_>>()
            .join(", ");
        format!(" AND i.type IN ({ph})")
    };
    let lib_clause = if library_id.is_some() {
        " AND i.library_id = ?"
    } else {
        ""
    };
    // Tags 过滤：item ↔ tag 多对多（item_tag / tag 规范表）；多个 tag 须全部命中
    // （AND 语义，每个 tag 一个 EXISTS，与旧 CSV 实现一致）
    let tags_vec: Vec<&str> = tags
        .map(|t| {
            t.split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let tags_clause = if tags_vec.is_empty() {
        String::new()
    } else {
        let mut clause = String::new();
        for _ in &tags_vec {
            clause.push_str(
                " AND EXISTS (SELECT 1 FROM item_tag itt \
                 JOIN tag t ON t.id = itt.tag_id \
                 WHERE itt.item_id = i.id AND t.name = ?)",
            );
        }
        clause
    };
    let search_clause = if search_term.is_some_and(|t| !t.trim().is_empty()) {
        " AND LOWER(i.title) LIKE LOWER(?)"
    } else {
        ""
    };
    // 已看/未看过滤（Filters=IsPlayed / IsUnplayed）；字面量无需额外 bind。
    // 未看含"从未看过"（uid 无行）：uid.played IS NULL。
    let played_clause = match is_played {
        Some(true) => " AND uid.played = 1",
        Some(false) => " AND (uid.played IS NULL OR uid.played = 0)",
        None => "",
    };
    let order = sort_order.and_then(|d| match d.to_ascii_lowercase().as_str() {
        "descending" | "desc" => Some("DESC"),
        "ascending" | "asc" => Some("ASC"),
        _ => None,
    });
    let sort_by_lower = sort_by.map(|s| s.to_ascii_lowercase());
    let order_by = match sort_by_lower.as_deref() {
        Some("sortname" | "name") => "COALESCE(i.sort_title, i.title)",
        Some("datecreated") => "i.created_at",
        Some("premieredate" | "productionyear") => "i.date_air",
        Some("communityrating") => "COALESCE(i.community_rating, 0)",
        _ => "i.title",
    };
    let dir = order.unwrap_or("ASC");
    let order_sql = format!("ORDER BY {order_by} {dir}");

    let total_sql = format!(
        "SELECT COUNT(*) FROM item i \
         LEFT JOIN user_item_data uid ON uid.item_id = i.id AND uid.user_id = ? \
         WHERE 1=1{type_clause}{lib_clause}{search_clause}{played_clause}{tags_clause}"
    );
    let item_sql = format!(
        "SELECT {FOLDER_COLS} FROM item i \
         LEFT JOIN user_item_data uid ON uid.item_id = i.id AND uid.user_id = ? \
         WHERE 1=1{type_clause}{lib_clause}{search_clause}{played_clause}{tags_clause} \
         {order_sql} LIMIT ? OFFSET ?"
    );

    let total: i64 = {
        let mut q = sqlx::query_scalar::<_, i64>(&total_sql).bind(user_id);
        for t in item_types {
            q = q.bind(t);
        }
        if let Some(lib) = library_id {
            q = q.bind(lib);
        }
        if let Some(term) = search_term.filter(|t| !t.trim().is_empty()) {
            q = q.bind(format!("%{}%", term.trim()));
        }
        for tag in &tags_vec {
            q = q.bind(*tag);
        }
        q.fetch_one(db.pool())
            .await
            .context("count movies_series")?
    };

    let mut q = sqlx::query_as::<_, ItemRow>(&item_sql).bind(user_id);
    for t in item_types {
        q = q.bind(t);
    }
    if let Some(lib) = library_id {
        q = q.bind(lib);
    }
    if let Some(term) = search_term.filter(|t| !t.trim().is_empty()) {
        q = q.bind(format!("%{}%", term.trim()));
    }
    for tag in &tags_vec {
        q = q.bind(*tag);
    }
    q = q.bind(limit).bind(start);
    let items = q
        .fetch_all(db.pool())
        .await
        .context("query movies_series")?;
    Ok(ItemsResult { items, total })
}

/// 查询单个 Item 详情（Movie/Series，folder 项，含用户数据）。
pub async fn get_item(db: &Db, id: i64, user_id: i64) -> Result<Option<ItemRow>> {
    let item = sqlx::query_as::<_, ItemRow>(&format!(
        "SELECT {FOLDER_COLS} \
         FROM item i \
         LEFT JOIN user_item_data uid ON uid.item_id = i.id AND uid.user_id = ? \
         WHERE i.id = ? LIMIT 1"
    ))
    .bind(user_id)
    .bind(id)
    .fetch_optional(db.pool())
    .await
    .context("get item by id")?;
    Ok(item)
}

/// 查询 item 的 DB `type`（movie/series/season/episode）；不存在或已删返回 None。
/// 路由层拿纯数字 ItemId 后据此分派类型特化查询。
pub async fn get_item_type(db: &Db, id: i64) -> Result<Option<String>> {
    let ty: Option<String> = sqlx::query_scalar("SELECT type FROM item WHERE id = ? LIMIT 1")
        .bind(id)
        .fetch_optional(db.pool())
        .await
        .context("get item type")?;
    Ok(ty)
}

/// item 是否存在且未删除（存在性校验）。
///
/// `user_item_data` 无 item_id 外键，写操作前用它确认 id 有效，避免悬空行。
/// 用 `SELECT 1` 只做存在性探测，不取列、不分配 String（比 `get_item_type` 轻量）。
pub async fn item_exists(db: &Db, id: i64) -> Result<bool> {
    let row: Option<i64> = sqlx::query_scalar("SELECT 1 FROM item WHERE id = ? LIMIT 1")
        .bind(id)
        .fetch_optional(db.pool())
        .await
        .context("item exists")?;
    Ok(row.is_some())
}

/// 查询单个 Season 行（含所属剧集信息与收藏状态）。
pub async fn get_season(db: &Db, season_id: i64, user_id: i64) -> Result<Option<ItemRow>> {
    let item = sqlx::query_as::<_, ItemRow>(
        "SELECT i.id, i.library_id AS library_id, 'Season' AS item_type, \
                COALESCE(i.title, 'Season ' || CAST(i.season_number AS TEXT)) AS title, \
                i.description, i.date_air, i.created_at, i.updated_at, \
                NULL AS container, NULL AS file_second, \
                NULL AS uuid, NULL AS name, NULL AS path_type, NULL AS path_url, \
                0 AS play_ms, \
                COALESCE(uid.played, 0) AS is_complete, \
                COALESCE(uid.play_count, 0) AS play_count, \
                COALESCE(uid.is_favorite, 0) AS is_favorite, \
                i.season_number, NULL AS episode_number, \
                i.is_virtual AS is_virtual, \
                series_item.id AS series_id, series_item.title AS series_name, \
                NULL AS season_id, COALESCE(i.title, 'Season ' || CAST(i.season_number AS TEXT)) AS season_name, i.id AS parent_id, \
                i.tmdb_id, i.imdb_id, i.tvdb_id, \
                i.community_rating, i.official_rating, i.tagline, i.sort_title, \
                i.end_date, i.status, i.production_year \
         FROM item i \
         JOIN item series_item ON series_item.id = i.parent_id \
         LEFT JOIN user_item_data uid ON uid.item_id = i.id AND uid.user_id = ? \
         WHERE i.id = ? AND i.type = 'season' LIMIT 1",
    )
    .bind(user_id)
    .bind(season_id)
    .fetch_optional(db.pool())
    .await
    .context("get season by id")?;
    Ok(item)
}

/// 查询单个 Episode 行（含剧集 / 季 / 媒体 / 播放信息）。
///
/// **拆 4 表 JOIN 为「两表取自身 + 应用层单表批取补全」**（见 [`assemble_item_rows`]）：
/// Q1 `item ⋈ user_item_data`（单集自身列 + 用户数据）→ 批取 media_source / season / series
/// 回填完整 `ItemRow`（供 `item_to_json` 详情序列化）。
pub async fn get_episode(db: &Db, episode_id: i64, user_id: i64) -> Result<Option<ItemRow>> {
    let mut rows = sqlx::query_as::<_, ItemRow>(&format!(
        "SELECT {FOLDER_COLS} \
         FROM item i \
         LEFT JOIN user_item_data uid ON uid.item_id = i.id AND uid.user_id = ? \
         WHERE i.id = ? AND i.type = 'episode' LIMIT 1"
    ))
    .bind(user_id)
    .bind(episode_id)
    .fetch_all(db.pool())
    .await
    .context("get episode by id")?;
    assemble_item_rows(db, &mut rows).await?;
    Ok(rows.into_iter().next())
}

/// 续看 / NextUp「每剧代表集」筛选前的中间种子行：已定出的一集（episode）或一部（movie），
/// 携带各自身进度 + 排序用 `recency`（时间）。季名 / 剧名 / 时长在分页切片后再批取补全，
/// 最终映射为对外精简行 [`ResumeEntry`]（Resume）或经 [`load_episode_rows`] 重组为 [`ItemRow`]（NextUp）。
struct Seed {
    id: i64,
    item_type: String,
    title: String,
    created_at: String,
    season_number: Option<i64>,
    episode_number: Option<i64>,
    production_year: Option<i64>,
    date_air: Option<String>,
    tmdb_id: Option<String>,
    imdb_id: Option<String>,
    tvdb_id: Option<String>,
    play_ms: i64,
    is_complete: i64,
    play_count: i64,
    is_favorite: i64,
    /// 仅 episode：回溯季 / 剧名用；movie 为 None。
    season_id: Option<i64>,
    series_id: Option<i64>,
    /// 排序键（ISO 时间字符串，字典序即时间序）：episode 取 anchor 集的 `updated_at`，
    /// movie 取自身 `updated_at`。
    recency: String,
}

/// `resume_frontier_episodes` Step 1 行：给定剧集合下每部**真实集**（`is_virtual=0`）的自身列 +
/// 层级（`season_id`=ep.parent_id，`series_id`=season.parent_id）。2 表（item ep ⋈ item season）。
#[derive(sqlx::FromRow)]
struct EpCandidateRow {
    id: i64,
    title: String,
    created_at: String,
    season_id: Option<i64>,
    season_number: Option<i64>,
    episode_number: Option<i64>,
    production_year: Option<i64>,
    date_air: Option<String>,
    tmdb_id: Option<String>,
    imdb_id: Option<String>,
    tvdb_id: Option<String>,
    series_id: Option<i64>,
}

/// `resume_frontier_episodes` Step 2 行：某批 item 的用户播放数据（单表 `user_item_data`）。
#[derive(sqlx::FromRow)]
struct EpUidRow {
    item_id: i64,
    played: i64,
    play_count: i64,
    position_ticks: i64,
    is_favorite: i64,
    updated_at: String,
}

/// Resume 电影分支行：`user_item_data ⋈ item`（2 表）看到一半未看完的 movie + `recency`。
#[derive(sqlx::FromRow)]
struct MovieSeedRow {
    id: i64,
    title: String,
    created_at: String,
    production_year: Option<i64>,
    date_air: Option<String>,
    tmdb_id: Option<String>,
    imdb_id: Option<String>,
    tvdb_id: Option<String>,
    play_ms: i64,
    is_complete: i64,
    play_count: i64,
    is_favorite: i64,
    recency: String,
}

/// 查询用户的 Resume（继续观看）列表，返回精简 [`ResumeEntry`]。
///
/// **同剧只显示一集**：该剧「时间最近播放的集（anchor）往后、第一个未看且可播（非虚拟）的集」作代表，
/// 携带其自身续播进度；anchor 未看完则续 anchor 本身，看完则顺延下一集；全看完 / 其后无可播集 → 该剧不出现。
/// 选集逻辑由 [`resume_frontier_episodes`] 统一实现（NextUp 复用同一套）。
///
/// **电影**：仍「开播过（`play_count>0`）且未看完（`played=0`）」各算一条（无「下一集」概念）。
///
/// 合并剧集代表 + 电影后按 `recency`（时间）DESC，**分页在去重之后**（避免每页因去重缩水）。
/// 切片后再对当页 `id` 批取时长 / 季名 / 剧名，组装 `ResumeEntry`。全程单表 / ≤3 表，无 N+1。
///
/// `ParentId` 过滤：`library_id`（`ParentId=l-{n}`）只留该库；`parent_item`（`ParentId=i-{n}`）按类型
/// 下钻——series 只留该剧代表集、season 在该季内独立定 anchor + 选代表（季级精确）。过滤在选集 SQL 内
/// 下推（[`ResumeScope`]），使 top-K 截断 + 分页对**过滤后的**候选集精确，而非事后 retain。
pub async fn list_resume(
    db: &Db,
    user_id: i64,
    library_id: Option<i64>,
    parent_item: Option<i64>,
    limit: i64,
    start: i64,
) -> Result<Vec<ResumeEntry>> {
    let start = start.max(0);
    let limit = limit.max(0);
    if limit == 0 {
        return Ok(Vec::new());
    }

    // ParentId → 过滤作用域。罕见同时给 → 视为无过滤。
    let scope = match (library_id, parent_item) {
        (Some(l), None) => ResumeScope::Library(l),
        (None, Some(p)) => match get_item_type(db, p).await.ok().flatten().as_deref() {
            Some("series") => ResumeScope::Series(p),
            // 季下钻：`ep.parent_id = p` 自然限定季，在该季内独立选代表。
            // 电影误用 ParentId=i-{movieId} 当父级：当季处理 → 无集命中 → 空（电影续看走库 / 全量）。
            Some("season") | Some("movie") => ResumeScope::Season(p),
            _ => return Ok(Vec::new()), // episode / 不存在 → 非有效续看父级
        },
        _ => ResumeScope::All,
    };

    // 各分支至少备够 start+limit 条：合并集 skip(start).take(limit) 的窗口只可能落在两分支前
    // (start+limit) 名的并集内，故 top-K 截断取 want 即精确。
    let want = (start + limit) as usize;

    // top-K 选集器：只解析 recency 最高的若干剧（Step A 排序 + 分块惰性展开），O(limit) 非 O(全部在追)。
    let mut seeds = resume_frontier_episodes(db, user_id, scope, want).await?;
    seeds.extend(resume_movies(db, user_id, scope, want).await?);

    // 时间倒序 + 分页（次级键 id：跨请求确定序；frontier 按剧分块惰性解析，合并后仍需全局排序）。
    seeds.sort_by(|a, b| b.recency.cmp(&a.recency).then_with(|| b.id.cmp(&a.id)));
    let page: Vec<Seed> = seeds
        .into_iter()
        .skip(start as usize)
        .take(limit as usize)
        .collect();
    if page.is_empty() {
        return Ok(Vec::new());
    }

    // 切片后再批取：时长一条；季名 + 剧名同源于 `item` 表（id → title），合并成一次单表 IN。
    let item_ids: Vec<i64> = page.iter().map(|s| s.id).collect();
    let durations = fetch_resume_durations(db, &item_ids).await?;

    let parent_ids = uniq_ids(
        page.iter()
            .filter_map(|s| s.season_id)
            .chain(page.iter().filter_map(|s| s.series_id)),
    );
    let parents = fetch_parents_by_ids(db, &parent_ids).await?;

    let rows = page
        .into_iter()
        .map(|s| {
            let season_name = s
                .season_id
                .and_then(|id| parents.get(&id))
                .map(|(t, _)| t.clone());
            let series_name = s
                .series_id
                .and_then(|id| parents.get(&id))
                .map(|(t, _)| t.clone());
            ResumeEntry {
                id: s.id,
                item_type: s.item_type,
                title: s.title,
                created_at: s.created_at,
                play_ms: s.play_ms,
                is_complete: s.is_complete,
                play_count: s.play_count,
                is_favorite: s.is_favorite,
                file_second: durations.get(&s.id).copied().flatten(),
                production_year: s.production_year,
                date_air: s.date_air,
                season_number: s.season_number,
                episode_number: s.episode_number,
                series_id: s.series_id,
                series_name,
                season_id: s.season_id,
                season_name,
                tmdb_id: s.tmdb_id,
                imdb_id: s.imdb_id,
                tvdb_id: s.tvdb_id,
            }
        })
        .collect();
    Ok(rows)
}

/// Resume 列表的 `ParentId` 过滤作用域。在选集 SQL 内下推（而非事后 retain），使 top-K 截断 +
/// 分页对**过滤后的**候选集精确。
#[derive(Clone, Copy)]
enum ResumeScope {
    All,
    /// `ParentId=l-{n}`：只留该库的条目。
    Library(i64),
    /// `ParentId=i-{seriesId}`：只留该剧的代表集。
    Series(i64),
    /// `ParentId=i-{seasonId}`：`ep.parent_id = season` 自然限定该季，在季内独立定 anchor + 选代表。
    Season(i64),
}

impl ResumeScope {
    /// 下推过滤要绑定的值（`All` 无值）。
    fn value(self) -> Option<i64> {
        match self {
            ResumeScope::All => None,
            ResumeScope::Library(v) | ResumeScope::Series(v) | ResumeScope::Season(v) => Some(v),
        }
    }
    /// 电影无 series / season → series / season 作用域下直接空（跳过查询）。
    fn excludes_movies(self) -> bool {
        matches!(self, ResumeScope::Series(_) | ResumeScope::Season(_))
    }
}

/// 「用户开播追过的剧」→ 每剧代表集的共享选集器（Resume / NextUp 复用）。
///
/// 规则（见 [`list_resume`] 文档）：anchor = 该剧 `play_count>0` 的集中 `updated_at` 最大者；代表集 =
/// 集序 `>=` anchor 且 `is_virtual=0` 且未看完的第一集；找不到 → 该剧不出。
///
/// **top-K：扫描量 O(`want`) 而非 O(全部在追)**。代表集的 `recency` 恰等于其所属剧的 anchor
/// `updated_at` = 该剧 `play_count>0` 集的 `MAX(updated_at)`，故最终排序键由剧决定。分两步：
/// - Step A（[`frontier_series_order`]）：一条普通聚合 SQL 取「在追剧 → recency」按 recency DESC 排序
///   （行数 = 在追剧数，小）；作用域过滤在此下推。
/// - 按该序分块（每块 ≤900 剧）交给 [`frontier_reps_for_series`] 选代表，累计够 `want` 条即停——
///   剧按 recency 降序处理，累计的前 `want` 条代表集即全局 top-`want`（无代表的剧自动跳过、不占位）。
///
/// 全程单条 SQL ≤3 表、无窗口函数 / 无相关子查询 / 无 N+1。返回至多 `want` 条、已按 recency 降序的
/// [`Seed`]（`item_type='Episode'`；调用方仍会做最终稳定排序）。`want=0` → 空。
async fn resume_frontier_episodes(
    db: &Db,
    user_id: i64,
    scope: ResumeScope,
    want: usize,
) -> Result<Vec<Seed>> {
    if want == 0 {
        return Ok(Vec::new());
    }
    let series_ids = frontier_series_order(db, user_id, scope).await?;
    let step = want.clamp(1, 900);
    let mut seeds = Vec::new();
    for chunk in series_ids.chunks(step) {
        seeds.append(&mut frontier_reps_for_series(db, user_id, chunk, scope).await?);
        if seeds.len() >= want {
            break;
        }
    }
    Ok(seeds)
}

/// Step A：作用域内「追过的剧 → anchor recency」，按 recency DESC、`series_id` DESC 排序（确定序）。
/// 单条普通聚合（`GROUP BY` + `MAX`，无窗口函数），行数 = 在追剧数。`idx_uid_user_played_recent`
/// （`WHERE play_count > 0`）加速 `play_count>0` 筛选。
async fn frontier_series_order(db: &Db, user_id: i64, scope: ResumeScope) -> Result<Vec<i64>> {
    let scope_sql = match scope {
        ResumeScope::All => "",
        ResumeScope::Library(_) => " AND ep.library_id = ?",
        ResumeScope::Series(_) => " AND season_item.parent_id = ?",
        ResumeScope::Season(_) => " AND ep.parent_id = ?",
    };
    let sql = format!(
        "SELECT season_item.parent_id AS series_id, MAX(uid.updated_at) AS recency \
         FROM item ep \
         JOIN item season_item ON season_item.id = ep.parent_id \
         JOIN user_item_data uid ON uid.item_id = ep.id AND uid.user_id = ? AND uid.play_count > 0 \
         WHERE ep.type = 'episode'{scope_sql} \
         GROUP BY season_item.parent_id \
         ORDER BY recency DESC, series_id DESC"
    );
    let mut q = sqlx::query_as::<_, (Option<i64>, String)>(&sql).bind(user_id);
    if let Some(v) = scope.value() {
        q = q.bind(v);
    }
    let rows = q
        .fetch_all(db.pool())
        .await
        .context("resume frontier order")?;
    Ok(rows.into_iter().filter_map(|(s, _)| s).collect())
}

/// 给定一批剧（`series_ids`，调用方保证 ≤900）→ 每剧代表集 Seed。
///
/// 两条 SQL：Step 1 `ep ⋈ season WHERE season.parent_id IN(剧)`→ 真实集（自身列 + series_id）；
/// Step 2 `user_item_data WHERE item_id IN(集)`→ 播放数据。应用层分组、定 anchor、往后取代表集。
/// Step A 已保证每剧有 `play_count>0` 集（anchor 必存在），无代表的剧（全看完）自动跳过。
///
/// `Season` 作用域下 Step 1 追加 `ep.parent_id = season`：候选集与 anchor 都限定在该季内（季内独立
/// 续看），而非跨整剧取代表。`All` / `Library` / `Series` 无此约束（Step A 已把剧集收窄到库 / 单剧）。
async fn frontier_reps_for_series(
    db: &Db,
    user_id: i64,
    series_ids: &[i64],
    scope: ResumeScope,
) -> Result<Vec<Seed>> {
    let season_only = match scope {
        ResumeScope::Season(s) => Some(s),
        _ => None,
    };
    // Step 1：这些剧的全部真实集（分剧块，规避占位符上限）。
    let mut eps: Vec<EpCandidateRow> = Vec::new();
    for chunk in series_ids.chunks(900) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let season_sql = if season_only.is_some() {
            " AND ep.parent_id = ?"
        } else {
            ""
        };
        let sql = format!(
            "SELECT ep.id, ep.title, ep.created_at, ep.parent_id AS season_id, \
                    ep.season_number, ep.episode_number, ep.production_year, ep.date_air, \
                    ep.tmdb_id, ep.imdb_id, ep.tvdb_id, \
                    season_item.parent_id AS series_id \
             FROM item ep \
             JOIN item season_item ON season_item.id = ep.parent_id \
             WHERE ep.type = 'episode' AND ep.is_virtual = 0 \
               AND season_item.parent_id IN ({placeholders}){season_sql}"
        );
        let mut q = sqlx::query_as::<_, EpCandidateRow>(&sql);
        for id in chunk {
            q = q.bind(id);
        }
        if let Some(s) = season_only {
            q = q.bind(s);
        }
        eps.extend(
            q.fetch_all(db.pool())
                .await
                .context("resume frontier episodes")?,
        );
    }
    if eps.is_empty() {
        return Ok(Vec::new());
    }

    // Step 2：全部候选集的播放数据（单表 IN，分块）。
    let ep_ids: Vec<i64> = eps.iter().map(|e| e.id).collect();
    let mut uidmap: HashMap<i64, EpUidRow> = HashMap::new();
    for chunk in ep_ids.chunks(900) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT item_id, COALESCE(played, 0) AS played, COALESCE(play_count, 0) AS play_count, \
                    COALESCE(playback_position_ticks, 0) AS position_ticks, \
                    COALESCE(is_favorite, 0) AS is_favorite, updated_at \
             FROM user_item_data WHERE user_id = ? AND item_id IN ({placeholders})"
        );
        let mut q = sqlx::query_as::<_, EpUidRow>(&sql).bind(user_id);
        for id in chunk {
            q = q.bind(id);
        }
        for row in q
            .fetch_all(db.pool())
            .await
            .context("resume frontier uid")?
        {
            uidmap.insert(row.item_id, row);
        }
    }

    // 按剧分组，集序排序。
    let mut by_series: HashMap<i64, Vec<EpCandidateRow>> = HashMap::new();
    for e in eps {
        if let Some(sid) = e.series_id {
            by_series.entry(sid).or_default().push(e);
        }
    }

    let mut seeds = Vec::new();
    for (series_id, mut group) in by_series {
        group.sort_by_key(|e| {
            (
                e.season_number.unwrap_or(i64::MAX),
                e.episode_number.unwrap_or(i64::MAX),
            )
        });
        // anchor：play_count>0 的集中 updated_at 最大者（并列取集序较大者）。
        let mut anchor: Option<&EpCandidateRow> = None;
        for e in &group {
            let Some(u) = uidmap.get(&e.id) else { continue };
            if u.play_count <= 0 {
                continue;
            }
            let better = match anchor {
                None => true,
                Some(a) => {
                    let au = uidmap
                        .get(&a.id)
                        .map(|x| x.updated_at.as_str())
                        .unwrap_or("");
                    (u.updated_at.as_str(), e.season_number, e.episode_number)
                        > (au, a.season_number, a.episode_number)
                }
            };
            if better {
                anchor = Some(e);
            }
        }
        // Step A 已保证该剧有 play_count>0 → anchor 必存在；防御性跳过。
        let Some(a) = anchor else { continue };
        let a_key = (
            a.season_number.unwrap_or(i64::MIN),
            a.episode_number.unwrap_or(i64::MIN),
        );
        let recency = uidmap
            .get(&a.id)
            .map(|x| x.updated_at.clone())
            .unwrap_or_default();
        // 从 anchor 往后（集序 >= a_key）取第一个未看完的可播集。
        let rep = group.iter().find(|e| {
            let key = (
                e.season_number.unwrap_or(i64::MAX),
                e.episode_number.unwrap_or(i64::MAX),
            );
            key >= a_key && uidmap.get(&e.id).map(|u| u.played == 0).unwrap_or(true)
        });
        let Some(rep) = rep else { continue };
        let u = uidmap.get(&rep.id);
        seeds.push(Seed {
            id: rep.id,
            item_type: "Episode".to_string(),
            title: rep.title.clone(),
            created_at: rep.created_at.clone(),
            season_number: rep.season_number,
            episode_number: rep.episode_number,
            production_year: rep.production_year,
            date_air: rep.date_air.clone(),
            tmdb_id: rep.tmdb_id.clone(),
            imdb_id: rep.imdb_id.clone(),
            tvdb_id: rep.tvdb_id.clone(),
            play_ms: u.map(|x| x.position_ticks / 10_000).unwrap_or(0),
            is_complete: u.map(|x| x.played).unwrap_or(0),
            play_count: u.map(|x| x.play_count).unwrap_or(0),
            is_favorite: u.map(|x| x.is_favorite).unwrap_or(0),
            season_id: rep.season_id,
            series_id: Some(series_id),
            recency,
        });
    }
    Ok(seeds)
}

/// Resume 电影分支：`user_item_data ⋈ item`（2 表），开播过且未看完的 movie，各带 `recency`。
/// 作用域：库过滤下推 `i.library_id`；series / season 作用域无电影 → 直接空。按 recency DESC
/// 取前 `want` 条（`LIMIT`），与剧集分支各备 `want` 条再合并排序即保证分页精确。
async fn resume_movies(
    db: &Db,
    user_id: i64,
    scope: ResumeScope,
    want: usize,
) -> Result<Vec<Seed>> {
    if want == 0 || scope.excludes_movies() {
        return Ok(Vec::new());
    }
    let scope_sql = match scope {
        ResumeScope::Library(_) => " AND i.library_id = ?",
        _ => "",
    };
    let sql = format!(
        "SELECT i.id, i.title, i.created_at, i.production_year, i.date_air, \
                i.tmdb_id, i.imdb_id, i.tvdb_id, \
                COALESCE(uid.playback_position_ticks, 0) / 10000 AS play_ms, \
                COALESCE(uid.played, 0) AS is_complete, \
                COALESCE(uid.play_count, 0) AS play_count, \
                COALESCE(uid.is_favorite, 0) AS is_favorite, \
                uid.updated_at AS recency \
         FROM user_item_data uid \
         JOIN item i ON i.id = uid.item_id \
         WHERE uid.user_id = ? AND uid.play_count > 0 AND uid.played = 0 AND i.type = 'movie'{scope_sql} \
         ORDER BY uid.updated_at DESC LIMIT ?",
    );
    let mut q = sqlx::query_as::<_, MovieSeedRow>(&sql).bind(user_id);
    if let Some(v) = scope.value() {
        q = q.bind(v);
    }
    let rows = q
        .bind(want as i64)
        .fetch_all(db.pool())
        .await
        .context("resume movies")?;
    Ok(rows
        .into_iter()
        .map(|m| Seed {
            id: m.id,
            item_type: "Movie".to_string(),
            title: m.title,
            created_at: m.created_at,
            season_number: None,
            episode_number: None,
            production_year: m.production_year,
            date_air: m.date_air,
            tmdb_id: m.tmdb_id,
            imdb_id: m.imdb_id,
            tvdb_id: m.tvdb_id,
            play_ms: m.play_ms,
            is_complete: m.is_complete,
            play_count: m.play_count,
            is_favorite: m.is_favorite,
            season_id: None,
            series_id: None,
            recency: m.recency,
        })
        .collect())
}

/// 去重 + 排序（供 `IN` 批取的 id 列表，保持确定性）。
fn uniq_ids(ids: impl Iterator<Item = i64>) -> Vec<i64> {
    let mut v: Vec<i64> = ids.collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// Resume Q2：批量取 media_source 时长（单表 `item_id IN (...)`），按 `item_id, id` 升序，
/// 一集多源时取 id 最小首源的 `file_duration`。返回 `item_id → Option<file_duration>`。
async fn fetch_resume_durations(db: &Db, item_ids: &[i64]) -> Result<HashMap<i64, Option<i64>>> {
    let mut out = HashMap::new();
    if item_ids.is_empty() {
        return Ok(out);
    }
    let placeholders = std::iter::repeat_n("?", item_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT item_id, file_duration FROM media_source \
         WHERE item_id IN ({placeholders}) ORDER BY item_id, id"
    );
    let mut q = sqlx::query_as::<_, (i64, Option<i64>)>(&sql);
    for id in item_ids {
        q = q.bind(id);
    }
    let rows = q.fetch_all(db.pool()).await.context("resume durations")?;
    for (item_id, file_duration) in rows {
        // 只保留首源（id 最小，已按 id 升序）。
        out.entry(item_id).or_insert(file_duration);
    }
    Ok(out)
}

/// 按 id 批量取 item 的 `(title, parent_id)`（单表 `id IN (...)`）。
///
/// 供各端点做「季 → 剧」层级回溯：Q3 用它取季（`parent_id` = `series_id`），
/// Q4 用它取剧（`parent_id` 忽略）。空输入返回空 map。
async fn fetch_parents_by_ids(db: &Db, ids: &[i64]) -> Result<HashMap<i64, (String, Option<i64>)>> {
    let mut out = HashMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let placeholders = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("SELECT id, title, parent_id FROM item WHERE id IN ({placeholders})");
    let mut q = sqlx::query_as::<_, (i64, String, Option<i64>)>(&sql);
    for id in ids {
        q = q.bind(id);
    }
    let rows = q.fetch_all(db.pool()).await.context("parents by id")?;
    for (id, title, parent_id) in rows {
        out.insert(id, (title, parent_id));
    }
    Ok(out)
}

/// `media_source` 单表 `item_id IN (...)` 批取：一集多源取 id 最小首源（与旧 LEFT JOIN
/// 语义一致），回填胖 [`ItemRow`] 的媒体列。供 `list_active_sessions` / `get_episode` /
/// `list_favorites` / `list_episodes` 等重组完整 `ItemRow` 的端点复用。
///
/// `path_type` 复刻 `ITEM_COLS` 的 `CASE ms.protocol`（file→local / strm→strm）；
/// `path_url` = `COALESCE(path, remote_path)`。空输入返回空 map。
async fn fetch_media_batch(db: &Db, item_ids: &[i64]) -> Result<HashMap<i64, MediaCols>> {
    let mut out = HashMap::new();
    if item_ids.is_empty() {
        return Ok(out);
    }
    let placeholders = std::iter::repeat_n("?", item_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT item_id, \
                container, file_duration, uuid, name, \
                CASE protocol WHEN 'file' THEN 'local' WHEN 'strm' THEN 'strm' \
                              ELSE protocol END AS path_type, \
                COALESCE(path, remote_path) AS path_url \
         FROM media_source WHERE item_id IN ({placeholders}) ORDER BY item_id, id"
    );
    let mut q = sqlx::query_as::<
        _,
        (
            i64,
            Option<String>,
            Option<i64>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        ),
    >(&sql);
    for id in item_ids {
        q = q.bind(id);
    }
    let rows = q.fetch_all(db.pool()).await.context("media batch")?;
    for (item_id, container, file_duration, uuid, name, path_type, path_url) in rows {
        // 只保留首源（id 最小，已按 item_id, id 升序）。
        out.entry(item_id).or_insert(MediaCols {
            container,
            file_second: file_duration,
            uuid,
            name,
            path_type,
            path_url,
        });
    }
    Ok(out)
}

/// [`fetch_media_batch`] 单行媒体列（回填 `ItemRow` 的 media_source 部分）。
struct MediaCols {
    container: Option<String>,
    file_second: Option<i64>,
    uuid: Option<String>,
    name: Option<String>,
    path_type: Option<String>,
    path_url: Option<String>,
}

/// 把一个基础 [`ItemRow`]（Q1 已填 item 自身列 + uid 用户数据 + `series_id`/`season_id` 待补，
/// `parent_id` 语义见参数）用媒体 / 季 / 剧三张批取 map 原地补全，复刻旧 `ITEM_COLS` 大 JOIN 的
/// 输出。层级：`ty == "episode"` → 父为季，季父为剧；`ty == "season"` → 父即剧，季自身补
/// `season_id`/`season_name`；其余（movie/series）无层级链。
///
/// `base_parent_id`：episode 的 `parent_id`(=season_id) / season 的 `parent_id`(=series_id)，
/// movie/series 传 `None`。`season_name` 对 season 用自身 `title`（对齐 `SEASON_COLS`）。
fn backfill_item_row(
    row: &mut ItemRow,
    parent_id: Option<i64>,
    media: &HashMap<i64, MediaCols>,
    season: &HashMap<i64, (String, Option<i64>)>,
    series: &HashMap<i64, (String, Option<i64>)>,
) {
    if let Some(m) = media.get(&row.id) {
        row.container = m.container.clone();
        row.file_second = m.file_second;
        row.uuid = m.uuid.clone();
        row.name = m.name.clone();
        row.path_type = m.path_type.clone();
        row.path_url = m.path_url.clone();
    }
    match row.item_type.as_str() {
        "Episode" => {
            let season_id = parent_id;
            let season_name = season_id.and_then(|sid| season.get(&sid));
            row.season_id = season_id;
            row.season_name = season_name.map(|(t, _)| t.clone());
            let series_id = season_name.and_then(|(_, parent)| *parent);
            row.series_id = series_id;
            row.series_name = series_id.and_then(|fid| series.get(&fid).map(|(t, _)| t.clone()));
        }
        "Season" => {
            // season.parent_id = series_id；season 自身即季。
            row.season_id = Some(row.id);
            row.season_name = Some(row.title.clone());
            row.series_id = parent_id;
            row.series_name = parent_id.and_then(|fid| series.get(&fid).map(|(t, _)| t.clone()));
        }
        _ => {}
    }
}

/// 把一批「仅含 item 自身列 + uid 用户数据」（媒体 / 季 / 剧列为 NULL）的胖 [`ItemRow`]
/// 原地补全为与旧 `ITEM_COLS` 大 JOIN 等价的完整行：**全走单表 `IN` 批取 + 应用层组装，
/// 杜绝 N+1，无一条 SQL 连表超过 2 张**。
///
/// 内部四次单表批取：`item`（自身 `parent_id`）→ `media_source` → `item`（季）→ `item`（剧）。
/// 层级：Episode 父=季、季父=剧；Season 父=剧；Movie/Series 无链。
async fn assemble_item_rows(db: &Db, rows: &mut [ItemRow]) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let item_ids: Vec<i64> = rows.iter().map(|r| r.id).collect();

    // 各 item 自身 parent_id（episode→season / season→series / movie→None）。
    let self_parents = fetch_parents_by_ids(db, &item_ids).await?;
    let parent_ids: Vec<Option<i64>> = rows
        .iter()
        .map(|r| self_parents.get(&r.id).and_then(|(_, pid)| *pid))
        .collect();

    let media = fetch_media_batch(db, &item_ids).await?;

    // 季：Episode 的 parent_id。
    let season_ids: Vec<i64> = {
        let mut v: Vec<i64> = rows
            .iter()
            .zip(&parent_ids)
            .filter(|(r, _)| r.item_type == "Episode")
            .filter_map(|(_, pid)| *pid)
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    let season = fetch_parents_by_ids(db, &season_ids).await?;

    // 剧：Episode 经季回溯的 series_id + Season 行的 parent(=series_id)。
    let series_ids: Vec<i64> = {
        let mut v: Vec<i64> = rows
            .iter()
            .zip(&parent_ids)
            .filter_map(|(r, pid)| match r.item_type.as_str() {
                "Episode" => pid
                    .and_then(|sid| season.get(&sid))
                    .and_then(|(_, series_id)| *series_id),
                "Season" => *pid,
                _ => None,
            })
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    let series = fetch_parents_by_ids(db, &series_ids).await?;

    for (row, pid) in rows.iter_mut().zip(&parent_ids) {
        backfill_item_row(row, *pid, &media, &season, &series);
    }
    Ok(())
}

/// `/Sessions` 数据源：进行中播放（play_count>0 且未看完且已有进度）的 item。
/// 返回 [`ItemRow`]（含 play_ms），供会话列表展示 NowPlayingItem + PlaybackPositionTicks。
///
/// **拆 5 表 JOIN 为「两表筛选 + 应用层单表批取组装」**（见 [`assemble_item_rows`]，杜绝 N+1、
/// 无 SQL 连表超 2 张）：Q1 `item ⋈ user_item_data`（分页筛选 + item 自身列 + 用户数据，媒体 /
/// 季 / 剧列先留 NULL），再批取 media_source / season / series 补全完整 `ItemRow`。
pub async fn list_active_sessions(db: &Db, user_id: i64) -> Result<Vec<ItemRow>> {
    let mut rows = sqlx::query_as::<_, ItemRow>(&format!(
        "SELECT {FOLDER_COLS} \
         FROM item i \
         JOIN user_item_data uid ON uid.item_id = i.id AND uid.user_id = ? \
         WHERE uid.play_count > 0 AND uid.played = 0 \
           AND COALESCE(uid.playback_position_ticks, 0) > 0 \
           AND i.type IN ('movie', 'episode') \
         ORDER BY uid.updated_at DESC LIMIT 20"
    ))
    .bind(user_id)
    .fetch_all(db.pool())
    .await
    .context("list_active_sessions")?;
    assemble_item_rows(db, &mut rows).await?;
    Ok(rows)
}

/// 查询 Latest（最新入库）列表。
/// `library_id` 为 Some 时仅返回该库的最新条目（`ParentId=l-{n}` 过滤）。
pub async fn list_latest(
    db: &Db,
    user_id: i64,
    library_id: Option<i64>,
    limit: i64,
) -> Result<Vec<ItemRow>> {
    let lib_clause = if library_id.is_some() {
        " AND i.library_id = ?"
    } else {
        ""
    };
    let sql = format!(
        "SELECT {FOLDER_COLS} \
         FROM item i \
         LEFT JOIN user_item_data uid ON uid.item_id = i.id AND uid.user_id = ? \
         WHERE i.type IN ('movie', 'series'){lib_clause} \
         ORDER BY i.created_at DESC LIMIT ?"
    );
    let mut q = sqlx::query_as::<_, ItemRow>(&sql).bind(user_id);
    if let Some(lib) = library_id {
        q = q.bind(lib);
    }
    let rows = q
        .bind(limit)
        .fetch_all(db.pool())
        .await
        .context("list_latest")?;
    Ok(rows)
}

/// 查询 NextUp（接下来播放）列表。
///
/// **与 Resume 共用同一套「追过的剧 → 每剧代表集」选集逻辑**（[`resume_frontier_episodes`]，规则见
/// [`list_resume`]）：每剧取「时间最近播放集往后、第一个未看且可播（非虚拟）的集」，同剧只一条；
/// 排除从未开播的剧（无任何 `play_count>0`），用户零观看时返回空（真实 Emby 语义，无兜底）。
/// 与 Resume 的差别仅在形态：NextUp 只出剧集、不合并电影，且需完整 [`ItemRow`]（`NextUpJson` 带 People /
/// Overview）→ 代表集 id 经 [`load_episode_rows`]（`item ⋈ user_item_data` + [`assemble_item_rows`]）重组。
/// 排序按 anchor 的 `updated_at` DESC（最近追的剧在前）。
///
/// - 无 `series_id`：全量「追过的剧」每剧**一条**代表集，按 anchor 时间倒序。
/// - 有 `series_id`：客户端点名某剧（`/Shows/NextUp?SeriesId=i-n`），返回**多条**——用户「正在看的那个季」
///   里、集序 `>=` anchor（时间最近播放集）、未看且非虚拟的集。**没看过的季（含整剧未开播）不返回**（空）。
pub async fn list_next_up(
    db: &Db,
    user_id: i64,
    series_id: Option<i64>,
    limit: i64,
    start: i64,
) -> Result<Vec<ItemRow>> {
    let start = start.max(0);
    let limit = limit.max(0);
    if let Some(sid) = series_id {
        return next_up_in_current_season(db, user_id, sid, limit, start).await;
    }
    if limit == 0 {
        return Ok(Vec::new());
    }

    // 全量：每剧一条代表集 → 按 anchor 时间倒序 → 取 [start, start+limit) → 重组 ItemRow（保序）。
    // top-K：want = start+limit，frontier 只解析 recency 最高的若干剧，O(limit) 非 O(全部在追)。
    let want = (start + limit) as usize;
    let mut seeds = resume_frontier_episodes(db, user_id, ResumeScope::All, want).await?;
    if seeds.is_empty() {
        return Ok(Vec::new());
    }
    // 次级键 id：同 `list_resume`，确保跨请求分页确定（recency 相同时不受 HashMap 迭代序影响）。
    seeds.sort_by(|a, b| b.recency.cmp(&a.recency).then_with(|| b.id.cmp(&a.id)));
    let ids: Vec<i64> = seeds
        .into_iter()
        .skip(start as usize)
        .take(limit as usize)
        .map(|s| s.id)
        .collect();
    let rows = load_episode_rows(db, user_id, &ids).await?;
    let mut by_id: HashMap<i64, ItemRow> = rows.into_iter().map(|r| (r.id, r)).collect();
    let ordered = ids.into_iter().filter_map(|id| by_id.remove(&id)).collect();
    Ok(ordered)
}

/// `next_up_in_current_season` Q1 行：anchor 候选（该剧开播过的集的层级 + 时间）。
#[derive(sqlx::FromRow)]
struct AnchorEp {
    season_id: Option<i64>,
    season_number: Option<i64>,
    episode_number: Option<i64>,
    updated_at: String,
}

/// `next_up_in_current_season` Q2 行：该剧未看完的真实集（层级，用于过滤 + 排序）。
#[derive(sqlx::FromRow)]
struct CandEp {
    id: i64,
    season_id: Option<i64>,
    season_number: Option<i64>,
    episode_number: Option<i64>,
}

/// NextUp 下钻「某剧接下来」：只列用户**正在看的那个季**里、anchor（该剧 `play_count>0` 且
/// `updated_at` 最大的集）往后、未看完且非虚拟的集，自 `start` 起按集序升序取至多 `limit` 条。
///
/// 「没看过的季不返回」：靠 anchor 单点定位「正在看的季」，候选 `season_id` 必须等于 anchor 所在季，
/// 且不回退到第 1 季；整剧无开播记录（无 anchor）→ 直接空。
///
/// 2 条查询各 ≤3 表、应用层组装，无 N+1：Q1 `ep ⋈ season ⋈ uid`(play_count>0) 定 anchor；
/// Q2 `ep ⋈ season LEFT JOIN uid` 取该剧未看真实集 → Rust 过滤「同季 + 集序 >= anchor」→ [`load_episode_rows`]。
async fn next_up_in_current_season(
    db: &Db,
    user_id: i64,
    sid: i64,
    limit: i64,
    start: i64,
) -> Result<Vec<ItemRow>> {
    // Q1：该剧用户开播过的集 → anchor（时间最近；并列取集序较大者）。
    let played: Vec<AnchorEp> = sqlx::query_as(
        "SELECT ep.parent_id AS season_id, ep.season_number, ep.episode_number, uid.updated_at \
         FROM item ep \
         JOIN item season ON season.id = ep.parent_id \
         JOIN user_item_data uid ON uid.item_id = ep.id AND uid.user_id = ? AND uid.play_count > 0 \
         WHERE ep.type = 'episode' AND season.parent_id = ?",
    )
    .bind(user_id)
    .bind(sid)
    .fetch_all(db.pool())
    .await
    .context("next_up anchor")?;
    let Some(anchor) = played.iter().max_by(|a, b| {
        (a.updated_at.as_str(), a.season_number, a.episode_number).cmp(&(
            b.updated_at.as_str(),
            b.season_number,
            b.episode_number,
        ))
    }) else {
        return Ok(Vec::new()); // 整剧没开播 → 无「接下来」
    };
    let anchor_season_id = anchor.season_id;
    let anchor_key = (anchor.season_number, anchor.episode_number);

    // Q2：该剧未看完的真实集（按集序）。
    let cand: Vec<CandEp> = sqlx::query_as(
        "SELECT ep.id, ep.parent_id AS season_id, ep.season_number, ep.episode_number \
         FROM item ep \
         JOIN item season ON season.id = ep.parent_id \
         LEFT JOIN user_item_data uid ON uid.item_id = ep.id AND uid.user_id = ? \
         WHERE ep.type = 'episode' AND ep.is_virtual = 0 AND season.parent_id = ? \
           AND (uid.id IS NULL OR uid.played = 0) \
         ORDER BY ep.season_number, ep.episode_number",
    )
    .bind(user_id)
    .bind(sid)
    .fetch_all(db.pool())
    .await
    .context("next_up candidates")?;

    // 只保留「anchor 同季」且集序 >= anchor 的集 → 自 start 起取 limit 条。
    let ids: Vec<i64> = cand
        .into_iter()
        .filter(|c| {
            c.season_id == anchor_season_id && (c.season_number, c.episode_number) >= anchor_key
        })
        .skip(start as usize)
        .take(limit as usize)
        .map(|c| c.id)
        .collect();
    let rows = load_episode_rows(db, user_id, &ids).await?;
    // load_episode_rows 用 `id IN(...)` 无序 → 按 ids（集序升序）重排，保证「接下来」顺序稳定。
    let mut by_id: HashMap<i64, ItemRow> = rows.into_iter().map(|r| (r.id, r)).collect();
    Ok(ids.into_iter().filter_map(|id| by_id.remove(&id)).collect())
}

/// 按 episode id 批量取完整 [`ItemRow`]（Q1 `item ⋈ user_item_data` 两表 + [`assemble_item_rows`]）。
/// 空输入返回空。供 NextUp 无 series 分支复用。
async fn load_episode_rows(db: &Db, user_id: i64, ids: &[i64]) -> Result<Vec<ItemRow>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT {FOLDER_COLS} \
         FROM item i \
         LEFT JOIN user_item_data uid ON uid.item_id = i.id AND uid.user_id = ? \
         WHERE i.type = 'episode' AND i.id IN ({placeholders})"
    );
    let mut q = sqlx::query_as::<_, ItemRow>(&sql).bind(user_id);
    for id in ids {
        q = q.bind(id);
    }
    let mut rows = q.fetch_all(db.pool()).await.context("load_episode_rows")?;
    assemble_item_rows(db, &mut rows).await?;
    Ok(rows)
}

pub async fn list_seasons(db: &Db, series_id: i64) -> Result<Vec<ItemRow>> {
    let rows = sqlx::query_as::<_, ItemRow>(&format!(
        "SELECT {SEASON_COLS} \
         FROM item i \
         JOIN item series_item ON series_item.id = i.parent_id \
         WHERE i.parent_id = ? AND i.type = 'season' \
         ORDER BY i.season_number"
    ))
    .bind(series_id)
    .fetch_all(db.pool())
    .await
    .context("list_seasons")?;
    Ok(rows)
}

/// 查询 Episodes（季下所有集，含剧集 / 季信息）。
///
/// **拆 4 表 JOIN 为「两表筛选 + 应用层单表批取组装」**（见 [`assemble_item_rows`]）：
/// Q1 `item ⋈ user_item_data`（`parent_id = 该季` + `type='episode'`，自身列 + 用户数据）
/// → 批取 media_source / season / series 补全 `ItemRow`，经 `EpisodeCardJson` 渲染。
pub async fn list_episodes(db: &Db, season_id: i64, user_id: i64) -> Result<Vec<ItemRow>> {
    let mut rows = sqlx::query_as::<_, ItemRow>(&format!(
        "SELECT {FOLDER_COLS} \
         FROM item i \
         LEFT JOIN user_item_data uid ON uid.item_id = i.id AND uid.user_id = ? \
         WHERE i.parent_id = ? AND i.type = 'episode' \
         ORDER BY i.episode_number"
    ))
    .bind(user_id)
    .bind(season_id)
    .fetch_all(db.pool())
    .await
    .context("list_episodes")?;
    assemble_item_rows(db, &mut rows).await?;
    Ok(rows)
}

/// 批量查询 folder 项（Season/Series）的子集计数，避免列表端点 N+1。
///
/// 返回 `item.id → (recursive_total, unplayed_count, season_count)`：
/// - Season：直接子集 = `parent_id` 指向该季的 episode 数；`season_count` 为 None。
/// - Series：递归子集 = 经 season 回溯到该 series 的 episode 数；
///   `season_count` = 直接子季数（供 Emby `ChildCount`）。
///
/// `unplayed_count` = 未看完集数（`user_item_data` 缺失或 `played=0`）。
/// 非文件夹项忽略；空输入返回空 map。
pub async fn child_counts_batch(
    db: &Db,
    items: &[ItemRow],
    user_id: i64,
) -> Result<HashMap<i64, (i64, i64, Option<i64>)>> {
    let mut out: HashMap<i64, (i64, i64, Option<i64>)> = HashMap::new();
    let season_ids: Vec<i64> = items
        .iter()
        .filter(|i| i.item_type == "Season")
        .map(|i| i.id)
        .collect();
    let series_ids: Vec<i64> = items
        .iter()
        .filter(|i| i.item_type == "Series")
        .map(|i| i.id)
        .collect();
    count_episodes_batch(db, &season_ids, user_id, false, &mut out).await?;
    count_episodes_batch(db, &series_ids, user_id, true, &mut out).await?;
    // Series 直接子季数 → ChildCount
    for (series_id, n) in count_seasons_batch(db, &series_ids).await? {
        if let Some(e) = out.get_mut(&series_id) {
            e.2 = Some(n);
        }
    }
    Ok(out)
}

/// 分组计数 Season 的直接子季数：`series_id → COUNT(*)`（`type='season'`）。
/// `series_ids` 为空时直接返回空 map。
async fn count_seasons_batch(db: &Db, series_ids: &[i64]) -> Result<HashMap<i64, i64>> {
    let mut out: HashMap<i64, i64> = HashMap::new();
    if series_ids.is_empty() {
        return Ok(out);
    }
    let placeholders = std::iter::repeat_n("?", series_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let rows = sqlx::query_as::<_, (i64, i64)>(&format!(
        "SELECT parent_id, COUNT(*) FROM item \
         WHERE type = 'season' AND parent_id IN ({placeholders}) \
         GROUP BY parent_id"
    ))
    .fetch_all(db.pool())
    .await
    .context("count_seasons_batch")?;
    for (parent_id, n) in rows {
        out.insert(parent_id, n);
    }
    Ok(out)
}

/// 分组计数 episode：`series_mode=false` 按 `ep.parent_id`（季）分组；
/// `series_mode=true` 经 season 回溯按 `season.parent_id`（剧集）分组。
/// `parent_ids` 为空时直接返回。结果合并入 `out`（key=父 id, value=(total, unplayed, None)）。
async fn count_episodes_batch(
    db: &Db,
    parent_ids: &[i64],
    user_id: i64,
    series_mode: bool,
    out: &mut HashMap<i64, (i64, i64, Option<i64>)>,
) -> Result<()> {
    if parent_ids.is_empty() {
        return Ok(());
    }
    let placeholders = std::iter::repeat_n("?", parent_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = if series_mode {
        format!(
            "SELECT season.parent_id, COUNT(*), \
                    COUNT(CASE WHEN uid.id IS NULL OR uid.played = 0 THEN 1 END) \
             FROM item ep \
             JOIN item season ON season.id = ep.parent_id \
             LEFT JOIN user_item_data uid ON uid.item_id = ep.id AND uid.user_id = ? \
             WHERE ep.type = 'episode' \
               AND season.parent_id IN ({placeholders}) \
             GROUP BY season.parent_id"
        )
    } else {
        format!(
            "SELECT ep.parent_id, COUNT(*), \
                    COUNT(CASE WHEN uid.id IS NULL OR uid.played = 0 THEN 1 END) \
             FROM item ep \
             LEFT JOIN user_item_data uid ON uid.item_id = ep.id AND uid.user_id = ? \
             WHERE ep.type = 'episode' \
               AND ep.parent_id IN ({placeholders}) \
             GROUP BY ep.parent_id"
        )
    };
    let mut q = sqlx::query_as::<_, (i64, i64, i64)>(&sql).bind(user_id);
    for id in parent_ids {
        q = q.bind(id);
    }
    let rows = q
        .fetch_all(db.pool())
        .await
        .context("count_episodes_batch")?;
    for (parent_id, total, unplayed) in rows {
        out.insert(parent_id, (total, unplayed, None));
    }
    Ok(())
}

/// 按 People 过滤 Items（`PersonIds` 参数，person 主页）。
///
/// 关联 `item_people` 表：`ip.item_id = i.id AND ip.people_id IN (...)`，
/// 支持多 person 逗号分隔（`PersonIds=p-24,p-25`）。`item_types` 为 DB 小写
/// 类型（movie/series/episode…）白名单；空切片表示不过滤类型。
/// 两步查询：先取去重的 item_id（防一人多角色造成重复行），再按 id 查 ItemRow。
pub async fn list_items_by_person(
    db: &Db,
    user_id: i64,
    person_ids: &[i64],
    item_types: &[&str],
    limit: i64,
    start: i64,
) -> Result<ItemsResult> {
    if person_ids.is_empty() {
        return Ok(ItemsResult {
            items: vec![],
            total: 0,
        });
    }
    let person_ph = std::iter::repeat_n("?", person_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let (type_clause, type_tail): (String, Vec<String>) = if item_types.is_empty() {
        (String::new(), vec![])
    } else {
        let ph = std::iter::repeat_n("?", item_types.len())
            .collect::<Vec<_>>()
            .join(", ");
        (
            format!(" AND i.type IN ({ph})"),
            item_types.iter().map(|s| s.to_string()).collect(),
        )
    };

    let total: i64 = {
        let sql = format!(
            "SELECT COUNT(DISTINCT ip.item_id) FROM item_people ip \
             JOIN item i ON i.id = ip.item_id \
             WHERE ip.people_id IN ({person_ph}){type_clause}"
        );
        let mut q = sqlx::query_scalar::<_, i64>(&sql);
        for pid in person_ids {
            q = q.bind(pid);
        }
        for t in &type_tail {
            q = q.bind(t);
        }
        q.fetch_one(db.pool())
            .await
            .context("count items by person")?
    };

    let ids: Vec<i64> = {
        let sql = format!(
            "SELECT DISTINCT ip.item_id FROM item_people ip \
             JOIN item i ON i.id = ip.item_id \
             WHERE ip.people_id IN ({person_ph}){type_clause} \
             ORDER BY i.title LIMIT ? OFFSET ?"
        );
        let mut q = sqlx::query_scalar::<_, i64>(&sql);
        for pid in person_ids {
            q = q.bind(pid);
        }
        for t in &type_tail {
            q = q.bind(t);
        }
        q.bind(limit)
            .bind(start)
            .fetch_all(db.pool())
            .await
            .context("query person item ids")?
    };

    if ids.is_empty() {
        return Ok(ItemsResult {
            items: vec![],
            total,
        });
    }
    let id_ph = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let items = {
        let sql = format!(
            "SELECT {FOLDER_COLS} \
             FROM item i \
             LEFT JOIN user_item_data uid ON uid.item_id = i.id AND uid.user_id = ? \
             WHERE i.id IN ({id_ph}) ORDER BY i.title"
        );
        let mut q = sqlx::query_as::<_, ItemRow>(&sql).bind(user_id);
        for id in &ids {
            q = q.bind(id);
        }
        q.fetch_all(db.pool())
            .await
            .context("query items by person")?
    };

    Ok(ItemsResult { items, total })
}

/// 查询用户的收藏列表（支持分页）。
///
/// **拆 5 表 JOIN 为「两表筛选 + 应用层单表批取组装」**（见 [`assemble_item_rows`]，杜绝 N+1、
/// 无 SQL 连表超 2 张）：Q1 `item ⋈ user_item_data`（`is_favorite=1` + 可选类型过滤 + 分页，
/// 自身列 + 用户数据）→ 批取 media_source / season / series 补全 `ItemRow`。收藏经
/// `render_media_cards`（`MovieSeriesCardJson`，与其余 media 列表共用同一精简 DTO）。
pub async fn list_favorites(
    db: &Db,
    user_id: i64,
    video_type: Option<&str>,
    limit: i64,
    start: i64,
) -> Result<ItemsResult> {
    let ty = video_type.unwrap_or("").trim();
    let start = start.max(0);

    let type_filter = if ty.eq_ignore_ascii_case("Movie") {
        Some("movie")
    } else if ty.eq_ignore_ascii_case("Series") {
        Some("series")
    } else if ty.eq_ignore_ascii_case("Season") {
        Some("season")
    } else if ty.eq_ignore_ascii_case("Episode") {
        Some("episode")
    } else if ty.eq_ignore_ascii_case("Video") {
        // Emby Video 类型在本实现等价 movie（与 parse_include_item_types 一致）
        Some("movie")
    } else if ty.is_empty() {
        None
    } else {
        return Ok(ItemsResult {
            items: vec![],
            total: 0,
        });
    };

    let type_clause = if type_filter.is_some() {
        " AND i.type = ?"
    } else {
        ""
    };

    let total: i64 = {
        let sql = format!(
            "SELECT COUNT(*) FROM item i \
             JOIN user_item_data uid ON uid.item_id = i.id \
             WHERE uid.user_id = ? AND uid.is_favorite = 1{type_clause}"
        );
        let mut q = sqlx::query_scalar::<_, i64>(&sql).bind(user_id);
        if let Some(tf) = type_filter {
            q = q.bind(tf);
        }
        q.fetch_one(db.pool())
            .await
            .context("list_favorites count")?
    };

    let mut rows = {
        let sql = format!(
            "SELECT {FOLDER_COLS} FROM item i \
             JOIN user_item_data uid ON uid.item_id = i.id \
             WHERE uid.user_id = ? AND uid.is_favorite = 1{type_clause} \
             ORDER BY uid.updated_at DESC LIMIT ? OFFSET ?"
        );
        let mut q = sqlx::query_as::<_, ItemRow>(&sql).bind(user_id);
        if let Some(tf) = type_filter {
            q = q.bind(tf);
        }
        q = q.bind(limit).bind(start);
        q.fetch_all(db.pool()).await.context("list_favorites")?
    };
    assemble_item_rows(db, &mut rows).await?;

    Ok(ItemsResult { items: rows, total })
}
/// 按 Studio 过滤 Items（`StudioIds` 参数，studio 主页）。
/// 关联 `item_studio` 表：`ist.item_id = i.id AND ist.studio_id IN (...)`。
/// 支持多 studio 逗号分隔。两步查询：先取去重的 item_id，再按 id 查 ItemRow。
pub async fn list_items_by_studio(
    db: &Db,
    user_id: i64,
    studio_ids: &[i64],
    item_types: &[&str],
    limit: i64,
    start: i64,
) -> Result<ItemsResult> {
    if studio_ids.is_empty() {
        return Ok(ItemsResult {
            items: vec![],
            total: 0,
        });
    }
    let studio_ph = std::iter::repeat_n("?", studio_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let (type_clause, type_tail): (String, Vec<String>) = if item_types.is_empty() {
        (String::new(), vec![])
    } else {
        let ph = std::iter::repeat_n("?", item_types.len())
            .collect::<Vec<_>>()
            .join(", ");
        (
            format!(" AND i.type IN ({ph})"),
            item_types.iter().map(|s| s.to_string()).collect(),
        )
    };

    let total: i64 = {
        let sql = format!(
            "SELECT COUNT(DISTINCT ist.item_id) FROM item_studio ist \
             JOIN item i ON i.id = ist.item_id \
             WHERE ist.studio_id IN ({studio_ph}){type_clause}"
        );
        let mut q = sqlx::query_scalar::<_, i64>(&sql);
        for sid in studio_ids {
            q = q.bind(sid);
        }
        for t in &type_tail {
            q = q.bind(t);
        }
        q.fetch_one(db.pool())
            .await
            .context("count items by studio")?
    };

    let ids: Vec<i64> = {
        let sql = format!(
            "SELECT DISTINCT ist.item_id FROM item_studio ist \
             JOIN item i ON i.id = ist.item_id \
             WHERE ist.studio_id IN ({studio_ph}){type_clause} \
             ORDER BY i.title LIMIT ? OFFSET ?"
        );
        let mut q = sqlx::query_scalar::<_, i64>(&sql);
        for sid in studio_ids {
            q = q.bind(sid);
        }
        for t in &type_tail {
            q = q.bind(t);
        }
        q.bind(limit)
            .bind(start)
            .fetch_all(db.pool())
            .await
            .context("query studio item ids")?
    };

    if ids.is_empty() {
        return Ok(ItemsResult {
            items: vec![],
            total,
        });
    }
    let id_ph = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let items = {
        let sql = format!(
            "SELECT {FOLDER_COLS} \
             FROM item i \
             LEFT JOIN user_item_data uid ON uid.item_id = i.id AND uid.user_id = ? \
             WHERE i.id IN ({id_ph}) ORDER BY i.title"
        );
        let mut q = sqlx::query_as::<_, ItemRow>(&sql).bind(user_id);
        for id in &ids {
            q = q.bind(id);
        }
        q.fetch_all(db.pool())
            .await
            .context("query items by studio")?
    };

    Ok(ItemsResult { items, total })
}

/// 按 Genre 过滤 Items（`GenreIds` 参数，genre 主页）。
///
/// 关联 `item_genre` 表：`ig.item_id = i.id AND ig.genre_id IN (...)`，
/// 支持多 genre 逗号分隔（`GenreIds=5,6`）。`item_types` 为 DB 小写类型白名单；
/// 空切片表示不过滤类型。两步查询：先取去重的 item_id，再按 id 查 ItemRow。
pub async fn list_items_by_genre(
    db: &Db,
    user_id: i64,
    genre_ids: &[i64],
    item_types: &[&str],
    limit: i64,
    start: i64,
) -> Result<ItemsResult> {
    if genre_ids.is_empty() {
        return Ok(ItemsResult {
            items: vec![],
            total: 0,
        });
    }
    let genre_ph = std::iter::repeat_n("?", genre_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let (type_clause, type_tail): (String, Vec<String>) = if item_types.is_empty() {
        (String::new(), vec![])
    } else {
        let ph = std::iter::repeat_n("?", item_types.len())
            .collect::<Vec<_>>()
            .join(", ");
        (
            format!(" AND i.type IN ({ph})"),
            item_types.iter().map(|s| s.to_string()).collect(),
        )
    };

    let total: i64 = {
        let sql = format!(
            "SELECT COUNT(DISTINCT ig.item_id) FROM item_genre ig \
             JOIN item i ON i.id = ig.item_id \
             WHERE ig.genre_id IN ({genre_ph}){type_clause}"
        );
        let mut q = sqlx::query_scalar::<_, i64>(&sql);
        for gid in genre_ids {
            q = q.bind(gid);
        }
        for t in &type_tail {
            q = q.bind(t);
        }
        q.fetch_one(db.pool())
            .await
            .context("count items by genre")?
    };

    let ids: Vec<i64> = {
        let sql = format!(
            "SELECT DISTINCT ig.item_id FROM item_genre ig \
             JOIN item i ON i.id = ig.item_id \
             WHERE ig.genre_id IN ({genre_ph}){type_clause} \
             ORDER BY i.title LIMIT ? OFFSET ?"
        );
        let mut q = sqlx::query_scalar::<_, i64>(&sql);
        for gid in genre_ids {
            q = q.bind(gid);
        }
        for t in &type_tail {
            q = q.bind(t);
        }
        q.bind(limit)
            .bind(start)
            .fetch_all(db.pool())
            .await
            .context("query genre item ids")?
    };

    if ids.is_empty() {
        return Ok(ItemsResult {
            items: vec![],
            total,
        });
    }
    let id_ph = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let items = {
        let sql = format!(
            "SELECT {FOLDER_COLS} \
             FROM item i \
             LEFT JOIN user_item_data uid ON uid.item_id = i.id AND uid.user_id = ? \
             WHERE i.id IN ({id_ph}) ORDER BY i.title"
        );
        let mut q = sqlx::query_as::<_, ItemRow>(&sql).bind(user_id);
        for id in &ids {
            q = q.bind(id);
        }
        q.fetch_all(db.pool())
            .await
            .context("query items by genre")?
    };

    Ok(ItemsResult { items, total })
}

/// 按指定 item id 集合过滤（`ListItemIds` 参数，BoxSet/合集内容）。
///
/// `item_ids` 为 item.id 列表，`IN (...)` 按 `ORDER BY i.title` 返回。
/// 空列表直接返回空结果。
pub async fn list_items_by_ids(db: &Db, user_id: i64, item_ids: &[i64]) -> Result<ItemsResult> {
    if item_ids.is_empty() {
        return Ok(ItemsResult {
            items: vec![],
            total: 0,
        });
    }
    let id_ph = std::iter::repeat_n("?", item_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let items = {
        let sql = format!(
            "SELECT {FOLDER_COLS} \
             FROM item i \
             LEFT JOIN user_item_data uid ON uid.item_id = i.id AND uid.user_id = ? \
             WHERE i.id IN ({id_ph}) ORDER BY i.title"
        );
        let mut q = sqlx::query_as::<_, ItemRow>(&sql).bind(user_id);
        for id in item_ids {
            q = q.bind(id);
        }
        q.fetch_all(db.pool()).await.context("query items by ids")?
    };
    Ok(ItemsResult {
        total: items.len() as i64,
        items,
    })
}

/// `/Items/{id}/Similar`：相似推荐。
///
/// 优先按与目标 item 的共同 genre 数量降序（交集越多越靠前）；目标无 genre
/// 或按 genre 找不到时，回退到同库 movie/series（按社区评分降序）。两步查询：
/// 先取候选 item_id，再按 id 查 ItemRow（含 user_item_data JOIN）。
pub async fn list_similar(db: &Db, user_id: i64, item_id: i64, limit: i64) -> Result<ItemsResult> {
    // 1. 目标 item 的 genre ids + library_id（不存在/已删 → 空结果）
    let target: Option<(Vec<i64>, Option<i64>)> = {
        let row =
            sqlx::query_as::<_, (Option<i64>,)>("SELECT library_id FROM item WHERE id = ? LIMIT 1")
                .bind(item_id)
                .fetch_optional(db.pool())
                .await
                .context("similar: query target library")?;
        let Some((library_id,)) = row else {
            return Ok(ItemsResult {
                items: vec![],
                total: 0,
            });
        };
        let genre_ids: Vec<i64> =
            sqlx::query_scalar("SELECT genre_id FROM item_genre WHERE item_id = ?")
                .bind(item_id)
                .fetch_all(db.pool())
                .await
                .context("similar: query target genres")?;
        Some((genre_ids, library_id))
    };
    let (genre_ids, library_id) = target.unwrap();

    // 2a. 按共同 genre 数排序取候选
    let mut ids: Vec<i64> = Vec::new();
    if !genre_ids.is_empty() {
        let ph = std::iter::repeat_n("?", genre_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT ig.item_id FROM item_genre ig \
             JOIN item i ON i.id = ig.item_id \
             WHERE i.id != ? AND ig.genre_id IN ({ph}) \
               AND i.type IN ('movie','series') \
             GROUP BY ig.item_id \
             ORDER BY COUNT(*) DESC, i.title \
             LIMIT ?"
        );
        let mut q = sqlx::query_scalar::<_, i64>(&sql).bind(item_id);
        for gid in &genre_ids {
            q = q.bind(gid);
        }
        q = q.bind(limit);
        ids = q
            .fetch_all(db.pool())
            .await
            .context("similar: query by genre")?;
    }

    // 2b. genre 命中为空时回退同库（按社区评分降序）
    if ids.is_empty()
        && let Some(lib_id) = library_id
    {
        ids = sqlx::query_scalar(
            "SELECT id FROM item \
             WHERE id != ? AND library_id = ? \
               AND type IN ('movie','series') \
             ORDER BY COALESCE(community_rating, 0) DESC, title \
             LIMIT ?",
        )
        .bind(item_id)
        .bind(lib_id)
        .bind(limit)
        .fetch_all(db.pool())
        .await
        .context("similar: query by library")?;
    }

    if ids.is_empty() {
        return Ok(ItemsResult {
            items: vec![],
            total: 0,
        });
    }
    let id_ph = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let items = {
        let sql = format!(
            "SELECT {FOLDER_COLS} \
             FROM item i \
             LEFT JOIN user_item_data uid ON uid.item_id = i.id AND uid.user_id = ? \
             WHERE i.id IN ({id_ph}) ORDER BY i.title"
        );
        let mut q = sqlx::query_as::<_, ItemRow>(&sql).bind(user_id);
        for id in &ids {
            q = q.bind(id);
        }
        q.fetch_all(db.pool())
            .await
            .context("query similar items")?
    };

    Ok(ItemsResult {
        total: items.len() as i64,
        items,
    })
}
