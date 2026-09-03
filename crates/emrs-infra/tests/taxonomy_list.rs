//! 分类端点（/Genres /Persons /Studios /Years /OfficialRatings）store 层回归测试。
//!
//! 锁定两点行为：
//! 1. `library_id` 过滤用 EXISTS 实现——同库多 item 挂同一分类**不得**复制成多行，
//!    分类只挂他库 item 时**不得**出现在本库结果（total 与 items 一致）；
//! 2. 分页 `limit/start` 跨库过滤后仍正确切片。

use emrs_core::config::StorageConfig;
use emrs_infra::stores::ItemsStore;

fn tmp_sqlite_dsn(tag: &str) -> String {
    let dir = std::env::temp_dir().join(format!("emrs-taxo-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("test.db");
    format!(
        "sqlite:{}?mode=rwc",
        db.to_string_lossy().replace('\\', "/")
    )
}

async fn setup_db(tag: &str) -> emrs_infra::db::Db {
    let cfg = StorageConfig {
        dsn: tmp_sqlite_dsn(tag),
        max_connections: 4,
    };
    let db = emrs_infra::db::Db::connect(&cfg).await.unwrap();
    db.migrate().await.unwrap();
    db
}

async fn new_library(db: &emrs_infra::db::Db, name: &str) -> i64 {
    sqlx::query("INSERT INTO library (name, collection_type) VALUES (?, 'movies')")
        .bind(name)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query_scalar("SELECT id FROM library WHERE name = ?")
        .bind(name)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

async fn ins_movie(
    db: &emrs_infra::db::Db,
    library_id: i64,
    title: &str,
    date_air: Option<&str>,
    official_rating: Option<&str>,
) -> i64 {
    sqlx::query(
        "INSERT INTO item (type, library_id, title, date_air, official_rating) \
         VALUES ('movie', ?, ?, ?, ?)",
    )
    .bind(library_id)
    .bind(title)
    .bind(date_air)
    .bind(official_rating)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query_scalar("SELECT id FROM item WHERE title = ?")
        .bind(title)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

async fn ins_genre(db: &emrs_infra::db::Db, name: &str) -> i64 {
    sqlx::query("INSERT INTO genre (name) VALUES (?)")
        .bind(name)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query_scalar("SELECT id FROM genre WHERE name = ?")
        .bind(name)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

async fn ins_person(db: &emrs_infra::db::Db, tmdb: &str, name: &str) -> i64 {
    sqlx::query("INSERT INTO people (tmdb_id, name) VALUES (?, ?)")
        .bind(tmdb)
        .bind(name)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query_scalar("SELECT id FROM people WHERE tmdb_id = ?")
        .bind(tmdb)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

async fn ins_studio(db: &emrs_infra::db::Db, name: &str) -> i64 {
    sqlx::query("INSERT INTO studio (name) VALUES (?)")
        .bind(name)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query_scalar("SELECT id FROM studio WHERE name = ?")
        .bind(name)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

async fn link_genre(db: &emrs_infra::db::Db, item_id: i64, genre_id: i64) {
    sqlx::query("INSERT INTO item_genre (item_id, genre_id) VALUES (?, ?)")
        .bind(item_id)
        .bind(genre_id)
        .execute(db.pool())
        .await
        .unwrap();
}

async fn link_person(db: &emrs_infra::db::Db, item_id: i64, people_id: i64) {
    sqlx::query("INSERT INTO item_people (item_id, people_id) VALUES (?, ?)")
        .bind(item_id)
        .bind(people_id)
        .execute(db.pool())
        .await
        .unwrap();
}

async fn link_studio(db: &emrs_infra::db::Db, item_id: i64, studio_id: i64) {
    sqlx::query("INSERT INTO item_studio (item_id, studio_id) VALUES (?, ?)")
        .bind(item_id)
        .bind(studio_id)
        .execute(db.pool())
        .await
        .unwrap();
}

/// 两库场景：A 库两部片共享「科幻」+ 演员张三 + 工作室「东宝」；B 库一部片挂「恐怖」+ 演员李四。
/// 断言库过滤去重、跨库隔离、无过滤全量。
async fn seed_two_libraries(db: &emrs_infra::db::Db) -> (i64, i64) {
    let lib_a = new_library(db, "电影A").await;
    let lib_b = new_library(db, "电影B").await;

    let m1 = ins_movie(db, lib_a, "M1", Some("2021-05-01"), Some("PG-13,G")).await;
    let m2 = ins_movie(db, lib_a, "M2", Some("2021-11-01"), Some("G")).await;
    let m3 = ins_movie(db, lib_b, "M3", Some("2019-01-01"), Some("R")).await;

    let g_sci = ins_genre(db, "科幻").await;
    let g_hor = ins_genre(db, "恐怖").await;
    link_genre(db, m1, g_sci).await;
    link_genre(db, m2, g_sci).await; // 同库两片共享 → 只能出现一次
    link_genre(db, m3, g_hor).await;

    let p_zhang = ins_person(db, "p1", "张三").await;
    let p_li = ins_person(db, "p2", "李四").await;
    link_person(db, m1, p_zhang).await;
    link_person(db, m2, p_zhang).await; // 共享去重
    link_person(db, m3, p_li).await;

    let s_toho = ins_studio(db, "东宝").await;
    let s_wb = ins_studio(db, "华纳").await;
    link_studio(db, m1, s_toho).await;
    link_studio(db, m2, s_toho).await; // 共享去重
    link_studio(db, m3, s_wb).await;

    (lib_a, lib_b)
}

fn titles(rows: &[emrs_infra::stores::ItemRow]) -> Vec<String> {
    rows.iter().map(|r| r.title.clone()).collect()
}

#[tokio::test]
async fn genres_library_filter_dedups_and_isolates() {
    let db = setup_db("genres").await;
    let (lib_a, _lib_b) = seed_two_libraries(&db).await;

    let r = ItemsStore::list_genres(&db, Some(lib_a), 100, 0)
        .await
        .unwrap();
    assert_eq!(r.total, 1, "A 库只应有「科幻」一条");
    assert_eq!(titles(&r.items), vec!["科幻".to_string()]);

    let all = ItemsStore::list_genres(&db, None, 100, 0).await.unwrap();
    assert_eq!(all.total, 2);
    assert_eq!(
        titles(&all.items),
        vec!["恐怖".to_string(), "科幻".to_string()]
    );
}

#[tokio::test]
async fn persons_library_filter_dedups_and_isolates() {
    let db = setup_db("persons").await;
    let (lib_a, _lib_b) = seed_two_libraries(&db).await;

    let r = ItemsStore::list_persons(&db, Some(lib_a), 100, 0)
        .await
        .unwrap();
    assert_eq!(r.total, 1, "A 库只应有「张三」一条（两部片共享去重）");
    assert_eq!(titles(&r.items), vec!["张三".to_string()]);
}

#[tokio::test]
async fn studios_library_filter_pagination() {
    let db = setup_db("studios").await;
    let (lib_a, _lib_b) = seed_two_libraries(&db).await;

    let r = ItemsStore::list_studios(&db, Some(lib_a), 100, 0)
        .await
        .unwrap();
    assert_eq!(r.total, 1, "A 库只应有「东宝」（两部片共享去重）");
    assert_eq!(titles(&r.items), vec!["东宝".to_string()]);

    let all = ItemsStore::list_studios(&db, None, 100, 0).await.unwrap();
    assert_eq!(all.total, 2);
    // 分页切片：limit=1 两页拼出全量且不重叠
    let p0 = ItemsStore::list_studios(&db, None, 1, 0).await.unwrap();
    let p1 = ItemsStore::list_studios(&db, None, 1, 1).await.unwrap();
    assert_eq!(titles(&p0.items), vec!["东宝".to_string()]);
    assert_eq!(titles(&p1.items), vec!["华纳".to_string()]);
}

#[tokio::test]
async fn years_library_filter() {
    let db = setup_db("years").await;
    let (lib_a, _lib_b) = seed_two_libraries(&db).await;

    let r = ItemsStore::list_years(&db, Some(lib_a), 100, 0)
        .await
        .unwrap();
    assert_eq!(r.total, 1, "A 库只有 2021 年");
    assert_eq!(titles(&r.items), vec!["2021".to_string()]);

    let all = ItemsStore::list_years(&db, None, 100, 0).await.unwrap();
    assert_eq!(
        titles(&all.items),
        vec!["2019".to_string(), "2021".to_string()]
    );
}

#[tokio::test]
async fn official_ratings_library_filter_csv_split() {
    let db = setup_db("ratings").await;
    let (lib_a, _lib_b) = seed_two_libraries(&db).await;

    let r = ItemsStore::list_official_ratings(&db, Some(lib_a), 100, 0)
        .await
        .unwrap();
    assert_eq!(r.total, 2, "A 库 CSV 拆分后应有 G / PG-13");
    assert_eq!(titles(&r.items), vec!["G".to_string(), "PG-13".to_string()]);

    let all = ItemsStore::list_official_ratings(&db, None, 100, 0)
        .await
        .unwrap();
    assert_eq!(all.total, 3, "全库应有 G / PG-13 / R");
}
