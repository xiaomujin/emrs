//! watch 触发扫描的端到端集成测试（原 emrs-core watcher.rs 内联测试迁移：
//! 依赖 service 层 `Pipeline` 作为 ScanWaker 实现，随消费方归属本 crate）。

use std::sync::Arc;
use std::time::Duration;

use emrs_core::config::{PipelineConfig, StorageConfig};
use emrs_infra::db::Db;
use emrs_infra::http_client::Outbound;
use emrs_infra::watcher::LibraryWatcher;
use emrs_service::importer::pipeline::Pipeline;

#[tokio::test]
async fn watch_scans_new_strm() {
    // 独立 sqlite 临时库
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("w.db");
    let dsn = format!(
        "sqlite:{}?mode=rwc",
        db_path.to_string_lossy().replace('\\', "/")
    );
    let db = Db::connect(&StorageConfig {
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
        PipelineConfig {
            enabled: true,
            ..Default::default()
        },
        String::new(),
        Outbound::none(),
    ));
    pipeline.start();

    // 库根目录
    let lib = tempfile::tempdir().unwrap();
    let watcher = Arc::new(LibraryWatcher::with_waker(
        db.clone(),
        pipeline.clone() as Arc<dyn emrs_core::scan::ScanWaker>,
    ));
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
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 1, "watch 触发的 scan_job 应被流水线扫入库");

    let status = watcher.status().await;
    assert_eq!(status["running"], serde_json::json!(true));

    // 停止前记录 watch 触发基线（debounce 窗口外的迟到重复事件也可能合法入队，
    // 断言点应是"stop 后不新增"而非"全程仅一次"）
    let baseline_jobs =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM scan_job WHERE triggered_by = 'watch'")
            .fetch_one(db.pool())
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
    let watch_jobs =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM scan_job WHERE triggered_by = 'watch'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(watch_jobs, baseline_jobs, "停止后不应再产生新的 watch 触发");
}
