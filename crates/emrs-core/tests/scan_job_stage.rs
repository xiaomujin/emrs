//! ScanStage scan_job 生命周期回归测试。
//!
//! 历史 bug：`update_scan_job_status` 的 UPDATE 占位符 7 个而 bind 8 个
//! （`deleted_items` 行漏了 `+ ?`），流水线此前从未消费过 scan_job 所以
//! 未暴露；scan_job 入队化后导致状态永远停留 pending、任务每 tick 重扫。
//! 本测试锁定 create → running → done 全链路。

use std::sync::Arc;

use emrs_core::db::Db;
use emrs_core::importer::stages::ScanStage;

#[tokio::test]
async fn scan_job_lifecycle_transitions() {
    let dir = std::env::temp_dir().join(format!("emrs-sjl-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dsn = format!(
        "sqlite:{}?mode=rwc",
        dir.join("p.db").to_string_lossy().replace('\\', "/")
    );
    let db = Arc::new(
        Db::connect(&emrs_core::config::StorageConfig {
            dsn,
            max_connections: 2,
        })
        .await
        .unwrap(),
    );
    db.migrate().await.unwrap();

    let stage = ScanStage::new(db.clone());
    let job = stage.create_scan_job(1, "probe").await.unwrap();

    // pending 可被消费
    assert_eq!(stage.pending_scan_jobs().await.len(), 1);

    // running：打 started_at
    stage.update_scan_job_status(job, "running", None).await;
    let (status, started): (String, Option<String>) =
        sqlx::query_as("SELECT status, started_at FROM scan_job WHERE id = ?")
            .bind(job)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(status, "running");
    assert!(started.is_some(), "running 应写入 started_at");

    // done：写 finished_at + 统计
    stage
        .update_scan_job_status(
            job,
            "done",
            Some(&emrs_core::importer::scanner::ScanStats {
                media: 3,
                ..Default::default()
            }),
        )
        .await;
    let (status, finished, updated): (String, Option<String>, i64) =
        sqlx::query_as("SELECT status, finished_at, updated_items FROM scan_job WHERE id = ?")
            .bind(job)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(status, "done");
    assert!(finished.is_some(), "done 应写入 finished_at");
    assert_eq!(updated, 3, "media 计数应写入 updated_items");

    // 终态后不再进入消费队列
    assert!(
        stage.pending_scan_jobs().await.is_empty(),
        "done 的 job 不应再被消费"
    );
}
