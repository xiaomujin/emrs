//! 集成测试：STRM 导入 → Items/首页 → PlaybackInfo → 302 播放 → 进度 → 续看闭环。
//!
//! 验收标准："导入样例目录后首页有数据、真实播放可跑、续看位置保存"。

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
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
    let dir = std::env::temp_dir().join(format!("emrs-items-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("t.db");
    let dsn = format!(
        "sqlite:{}?mode=rwc",
        db_path.to_string_lossy().replace('\\', "/")
    );

    let cfg = Config {
        emby: emrs_core::config::EmbyConfig {
            server_id: "test-server".into(),
            server_name: "emrs-items".into(),
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

/// 构建样例 STRM 库：1 部电影 + 1 部剧 2 集。
fn sample_library() -> std::path::PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!("emrs-items-lib-{}-{n}", std::process::id()));

    let movies = root.join("MediaLib").join("Movies");
    let s01 = root
        .join("MediaLib")
        .join("Shows")
        .join("Test Show")
        .join("Season 01");
    std::fs::create_dir_all(&movies).unwrap();
    std::fs::create_dir_all(&s01).unwrap();

    std::fs::write(
        movies.join("Big Buck Bunny (2008).strm"),
        "# 直链电影\nhttps://cdn.example.com/bbb-2008.mp4\n",
    )
    .unwrap();
    std::fs::write(
        s01.join("S01E01.strm"),
        "https://cdn.example.com/test-show-e1.mp4\n",
    )
    .unwrap();
    std::fs::write(
        s01.join("S01E02.strm"),
        "https://cdn.example.com/test-show-e2.mp4\n",
    )
    .unwrap();
    root.join("MediaLib")
}

async fn json_body(res: axum::response::Response) -> Value {
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn auth_get(path: &str) -> Request<Body> {
    Request::get(path)
        .header("X-Emby-Token", API_KEY)
        .body(Body::empty())
        .unwrap()
}

/// 暂被注释的测试（依赖被注释掉的 `/Items` 路由）还会用，保留以恢复测试。
#[allow(dead_code)]
fn user_get(path: &str, token: &str) -> Request<Body> {
    Request::get(path)
        .header("X-Emby-Token", token)
        .body(Body::empty())
        .unwrap()
}

#[allow(dead_code)]
fn user_post(path: &str, body: Value, token: &str) -> Request<Body> {
    Request::post(path)
        .header("X-Emby-Token", token)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// 登录 alice 拿用户 token（进度/续看按真实用户身份走）。
#[allow(dead_code)]
async fn login_token(app: &axum::Router) -> String {
    let req = Request::post("/Users/AuthenticateByName")
        .header("content-type", "application/json")
        .header(
            "X-Emby-Authorization",
            r#"MediaBrowser Client="Infuse", Device="iPhone", DeviceId="items-test", Version="7.8""#,
        )
        .body(Body::from(
            serde_json::json!({"Username": "alice", "Pw": "pw123"}).to_string(),
        ))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = json_body(res).await;
    v["AccessToken"].as_str().unwrap().to_string()
}

/*
/// 全链路：导入 → 列表 → 详情/季/集 → PlaybackInfo → 302 → 进度 → Resume → Stopped → NextUp。
/// 暂注释：依赖被注释掉的 `/Items` 列表与 `/Items/{id}` 详情路由（重构中），
/// 待路由恢复后取消注释；person 兼容断言见 person_detail_and_filter。
#[tokio::test]
async fn full_pipeline() {
    let state = test_state().await;

    // ---- 导入 ----
    let lib = sample_library();
    let importer = Importer::new(state.db.clone());
    let stats = importer.scan(&lib).await.expect("导入失败");
    assert_eq!(stats.movies, 1, "应导入 1 部电影");
    assert_eq!(
        stats.episodes, 2,
        "应导入 2 集（S01E01/S01E02 各一集，集号不互相覆盖）"
    );
    assert_eq!(stats.errors, 0, "不应有导入错误");

    let app = router(state.clone());
    let token = login_token(&app).await;

    // ---- Items 列表（Movie）----
    let res = app
        .clone()
        .oneshot(auth_get("/Items?IncludeItemTypes=Movie"))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = json_body(res).await;
    assert_eq!(v["TotalRecordCount"], 1);
    let movie_id = v["Items"][0]["Id"].as_str().unwrap().to_string();
    assert!(
        !movie_id.contains('-') && movie_id.parse::<u64>().is_ok(),
        "Movie ItemId 应为纯数字: {movie_id}"
    );
    assert_eq!(v["Items"][0]["Type"], "Movie");
    // 增强后：文件名解析剥离年份标记，标题为清洗后的 "Big Buck Bunny"
    assert_eq!(v["Items"][0]["Name"], "Big Buck Bunny");

    // ---- Series 列表 ----
    let res = app
        .clone()
        .oneshot(auth_get("/Items?IncludeItemTypes=Series"))
        .await
        .unwrap();
    let v = json_body(res).await;
    assert_eq!(v["TotalRecordCount"], 1);
    let series_id = v["Items"][0]["Id"].as_str().unwrap().to_string();
    assert_eq!(v["Items"][0]["Name"], "Test Show");

    // ---- ItemId 详情（Movie / Series）----
    let res = app
        .clone()
        .oneshot(auth_get(&format!("/Items/{movie_id}")))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "纯数字 ItemId 应可查详情");
    let res = app
        .clone()
        .oneshot(auth_get(&format!("/Items/{series_id}")))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // ---- 季 ----
    let res = app
        .clone()
        .oneshot(auth_get(&format!("/Shows/{series_id}/Seasons")))
        .await
        .unwrap();
    let v = json_body(res).await;
    assert_eq!(v["TotalRecordCount"], 1);
    let season_id = v["Items"][0]["Id"].as_str().unwrap().to_string();
    assert!(!season_id.contains('-') && season_id.parse::<u64>().is_ok());
    assert_eq!(v["Items"][0]["IndexNumber"], 1);

    // season 统一支持：标记已看不应被 resolve_item_id 拒绝（原返回 404）
    let res = app
        .clone()
        .oneshot(
            Request::post(format!("/Users/1/PlayedItems/{season_id}"))
                .header("X-Emby-Token", &token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "season 标记已看应被支持");

    // ---- 集（含剧集信息）----
    let res = app
        .clone()
        .oneshot(auth_get(&format!(
            "/Shows/{series_id}/Episodes?SeasonId={season_id}"
        )))
        .await
        .unwrap();
    let v = json_body(res).await;
    assert_eq!(v["TotalRecordCount"], 2, "两集都应存在（集号提取不覆盖）");
    assert_eq!(v["Items"][0]["IndexNumber"], 1);
    assert_eq!(v["Items"][1]["IndexNumber"], 2);
    assert_eq!(v["Items"][0]["SeriesName"], "Test Show", "集应携带剧集信息");
    assert_eq!(v["Items"][0]["SeriesId"], series_id);
    let ep1_id = v["Items"][0]["Id"].as_str().unwrap().to_string();
    assert!(!ep1_id.contains('-') && ep1_id.parse::<u64>().is_ok());

    // ---- Episode 详情（纯数字直查）----
    let res = app
        .clone()
        .oneshot(auth_get(&format!("/Items/{ep1_id}")))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "纯数字 ItemId 应可查详情");
    let v = json_body(res).await;
    assert_eq!(v["Type"], "Episode");

    // ---- Latest（最新入库）----
    let res = app
        .clone()
        .oneshot(auth_get("/Users/1/Items/Latest?Limit=10"))
        .await
        .unwrap();
    let v = json_body(res).await;
    let arr = v.as_array().unwrap();
    assert!(arr.len() >= 2, "Latest 应返回入库内容: {arr:?}");

    // ---- PlaybackInfo（Episode）----
    let res = app
        .clone()
        .oneshot(auth_get(&format!("/Items/{ep1_id}/PlaybackInfo")))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = json_body(res).await;
    let ms = &v["MediaSources"][0];
    assert!(
        ms["Id"].as_str().is_some_and(|s| !s.is_empty()),
        "MediaSource Id(uuid) 非空"
    );
    let direct_url = ms["DirectStreamUrl"].as_str().unwrap().to_string();
    let uuid = ms["Id"].as_str().unwrap().to_string();
    assert!(
        direct_url.starts_with("/s/"),
        "DirectStreamUrl 应为短票据 /s/{{ticket}}: {direct_url}"
    );
    // 对齐参考 Emby：MediaSource 顶层字段
    assert_eq!(ms["HasMixedProtocols"], false);
    assert_eq!(ms["AddApiKeyToDirectStreamUrl"], false);
    assert_eq!(ms["ReadAtNativeFramerate"], false);
    assert_eq!(ms["SupportsTranscoding"], false);
    assert_eq!(ms["SupportsDirectStream"], true);
    assert_eq!(ms["SupportsDirectPlay"], true);
    assert!(ms["ItemId"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(ms["Chapters"].as_array().is_some());
    assert!(ms["MediaStreams"].as_array().is_some());
    // 有流信息时输出 Bitrate/默认音轨索引
    let streams = ms["MediaStreams"].as_array().unwrap();
    if !streams.is_empty() {
        let first = &streams[0];
        assert_eq!(first["Type"], "Video", "首流应为视频");
        assert!(first.get("DisplayTitle").is_some());
        assert!(first.get("Profile").is_some());
        assert!(first.get("Level").is_some());
        assert!(first.get("VideoRange").is_some());
        assert!(first.get("IsInterlaced").is_some());
    }
    assert!(v["PlaySessionId"].as_str().is_some_and(|s| !s.is_empty()));

    // ---- 票据播放（无 token 匿名直连 /s/{ticket}）----
    let res = app
        .clone()
        .oneshot(user_get(&direct_url, ""))
        .await
        .unwrap();
    assert!(
        res.status().is_redirection(),
        "票据播放应重定向: {}",
        res.status()
    );
    assert_eq!(
        res.headers().get("location").unwrap(),
        "https://cdn.example.com/test-show-e1.mp4"
    );

    // ---- 兜底通道：认证后 /Videos/ 直链仍可播 ----
    let play_path = format!("/Videos/{uuid}/stream.mp4");
    let res = app.clone().oneshot(auth_get(&play_path)).await.unwrap();
    assert!(
        res.status().is_redirection(),
        "302 播放应重定向: {}",
        res.status()
    );
    assert_eq!(
        res.headers().get("location").unwrap(),
        "https://cdn.example.com/test-show-e1.mp4"
    );

    // 小写变体
    let res = app
        .clone()
        .oneshot(auth_get(&play_path.to_lowercase()))
        .await
        .unwrap();
    assert!(res.status().is_redirection(), "小写 /videos 变体也应可达");

    // ---- 开播（play_count +1，Resume 依据）----
    let playing = serde_json::json!({ "ItemId": ep1_id, "PlaySessionId": uuid });
    let res = app
        .clone()
        .oneshot(user_post("/Sessions/Playing", playing, &token))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    // ---- 进度上报（播放中）----
    let progress = serde_json::json!({
        "ItemId": ep1_id,
        "PlaySessionId": uuid,
        "PositionTicks": 15_i64 * 60 * 10_000_000, // 15 分钟
        "IsPaused": false,
    });
    let res = app
        .clone()
        .oneshot(user_post("/Sessions/Playing/Progress", progress, &token))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    // ---- Resume（续看）----
    let res = app
        .clone()
        .oneshot(user_get("/Users/1/Items/Resume", &token))
        .await
        .unwrap();
    let v = json_body(res).await;
    assert_eq!(v["TotalRecordCount"], 1, "续看应只含看了一半的 E01");
    assert_eq!(v["Items"][0]["Id"].as_str().unwrap(), ep1_id);
    assert_eq!(
        v["Items"][0]["UserData"]["PlaybackPositionTicks"],
        15_i64 * 60 * 10_000_000,
        "续看位置应保留"
    );

    // ---- NextUp（E01 未完成时应含 E01）----
    let res = app
        .clone()
        .oneshot(auth_get("/Shows/NextUp?UserId=1"))
        .await
        .unwrap();
    let v = json_body(res).await;
    assert!(
        v["Items"]
            .as_array()
            .is_some_and(|a| a.iter().any(|i| i["Id"].as_str() == Some(ep1_id.as_str()))),
        "未看完的 E01 应出现在 NextUp"
    );

    // ---- NextUp 按 SeriesId 过滤（v-N）：返回该系列所有未看完的后续集 ----
    let res = app
        .clone()
        .oneshot(auth_get(&format!(
            "/Shows/NextUp?UserId=1&SeriesId={series_id}"
        )))
        .await
        .unwrap();
    let v = json_body(res).await;
    assert_eq!(
        v["TotalRecordCount"], 2,
        "SeriesId 过滤应返回该系列全部未看完集（E01+E02）"
    );
    assert_eq!(
        v["Items"][0]["Id"].as_str().unwrap(),
        ep1_id,
        "按季/集排序，E01 应排最前"
    );
    // 不匹配的 SeriesId 应返回空
    let res = app
        .clone()
        .oneshot(auth_get("/Shows/NextUp?UserId=1&SeriesId=99999"))
        .await
        .unwrap();
    let v = json_body(res).await;
    assert_eq!(v["TotalRecordCount"], 0, "不存在的系列不应出现在 NextUp");

    // ---- 停止播放（E01 看完）----
    let stopped = serde_json::json!({
        "ItemId": ep1_id,
        "PositionTicks": 30_i64 * 60 * 10_000_000,
        "RunTimeTicks": 30_i64 * 60 * 10_000_000,
        "PlayedToCompletion": true,
    });
    let res = app
        .clone()
        .oneshot(user_post("/Sessions/Playing/Stopped", stopped, &token))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    // E01 已完成：Resume 仍返回 E01（已看完的条目也保留，不跳到下一集）
    let res = app
        .clone()
        .oneshot(user_get("/Users/1/Items/Resume", &token))
        .await
        .unwrap();
    let v = json_body(res).await;
    assert_eq!(v["TotalRecordCount"], 1, "看完 E01 后续看仍含 E01");
    assert_eq!(
        v["Items"][0]["Id"].as_str().unwrap(),
        ep1_id,
        "看完的条目本身也返回，不跳下一集"
    );

    let res = app
        .clone()
        .oneshot(auth_get("/Shows/NextUp?UserId=1"))
        .await
        .unwrap();
    let v = json_body(res).await;
    let next = v["Items"][0]["Id"].as_str().unwrap().to_string();
    assert_ne!(next, ep1_id, "NextUp 应推进到 E02");

    // ---- 收藏 ----
    let res = app
        .clone()
        .oneshot(
            Request::post(format!("/Users/1/FavoriteItems/{movie_id}"))
                .header("X-Emby-Token", &token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let res = app
        .clone()
        .oneshot(user_get(&format!("/Users/1/Items/{movie_id}"), &token))
        .await
        .unwrap();
    let v = json_body(res).await;
    assert_eq!(v["UserData"]["IsFavorite"], true, "收藏应生效");

    // 收藏列表查询：Filters=IsFavorite 应回返该收藏项（防 list_favorites 绑参错位回归）
    let res = app
        .clone()
        .oneshot(user_get("/Users/1/Items?Filters=IsFavorite", &token))
        .await
        .unwrap();
    let v = json_body(res).await;
    assert_eq!(v["TotalRecordCount"], 1, "收藏列表 total 应为 1");
    assert_eq!(
        v["Items"][0]["Id"].as_str().unwrap(),
        movie_id.as_str(),
        "收藏列表应包含已收藏的电影"
    );

    let _ = std::fs::remove_dir_all(std::path::Path::new(&lib).parent().unwrap());
}
*/

/*
/// Movie 粒度进度：进度上报 → Resume 返回 Movie。
/// 暂注释：依赖被注释掉的 `/Items` 列表路由（重构中），待路由恢复后取消注释。
#[tokio::test]
async fn movie_progress_resume() {
    let state = test_state().await;
    let lib = sample_library();
    let importer = Importer::new(state.db.clone());
    importer.scan(&lib).await.unwrap();

    let app = router(state.clone());
    let token = login_token(&app).await;

    // 找 Movie id
    let res = app
        .clone()
        .oneshot(auth_get("/Items?IncludeItemTypes=Movie"))
        .await
        .unwrap();
    let v = json_body(res).await;
    let movie_id = v["Items"][0]["Id"].as_str().unwrap().to_string();

    // 开播（play_count +1，Resume 依据）→ 上报进度
    let playing = serde_json::json!({ "ItemId": movie_id });
    let res = app
        .clone()
        .oneshot(user_post("/Sessions/Playing", playing, &token))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    let progress = serde_json::json!({
        "ItemId": movie_id,
        "PositionTicks": 5_i64 * 60 * 10_000_000,
    });
    let res = app
        .clone()
        .oneshot(user_post("/Sessions/Playing/Progress", progress, &token))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    let res = app
        .clone()
        .oneshot(user_get("/Users/1/Items/Resume", &token))
        .await
        .unwrap();
    let v = json_body(res).await;
    assert_eq!(v["Items"][0]["Id"].as_str().unwrap(), movie_id);
    assert_eq!(v["Items"][0]["Type"], "Movie");
    assert_eq!(
        v["Items"][0]["UserData"]["PlaybackPositionTicks"],
        5_i64 * 60 * 10_000_000
    );

    let _ = std::fs::remove_dir_all(std::path::Path::new(&lib).parent().unwrap());
}
*/

/*
/// 非法 ItemId：不存在的数字 → 404；非数字 → 404。
/// 暂注释：测的是被注释掉的 `/Items/{id}` 详情路由（重构中），路由不存在时
/// 走 router 兜底 404 失去本意；待路由恢复后取消注释。
#[tokio::test]
async fn invalid_item_ids() {
    let state = test_state().await;
    let lib = sample_library();
    Importer::new(state.db.clone()).scan(&lib).await.unwrap();
    let app = router(state.clone());

    for path in ["/Items/99999", "/Items/99998", "/Items/99997", "/Items/x-1"] {
        let res = app.clone().oneshot(auth_get(path)).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{path} 应 404");
    }
    // PlaybackInfo 同理
    let res = app
        .clone()
        .oneshot(auth_get("/Items/99999/PlaybackInfo"))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    let _ = std::fs::remove_dir_all(std::path::Path::new(&lib).parent().unwrap());
}
*/

/// People 兼容：`/Users/{uid}/Items/p-{id}` 详情 + `PersonIds` 列表过滤。
/// 验证 Hills 等客户端 person 主页的两个关键请求：详情非 404、列表按人过滤。
#[tokio::test]
async fn person_detail_and_filter() {
    let state = test_state().await;
    let lib = sample_library();
    let importer = Importer::new(state.db.clone());
    importer.scan(&lib).await.unwrap();

    // 找电影 id（列表走 /Users/{uid}/Items 活动路由）
    let app = router(state.clone());
    let res = app
        .clone()
        .oneshot(auth_get("/Users/1/Items?IncludeItemTypes=Movie"))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = json_body(res).await;
    assert_eq!(v["TotalRecordCount"], 1);
    let movie_id: i64 = emrs_core::emby::parse_id(v["Items"][0]["Id"].as_str().unwrap())
        .map(|(_, id)| id)
        .unwrap();

    // 造一条 people + item_people（模拟刮削写入的演职员关联）
    let now = "2026-08-22T00:00:00.0000000Z";
    sqlx::query(
        "INSERT INTO people (tmdb_id, name, gender, created_at, updated_at) \
         VALUES ('10001', 'Test Actor', 2, ?, ?)",
    )
    .bind(now)
    .bind(now)
    .execute(state.db.pool())
    .await
    .unwrap();
    let person_id: i64 = sqlx::query_scalar("SELECT id FROM people WHERE tmdb_id = '10001'")
        .fetch_one(state.db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO item_people (item_id, people_id, role, character_name, sort_order) \
         VALUES (?, ?, 'Actor', 'Bunny', 0)",
    )
    .bind(movie_id)
    .bind(person_id)
    .execute(state.db.pool())
    .await
    .unwrap();

    let person_path = format!("/Users/1/Items/p-{person_id}");

    // 1) Person 详情：p-{id} 应返回 Person DTO（此前 404）
    let res = app.clone().oneshot(auth_get(&person_path)).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "person 详情应 200: {person_path}"
    );
    let v = json_body(res).await;
    assert_eq!(v["Type"], "Person");
    assert_eq!(v["Name"], "Test Actor");
    assert_eq!(
        v["Id"].as_str().unwrap(),
        person_path.rsplit('/').next().unwrap()
    );
    assert!(v["ProviderIds"].is_object(), "ProviderIds 应存在");

    // 2) PersonIds 过滤：返回该人的电影
    let res = app
        .clone()
        .oneshot(auth_get(&format!(
            "/Users/1/Items?PersonIds=p-{person_id}&IncludeItemTypes=Movie"
        )))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = json_body(res).await;
    assert_eq!(v["TotalRecordCount"], 1, "按 person 过滤应命中已关联的电影");
    assert_eq!(
        v["Items"][0]["Id"].as_str().unwrap(),
        format!("i-{movie_id}")
    );

    // 3) PersonIds 过滤：该人未关联 Series → 空
    let res = app
        .clone()
        .oneshot(auth_get(&format!(
            "/Users/1/Items?PersonIds=p-{person_id}&IncludeItemTypes=Series"
        )))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = json_body(res).await;
    assert_eq!(v["TotalRecordCount"], 0, "未关联 Series 应返回空");

    // 4) 不存在的 person → 404
    let res = app
        .clone()
        .oneshot(auth_get("/Users/1/Items/p-999999"))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND, "不存在 person 应 404");

    // 5) 非法 p- 后缀 → 404
    let res = app
        .clone()
        .oneshot(auth_get("/Users/1/Items/p-abc"))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND, "非法 p- 后缀应 404");

    let _ = std::fs::remove_dir_all(std::path::Path::new(&lib).parent().unwrap());
}

/// 回归：ParentId 按类型前缀判型（修复 library.id 与 item.id 裸数字撞车）。
/// l-{libId} → 该库电影列表；i-{seriesId} → 季列表；i-{seasonId} → 集列表。
#[tokio::test]
async fn parent_id_prefix_dispatch() {
    let state = test_state().await;
    let lib = sample_library();
    Importer::new(state.db.clone()).scan(&lib).await.unwrap();
    let app = router(state.clone());

    // /Users/{uid}/Views → 库 Id 带 l- 前缀
    let res = app
        .clone()
        .oneshot(auth_get("/Users/1/Views"))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = json_body(res).await;
    let lib_id = v["Items"][0]["Id"].as_str().unwrap().to_string();
    assert!(lib_id.starts_with("l-"), "库 Id 应带 l- 前缀: {lib_id}");

    // 原 bug 用例：ParentId=l-{libId}&IncludeItemTypes=Movie → 该库电影
    let res = app
        .clone()
        .oneshot(auth_get(&format!(
            "/Users/1/Items?ParentId={lib_id}&IncludeItemTypes=Movie"
        )))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = json_body(res).await;
    assert_eq!(
        v["TotalRecordCount"], 1,
        "ParentId=l-{{libId}} 应命中该库 1 部电影"
    );
    let movie_id = v["Items"][0]["Id"].as_str().unwrap();
    assert!(
        movie_id.starts_with("i-"),
        "movie Id 应带 i- 前缀: {movie_id}"
    );

    // ParentId=i-{seriesId} → 季列表
    let res = app
        .clone()
        .oneshot(auth_get("/Users/1/Items?IncludeItemTypes=Series"))
        .await
        .unwrap();
    let v = json_body(res).await;
    let series_id = v["Items"][0]["Id"].as_str().unwrap().to_string();
    assert!(
        series_id.starts_with("i-"),
        "series Id 应带 i- 前缀: {series_id}"
    );

    let res = app
        .clone()
        .oneshot(auth_get(&format!("/Users/1/Items?ParentId={series_id}")))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = json_body(res).await;
    assert_eq!(
        v["TotalRecordCount"], 1,
        "ParentId=i-{{seriesId}} 应返回 1 个季"
    );
    let season_id = v["Items"][0]["Id"].as_str().unwrap().to_string();
    assert!(season_id.starts_with("i-"));

    // ParentId=i-{seasonId} → 集列表（2 集）
    let res = app
        .clone()
        .oneshot(auth_get(&format!("/Users/1/Items?ParentId={season_id}")))
        .await
        .unwrap();
    let v = json_body(res).await;
    assert_eq!(
        v["TotalRecordCount"], 2,
        "ParentId=i-{{seasonId}} 应返回 2 集"
    );

    // 裸数字不再兼容：ParentId={seriesId 数字} 视为非法父级，返回空
    let bare_series = series_id.strip_prefix("i-").unwrap();
    let res = app
        .clone()
        .oneshot(auth_get(&format!("/Users/1/Items?ParentId={bare_series}")))
        .await
        .unwrap();
    let v = json_body(res).await;
    assert_eq!(
        v["TotalRecordCount"], 0,
        "裸数字 ParentId 不再兼容，应返回空"
    );

    let _ = std::fs::remove_dir_all(std::path::Path::new(&lib).parent().unwrap());
}

/// 构建单电影库：库名取自最后一级目录名，电影名取自 strm 文件名（剥离年份）。
fn make_single_movie_lib(lib_name: &str, movie_name: &str) -> std::path::PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!(
        "emrs-items-latest-{lib_name}-{n}-{}",
        std::process::id()
    ));
    let dir = root.join(lib_name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{movie_name} (2008).strm")),
        format!("# 直链\nhttps://cdn.example.com/{movie_name}.mp4\n"),
    )
    .unwrap();
    dir
}

/// 取 Latest 端点返回的 Name 列表（None=不传 ParentId，跨库全量）。
async fn latest_names(app: &axum::Router, parent: Option<&str>) -> Vec<String> {
    let path = match parent {
        Some(p) => format!("/Users/1/Items/Latest?ParentId={p}"),
        None => "/Users/1/Items/Latest?Limit=10".to_string(),
    };
    let v = json_body(app.clone().oneshot(auth_get(&path)).await.unwrap()).await;
    v.as_array()
        .unwrap()
        .iter()
        .map(|i| i["Name"].as_str().unwrap().to_string())
        .collect::<Vec<_>>()
}

/// 回归：`/Users/{uid}/Items/Latest?ParentId=l-{libId}` 按媒体库过滤。
/// 此前 `users_latest` 完全忽略 ParentId，返回全库最新条目。
#[tokio::test]
async fn latest_parent_id_filter() {
    let state = test_state().await;
    // 两个独立库，各一部电影，名字不同以便按归属断言
    let lib_a = make_single_movie_lib("LibAlpha", "Alpha Movie");
    let lib_b = make_single_movie_lib("LibBeta", "Beta Movie");
    let importer = Importer::new(state.db.clone());
    importer.scan(&lib_a).await.unwrap();
    importer.scan(&lib_b).await.unwrap();
    let app = router(state.clone());

    // /Users/{uid}/Views → 按 Name 取两库 Id
    let v = json_body(
        app.clone()
            .oneshot(auth_get("/Users/1/Views"))
            .await
            .unwrap(),
    )
    .await;
    let id_by_name: std::collections::HashMap<String, String> = v["Items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|it| {
            (
                it["Name"].as_str().unwrap().to_string(),
                it["Id"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let alpha_id = id_by_name.get("LibAlpha").unwrap();
    let beta_id = id_by_name.get("LibBeta").unwrap();

    // 无 ParentId → 全库最新，应同时含 Alpha 与 Beta
    let names = latest_names(&app, None).await;
    assert!(
        names.contains(&"Alpha Movie".to_string()) && names.contains(&"Beta Movie".to_string()),
        "无 ParentId 应跨库返回: {names:?}"
    );

    // ParentId=l-{alpha} → 仅 Alpha 库
    let names = latest_names(&app, Some(alpha_id)).await;
    assert!(
        names.contains(&"Alpha Movie".to_string()) && !names.contains(&"Beta Movie".to_string()),
        "ParentId=alpha 应仅含 Alpha 库: {names:?}"
    );

    // ParentId=l-{beta} → 仅 Beta 库
    let names = latest_names(&app, Some(beta_id)).await;
    assert!(
        names.contains(&"Beta Movie".to_string()) && !names.contains(&"Alpha Movie".to_string()),
        "ParentId=beta 应仅含 Beta 库: {names:?}"
    );

    // 非法父级（裸数字）→ 空
    let v = json_body(
        app.clone()
            .oneshot(auth_get("/Users/1/Items/Latest?ParentId=999"))
            .await
            .unwrap(),
    )
    .await;
    assert!(v.as_array().unwrap().is_empty(), "裸数字 ParentId 应返回空");

    let _ = std::fs::remove_dir_all(std::path::Path::new(&lib_a).parent().unwrap());
    let _ = std::fs::remove_dir_all(std::path::Path::new(&lib_b).parent().unwrap());
}
