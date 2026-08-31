//! user_item_data UPSERT：收藏 / 已看 / 进度。
//!
//! 新表 `user_item_data` 统一旧 `user_video_record` + `favorites`，唯一键
//! `(user_id, item_id)`。本模块以统一 `item_id` 粒度操作（movie/series/episode
//! 均为 item.id），路由层 `PlaybackStore` 门面直接透传。
//!
//! UPSERT 策略：三方言兼容的"先查后 upsert"——SQLite `INSERT OR IGNORE` +
//! `UPDATE`，MySQL/PG 走 `INSERT ... ON CONFLICT` / `ON DUPLICATE KEY` 由
//! sqlx Any 驱动按方言翻译占位符（统一用 `?`）。

use anyhow::{Context, Result};

use crate::db::Db;

/// UPSERT user_item_data（三方言兼容：先 INSERT OR IGNORE 再 UPDATE）。
async fn upsert_user_item_data(db: &Db, user_id: i64, item_id: i64) -> Result<()> {
    let now = crate::emby::format_time_now();
    // INSERT OR IGNORE：SQLite 原生；MySQL/PG 通过 Any 驱动转 INSERT ... ON CONFLICT DO NOTHING
    sqlx::query(
        "INSERT OR IGNORE INTO user_item_data (user_id, item_id, played, play_count, is_favorite, created_at, updated_at) \
         VALUES (?, ?, 0, 0, 0, ?, ?)",
    )
    .bind(user_id)
    .bind(item_id)
    .bind(&now)
    .bind(&now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// 记录播放进度（upsert）。
///
/// `item_id` 为目标 item.id（movie/series/episode 统一粒度）。
/// `play_ms` → `playback_position_ticks`（× 10000）；`is_complete` → `played`。
/// `play_count` 由开播上报（[`mark_started`]）递增，此处不再 +1。
pub async fn upsert_progress(
    db: &Db,
    user_id: i64,
    item_id: i64,
    play_ms: i64,
    is_complete: bool,
) -> Result<()> {
    let now = crate::emby::format_time_now();
    let position_ticks = play_ms * 10_000;

    upsert_user_item_data(db, user_id, item_id).await?;

    sqlx::query(
        "UPDATE user_item_data \
         SET playback_position_ticks = ?, \
             played = ?, \
             last_played_date = ?, \
             updated_at = ? \
         WHERE user_id = ? AND item_id = ?",
    )
    .bind(position_ticks)
    .bind(is_complete as i64)
    .bind(&now)
    .bind(&now)
    .bind(user_id)
    .bind(item_id)
    .execute(db.pool())
    .await
    .context("upsert_progress")?;
    Ok(())
}

/// 记录开始播放（upsert）：`play_count` +1。
///
/// 每次开播（`POST /Sessions/Playing`）递增播放次数，作为 Resume 的
/// "看过/正在看" 标记。新用户首播会先插入一行（play_count 0 → 1）。
pub async fn mark_started(db: &Db, user_id: i64, item_id: i64) -> Result<()> {
    upsert_user_item_data(db, user_id, item_id).await?;

    let now = crate::emby::format_time_now();
    sqlx::query(
        "UPDATE user_item_data \
         SET play_count = play_count + 1, \
             updated_at = ? \
         WHERE user_id = ? AND item_id = ?",
    )
    .bind(&now)
    .bind(user_id)
    .bind(item_id)
    .execute(db.pool())
    .await
    .context("mark_started")?;
    Ok(())
}

/// 标记收藏（upsert, 三方言兼容）。
///
/// 新表 `user_item_data` 统一到 `item_id` 粒度，不再区分 video_list/season/episode。
pub async fn toggle_favorite(db: &Db, user_id: i64, item_id: i64, favorite: bool) -> Result<()> {
    upsert_user_item_data(db, user_id, item_id).await?;

    let now = crate::emby::format_time_now();
    sqlx::query(
        "UPDATE user_item_data SET is_favorite = ?, updated_at = ? \
         WHERE user_id = ? AND item_id = ?",
    )
    .bind(favorite as i64)
    .bind(&now)
    .bind(user_id)
    .bind(item_id)
    .execute(db.pool())
    .await
    .context("toggle_favorite")?;
    Ok(())
}

/// 标记已看 / 未看。
///
/// `played=true` → `played=1`，`last_played_date` = now。
/// `played=false` → `played=0`，`play_count` 不变（播放次数只由开播递增）。
pub async fn mark_played(db: &Db, user_id: i64, item_id: i64, played: bool) -> Result<()> {
    upsert_user_item_data(db, user_id, item_id).await?;

    let now = crate::emby::format_time_now();
    if played {
        // 标记已看：played=1（不动 play_count，播放次数由开播上报递增）
        sqlx::query(
            "UPDATE user_item_data \
             SET played = 1, \
                 last_played_date = ?, \
                 updated_at = ? \
             WHERE user_id = ? AND item_id = ?",
        )
        .bind(&now)
        .bind(&now)
        .bind(user_id)
        .bind(item_id)
        .execute(db.pool())
        .await?;
    } else {
        // 标记未看：played=0
        sqlx::query(
            "UPDATE user_item_data SET played = 0, updated_at = ? \
             WHERE user_id = ? AND item_id = ?",
        )
        .bind(&now)
        .bind(user_id)
        .bind(item_id)
        .execute(db.pool())
        .await?;
    }
    Ok(())
}

/// 读取 user_item_data 行（无记录返回 None）。
///
/// 收藏 / 已看端点写操作后回读实际字段，供响应体返回真实 UserData。
pub async fn get_user_data(
    db: &Db,
    user_id: i64,
    item_id: i64,
) -> Result<Option<super::UserItemData>> {
    let row = sqlx::query_as::<_, super::UserItemData>(
        "SELECT played, play_count, playback_position_ticks, last_played_date, is_favorite \
         FROM user_item_data WHERE user_id = ? AND item_id = ?",
    )
    .bind(user_id)
    .bind(item_id)
    .fetch_optional(db.pool())
    .await?;
    Ok(row)
}
