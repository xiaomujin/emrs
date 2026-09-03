//! 库目录监听：notify（inotify/FSEvents/ReadDirectoryChangesW）→ debounce → 触发扫描。
//!
//! 事件路径匹配已注册的库根（最长前缀命中），对命中的库根**写入
//! `scan_job(pending)`**（经 [`library_store::library_id_for_path`] +
//! [`scan_job_store::create`]，元数据分离后监听器不做实际扫描——Pipeline 的
//! scan 循环是唯一消费生），并通过可选的 [`ScanWaker`]（service 层实现，
//! server 装配注入）立即唤醒。debounce 窗口（默认 5s）内的连续变更合并为一次触发。

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
use crate::stores::{library_store, scan_job_store};
use emrs_core::scan::ScanWaker;

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
    /// 可选唤醒钩子（service 的 Pipeline）：入队后立即唤醒 scan 循环；
    /// 缺省依赖轮询间隔自然拾取。
    waker: Option<Arc<dyn ScanWaker>>,
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
            waker: None,
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    /// 创建监听器并绑定唤醒钩子（入队后立即唤醒，省一个轮询周期）。
    pub fn with_waker(db: Arc<Db>, waker: Arc<dyn ScanWaker>) -> Self {
        Self {
            state: tokio::sync::Mutex::new(WatchState {
                watcher: None,
                roots: Vec::new(),
            }),
            db,
            waker: Some(waker),
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
        let waker = self.waker.clone();
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
                        for root in hits {
                            tracing::info!(root = %root.display(), "watch 触发扫描入队");
                            match enqueue_watch_scan(&db, root).await {
                                Ok(_) => {
                                    if let Some(w) = &waker {
                                        w.wake_scan();
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

/// watch 触发的扫描入队：定位/创建库记录后写 `scan_job(pending)` 行。
/// 原 `ScanStage::enqueue_library_scan`（watch 路径）的 store 侧等价实现——
/// 监听器不构造 Scanner，直接经 store 完成同一动作。
async fn enqueue_watch_scan(db: &Db, root: &Path) -> Result<i64> {
    let library_id = library_store::library_id_for_path(db, root).await?;
    scan_job_store::create(db, library_id, "watch").await
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

// 注：`watch_scans_new_strm` 集成测试（watch 入队 → Pipeline 消费 → media_source
// 落库断言）依赖 service 层 `Pipeline`，随消费方迁至 emrs-service `tests/watch_scan.rs`。
