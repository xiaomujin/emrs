//! 集成测试：http 直链 302 播放 + 播放审计。
//!
//! 验收标准："挂载网盘可播放，管理面闭环"——网盘驱动已移除，仅保留 http 直链 302。

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use emrs_core::cache::MemoryCache;
use emrs_core::config::Config;
use emrs_core::db::Db;
use emrs_core::importer::Importer;
use emrs_server::{AppState, router};

const API_KEY: &str = "master-key-1";

async fn test_state() -> AppState {
    let _ = tracing_subscriber::fmt().with_env_filter("info").try_init();
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("emrs-cloud-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("t.db");
    let dsn = format!(
        "sqlite:{}?mode=rwc",
        db_path.to_string_lossy().replace('\\', "/")
    );

    let cfg = Config {
        emby: emrs_core::config::EmbyConfig {
            server_id: "test-server".into(),
            server_name: "emrs-cloud".into(),
            api_key: API_KEY.into(),
        },
        storage: emrs_core::config::StorageConfig {
            dsn,
            max_connections: 4,
        },
        ..Config::default()
    };

    let db = Db::connect(&cfg.storage).await.unwrap();
    db.migrate().await.unwrap();

    // 播种 admin 后台账号
    let admin_hash = emrs_core::auth::hash_password("admin123").unwrap();
    sqlx::query("INSERT INTO \"user\" (username, password_hash, role) VALUES (?, ?, 'admin')")
        .bind("admin")
        .bind(&admin_hash)
        .execute(db.pool())
        .await
        .unwrap();

    let db = Arc::new(db);

    AppState {
        db: db.clone(),
        cache: Arc::new(MemoryCache::new()),
        cfg: Arc::new(cfg.clone()),
        drivers: Arc::new(emrs_core::cloud::DriverRegistry::new(
            db.clone(),
            Arc::new(cfg),
        )),
        proxy: Arc::new(emrs_core::playback::proxy::ProxyClient::new(
            emrs_core::playback::proxy::ProxyConfig::default(),
        )),
        jobs: Arc::new(emrs_core::job::JobManager::new()),
        watcher: Arc::new(emrs_core::watcher::LibraryWatcher::new(db.clone())),
        block_cache: Arc::new(emrs_core::playback::block_cache::BlockCache::new(
            emrs_core::playback::block_cache::BlockCacheConfig::default(),
        )),
        pipeline: Arc::new(emrs_core::importer::pipeline::Pipeline::new(
            db.clone(),
            emrs_core::config::PipelineConfig {
                enabled: false,
                ..Default::default()
            },
            String::new(),
            None,
        )),
        cache_facade: Arc::new(emrs_core::cache::CacheFacade::new(Arc::new(
            emrs_core::cache::TwoTierCache::new(
                Arc::new(emrs_core::cache::MemoryCache::new()),
                None,
            ),
        ))),
    }
}

/// 建一个 STRM 样例库并导入，返回 (app, state)。
async fn import_library(state: &AppState, strm_content: &str, lib_name: &str) {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!("emrs-cloud-lib-{}-{n}", std::process::id()));
    let movies = root.join(lib_name).join("Movies");
    std::fs::create_dir_all(&movies).unwrap();
    std::fs::write(movies.join("Proxy Test (2026).strm"), strm_content).unwrap();

    let importer = Importer::new(state.db.clone());
    let stats = importer.scan(&root.join(lib_name)).await.unwrap();
    assert_eq!(stats.errors, 0, "导入不应有错误");
    assert_eq!(stats.movies, 1, "应导入 1 部电影");
}

fn auth_get(path: &str, token: &str) -> Request<Body> {
    Request::get(path)
        .header("X-Emby-Token", token)
        .body(Body::empty())
        .unwrap()
}

/// 查样例电影的 media uuid。
async fn movie_uuid(state: &AppState) -> String {
    let (uuid,): (String,) = sqlx::query_as("SELECT uuid FROM media_source LIMIT 1")
        .fetch_one(state.db.pool())
        .await
        .unwrap();
    uuid
}

// ---------------------------------------------------------------------------
// 用例 1：http 直链 302 + 缓存
// ---------------------------------------------------------------------------

#[tokio::test]
async fn http_direct_302_with_cache() {
    let state = test_state().await;
    import_library(&state, "https://cdn.example.com/bbb-2008.mp4\n", "LibHttp").await;

    let app = router(state.clone());
    let uuid = movie_uuid(&state).await;
    let path = format!("/Videos/{uuid}/stream.mp4");

    // 第一次播放：307 临时重定向到直链
    let res = app.clone().oneshot(auth_get(&path, API_KEY)).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::TEMPORARY_REDIRECT,
        "http 直链应 307"
    );
    let location = res
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .to_string();
    assert_eq!(location, "https://cdn.example.com/bbb-2008.mp4");

    // 直链缓存命中（cache key 约定 direct:{type}:{url}）
    let cache_key = "direct:url:https://cdn.example.com/bbb-2008.mp4";
    assert_eq!(
        state.cache.get(cache_key).await.as_deref(),
        Some("https://cdn.example.com/bbb-2008.mp4"),
        "直链应写入缓存（TTL 10 分钟）"
    );

    // 第二次播放：缓存命中仍 307
    let res = app.clone().oneshot(auth_get(&path, API_KEY)).await.unwrap();
    assert_eq!(res.status(), StatusCode::TEMPORARY_REDIRECT);
}
