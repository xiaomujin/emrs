//! taxonomy_batch 的 people 头像图片行 id 集成测试。
//!
//! 回归验证：`People[].PrimaryImageTag` 依赖 `item_image`（parent_type='people'）
//! 的头像行 id（`img-{id}`）。曾因 `IN (...)` 占位符漏 bind 导致整条 taxonomy 查询报错、
//! 被调用方 `.unwrap_or_default()` 吞掉 → People/Genres 全空。

use emrs_core::config::StorageConfig;
use emrs_core::emby::{ItemImageFlags, item_to_json};
use emrs_core::stores::taxonomy_store::PersonBrief;
use emrs_core::stores::{ItemRow, ItemsStore};

fn tmp_sqlite_dsn(tag: &str) -> String {
    let dir = std::env::temp_dir().join(format!("emrs-tax-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("test.db");
    format!(
        "sqlite:{}?mode=rwc",
        db.to_string_lossy().replace('\\', "/")
    )
}

async fn setup_db(dsn: &str) -> emrs_core::db::Db {
    let cfg = StorageConfig {
        dsn: dsn.to_string(),
        max_connections: 4,
    };
    let db = emrs_core::db::Db::connect(&cfg).await.unwrap();
    db.migrate().await.unwrap();
    db
}

/// 有头像 / 无头像两个 people，验证 taxonomy_batch 标志 + item_to_json 输出。
#[tokio::test]
async fn people_primary_image_flag() {
    let db = setup_db(&tmp_sqlite_dsn("people")).await;

    sqlx::query("INSERT INTO library (name, collection_type) VALUES (?, ?)")
        .bind("剧集库")
        .bind("tvshows")
        .execute(db.pool())
        .await
        .unwrap();
    let library_id: i64 = sqlx::query_scalar("SELECT id FROM library WHERE name = ?")
        .bind("剧集库")
        .fetch_one(db.pool())
        .await
        .unwrap();

    sqlx::query(
        "INSERT INTO item (type, library_id, title, scrape_status) \
         VALUES ('series', ?, '测试剧', 'done')",
    )
    .bind(library_id)
    .execute(db.pool())
    .await
    .unwrap();
    let item_id: i64 = sqlx::query_scalar("SELECT id FROM item WHERE title = ?")
        .bind("测试剧")
        .fetch_one(db.pool())
        .await
        .unwrap();

    // 两个 people：一个有头像、一个无
    for (i, name) in ["有头像演员", "无头像演员"].iter().enumerate() {
        sqlx::query("INSERT INTO people (tmdb_id, name) VALUES (?, ?)")
            .bind(format!("tmdb-{i}"))
            .bind(name)
            .execute(db.pool())
            .await
            .unwrap();
    }
    let with_img_id: i64 = sqlx::query_scalar("SELECT id FROM people WHERE name = ?")
        .bind("有头像演员")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let without_img_id: i64 = sqlx::query_scalar("SELECT id FROM people WHERE name = ?")
        .bind("无头像演员")
        .fetch_one(db.pool())
        .await
        .unwrap();
    for (pid, role) in [(with_img_id, "Actor"), (without_img_id, "Director")] {
        sqlx::query(
            "INSERT INTO item_people (item_id, people_id, role, character_name, sort_order) \
             VALUES (?, ?, ?, '角色X', 0)",
        )
        .bind(item_id)
        .bind(pid)
        .bind(role)
        .execute(db.pool())
        .await
        .unwrap();
    }
    // 有头像演员写入 people primary 图片（复用 item_image 表）
    sqlx::query(
        "INSERT INTO item_image (parent_type, parent_id, image_type, path_url) \
         VALUES ('people', ?, 'primary', 'https://image.tmdb.org/t/p/w500/abc.jpg')",
    )
    .bind(with_img_id)
    .execute(db.pool())
    .await
    .unwrap();
    let img_row_id: i64 =
        sqlx::query_scalar("SELECT id FROM item_image WHERE parent_type = 'people'")
            .fetch_one(db.pool())
            .await
            .unwrap();

    // taxonomy_batch：有头像 → 图片行 id；无头像 → None
    let tax = ItemsStore::taxonomy_batch(&db, &[item_id]).await.unwrap();
    let t = tax.get(&item_id).expect("item 应有 taxonomy");
    assert_eq!(t.people.len(), 2);
    let by_name = |n: &str| t.people.iter().find(|p| p.name == n).unwrap();
    assert_eq!(
        by_name("有头像演员").primary_image_id,
        Some(img_row_id),
        "有 item_image 的 people 应带头像图片行 id"
    );
    assert_eq!(
        by_name("无头像演员").primary_image_id,
        None,
        "无 item_image 的 people 应 primary_image_id=None"
    );

    // item_to_json：People[].PrimaryImageTag 仅在 primary_image_id 存在时输出
    let item = ItemRow {
        id: item_id,
        library_id: Some(library_id),
        item_type: "Series".into(),
        title: "测试剧".into(),
        description: None,
        date_air: None,
        created_at: String::new(),
        updated_at: String::new(),
        container: None,
        file_second: None,
        uuid: None,
        name: None,
        path_type: None,
        path_url: None,
        play_ms: 0,
        is_complete: 0,
        play_count: 0,
        is_favorite: 0,
        season_number: None,
        episode_number: None,
        series_id: None,
        series_name: None,
        season_id: None,
        season_name: None,
        is_virtual: 0,
        tmdb_id: None,
        imdb_id: None,
        tvdb_id: None,
        community_rating: None,
        official_rating: None,
        tagline: None,
        sort_title: None,
        end_date: None,
        status: None,
        production_year: None,
    };
    let v = serde_json::to_value(item_to_json(
        "srv",
        &item,
        &ItemImageFlags::default(),
        Some(t),
        None,
        None,
    ))
    .unwrap();
    let people = v["People"].as_array().unwrap();
    let with: &serde_json::Value = people.iter().find(|p| p["Name"] == "有头像演员").unwrap();
    let without: &serde_json::Value = people.iter().find(|p| p["Name"] == "无头像演员").unwrap();
    assert_eq!(
        with["PrimaryImageTag"],
        serde_json::json!(format!("img-{img_row_id}")),
        "有头像应输出 PrimaryImageTag（图片行 id）"
    );
    assert!(
        !without.as_object().unwrap().contains_key("PrimaryImageTag"),
        "无头像不应输出 PrimaryImageTag"
    );
    // tag 标识图片本身（img-{图片行 id}），仅作缓存标记；
    // 客户端取图仍走 /Items/p-{id}/Images/Primary（person Id 前缀不变）。
    assert_eq!(with["Id"], serde_json::json!(format!("p-{with_img_id}")));
}

/// 回归：PeopleBrief 构造器必须同步 primary_image_id 字段（编译期保障，防漏改）。
#[test]
fn person_brief_constructor_sync() {
    let _ = PersonBrief {
        id: 1,
        name: "x".into(),
        role: "Actor".into(),
        character_name: None,
        primary_image_id: None,
    };
}
