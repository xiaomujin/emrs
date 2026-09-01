//! HTTP 集成测试：发现层 / 三重前缀 / 认证矩阵 / 登录闭环（sqlite 内存级隔离）。
//!
//! 覆盖验收标准："客户端能发现服务器并出登录页" + 认证矩阵全命中。

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

use emrs_core::cache::MemoryCache;
use emrs_core::config::Config;
use emrs_core::db::Db;
use emrs_server::{AppState, router};

async fn test_state() -> AppState {
    // 独立 sqlite 临时库（原子计数器保证并发唯一；Windows 时钟粒度不够）
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("emrs-http-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("t.db");
    let dsn = format!(
        "sqlite:{}?mode=rwc",
        db_path.to_string_lossy().replace('\\', "/")
    );

    let cfg = Config {
        emby: emrs_core::config::EmbyConfig {
            server_id: "test-server".into(),
            server_name: "emrs-test".into(),
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

    // 测试用户：bcrypt
    let hash = emrs_core::auth::hash_password("pw123").unwrap();
    sqlx::query("INSERT INTO \"user\" (username, password_hash, role) VALUES (?, ?, 'admin')")
        .bind("alice")
        .bind(&hash)
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
            Arc::new(emrs_core::config::Config::default()),
        )),
        http: Arc::new(emrs_core::http_client::HttpClient::none()),
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

// ---------- 公开发现层 ----------

#[tokio::test]
async fn discovery_layer_public() {
    let state = test_state().await;
    let app = router(state);

    // System/Info/Public：匿名 200 + 关键字段
    let res = app
        .clone()
        .oneshot(
            Request::get("/System/Info/Public")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = json_body(res).await;
    assert_eq!(v["Id"], "test-server");
    assert_eq!(v["ServerName"], "emrs-test");
    assert!(v["Version"].as_str().is_some());

    // Ping：文本
    let res = app
        .clone()
        .oneshot(Request::get("/System/Ping").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // Users/Public：空数组
    let res = app
        .clone()
        .oneshot(Request::get("/Users/Public").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let v = json_body(res).await;
    assert!(v.as_array().unwrap().is_empty());

    // /web：HTML stub
    let res = app
        .clone()
        .oneshot(Request::get("/web").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        res.headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );

    // / → /web 重定向（3xx 即可，客户端会跟到 stub）
    let res = app
        .clone()
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert!(
        res.status().is_redirection(),
        "期望 3xx，实际 {}",
        res.status()
    );
    assert_eq!(res.headers().get("location").unwrap(), "/web");

    // /admin：管理后台 HTML 页面（含登录入口与数据 API 引用）
    let res = app
        .oneshot(Request::get("/admin").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        res.headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&bytes);
    assert!(html.contains("emrs · 管理后台"));
    assert!(html.contains("/admin/login"));
}

#[tokio::test]
async fn triple_prefix_mount() {
    let state = test_state().await;
    let app = router(state);

    for prefix in ["", "/emby", "/emby/emby"] {
        let res = app
            .clone()
            .oneshot(
                Request::get(format!("{prefix}/System/Info/Public"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "前缀 {prefix} 失败");
        let v = json_body(res).await;
        assert_eq!(v["Id"], "test-server");
    }
}

// ---------- 认证矩阵 ----------

#[tokio::test]
async fn auth_matrix() {
    let state = test_state().await;
    let app = router(state);

    // 1. 无 token：System/Info 属匿名兼容读 → 200
    let res = app
        .clone()
        .oneshot(Request::get("/System/Info").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // 2. 无 token：匿名兼容读 /Sessions → 200 空数组
    let res = app
        .clone()
        .oneshot(Request::get("/Sessions").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = json_body(res).await;
    assert!(v.as_array().unwrap().is_empty());

    let res = app
        .clone()
        .oneshot(
            Request::get("/emby/Items/Counts")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // 3. master API key → 200
    let res = app
        .clone()
        .oneshot(
            Request::get("/System/Info")
                .header("X-Emby-Token", "master-key-1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = json_body(res).await;
    assert!(v["ServerName"].as_str().is_some());

    // 4. 伪造 token + 非兼容读路径（/Genres）→ 401
    let res = app
        .clone()
        .oneshot(
            Request::get("/Genres")
                .header("X-Emby-Token", "not-exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // 4b. 无 token + 非兼容读路径 → 401
    let res = app
        .clone()
        .oneshot(Request::get("/Genres").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // 5. Bearer 头 + api_key query 均可
    let res = app
        .clone()
        .oneshot(
            Request::get("/System/Info?api_key=master-key-1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn sessions_and_stubs() {
    let state = test_state().await;
    let app = router(state);

    let res = app
        .clone()
        .oneshot(
            Request::get("/Genres")
                .header("X-Emby-Token", "master-key-1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = json_body(res).await;
    assert_eq!(v["TotalRecordCount"], 0);

    let res = app
        .clone()
        .oneshot(
            Request::post("/emby/Sessions/Capabilities/Full")
                .header("X-Emby-Token", "master-key-1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    let res = app
        .oneshot(
            Request::post("/Sessions/Playing/Progress")
                .header("X-Emby-Token", "master-key-1")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
}

// ---------- 登录闭环 ----------

#[tokio::test]
async fn login_roundtrip() {
    let state = test_state().await;
    let app = router(state);

    // 1. 缺 X-Emby-Authorization → 401
    let res = app
        .clone()
        .oneshot(
            Request::post("/Users/AuthenticateByName")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"Username":"alice","Pw":"pw123"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // 2. 正常登录（Pw 大写 + device 头）
    let res = app
        .clone()
        .oneshot(
            Request::post("/emby/Users/AuthenticateByName")
                .header("content-type", "application/json")
                .header(
                    "X-Emby-Authorization",
                    r#"MediaBrowser Client="Infuse", Device="iPhone", DeviceId="dev-1", Version="7.8""#,
                )
                .body(Body::from(r#"{"Username":"alice","Pw":"pw123"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = json_body(res).await;
    let token = v["AccessToken"].as_str().unwrap().to_string();
    assert!(!token.is_empty());
    assert_eq!(v["User"]["Name"], "alice");
    assert_eq!(v["User"]["Policy"]["IsAdministrator"], true);
    assert_eq!(v["SessionInfo"]["DeviceId"], "dev-1");

    // 3. token 访问认证端点
    let res = app
        .clone()
        .oneshot(
            Request::get("/Users/Me")
                .header("X-Emby-Token", &token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = json_body(res).await;
    assert_eq!(v["Name"], "alice");

    // 4. Users/{id}
    let user_id = v["Id"].as_str().unwrap().to_string();
    let res = app
        .clone()
        .oneshot(
            Request::get(format!("/emby/Users/{user_id}"))
                .header("X-Emby-Token", &token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // 5. 错误密码 → 401
    let res = app
        .clone()
        .oneshot(
            Request::post("/Users/AuthenticateByName")
                .header("content-type", "application/json")
                .header(
                    "X-Emby-Authorization",
                    r#"MediaBrowser Client="Infuse", Device="iPhone", DeviceId="dev-1", Version="7.8""#,
                )
                .body(Body::from(r#"{"Username":"alice","Pw":"wrong"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn security_headers_present() {
    let state = test_state().await;
    let app = router(state);
    let res = app
        .oneshot(Request::get("/System/Ping").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let h = res.headers();
    assert_eq!(h.get("x-content-type-options").unwrap(), "nosniff");
    assert_eq!(h.get("x-frame-options").unwrap(), "DENY");
    assert!(h.get("content-security-policy").is_some());
}
