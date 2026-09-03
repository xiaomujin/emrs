//! 流水线协调器：启动独立 tokio 任务循环，轮询各阶段待处理项。
//!
//! 阶段顺序：Scan → Probe → Scrape（Identify 已并入 Scrape 单一消费者）
//! 每阶段独立轮询，状态全部落 DB（`item.scrape_status` / `media_source.status` /
//! `scan_job`），重启不丢任务，内存仅管轮询节奏。
//!
//! 语义（元数据分离）：
//! - Scan 消费 `scan_job(pending)`：只落物理事实（目录遍历、item 树、media_source 入 pending）
//! - Probe 消费 `media_source(status='pending', protocol='file')`：ffprobe 回填 → ok/failed
//! - Scrape 消费 `item(scrape_status='pending')`：TMDB 匹配/按 ID 快路径 → scraped/none/retry
//!
//! 图片不落盘：`/Items/{id}/Images/{type}` 路由经 HttpClient 代理上游 URL 返回。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::Notify;
use tracing::{info, warn};

use emrs_core::config::PipelineConfig;
use emrs_core::scan::ScanWaker;
use emrs_infra::db::Db;
use emrs_infra::http_client::Outbound;
use emrs_infra::stores::{item_store, library_store, media_store};

use super::stages::{ProbeStage, ScanStage, ScrapeStage};

/// 流水线协调器。
///
/// `start()` 生成 3 个独立 tokio 任务，各自按配置间隔轮询 DB。
/// `notify()` 可立即唤醒某阶段（如 watch 触发后唤醒 scan、手动识别后唤醒 scrape）。
pub struct Pipeline {
    db: Arc<Db>,
    config: PipelineConfig,
    tmdb_api_key: String,
    outbound: Arc<Outbound>,
    /// scan 阶段唤醒信号
    scan_notify: Arc<Notify>,
    /// probe 阶段唤醒信号
    probe_notify: Arc<Notify>,
    /// scrape 阶段唤醒信号
    scrape_notify: Arc<Notify>,
}

impl Pipeline {
    pub fn new(
        db: Arc<Db>,
        config: PipelineConfig,
        tmdb_api_key: String,
        outbound: Arc<Outbound>,
    ) -> Self {
        Self {
            db,
            config,
            tmdb_api_key,
            outbound,
            scan_notify: Arc::new(Notify::new()),
            probe_notify: Arc::new(Notify::new()),
            scrape_notify: Arc::new(Notify::new()),
        }
    }

    /// 启动后台轮询。返回后各阶段在独立 tokio 任务中运行。
    pub fn start(self: &Arc<Self>) {
        if !self.config.enabled {
            info!("媒体流水线已禁用（pipeline.enabled=false）");
            return;
        }

        // 崩溃恢复：上次进程退出时滞留在 scraping 的条目复位为 pending（委托 item_store）
        let db = self.db.clone();
        tokio::spawn(async move {
            match item_store::reset_stale_scraping(&db).await {
                Ok(count) if count > 0 => {
                    info!(count, "启动清扫：scraping 条目复位为 pending");
                }
                Ok(_) => {}
                Err(e) => warn!(error = %e, "启动清扫失败"),
            }
        });

        let this = self.clone();
        tokio::spawn(async move {
            this.run_scan_loop().await;
        });

        let this = self.clone();
        tokio::spawn(async move {
            this.run_probe_loop().await;
        });

        let this = self.clone();
        tokio::spawn(async move {
            this.run_scrape_loop().await;
        });

        info!(
            poll_secs = self.config.poll_interval_secs,
            probe_conc = self.config.probe_concurrency,
            scrape_rate_per_sec = self.config.scrape_rate_limit_per_sec,
            retry_max = self.config.scrape_retry_max_attempts,
            delete_check_secs = self.config.delete_check_interval_secs,
            "三阶段流水线已启动（Scan / Probe / Scrape）"
        );
    }

    /// 唤醒 scan 阶段（如 watch 触发后）。
    pub fn notify_scan(&self) {
        self.scan_notify.notify_one();
    }

    /// 唤醒 probe 阶段。
    pub fn notify_probe(&self) {
        self.probe_notify.notify_one();
    }

    /// 唤醒 scrape 阶段（如手动识别 / force 重置后）。
    pub fn notify_scrape(&self) {
        self.scrape_notify.notify_one();
    }

    /// Scan 阶段：处理 pending scan_job，逐个执行目录扫描；
    /// 扫描完成后触发一次删除检测（空闲期按低频兜底间隔再跑）。
    async fn run_scan_loop(&self) {
        let interval = Duration::from_secs(self.config.poll_interval_secs.max(5));
        let delete_interval = Duration::from_secs(self.config.delete_check_interval_secs.max(60));
        let mut last_delete_check: Option<Instant> = None;

        loop {
            tokio::select! {
                _ = self.scan_notify.notified() => {}
                _ = tokio::time::sleep(interval) => {}
            }

            let stage = ScanStage::with_tmdb_and_yield(
                self.db.clone(),
                self.tmdb_api_key.clone(),
                self.outbound.clone(),
                self.config.scan_yield_every_files,
                self.config.scan_yield_ms,
            );

            let pending = stage.pending_scan_jobs().await;
            let mut scanned_any = false;
            for job in pending {
                stage.update_scan_job_status(job.id, "running", None).await;
                // 获取 library_path
                let paths = library_store::paths_of_library(&self.db, job.library_id)
                    .await
                    .unwrap_or_default();

                let mut had_error = false;
                for path in &paths {
                    match stage.scan_path(&PathBuf::from(&path)).await {
                        Ok(stats) => {
                            stage
                                .update_scan_job_status(job.id, "done", Some(&stats))
                                .await;
                            scanned_any = true;
                        }
                        Err(e) => {
                            warn!(job_id = job.id, path, error = %e, "scan 失败");
                            had_error = true;
                        }
                    }
                }
                if had_error {
                    stage.update_scan_job_status(job.id, "failed", None).await;
                }
            }

            // 删除检测触发条件：本轮有扫描完成，或距上次兜底超 delete_check_interval_secs。
            // 避免空闲期对全表 media_source 做逐行 fs stat 的周期性风暴。
            let due_fallback = last_delete_check
                .map(|t| t.elapsed() >= delete_interval)
                .unwrap_or(false);
            if scanned_any || due_fallback {
                match self.detect_deletions().await {
                    Ok(count) => {
                        if count > 0 || scanned_any {
                            info!(count, triggered_by_scan = scanned_any, "删除检测完成");
                        }
                    }
                    Err(e) => warn!(error = %e, "删除检测失败"),
                }
                last_delete_check = Some(Instant::now());
            }
        }
    }

    /// Probe 阶段：轮询 media_source.status='pending'（file 协议），并发 ffprobe 回填
    /// metadata/chapters 后置 ok；探测失败置 failed（播放链路不读 status）。
    async fn run_probe_loop(&self) {
        let interval = Duration::from_secs(self.config.poll_interval_secs.max(5));
        let batch = (self.config.probe_concurrency as i64 * 4).max(8);
        loop {
            tokio::select! {
                _ = self.probe_notify.notified() => {}
                _ = tokio::time::sleep(interval) => {}
            }

            let stage = ProbeStage::with_concurrency_and_yield(
                self.db.clone(),
                self.config.probe_concurrency,
                self.config.probe_yield_ms,
            );
            let (total, ok, failed) = stage.run_pending(batch).await;
            if total > 0 {
                // 收尾 checkpoint：把本轮探测写入的 WAL 刷回主库并截断，
                // 避免长扫描 + 探测期间 WAL 文件无界增长。
                let _ = self.db.checkpoint_truncate().await;
                info!(total, ok, failed, "probe 阶段处理完成");
            }
        }
    }

    /// Scrape 阶段：单一消费者轮询 item.scrape_status='pending'（series 优先），
    /// TMDB 搜索或按 ID 快路径 → scraped/none/retry（见 stages/scrape.rs 状态机注释）。
    async fn run_scrape_loop(&self) {
        let interval = Duration::from_secs(self.config.poll_interval_secs.max(5));
        let batch = self.config.scrape_concurrency as i64;
        loop {
            tokio::select! {
                _ = self.scrape_notify.notified() => {}
                _ = tokio::time::sleep(interval) => {}
            }

            if self.tmdb_api_key.is_empty() {
                continue;
            }

            let stage = ScrapeStage::with_options(
                self.db.clone(),
                self.tmdb_api_key.clone(),
                self.outbound.clone(),
                self.config.scrape_retry_max_attempts,
                self.config.scrape_rate_limit_per_sec,
            );
            let stats = stage.run_pending(batch).await;
            if stats.scraped > 0 || stats.failed > 0 || stats.none > 0 {
                info!(
                    scraped = stats.scraped,
                    none = stats.none,
                    retried_or_failed = stats.failed,
                    skipped = stats.skipped,
                    "scrape 阶段处理完成"
                );
            }
        }
    }

    /// 删除检测：遍历全部本地 media_source，检查文件存在性；不存在的物理删除，
    /// 并级联删除其 external_subtitle；无剩余源的非虚拟 item 一并物理删除。
    /// http/strm 不做本地文件检查。
    pub async fn detect_deletions(&self) -> Result<usize> {
        // 覆盖 pending/ok/failed 全部在册状态——否则扫描刚入库（pending）或
        // 探测失败（failed）的行永远逃过存在性检查
        let sources = media_store::list_all_source_paths(&self.db).await?;

        let mut count = 0;
        for row in sources {
            let id = row.id;
            let file_path = row.path.or(row.remote_path);
            let Some(p) = file_path else { continue };

            if p.starts_with("http://") || p.starts_with("https://") {
                continue; // strm/cloud 不做本地文件存在性检查
            }

            let exists = tokio::fs::metadata(&p).await.is_ok();
            if !exists {
                // 先捕获 item_id（删行后无法再回查）
                let item_id = media_store::item_id_of_source(&self.db, id).await?;

                // 物理删除外部字幕
                media_store::delete_subtitles_of_source(&self.db, id).await?;

                // 物理删除 media_source
                media_store::delete_source(&self.db, id).await?;

                // 检查该 item 是否还有其他 media_source
                let remaining = match item_id {
                    Some(item_id) => media_store::count_other_sources(&self.db, item_id, id).await,
                    None => 0,
                };

                // 如果 item 没有其他 media_source，物理删除 item（虚拟条目不受影响）
                if remaining == 0
                    && let Some(item_id) = item_id
                {
                    self.delete_item(item_id).await;
                }

                count += 1;
                info!(media_source_id = id, path = %p, "检测到文件删除，已物理删除");
            }
        }

        // 删除检测逐行 autocommit DELETE，收尾主动 checkpoint 把 WAL 刷回主库并截断。
        if count > 0 {
            let _ = self.db.checkpoint_truncate().await;
        }

        Ok(count)
    }

    /// 物理删除 item 及其全部关联数据（关联表 / 图片 / 用户进度 / 子项）。
    /// 调用方保证该 item 无剩余 media_source（虚拟条目单独处理）。
    /// 用显式栈替代递归（async fn 递归需 Box::pin，堆栈更直接）。
    async fn delete_item(&self, item_id: i64) {
        let mut stack = vec![item_id];
        while let Some(id) = stack.pop() {
            // 子项（season/episode）及其关联数据一并清理
            stack.extend(item_store::child_ids(&self.db, id).await);
            item_store::delete_item_cascade(&self.db, id).await;
        }
    }
}

/// watcher（emrs-infra）入队 scan_job 后立即唤醒 scan 循环：
/// 复用 [`Pipeline::notify_scan`] 的固有实现（裁定 B1-2 依赖注入）。
impl ScanWaker for Pipeline {
    fn wake_scan(&self) {
        Pipeline::notify_scan(self);
    }
}
