//! Stage 2: Probe — 轮询 media_source.status='pending'，调 ffprobe 写
//! `media_source.metadata`/`chapters` 后置 'ok'；ffprobe 失败置 'failed'
//! （播放链路不读 status，failed 行照常 DirectPlay；重扫或 admin force 可复位）。
//! 仅处理 protocol='file' 的本地源（http/strm 直链无可探文件）。
//! scanner/mod.rs `probe_media_source` 复用；时长用 probe.rs 纯头部解析。

use std::sync::Arc;

use tokio::task::JoinSet;
use tracing::{debug, warn};

use crate::db::Db;

use crate::importer::scanner::Scanner;

/// 探测阶段：轮询 status='pending' 的 media_source，调 ffprobe 写 `media_source.metadata`。
pub struct ProbeStage {
    db: Arc<Db>,
    /// 单批并发度（默认 2，即同时在飞的 ffprobe 进程数上限）。
    concurrency: usize,
    /// 批次之间让出写锁的毫秒（0 关闭）。见 `run_pending`。
    yield_ms: u64,
}

impl ProbeStage {
    pub fn new(db: Arc<Db>) -> Self {
        Self {
            db,
            concurrency: 2,
            yield_ms: 0,
        }
    }

    /// 指定并发度（pipeline 配置 probe_concurrency）。
    pub fn with_concurrency(db: Arc<Db>, concurrency: usize) -> Self {
        Self {
            db,
            concurrency: concurrency.max(1),
            yield_ms: 0,
        }
    }

    /// 指定并发度 + 批次间让出写锁毫秒（pipeline 配置 probe_yield_ms）。
    pub fn with_concurrency_and_yield(db: Arc<Db>, concurrency: usize, yield_ms: u64) -> Self {
        Self {
            db,
            concurrency: concurrency.max(1),
            yield_ms,
        }
    }

    /// 轮询 pending 的本地媒体源并发探测。返回 (总数, 成功, 失败)。
    pub async fn run_pending(&self, batch_size: i64) -> (usize, usize, usize) {
        let rows = sqlx::query_as::<_, (i64, Option<String>, Option<String>)>(
            "SELECT id, path, remote_path FROM media_source \
         WHERE status = 'pending' AND protocol = 'file' LIMIT ?",
        )
        .bind(batch_size)
        .fetch_all(self.db.pool())
        .await
        .unwrap_or_default();
        if rows.is_empty() {
            return (0, 0, 0);
        }

        let mut total = 0usize;
        let mut ok_count = 0usize;
        let mut fail_count = 0usize;

        // 按 concurrency 分块并发：块内 JoinSet 齐发、块间串行，
        // 在飞 ffprobe 进程数恒 <= concurrency。join 错误（panic）的行仍是
        // pending，下个 tick 自然重试。
        for chunk in rows.chunks(self.concurrency) {
            let mut set = JoinSet::new();
            for (ms_id, path, remote_path) in chunk {
                let ms_id = *ms_id;
                let path = path.clone();
                let remote_path = remote_path.clone();
                let db = self.db.clone();
                set.spawn(async move {
                    let Some(p) = path.or(remote_path) else {
                        // 无本地路径的 file 源属于脏数据：直接判失败，避免滞留 pending
                        warn!(media_source_id = ms_id, "file 协议源缺少 path，标记 failed");
                        let _ = sqlx::query(
                            "UPDATE media_source SET status = 'failed', updated_at = ? WHERE id = ?",
                        )
                        .bind(crate::emby::format_time_now())
                        .bind(ms_id)
                        .execute(db.pool())
                        .await;
                        return (ms_id, false);
                    };
                    // probe 不触 TMDB，Scanner key 恒空
                    let scanner = Scanner::new(db, String::new());
                    let ok = scanner.probe_media_source(ms_id, &p).await;
                    (ms_id, ok)
                });
            }
            while let Some(res) = set.join_next().await {
                total += 1;
                match res {
                    Ok((_, true)) => ok_count += 1,
                    Ok((_, false)) => fail_count += 1,
                    Err(e) => warn!(error = %e, "probe 任务异常"),
                }
            }

            // 批次之间主动 checkpoint + 让出写锁：探测期间每 chunk 会连续
            // UPDATE media_source，让认证/HTTP 读有机会插队（yield_ms=0 时跳过）。
            if self.yield_ms > 0 {
                let _ = self.db.checkpoint_truncate().await;
                tokio::time::sleep(std::time::Duration::from_millis(self.yield_ms)).await;
            }
        }

        if total > 0 {
            debug!(total, ok = ok_count, failed = fail_count, "probe 批次完成");
        }
        (total, ok_count, fail_count)
    }
}
