//! 集成测试：扫描 job 化轮询 + 目录监听闭环。
//!
//! 验收标准："扫描任务异步化、watch 增量入库"。

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use emrs_core::cache::MemoryCache;
use emrs_core::config::Config;
use emrs_core::db::Db;
use emrs_server::{AppState, router};

const API_KEY: &str = "master-key-1";

async fn test_state() -> AppState {
    let _ = tracing_subscriber::fmt().with_env_filter("info").try_init();
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("emrs-scan-jobs-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("t.db");
    let dsn = format!(
        "sqlite:{}?mode=rwc",
        db_path.to_string_lossy().replace('\\', "/")
    );

    let cfg = Config {
        emby: emrs_core::config::EmbyConfig {
            server_id: "test-server".into(),
            server_name: "emrs-scan-jobs".into(),
            api_key: API_KEY.into(),
        },
        storage: emrs_core::config::StorageConfig {
            dsn,
            max_connections: 4,
        },
        playback: emrs_core::config::PlaybackConfig {
            signing_key: Some("test-signing-key".into()),
            ..Default::default()
        },
        ..Config::default()
    };

    let db = Db::connect(&cfg.storage).await.unwrap();
    db.migrate().await.unwrap();

    let admin_hash = emrs_core::auth::hash_password("admin123").unwrap();
    sqlx::query("INSERT INTO \"user\" (username, password_hash, role) VALUES (?, ?, 'admin')")
        .bind("admin")
        .bind(&admin_hash)
        .execute(db.pool())
        .await
        .unwrap();

    let db = Arc::new(db);

    // 元数据分离后 scan/start 只入队 scan_job，扫描消费依赖流水线——测试内启动
    let pipeline = Arc::new(emrs_core::importer::pipeline::Pipeline::new(
        db.clone(),
        emrs_core::config::PipelineConfig::default(),
        String::new(),
        emrs_core::http_client::Outbound::none(),
    ));
    pipeline.start();

    AppState {
        db: db.clone(),
        cache: Arc::new(MemoryCache::new()),
        cfg: Arc::new(cfg.clone()),
        drivers: Arc::new(emrs_core::cloud::DriverRegistry::new(
            db.clone(),
            Arc::new(cfg),
        )),
        http: Arc::new(emrs_core::http_client::HttpClient::none()),
        jobs: Arc::new(emrs_core::job::JobManager::new()),
        watcher: Arc::new(emrs_core::watcher::LibraryWatcher::new(db.clone())),
        block_cache: Arc::new(emrs_core::playback::block_cache::BlockCache::new(
            emrs_core::playback::block_cache::BlockCacheConfig::default(),
        )),
        pipeline,
        cache_facade: Arc::new(emrs_core::cache::CacheFacade::new(Arc::new(
            emrs_core::cache::TwoTierCache::new(
                Arc::new(emrs_core::cache::MemoryCache::new()),
                None,
            ),
        ))),
    }
}

async fn json_body(res: axum::response::Response) -> Value {
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn auth(req: Request<Body>, token: &str) -> Request<Body> {
    let mut r = req;
    r.headers_mut()
        .insert("X-Emby-Token", token.parse().unwrap());
    r
}

/// admin 登录拿 token。
async fn admin_login(state: &AppState) -> String {
    let app = router(state.clone());
    let res = app
        .oneshot(
            Request::post("/admin/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "username": "admin", "password": "admin123" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    json_body(res).await["token"].as_str().unwrap().to_string()
}

/// 建 STRM 样例库（含一部电影），返回库根路径。
fn make_library(strm_content: &str) -> std::path::PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!("emrs-scan-jobs-lib-{}-{n}", std::process::id()));
    let movies = root.join("Movies");
    std::fs::create_dir_all(&movies).unwrap();
    std::fs::write(movies.join("Scan Movie (2026).strm"), strm_content).unwrap();
    root
}

// ---------------------------------------------------------------------------
// 1. 扫描 job 化：start → 轮询 completed → summary 有统计
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scan_job_lifecycle() {
    let state = test_state().await;
    let token = admin_login(&state).await;
    let lib = make_library("http://127.0.0.1:9100/scan-movie.mp4");

    let app = router(state.clone());

    // 建库（入库 library 表）
    let res = app
        .clone()
        .oneshot(auth(
            Request::post("/admin/libraries")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "name": "扫描任务测试库", "paths": [lib.display().to_string()] })
                        .to_string(),
                ))
                .unwrap(),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // 启动异步扫描
    let res = app
        .clone()
        .oneshot(auth(
            Request::post("/admin/library/scan/start")
                .body(Body::empty())
                .unwrap(),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = json_body(res).await;
    let job_id = body["job_id"].as_str().unwrap().to_string();

    // 轮询直到 completed（最多 15s）
    let mut final_body = Value::Null;
    for _ in 0..150 {
        let res = app
            .clone()
            .oneshot(auth(
                Request::get(format!("/admin/library/scan/{job_id}"))
                    .body(Body::empty())
                    .unwrap(),
                &token,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let b = json_body(res).await;
        if b["status"] == "completed" || b["status"] == "failed" {
            final_body = b;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(
        final_body["status"], "completed",
        "扫描 job 应完成：{final_body}"
    );
    assert_eq!(final_body["summary"]["media"], 1, "应扫到 1 个媒体");

    // 不存在的 job → 404
    let res = app
        .oneshot(auth(
            Request::get("/admin/library/scan/00000000-0000-0000-0000-000000000000")
                .body(Body::empty())
                .unwrap(),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    let _ = std::fs::remove_dir_all(&lib);
}

// ---------------------------------------------------------------------------
// 2. watch 端点闭环：start → status(running) → 停止 → status(stopped)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn watch_endpoint_lifecycle() {
    let state = test_state().await;
    let token = admin_login(&state).await;
    let lib = make_library("http://127.0.0.1:9100/watch-movie.mp4");

    let app = router(state.clone());

    // 初始未运行
    let res = app
        .clone()
        .oneshot(auth(
            Request::get("/admin/library/watch")
                .body(Body::empty())
                .unwrap(),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(json_body(res).await["running"], json!(false));

    // 启动（指定 roots）
    let res = app
        .clone()
        .oneshot(auth(
            Request::post("/admin/library/watch/start")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "roots": [lib.display().to_string()] }).to_string(),
                ))
                .unwrap(),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = json_body(res).await;
    assert_eq!(body["watching"].as_array().unwrap().len(), 1, "{body}");
    assert!(body["failed"].as_array().unwrap().is_empty());

    // 运行中
    let res = app
        .clone()
        .oneshot(auth(
            Request::get("/admin/library/watch")
                .body(Body::empty())
                .unwrap(),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(json_body(res).await["running"], json!(true));

    // 停止
    let res = app
        .clone()
        .oneshot(auth(
            Request::delete("/admin/library/watch")
                .body(Body::empty())
                .unwrap(),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    let res = app
        .oneshot(auth(
            Request::get("/admin/library/watch")
                .body(Body::empty())
                .unwrap(),
            &token,
        ))
        .await
        .unwrap();
    let st = json_body(res).await;
    assert_eq!(st["running"], json!(false), "停止后 running=false");

    let _ = std::fs::remove_dir_all(&lib);
}

// ---------------------------------------------------------------------------
// 3. 元数据刮削 job：start → 轮询 completed → 未配置 TMDB key 时全部 skipped
// ---------------------------------------------------------------------------

/// 轮询 job 直到 completed / failed，返回最终响应体。
async fn poll_job_until(app: &axum::Router, token: &str, path: &str) -> Value {
    let mut final_body = Value::Null;
    for _ in 0..150 {
        let res = app
            .clone()
            .oneshot(auth(Request::get(path).body(Body::empty()).unwrap(), token))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "轮询 {path} 应 200");
        let b = json_body(res).await;
        if b["status"] == "completed" || b["status"] == "failed" {
            final_body = b;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    final_body
}

#[tokio::test]
async fn scrape_job_lifecycle_without_key() {
    let state = test_state().await;
    let token = admin_login(&state).await;
    let lib = make_library("http://127.0.0.1:9100/scrape-movie.mp4");

    let app = router(state.clone());

    // 建库并扫描，产生 1 部无 tmdb_id 的电影（未配置 key，不刮削）
    let res = app
        .clone()
        .oneshot(auth(
            Request::post("/admin/libraries")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "name": "刮削测试库", "paths": [lib.display().to_string()] })
                        .to_string(),
                ))
                .unwrap(),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let lib_id = json_body(res).await["id"].as_i64().unwrap();

    let res = app
        .clone()
        .oneshot(auth(
            Request::post("/admin/library/scan/start")
                .body(Body::empty())
                .unwrap(),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let scan_job = json_body(res).await["job_id"].as_str().unwrap().to_string();
    let fin = poll_job_until(&app, &token, &format!("/admin/library/scan/{scan_job}")).await;
    assert_eq!(fin["status"], "completed", "前置扫描应完成：{fin}");

    // 启动刮削（指定库，未配置 tmdb key → 全部 skipped）
    let res = app
        .clone()
        .oneshot(auth(
            Request::post("/admin/library/scrape/start")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "library_id": lib_id, "force": false }).to_string(),
                ))
                .unwrap(),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = json_body(res).await;
    assert_eq!(body["kind"], "scrape");
    let job_id = body["job_id"].as_str().unwrap().to_string();

    // 轮询完成
    let fin = poll_job_until(&app, &token, &format!("/admin/library/scrape/{job_id}")).await;
    assert_eq!(fin["status"], "completed", "刮削 job 应完成：{fin}");
    assert_eq!(fin["summary"]["tmdb_configured"], json!(false));
    assert_eq!(fin["summary"]["scraped"], 0);
    assert_eq!(
        fin["summary"]["skipped"], 1,
        "未配置 key 时 1 部电影应计 skipped"
    );
    assert_eq!(fin["summary"]["failed"], 0);

    // 不存在的库 → 无候选 → skipped 0
    let res = app
        .clone()
        .oneshot(auth(
            Request::post("/admin/library/scrape/start")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "library_id": 99999 }).to_string()))
                .unwrap(),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let job_id2 = json_body(res).await["job_id"].as_str().unwrap().to_string();
    let fin2 = poll_job_until(&app, &token, &format!("/admin/library/scrape/{job_id2}")).await;
    assert_eq!(fin2["summary"]["skipped"], 0, "不存在库应无候选：{fin2}");

    // 不存在的 job → 404
    let res = app
        .oneshot(auth(
            Request::get("/admin/library/scrape/00000000-0000-0000-0000-000000000000")
                .body(Body::empty())
                .unwrap(),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    let _ = std::fs::remove_dir_all(&lib);
}
