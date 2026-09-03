//! 媒体库 + library_path 聚合查询。
//!
//! 媒体库行 + CollectionType 直接读 `library.collection_type` 字段，
//! 由管理员在创建/编辑库时设置，默认 `"tvshows"`。
//!
//! 合法值参见 [`COLLECTION_TYPES`] 与 [`is_valid_collection_type`]。

use anyhow::{Context, Result};

use super::LibraryView;
use crate::db::Db;

/// 合法 CollectionType 值列表（按 Emby 协议定义）。
pub const COLLECTION_TYPES: &[&str] = &[
    "movies",
    "tvshows",
    "music",
    "musicvideos",
    "homevideos",
    "games",
    "books",
    "livetv",
    "channels",
    "boxsets",
    "mixed",
    "audiobooks",
    "playlists",
];

/// 校验字符串是否为合法 CollectionType 值。
pub fn is_valid_collection_type(s: &str) -> bool {
    COLLECTION_TYPES.contains(&s)
}

/// 查询所有媒体库（Emby `/Users/{id}/Views` 的数据源）。
///
/// 直接返回库名称/创建时间/collection_type，不再伪造 ItemRow。
/// 库数即总数，不再做无意义的 `LIMIT`/`COUNT`。
pub async fn list_libraries(db: &Db) -> Result<Vec<LibraryView>> {
    let rows = sqlx::query_as::<_, LibraryView>(
        "SELECT id, name, created_at, updated_at, collection_type \
         FROM library \
         ORDER BY name",
    )
    .fetch_all(db.pool())
    .await
    .context("query libraries")?;

    Ok(rows)
}

/// 按路径获取或创建媒体库：`library` + `library_path` 的原子 get-or-create。
///
/// 命中已有 `library_path.path` → 返回其 `library_id`，**绝不改名**（库名由 admin
/// 建库时设定，扫描只入库条目，不得用文件夹 basename 覆盖）。未命中 → 以 `name`
/// 新建 library（CLI / watch / 手输路径首次扫描的兜底命名）+ 一条 library_path。
/// `path_type`：`http(s)://` → `"strm"`，否则 `"local"`。
///
/// 全仓唯一一处对 `library` / `library_path` 的写入（I1 单一写者）。
pub async fn get_or_create_by_path(db: &Db, name: &str, path: &str) -> Result<i64> {
    let now = emrs_core::emby::format_time_now();

    let existing_lib_id: Option<i64> =
        sqlx::query_scalar("SELECT library_id FROM library_path WHERE path = ? LIMIT 1")
            .bind(path)
            .fetch_optional(db.pool())
            .await?;
    if let Some(lib_id) = existing_lib_id {
        return Ok(lib_id);
    }

    sqlx::query("INSERT INTO library (name, created_at, updated_at) VALUES (?, ?, ?)")
        .bind(name)
        .bind(&now)
        .bind(&now)
        .execute(db.pool())
        .await?;

    let lib_id = sqlx::query_scalar::<_, i64>("SELECT id FROM library ORDER BY id DESC LIMIT 1")
        .fetch_one(db.pool())
        .await?;

    let path_type = if path.starts_with("http://") || path.starts_with("https://") {
        "strm"
    } else {
        "local"
    };
    sqlx::query(
        "INSERT INTO library_path (library_id, path, path_type, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(lib_id)
    .bind(path)
    .bind(path_type)
    .bind(&now)
    .bind(&now)
    .execute(db.pool())
    .await?;

    Ok(lib_id)
}

/// 查询指定库的全部扫描路径（`library_path.path`）。
///
/// 查询失败按空列表处理（调用方本轮跳过，与迁移前 pipeline 内联 SQL 的
/// `unwrap_or_default()` 语义逐字一致）。
pub async fn paths_of_library(db: &Db, library_id: i64) -> Result<Vec<String>> {
    let rows =
        sqlx::query_scalar::<_, String>("SELECT path FROM library_path WHERE library_id = ?")
            .bind(library_id)
            .fetch_all(db.pool())
            .await
            .unwrap_or_default();
    Ok(rows)
}

/// 按路径定位媒体库 id：canonicalize → 归一化 → 查 `library_path`，
/// 未命中则回退 [`get_or_create_by_path`]（以目录 basename 兜底命名）。
///
/// 原 `Scanner::library_id_for_path` 的 store 侧归属（watcher 入队与
/// ScanStage 共用；I1：library/library_path 写路径仍只经本模块）。
pub async fn library_id_for_path(db: &Db, path: &std::path::Path) -> Result<i64> {
    let canonical =
        std::fs::canonicalize(path).with_context(|| format!("无法解析路径: {}", path.display()))?;
    let path_str = crate::scanner::normalize_canonical_path(&canonical);

    if let Some(id) = sqlx::query_scalar::<_, i64>(
        "SELECT lp.library_id FROM library_path lp \
         JOIN library l ON l.id = lp.library_id \
         WHERE lp.path = ? LIMIT 1",
    )
    .bind(&path_str)
    .fetch_optional(db.pool())
    .await?
    {
        return Ok(id);
    }

    let library_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "Library".to_string());
    get_or_create_by_path(db, &library_name, &path_str).await
}
