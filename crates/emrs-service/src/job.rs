//! Job 管理器：后台任务（扫描/刮削/探测）的状态表 + tokio::spawn。
//!
//! `DashMap<Uuid, JobState>` 内存态（重启即清）；
//! cancel 为协作式：任务在检查点调 [`JobManager::is_cancelled`]。

use std::time::Duration;

use dashmap::DashMap;
use serde_json::Value;
use uuid::Uuid;

/// 任务状态。
#[derive(Debug, Clone, PartialEq)]
pub enum JobStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// 单个任务的内存态。
#[derive(Debug, Clone)]
struct JobState {
    kind: String,
    status: JobStatus,
    /// 进度描述（如 "已扫描 12 个目录"）。
    progress: String,
    /// 完成时的结果摘要（如 ScanStats）。
    summary: Option<Value>,
    error: Option<String>,
    created_at: String,
    finished_at: Option<String>,
    cancel_requested: bool,
}

/// 对外快照（可序列化）。
#[derive(Debug, Clone)]
pub struct JobView {
    pub id: Uuid,
    pub kind: String,
    pub status: JobStatus,
    pub progress: String,
    pub summary: Option<Value>,
    pub error: Option<String>,
    pub created_at: String,
    pub finished_at: Option<String>,
}

/// Job 管理器。
#[derive(Default)]
pub struct JobManager {
    /// Arc 包装：`DashMap::clone` 是深拷贝，spawn 出去的闭包必须共享同一张表。
    jobs: std::sync::Arc<DashMap<Uuid, JobState>>,
}

fn now_str() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

impl JobManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// 提交后台任务：立即返回 job id，`f` 内部可返回结果摘要。
    pub fn spawn<F, Fut>(&self, kind: &str, f: F) -> Uuid
    where
        F: FnOnce(Uuid) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = anyhow::Result<Value>> + Send + 'static,
    {
        let id = Uuid::new_v4();
        self.jobs.insert(
            id,
            JobState {
                kind: kind.to_string(),
                status: JobStatus::Running,
                progress: String::new(),
                summary: None,
                error: None,
                created_at: now_str(),
                finished_at: None,
                cancel_requested: false,
            },
        );

        let jobs = std::sync::Arc::clone(&self.jobs);
        let id2 = id;
        tokio::spawn(async move {
            let result = f(id2).await;
            if let Some(mut st) = jobs.get_mut(&id2) {
                st.finished_at = Some(now_str());
                match result {
                    Ok(summary) => {
                        st.status = if st.cancel_requested {
                            JobStatus::Cancelled
                        } else {
                            st.summary = Some(summary);
                            JobStatus::Completed
                        };
                    }
                    Err(e) => {
                        st.status = JobStatus::Failed;
                        st.error = Some(format!("{e:#}"));
                    }
                }
            }
        });

        id
    }

    /// 更新进度描述（best-effort）。
    pub fn set_progress(&self, id: &Uuid, msg: impl Into<String>) {
        if let Some(mut st) = self.jobs.get_mut(id) {
            st.progress = msg.into();
        }
    }

    /// 请求取消（协作式，任务在检查点自行退出）。
    pub fn cancel(&self, id: &Uuid) -> bool {
        match self.jobs.get_mut(id) {
            Some(mut st) if st.status == JobStatus::Running => {
                st.cancel_requested = true;
                true
            }
            _ => false,
        }
    }

    /// 任务是否收到取消请求（任务内检查点调用）。
    pub fn is_cancelled(&self, id: &Uuid) -> bool {
        self.jobs
            .get(id)
            .map(|st| st.cancel_requested)
            .unwrap_or(true) // 不存在的任务视为已取消，避免孤儿任务空转
    }

    pub fn get(&self, id: &Uuid) -> Option<JobView> {
        self.jobs.get(id).map(|st| JobView {
            id: *id,
            kind: st.kind.clone(),
            status: st.status.clone(),
            progress: st.progress.clone(),
            summary: st.summary.clone(),
            error: st.error.clone(),
            created_at: st.created_at.clone(),
            finished_at: st.finished_at.clone(),
        })
    }

    /// 按类型列出任务（最近的在前，最多 100 条）。
    pub fn list_by_kind(&self, kind: &str) -> Vec<JobView> {
        let mut out: Vec<JobView> = self
            .jobs
            .iter()
            .filter(|e| e.value().kind == kind)
            .map(|e| JobView {
                id: *e.key(),
                kind: e.value().kind.clone(),
                status: e.value().status.clone(),
                progress: e.value().progress.clone(),
                summary: e.value().summary.clone(),
                error: e.value().error.clone(),
                created_at: e.value().created_at.clone(),
                finished_at: e.value().finished_at.clone(),
            })
            .collect();
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        out.truncate(100);
        out
    }

    /// 清理已结束超过 TTL 的任务（防内存膨胀；调用方低频触发）。
    pub fn gc(&self, ttl: Duration) {
        let cutoff = chrono::Utc::now()
            - chrono::Duration::from_std(ttl).unwrap_or(chrono::Duration::seconds(3600));
        self.jobs
            .retain(|_, st| match (&st.status, &st.finished_at) {
                (JobStatus::Running, _) => true,
                (_, None) => true,
                (_, Some(f)) => chrono::DateTime::parse_from_rfc3339(f)
                    .map(|t| t.with_timezone(&chrono::Utc) > cutoff)
                    .unwrap_or(true),
            });
    }
}

impl JobView {
    pub fn to_json(&self) -> Value {
        let status = match self.status {
            JobStatus::Running => "running",
            JobStatus::Completed => "completed",
            JobStatus::Failed => "failed",
            JobStatus::Cancelled => "cancelled",
        };
        serde_json::json!({
            "id": self.id.to_string(),
            "kind": self.kind,
            "status": status,
            "progress": self.progress,
            "summary": self.summary,
            "error": self.error,
            "created_at": self.created_at,
            "finished_at": self.finished_at,
        })
    }
}

/// 便捷：HashMap 包装（`Uuid → JobView` 序列化）。
pub fn views_to_json(views: &[JobView]) -> Value {
    let items: Vec<Value> = views.iter().map(|v| v.to_json()).collect();
    serde_json::json!({ "items": items })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spawn_completes() {
        let mgr = JobManager::new();
        let id = mgr.spawn("scan", |_| async { Ok(serde_json::json!({ "movies": 3 })) });
        // 等任务结束
        for _ in 0..100 {
            if mgr.get(&id).map(|v| v.status) == Some(JobStatus::Completed) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let v = mgr.get(&id).unwrap();
        assert_eq!(v.status, JobStatus::Completed);
        assert_eq!(v.summary.unwrap()["movies"], 3);
        assert!(v.finished_at.is_some());
    }

    #[tokio::test]
    async fn spawn_failure_recorded() {
        let mgr = JobManager::new();
        let id = mgr.spawn("scan", |_| async { anyhow::bail!("boom") });
        for _ in 0..100 {
            if mgr.get(&id).map(|v| v.status) == Some(JobStatus::Failed) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let v = mgr.get(&id).unwrap();
        assert_eq!(v.status, JobStatus::Failed);
        assert!(v.error.unwrap().contains("boom"));
    }

    #[tokio::test]
    async fn cancel_is_cooperative() {
        let mgr = std::sync::Arc::new(JobManager::new());
        let m2 = mgr.clone();
        let id = mgr.spawn("watch", move |job_id| {
            let m3 = m2.clone();
            async move {
                for _ in 0..200 {
                    if m3.is_cancelled(&job_id) {
                        return Ok(serde_json::json!({ "cancelled": true }));
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                Ok(serde_json::json!({ "done": true }))
            }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(mgr.cancel(&id));
        for _ in 0..200 {
            if mgr.get(&id).map(|v| v.status) == Some(JobStatus::Cancelled) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(mgr.get(&id).unwrap().status, JobStatus::Cancelled);
    }

    #[tokio::test]
    async fn gc_removes_finished() {
        let mgr = JobManager::new();
        let id = mgr.spawn("scan", |_| async { Ok(serde_json::json!({})) });
        for _ in 0..100 {
            if mgr.get(&id).map(|v| v.status) == Some(JobStatus::Completed) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        mgr.gc(Duration::ZERO);
        assert!(mgr.get(&id).is_none());
    }

    #[tokio::test]
    async fn list_by_kind_filters() {
        let mgr = JobManager::new();
        let _a = mgr.spawn("scan", |_| async { Ok(serde_json::json!({})) });
        let _b = mgr.spawn("watch", |_| async { Ok(serde_json::json!({})) });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(mgr.list_by_kind("scan").len(), 1);
        assert_eq!(mgr.list_by_kind("watch").len(), 1);
        assert_eq!(mgr.list_by_kind("other").len(), 0);
    }
}
