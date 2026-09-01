//! 集成测试：后台媒体树单层懒加载端点 `/admin/tree/children`。
//!
//! 直接种入 库 → 剧 → 季 → 集(+media_source) → 电影，逐层验证返回的形状、可展开标志、
//! 集首源路径，以及缺参 400。

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

async fn test_state() -> AppState {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("emrs-media-tree-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("t.db");
    let dsn = format!(
        "sqlite:{}?mode=rwc",
        db_path.to_string_lossy().replace('\\', "/")
    );

    let cfg = Config {
        emby: emrs_core::config::EmbyConfig {
            server_id: "test-server".into(),
            server_name: "emrs-media-tree".into(),
            api_key: "master-key-1".into(),
        },
        storage: emrs_core::config::StorageConfig {
            dsn,
            max_connections: 4,
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
    AppState {
        db: db.clone(),
        cache: Arc::new(MemoryCache::new()),
        cfg: Arc::new(cfg),
        drivers: Arc::new(emrs_core::cloud::DriverRegistry::new(
            db.clone(),
            Arc::new(Config::default()),
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
            emrs_core::http_client::Outbound::none(),
        )),
        cache_facade: Arc::new(emrs_core::cache::CacheFacade::new(Arc::new(
            emrs_core::cache::TwoTierCache::new(Arc::new(MemoryCache::new()), None),
        ))),
    }
}

async fn json_body(res: axum::response::Response) -> Value {
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

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

/// 种一棵完整层级：库 TreeTV → 剧 Demo Show → 季1 → 集1（带首源），另加一部电影。
/// 返回 (library_id, series_id, season_id, episode_id, movie_id)。
async fn seed_hierarchy(state: &AppState) -> (i64, i64, i64, i64, i64) {
    let pool = state.db.pool();
    sqlx::query("INSERT INTO library (name, collection_type) VALUES (?, 'tvshows')")
        .bind("TreeTV")
        .execute(pool)
        .await
        .unwrap();
    let lib: i64 = sqlx::query_scalar("SELECT id FROM library WHERE name = ?")
        .bind("TreeTV")
        .fetch_one(pool)
        .await
        .unwrap();

    let ins_item = |title: &str, ty: &str, parent: Option<i64>| {
        let pool2 = pool.clone();
        let title = title.to_string();
        let ty = ty.to_string();
        async move {
            sqlx::query(
                "INSERT INTO item (type, parent_id, library_id, title, season_number, episode_number) \
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(&ty)
            .bind(parent)
            .bind(lib)
            .bind(&title)
            .bind(if ty == "season" { Some(1i64) } else { None })
            .bind(if ty == "episode" { Some(1i64) } else { None })
            .execute(&pool2)
            .await
            .unwrap();
            sqlx::query_scalar::<_, i64>("SELECT id FROM item WHERE title = ? AND type = ?")
                .bind(&title)
                .bind(&ty)
                .fetch_one(&pool2)
                .await
                .unwrap()
        }
    };

    let series = ins_item("Demo Show", "series", None).await;
    let season = ins_item("Season 1", "season", Some(series)).await;
    let episode = ins_item("Ep 1", "episode", Some(season)).await;
    let movie = ins_item("Demo Movie", "movie", None).await;

    sqlx::query(
        "INSERT INTO media_source (uuid, item_id, name, protocol, path) \
         VALUES (?, ?, ?, 'file', ?)",
    )
    .bind("ms-tree-1")
    .bind(episode)
    .bind("Ep 1.mkv")
    .bind("/media/tree/ep01.mkv")
    .execute(pool)
    .await
    .unwrap();

    (lib, series, season, episode, movie)
}

async fn get_children(state: &AppState, token: &str, query: &str) -> Value {
    let app = router(state.clone());
    let res = app
        .oneshot(
            Request::get(format!("/admin/tree/children{query}"))
                .header("X-Emby-Token", token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "query={query}");
    json_body(res).await
}

fn find(items: &[Value], id: i64) -> &Value {
    items
        .iter()
        .find(|v| v["id"].as_i64() == Some(id))
        .unwrap_or_else(|| panic!("id {id} not found"))
}

#[tokio::test]
async fn tree_children_levels() {
    let state = test_state().await;
    let (lib, series, season, episode, movie) = seed_hierarchy(&state).await;
    let token = admin_login(&state).await;

    // 1. 库顶层：movie + series，series 可展开、movie 叶子。
    let r = get_children(&state, &token, &format!("?library_id={lib}")).await;
    let items = r["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "library children = movie+series");
    assert_eq!(find(items, series)["type"], "series");
    assert_eq!(find(items, series)["has_children"], json!(true));
    // 顶层只含 movie/series，季/集不应出现。
    assert!(!items.iter().any(|v| v["id"].as_i64() == Some(episode)));
    assert!(!items.iter().any(|v| v["id"].as_i64() == Some(season)));

    // 2. 展开 series → 季（可展开）。
    let r = get_children(&state, &token, &format!("?parent_id={series}")).await;
    let items = r["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    let s = find(items, season);
    assert_eq!(s["type"], "season");
    assert_eq!(s["has_children"], json!(true));
    assert_eq!(s["season_number"], json!(1));

    // 3. 展开 season → 集（叶子，带首源路径 + protocol 归一 file→local）。
    let r = get_children(&state, &token, &format!("?parent_id={season}")).await;
    let items = r["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    let e = find(items, episode);
    assert_eq!(e["type"], "episode");
    assert_eq!(e["has_children"], json!(false));
    assert_eq!(e["is_virtual"], json!(false));
    assert_eq!(e["media"]["name"], "Ep 1.mkv");
    assert_eq!(e["media"]["path_type"], "local");
    assert_eq!(e["media"]["path_url"], "/media/tree/ep01.mkv");

    // 4. 展开 movie → 空（无 item 子级）。
    let r = get_children(&state, &token, &format!("?parent_id={movie}")).await;
    assert_eq!(r["items"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn tree_children_requires_param() {
    let state = test_state().await;
    let token = admin_login(&state).await;
    let app = router(state.clone());
    let res = app
        .oneshot(
            Request::get("/admin/tree/children")
                .header("X-Emby-Token", token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}
