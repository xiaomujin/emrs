//! Stage 3: Scrape — 单一元数据消费者（原 Identify + Scrape 两阶段合并）。
//!
//! 轮询 `item.scrape_status='pending'` 且 `type IN ('movie','series')`，
//! series 优先于 movie；季/集不单独消费，一律由父级 series 刮削派生回填后级联置态。
//!
//! 状态机（对齐 docs/design-metadata-separation.md）：
//! ```text
//! pending ──取件──> scraping ──┬─ 成功 ───────> scraped（子 season/episode 级联）
//!                              ├─ 无匹配 ─────> none（终态，保留基础 title，条目仍可见）
//!                              └─ 网络/API 异常 ── attempts+1 < 上限 → 回 pending（退避重试）
//!                                                └─ 达上限 → failed（终态，级联）
//! ```
//! 崩溃恢复：启动时 `scraping` 全部复位为 `pending`（`Pipeline::start` 内执行）。

use std::sync::Arc;

use tracing::{info, warn};

use crate::db::Db;
use crate::http_client::Outbound;

use crate::importer::scanner::{Scanner, ScrapeOutcome, ScrapeStats};

/// 元数据刮削阶段：统一消费 pending 的 movie/series 条目。
pub struct ScrapeStage {
    db: Arc<Db>,
    scanner: Scanner,
    /// 失败重试上限（网络/API 异常类），达到即转 failed 终态。
    retry_max_attempts: u64,
}

impl ScrapeStage {
    pub fn new(db: Arc<Db>, tmdb_api_key: String, outbound: Arc<Outbound>) -> Self {
        Self::with_options(db, tmdb_api_key, outbound, 5, 20)
    }

    /// 完整构造：`retry_max_attempts` 为异常重试上限；
    /// `rate_limit_per_sec` 进进程级 TMDB 限速（0 不限速）。
    pub fn with_options(
        db: Arc<Db>,
        tmdb_api_key: String,
        outbound: Arc<Outbound>,
        retry_max_attempts: u64,
        rate_limit_per_sec: u32,
    ) -> Self {
        let scanner = Scanner::with_rate(db.clone(), tmdb_api_key, outbound, rate_limit_per_sec);
        Self {
            db,
            scanner,
            retry_max_attempts,
        }
    }

    /// 消费一批 pending 条目（series 优先于 movie；子级从不入队）。
    pub async fn run_pending(&self, batch_size: i64) -> ScrapeStats {
        let mut stats = ScrapeStats::default();
        // ORDER BY CASE 兼容三方言，series 排前保证"先父后子"
        let rows = sqlx::query_as::<_, (i64, String, String, Option<String>, Option<String>, i64)>(
            "SELECT id, title, type, date_air, tmdb_id, COALESCE(scrape_attempts, 0) FROM item \
             WHERE scrape_status = 'pending' \
             AND type IN ('movie', 'series') \
             ORDER BY CASE type WHEN 'series' THEN 0 ELSE 1 END, id LIMIT ?",
        )
        .bind(batch_size)
        .fetch_all(self.db.pool())
        .await
        .unwrap_or_default();

        for (id, title, item_type, date_air, tmdb_id_raw, attempt_no) in rows {
            // 取件打标：处理中（进程崩溃后由启动清扫复位回 pending）
            self.mark_scraping(id).await;
            let started = std::time::Instant::now();

            let year = date_air
                .as_deref()
                .and_then(|d| d.get(..4))
                .and_then(|y| y.parse().ok());

            // 已有有效 tmdb_id（NFO 写入 / 手动指定）→ 按 ID 快路径；否则搜索路径
            let known_tmdb = tmdb_id_raw
                .as_deref()
                .filter(|s| !s.is_empty())
                .and_then(|s| s.parse::<i64>().ok())
                .filter(|&v| v > 0);
            let is_movie = item_type == "movie";

            let outcome = match (is_movie, known_tmdb) {
                (true, Some(tmdb_id)) => {
                    self.scanner.scrape_movie_by_tmdb(id, tmdb_id, &title).await
                }
                (false, Some(tmdb_id)) => self.scanner.scrape_tv_by_tmdb(id, tmdb_id, &title).await,
                (true, None) => self.scanner.scrape_movie(id, &title, year, false).await,
                (false, None) => self.scanner.scrape_tv(id, &title, false).await,
            };

            let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            match outcome {
                ScrapeOutcome::Scraped => {
                    stats.scraped += 1;
                    info!(
                        item_id = id,
                        item_type = %item_type,
                        title = %title,
                        tmdb_id = tmdb_id_raw.as_deref().unwrap_or(""),
                        outcome = "scraped",
                        attempts = attempt_no,
                        duration_ms,
                        "scrape 完成"
                    );
                    // 季/集由父级派生回填完成，级联置 scraped（虚拟行本就是 scraped，幂等）
                    if !is_movie {
                        self.cascade_children(id, "scraped").await;
                    }
                }
                ScrapeOutcome::Skipped => {
                    // 正常流程不会出现（key 守卫/快路径已分流）；稳妥回 pending 防滞留
                    stats.skipped += 1;
                    warn!(item_id = id, "scrape 返回 Skipped，复位 pending");
                    let _ = sqlx::query(
                        "UPDATE item SET scrape_status = 'pending', updated_at = ? WHERE id = ?",
                    )
                    .bind(crate::emby::format_time_now())
                    .bind(id)
                    .execute(self.db.pool())
                    .await;
                }
                ScrapeOutcome::NotFound => {
                    // 业务性无匹配：终态 none，条目保留基础信息照常可见可播
                    stats.none += 1;
                    info!(
                        item_id = id,
                        item_type = %item_type,
                        title = %title,
                        outcome = "none",
                        duration_ms,
                        "TMDB 未找到匹配"
                    );
                    let _ = sqlx::query(
                        "UPDATE item SET scrape_status = 'none', updated_at = ? WHERE id = ?",
                    )
                    .bind(crate::emby::format_time_now())
                    .bind(id)
                    .execute(self.db.pool())
                    .await;
                    if !is_movie {
                        self.cascade_children(id, "none").await;
                    }
                }
                ScrapeOutcome::Failed => {
                    // 网络/API 异常类：attempts+1，未达上限保持 pending 退避重试，
                    // 达上限转 failed 终态（series 同时级联子级）
                    let reached_limit =
                        u64::try_from(attempt_no).unwrap_or(0) + 1 >= self.retry_max_attempts;
                    let next = if reached_limit { "failed" } else { "pending" };
                    let _ = sqlx::query(
                        "UPDATE item SET scrape_attempts = scrape_attempts + 1, \
                         scrape_status = ?, updated_at = ? WHERE id = ?",
                    )
                    .bind(next)
                    .bind(crate::emby::format_time_now())
                    .bind(id)
                    .execute(self.db.pool())
                    .await;
                    stats.failed += 1;
                    warn!(
                        item_id = id,
                        item_type = %item_type,
                        title = %title,
                        outcome = next,
                        attempts = attempt_no + 1,
                        duration_ms,
                        "scrape 异常重试"
                    );
                    if !is_movie && reached_limit {
                        self.cascade_children(id, "failed").await;
                    }
                }
            }
        }
        stats
    }

    /// 取件即置处理中。
    async fn mark_scraping(&self, item_id: i64) {
        let _ =
            sqlx::query("UPDATE item SET scrape_status = 'scraping', updated_at = ? WHERE id = ?")
                .bind(crate::emby::format_time_now())
                .bind(item_id)
                .execute(self.db.pool())
                .await;
    }

    /// 把 series 的直属 season 与 episode 批量置为给定状态（两级，任意方言安全的子查询写法）。
    async fn cascade_children(&self, series_id: i64, status: &str) {
        let now = crate::emby::format_time_now();
        let _ = sqlx::query(
            "UPDATE item SET scrape_status = ?, updated_at = ? \
             WHERE parent_id = ? AND type = 'season'",
        )
        .bind(status)
        .bind(&now)
        .bind(series_id)
        .execute(self.db.pool())
        .await;
        let _ = sqlx::query(
            "UPDATE item SET scrape_status = ?, updated_at = ? \
             WHERE type = 'episode' AND parent_id IN \
             (SELECT id FROM item WHERE parent_id = ? AND type = 'season')",
        )
        .bind(status)
        .bind(&now)
        .bind(series_id)
        .execute(self.db.pool())
        .await;
    }
}
