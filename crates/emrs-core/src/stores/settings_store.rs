//! `app_setting` 表的唯一属主：运行时键值配置的读写。
//!
//! 全仓仅此模块直接触碰 `app_setting` 表（I1 单一写者）。对外经
//! [`crate::stores::ItemsStore`] 门面暴露（`get_setting` / `set_setting` / `list_settings`）。

use crate::db::Db;

/// 读取单个 `app_setting`（不存在或值为 NULL → `None`）。
pub async fn get_setting(db: &Db, key: &str) -> anyhow::Result<Option<String>> {
    let row =
        sqlx::query_scalar::<_, Option<String>>("SELECT value FROM app_setting WHERE key = ?")
            .bind(key)
            .fetch_optional(db.pool())
            .await?;
    Ok(row.flatten())
}

/// 写入 `app_setting`（按 `key` UPSERT）。
pub async fn set_setting(db: &Db, key: &str, value: &str) -> anyhow::Result<()> {
    let now = crate::emby::format_time_now();
    sqlx::query(
        "INSERT INTO app_setting (key, value, updated_at) VALUES (?, ?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(key)
    .bind(value)
    .bind(&now)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// 读取全部 `app_setting`（按 key 升序）。
pub async fn list_settings(db: &Db) -> anyhow::Result<Vec<(String, String)>> {
    let rows =
        sqlx::query_as::<_, (String, String)>("SELECT key, value FROM app_setting ORDER BY key")
            .fetch_all(db.pool())
            .await?;
    Ok(rows)
}
