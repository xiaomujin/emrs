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
