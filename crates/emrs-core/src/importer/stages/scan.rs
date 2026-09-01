//! Stage 1: Scan — 遍历 library_path，创建/更新 item + media_source 记录。
//! 从 scanner.rs 拆出，保留 Scanner 公开入口的兼容语义。
//!
//! 入队约定：admin / watcher 一律经 [`ScanStage::enqueue_library_scan`] 写入
//! `scan_job(pending)` 并由 Pipeline 的 scan 循环消费——单一扫描消费者，
//! 天然串行，避免并发 SELECT-then-INSERT upsert 竞态。

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

use crate::db::Db;

use crate::importer::scanner::{ScanStats, Scanner};

/// 扫描阶段：递归扫描目录，创建 item/media_source 记录。
///
/// 本阶段委托 Scanner::scan_path 完成实际扫描工作，
/// 并管理 scan_job 全生命周期（pending/running/done/failed/canceled）。
pub struct ScanStage {
    db: Arc<Db>,
    scanner: Scanner,
}

impl ScanStage {
    pub fn new(db: Arc<Db>) -> Self {
        let scanner = Scanner::new(db.clone(), String::new());
        Self { db, scanner }
    }

    pub fn with_tmdb(db: Arc<Db>, tmdb_api_key: String, proxy_url: Option<String>) -> Self {
        Self::with_tmdb_and_yield(db, tmdb_api_key, proxy_url, 0, 0)
    }

    /// 带扫描写库节流参数的构造（`yield_every_files`/`yield_ms` 见 [`Scanner::with_yield`]）。
    pub fn with_tmdb_and_yield(
        db: Arc<Db>,
        tmdb_api_key: String,
        proxy_url: Option<String>,
        yield_every_files: usize,
        yield_ms: u64,
    ) -> Self {
        let scanner = Scanner::with_proxy(db.clone(), tmdb_api_key, proxy_url)
            .with_yield(yield_every_files, yield_ms);
        Self { db, scanner }
    }

    /// 扫描指定路径，创建/更新库记录并扫描媒体文件。
    pub async fn scan_path(&self, path: &Path) -> Result<ScanStats> {
        self.scanner.scan_path(path).await
    }

    /// 创建 scan_job 记录。
    pub async fn create_scan_job(&self, library_id: i64, triggered_by: &str) -> Result<i64> {
        let now = crate::emby::format_time_now();
        sqlx::query(
            "INSERT INTO scan_job (library_id, status, triggered_by, created_at) \
             VALUES (?, 'pending', ?, ?)",
        )
        .bind(library_id)
        .bind(triggered_by)
        .bind(&now)
        .execute(self.db.pool())
        .await?;
        Ok(
            sqlx::query_scalar::<_, i64>("SELECT id FROM scan_job ORDER BY id DESC LIMIT 1")
                .fetch_one(self.db.pool())
                .await?,
        )
    }

    /// 更新 scan_job 状态。
    pub async fn update_scan_job_status(
        &self,
        job_id: i64,
        status: &str,
        stats: Option<&ScanStats>,
    ) {
        let now = crate::emby::format_time_now();
        let (started_at, finished_at) = match status {
            "running" => (Some(now.as_str()), None),
            "done" | "failed" | "canceled" => (None, Some(now.as_str())),
            _ => (None, None),
        };
        let added = stats.map(|s| s.movies + s.series + s.episodes).unwrap_or(0) as i64;
        let updated = stats.map(|s| s.media).unwrap_or(0) as i64;

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
        .execute(self.db.pool())
        .await;
        if let Err(e) = &result {
            tracing::warn!(job_id, status, error = %e, "scan_job 状态更新失败");
        }
    }

    /// 扫描指定库路径，带 scan_job 生命周期管理。
    pub async fn scan_with_job(&self, path: &Path, triggered_by: &str) -> Result<ScanStats> {
        // 创建 library 获取 id
        let library_id = self.scanner.library_id_for_path(path).await?;
        let job_id = self.create_scan_job(library_id, triggered_by).await?;
        self.update_scan_job_status(job_id, "running", None).await;

        match self.scanner.scan_path(path).await {
            Ok(stats) => {
                self.update_scan_job_status(job_id, "done", Some(&stats))
                    .await;
                Ok(stats)
            }
            Err(e) => {
                self.update_scan_job_status(job_id, "failed", None).await;
                let _ = sqlx::query("UPDATE scan_job SET error = ? WHERE id = ?")
                    .bind(format!("{e:#}"))
                    .bind(job_id)
                    .execute(self.db.pool())
                    .await;
                Err(e)
            }
        }
    }

    /// 查询待处理的 scan_job（status='pending'）。
    pub async fn pending_scan_jobs(&self) -> Vec<(i64, i64, String)> {
        sqlx::query_as::<_, (i64, i64, String)>(
            "SELECT id, library_id, triggered_by FROM scan_job \
             WHERE status = 'pending' ORDER BY created_at ASC LIMIT 10",
        )
        .fetch_all(self.db.pool())
        .await
        .unwrap_or_default()
    }

    /// 统一扫描入口：定位/创建库记录后写入 `scan_job(pending)` 行，
    /// 由 Pipeline 的 scan 循环消费。admin 路由与目录监听共用。
    /// 返回 scan_job id（调用方随后调 `Pipeline::notify_scan()` 加速唤醒）。
    pub async fn enqueue_library_scan(&self, root: &Path, triggered_by: &str) -> Result<i64> {
        let library_id = self.scanner.library_id_for_path(root).await?;
        self.create_scan_job(library_id, triggered_by).await
    }
}
