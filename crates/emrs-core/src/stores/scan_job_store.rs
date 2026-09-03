//! `scan_job` 表的唯一属主：目录扫描任务的状态机持久化。
//!
//! 全仓仅此模块对 `scan_job` 发裸 SQL（I1 单一写者）。生命周期：
//! `pending → running → done|failed|canceled`。状态推进用 `COALESCE` 累积计数、
//! 首次进 running 记 `started_at`、进终态记 `finished_at`，语义与迁移前逐字一致。
//!
//! 说明：`added` / `updated` 由调用方（importer 阶段）从 `ScanStats` 折算成 i64 传入，
//! store 不依赖 importer 类型，保持数据访问层的下游纯净。

use crate::db::Db;

/// 创建一个 pending 扫描任务，返回其 id。
pub async fn create(db: &Db, library_id: i64, triggered_by: &str) -> anyhow::Result<i64> {
    let now = crate::emby::format_time_now();
    sqlx::query(
        "INSERT INTO scan_job (library_id, status, triggered_by, created_at) \
         VALUES (?, 'pending', ?, ?)",
    )
    .bind(library_id)
    .bind(triggered_by)
    .bind(&now)
    .execute(db.pool())
    .await?;
    let id = sqlx::query_scalar::<_, i64>("SELECT id FROM scan_job ORDER BY id DESC LIMIT 1")
        .fetch_one(db.pool())
        .await?;
    Ok(id)
}

/// 推进任务状态。
///
/// - `status = "running"`：记 `started_at`（首次）
/// - `status ∈ {done,failed,canceled}`：记 `finished_at`
/// - `added` / `updated`：累加到 `added_items` / `updated_items`
pub async fn update_status(db: &Db, job_id: i64, status: &str, added: i64, updated: i64) {
    let now = crate::emby::format_time_now();
    let (started_at, finished_at) = match status {
        "running" => (Some(now.as_str()), None),
        "done" | "failed" | "canceled" => (None, Some(now.as_str())),
        _ => (None, None),
    };
    let result = sqlx::query(
        "UPDATE scan_job SET status = ?, started_at = COALESCE(?, started_at), \
         finished_at = COALESCE(?, finished_at), \
         scanned_dirs = COALESCE(scanned_dirs, 0) + ?, \
         added_items = COALESCE(added_items, 0) + ?, \
         updated_items = COALESCE(updated_items, 0) + ? \
         WHERE id = ?",
    )
    .bind(status)
    .bind(started_at)
    .bind(finished_at)
    .bind(0i64)
    .bind(added)
    .bind(updated)
    .bind(job_id)
    .execute(db.pool())
    .await;
    if let Err(e) = &result {
        tracing::warn!(job_id, status, error = %e, "scan_job 状态更新失败");
    }
}

/// 记录任务失败原因（不改变 status）。
pub async fn set_error(db: &Db, job_id: i64, error: &str) -> anyhow::Result<()> {
    sqlx::query("UPDATE scan_job SET error = ? WHERE id = ?")
        .bind(error)
        .bind(job_id)
        .execute(db.pool())
        .await?;
    Ok(())
}

/// 查询待处理任务（status='pending'，按创建先后，最多 10 条）。
/// 返回 `(id, library_id, triggered_by)`。
pub async fn pending(db: &Db) -> Vec<(i64, i64, String)> {
    sqlx::query_as::<_, (i64, i64, String)>(
        "SELECT id, library_id, triggered_by FROM scan_job \
         WHERE status = 'pending' ORDER BY created_at ASC LIMIT 10",
    )
    .fetch_all(db.pool())
    .await
    .unwrap_or_default()
}

/// 批量读取指定任务的 `(status, added_items, updated_items)`，供 admin 轮询进度。
/// `added_items` / `updated_items` 为 NULL 时取 0（与旧口径一致）。
pub async fn poll_status_batch(db: &Db, ids: &[i64]) -> Vec<(String, i64, i64)> {
    if ids.is_empty() {
        return Vec::new();
    }
    let ph = vec!["?"; ids.len()].join(",");
    let sql = format!(
        "SELECT status, COALESCE(added_items, 0), COALESCE(updated_items, 0) \
         FROM scan_job WHERE id IN ({ph})"
    );
    let mut q = sqlx::query_as::<_, (String, i64, i64)>(&sql);
    for id in ids {
        q = q.bind(id);
    }
    q.fetch_all(db.pool()).await.unwrap_or_default()
}

/// 协作式取消：把仍处于 pending 的任务置 `canceled` 并记 finished_at。
/// running 行不动（由流水线跑完，粒度同旧实现）。
pub async fn cancel_pending_batch(db: &Db, ids: &[i64]) {
    if ids.is_empty() {
        return;
    }
    let ph = vec!["?"; ids.len()].join(",");
    let sql = format!(
        "UPDATE scan_job SET status = 'canceled', finished_at = ? \
         WHERE id IN ({ph}) AND status = 'pending'"
    );
    let mut q = sqlx::query(&sql).bind(crate::emby::format_time_now());
    for id in ids {
        q = q.bind(id);
    }
    let _ = q.execute(db.pool()).await;
}
