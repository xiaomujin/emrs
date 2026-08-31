//! 库目录监听：notify（inotify/FSEvents/ReadDirectoryChangesW）→ debounce → 触发扫描。
//!
//! 事件路径匹配已注册的库根（最长前缀命中），对命中的库根**写入
//! `scan_job(pending)`**（经 [`ScanStage::enqueue_library_scan`]，元数据分离后
//! 监听器不做实际扫描——Pipeline 的 scan 循环是唯一消费者），并通过可选的
//! [`Pipeline`] 引用立即唤醒。debounce 窗口（默认 5s）内的连续变更合并为一次触发。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use anyhow::{Context, Result};
use notify::Watcher as _;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::db::Db;
use crate::importer::pipeline::Pipeline;
use crate::importer::stages::ScanStage;

/// debounce 聚合窗口。
const DEBOUNCE: Duration = Duration::from_secs(5);

struct WatchState {
    watcher: Option<notify::RecommendedWatcher>,
    roots: Vec<PathBuf>,
}

/// 库目录监听器（进程内单例语义，由 AppState 持有）。
pub struct LibraryWatcher {
    state: tokio::sync::Mutex<WatchState>,
    db: Arc<Db>,
    /// 可选流水线引用：入队后立即唤醒 scan 循环；缺省依赖轮询间隔自然拾取。
    pipeline: Option<Arc<Pipeline>>,
    /// 会话代数：start/stop 递增，旧事件循环发现代数变化即退出
    /// （防止 stop 后 rx 积压事件再触发扫描）。
    generation: Arc<AtomicU64>,
}

impl LibraryWatcher {
    pub fn new(db: Arc<Db>) -> Self {
        Self {
            state: tokio::sync::Mutex::new(WatchState {
                watcher: None,
                roots: Vec::new(),
            }),
            db,
            pipeline: None,
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    /// 创建监听器并绑定流水线（入队后 notify_scan 立即消费，省一个轮询周期）。
    pub fn with_pipeline(db: Arc<Db>, pipeline: Arc<Pipeline>) -> Self {
        Self {
            state: tokio::sync::Mutex::new(WatchState {
                watcher: None,
                roots: Vec::new(),
            }),
            db,
            pipeline: Some(pipeline),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    /// 开始监听一组库根；返回（成功监听的根, 失败的根及原因）。
    /// 重复调用会先停掉旧监听再重建（合并新根）。
    pub async fn start(&self, roots: Vec<PathBuf>) -> Result<(Vec<String>, Vec<(String, String)>)> {
        // 归一化 + 去重 + 过滤不存在目录
        let mut canonical: Vec<PathBuf> = Vec::new();
        let mut failed: Vec<(String, String)> = Vec::new();
        for r in roots {
            match tokio::fs::canonicalize(&r).await {
                Ok(c) => {
                    if !canonical.contains(&c) {
                        canonical.push(c);
                    }
                }
                Err(e) => failed.push((r.display().to_string(), format!("{e}"))),
            }
        }

        let (tx, mut rx) = mpsc::channel::<PathBuf>(256);

        let mut watcher = notify::recommended_watcher(
            move |res: std::result::Result<notify::Event, notify::Error>| {
                if let Ok(ev) = res {
                    // 只关心文件系统变更路径，忽略错误事件
                    for p in ev.paths {
                        let _ = tx.blocking_send(p);
                    }
                }
            },
        )
        .context("创建文件监听器失败")?;

        for root in &canonical {
            watcher
                .watch(root, notify::RecursiveMode::Recursive)
                .with_context(|| format!("监听 {} 失败", root.display()))?;
        }

        // 先使旧会话失效，再装新 watcher
        let session_gen = self.generation.fetch_add(1, Ordering::SeqCst) + 1;

        let mut st = self.state.lock().await;
        st.watcher = Some(watcher);
        st.roots = canonical.clone();
        drop(st);

        let ok: Vec<String> = canonical.iter().map(|p| p.display().to_string()).collect();

        // 事件循环：debounce 聚合 → 匹配库根 → 入队 scan_job + 唤醒
        let db = self.db.clone();
        let pipeline_hook = self.pipeline.clone();
        let generation = Arc::clone(&self.generation);
        let roots = canonical;
        tokio::spawn(async move {
            let mut pending: HashSet<PathBuf> = HashSet::new();
            let timer = tokio::time::sleep(DEBOUNCE);
            tokio::pin!(timer);

            loop {
                if generation.load(Ordering::SeqCst) != session_gen {
                    break; // 会话已被 stop()/重启替换
                }
                tokio::select! {
                    Some(path) = rx.recv() => {
                        pending.insert(path);
                        // 收到新事件即重置聚合窗口
                        timer.as_mut().reset(tokio::time::Instant::now() + DEBOUNCE);
                    }
                    _ = &mut timer, if !pending.is_empty() => {
                        // 扫描前再查一次代数，防止 stop 与 timer 竞态
                        if generation.load(Ordering::SeqCst) != session_gen {
                            break;
                        }
                        let hits = match_roots(&pending, &roots);
                        pending.clear();
                        let stage = ScanStage::new(db.clone());
                        for root in hits {
                            tracing::info!(root = %root.display(), "watch 触发扫描入队");
                            match stage.enqueue_library_scan(root, "watch").await {
                                Ok(_) => {
                                    if let Some(pl) = &pipeline_hook {
                                        pl.notify_scan();
                                    }
                                }
                                Err(e) => tracing::warn!(error = %e, "watch 扫描入队失败"),
                            }
                        }
                        // 挂起 timer 直到下一个事件
                        timer.as_mut().reset(tokio::time::Instant::now() + DEBOUNCE);
                    }
                    else => break, // rx 关闭（watcher drop）
                }
            }
        });

        Ok((ok, failed))
    }

    /// 停止监听。
    pub async fn stop(&self) {
        // 先递增代数（事件循环立即失效），再 drop watcher
        self.generation.fetch_add(1, Ordering::SeqCst);
        let mut st = self.state.lock().await;
        st.watcher = None; // drop watcher → 事件通道关闭 → 事件循环退出
        st.roots.clear();
    }

    /// 状态快照。
    pub async fn status(&self) -> Value {
        let st = self.state.lock().await;
        json!({
            "running": st.watcher.is_some(),
            "roots": st.roots.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        })
    }
}

/// 事件路径 → 命中的库根集合（最长前缀匹配）。
fn match_roots<'a>(pending: &HashSet<PathBuf>, roots: &'a [PathBuf]) -> Vec<&'a Path> {
    let mut hits: Vec<&Path> = Vec::new();
    for root in roots {
        if pending.iter().any(|p| p.starts_with(root)) {
            hits.push(root);
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn watch_scans_new_strm() {
        // 独立 sqlite 临时库
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("w.db");
        let dsn = format!(
            "sqlite:{}?mode=rwc",
            db_path.to_string_lossy().replace('\\', "/")
        );
        let db = Db::connect(&crate::config::StorageConfig {
            dsn,
            max_connections: 2,
        })
        .await
        .unwrap();
        db.migrate().await.unwrap();
        let db = Arc::new(db);

        // 元数据分离后 watch 只入队 scan_job，扫描消费依赖 Pipeline 循环：
        // poll_interval_secs 取最小生效值 5s（run_scan_loop 内 clamp）
        let pipeline = Arc::new(Pipeline::new(
            db.clone(),
            crate::config::PipelineConfig {
                enabled: true,
                ..Default::default()
            },
            String::new(),
            None,
        ));
        pipeline.start();

        // 库根目录
        let lib = tempfile::tempdir().unwrap();
        let watcher = Arc::new(LibraryWatcher::with_pipeline(db, pipeline));
        let (ok, failed) = watcher.start(vec![lib.path().to_path_buf()]).await.unwrap();
        assert_eq!(ok.len(), 1);
        assert!(failed.is_empty());

        // 写入 STRM → debounce 后入队 → Pipeline scan 消费入库
        let movie_dir = lib.path().join("Watch Movie (2026)");
        tokio::fs::create_dir_all(&movie_dir).await.unwrap();
        tokio::fs::write(
            movie_dir.join("Watch Movie (2026).strm"),
            "http://127.0.0.1:9100/watch-movie.mp4\n",
        )
        .await
        .unwrap();

        // 等 debounce(5s) + scan 轮询(<=5s) + 扫描耗时 + 余量
        tokio::time::sleep(Duration::from_secs(14)).await;

        let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM media_source")
            .fetch_one(watcher.db.pool())
            .await
            .unwrap();
        assert_eq!(count, 1, "watch 触发的 scan_job 应被流水线扫入库");

        let status = watcher.status().await;
        assert_eq!(status["running"], serde_json::json!(true));

        // 停止前记录 watch 触发基线（debounce 窗口外的迟到重复事件也可能合法入队，
        // 断言点应是"stop 后不新增"而非"全程仅一次"）
        let baseline_jobs = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM scan_job WHERE triggered_by = 'watch'",
        )
        .fetch_one(watcher.db.pool())
        .await
        .unwrap();

        // 停止后：事件循环已退出，再写文件不应产生新的 watch 触发
        watcher.stop().await;
        let status = watcher.status().await;
        assert_eq!(status["running"], serde_json::json!(false));

        let movie2 = lib.path().join("After Stop (2026)");
        tokio::fs::create_dir_all(&movie2).await.unwrap();
        tokio::fs::write(
            movie2.join("After Stop (2026).strm"),
            "http://127.0.0.1:9100/after-stop.mp4\n",
        )
        .await
        .unwrap();
        // 超过 debounce 窗口仍无新入队 → 事件循环确已失效
        tokio::time::sleep(Duration::from_secs(8)).await;
        let watch_jobs = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM scan_job WHERE triggered_by = 'watch'",
        )
        .fetch_one(watcher.db.pool())
        .await
        .unwrap();
        assert_eq!(watch_jobs, baseline_jobs, "停止后不应再产生新的 watch 触发");
    }

    #[test]
    fn match_roots_prefix() {
        let roots = [PathBuf::from("/lib")];
        let mut pending = HashSet::new();
        pending.insert(PathBuf::from("/lib/Movies/a.strm"));
        pending.insert(PathBuf::from("/other/b.strm"));
        let hits = match_roots(&pending, &roots);
        assert_eq!(hits.len(), 1);
    }
}
