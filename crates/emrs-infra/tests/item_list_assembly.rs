//! 拆连表端点的「应用层组装」回归测试（覆盖 [`docs`] 规范改造的 5 个 store 函数）。
//!
//! 这些函数原为 4~5 表 JOIN，现拆为「≤3 表筛选 + 多次单表 `IN` 批取 + Rust HashMap 合并」
//! （`item_store::assemble_item_rows`）。测试锁定：拆查询后，`media_source` 时长/容器、
//! `season`/`series` 层级回溯、`user_item_data` 进度仍能在应用层正确补全到完整 `ItemRow`。

use emrs_core::config::StorageConfig;
use emrs_infra::stores::{ItemRow, ItemsStore};

fn tmp_sqlite_dsn(tag: &str) -> String {
    let dir = std::env::temp_dir().join(format!("emrs-listasm-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("test.db");
    format!(
        "sqlite:{}?mode=rwc",
        db.to_string_lossy().replace('\\', "/")
    )
}

async fn setup_db(dsn: &str) -> emrs_infra::db::Db {
    let cfg = StorageConfig {
        dsn: dsn.to_string(),
        max_connections: 4,
    };
    let db = emrs_infra::db::Db::connect(&cfg).await.unwrap();
    db.migrate().await.unwrap();
    db
}

async fn new_library(db: &emrs_infra::db::Db, name: &str) -> i64 {
    sqlx::query("INSERT INTO library (name, collection_type) VALUES (?, 'tvshows')")
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

async fn ins_item(
    db: &emrs_infra::db::Db,
    ty: &str,
    library_id: i64,
    parent_id: Option<i64>,
    title: &str,
    season_number: Option<i64>,
    episode_number: Option<i64>,
) -> i64 {
    sqlx::query(
        "INSERT INTO item (type, library_id, parent_id, title, season_number, episode_number) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(ty)
    .bind(library_id)
    .bind(parent_id)
    .bind(title)
    .bind(season_number)
    .bind(episode_number)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query_scalar("SELECT id FROM item WHERE title = ? AND type = ?")
        .bind(title)
        .bind(ty)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

async fn ins_media(db: &emrs_infra::db::Db, item_id: i64, uuid: &str, dur: i64) {
    sqlx::query(
        "INSERT INTO media_source (uuid, item_id, name, protocol, container, file_duration, path) \
         VALUES (?, ?, ?, 'file', 'mkv', ?, ?)",
    )
    .bind(uuid)
    .bind(item_id)
    .bind(format!("f-{uuid}.mkv"))
    .bind(dur)
    .bind(format!("/media/{uuid}.mkv"))
    .execute(db.pool())
    .await
    .unwrap();
}

/// user_item_data：进度 / 完成 / 收藏三态按需置位。
#[allow(clippy::too_many_arguments)]
async fn mark(
    db: &emrs_infra::db::Db,
    user_id: i64,
    item_id: i64,
    played: i64,
    play_count: i64,
    ticks: i64,
    favorite: i64,
) {
    sqlx::query(
        "INSERT INTO user_item_data \
         (user_id, item_id, played, play_count, playback_position_ticks, is_favorite) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(user_id)
    .bind(item_id)
    .bind(played)
    .bind(play_count)
    .bind(ticks)
    .bind(favorite)
    .execute(db.pool())
    .await
    .unwrap();
}

/// user_item_data + 显式 `updated_at`（NextUp / Resume 按 recency 排序时需要确定时间）。
async fn mark_ts(
    db: &emrs_infra::db::Db,
    user_id: i64,
    item_id: i64,
    played: i64,
    play_count: i64,
    ticks: i64,
    updated_at: &str,
) {
    sqlx::query(
        "INSERT INTO user_item_data \
         (user_id, item_id, played, play_count, playback_position_ticks, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(user_id)
    .bind(item_id)
    .bind(played)
    .bind(play_count)
    .bind(ticks)
    .bind(updated_at)
    .execute(db.pool())
    .await
    .unwrap();
}

/// 造一集（可虚拟），返回 id。
async fn ins_ep(
    db: &emrs_infra::db::Db,
    library_id: i64,
    season_id: i64,
    title: &str,
    season_number: i64,
    episode_number: i64,
    is_virtual: bool,
) -> i64 {
    sqlx::query(
        "INSERT INTO item (type, library_id, parent_id, title, season_number, episode_number, is_virtual) \
         VALUES ('episode', ?, ?, ?, ?, ?, ?)",
    )
    .bind(library_id)
    .bind(season_id)
    .bind(title)
    .bind(season_number)
    .bind(episode_number)
    .bind(is_virtual as i64)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query_scalar("SELECT id FROM item WHERE title = ? AND type = 'episode'")
        .bind(title)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

/// 造 series→season 链，返回 (series_id, season_id)。
async fn mk_show(db: &emrs_infra::db::Db, library_id: i64, name: &str) -> (i64, i64) {
    let series = ins_item(
        db,
        "series",
        library_id,
        None,
        &format!("{name}·剧"),
        None,
        None,
    )
    .await;
    let season = ins_item(
        db,
        "season",
        library_id,
        Some(series),
        &format!("{name}·季"),
        Some(1),
        None,
    )
    .await;
    (series, season)
}

/// 造 series → season → {ep1, ep2} 链 + 一部 movie，各挂 media_source。
/// 返回 (library_id, series_id, season_id, ep1, ep2, movie)。
async fn seed(db: &emrs_infra::db::Db) -> (i64, i64, i64, i64, i64, i64) {
    let lib = new_library(db, "动漫").await;
    let series = ins_item(db, "series", lib, None, "骸骨骑士", None, None).await;
    let season = ins_item(db, "season", lib, Some(series), "第 1 季", Some(1), None).await;
    let ep1 = ins_item(
        db,
        "episode",
        lib,
        Some(season),
        "浮现于龙王之泉",
        Some(1),
        Some(1),
    )
    .await;
    let ep2 = ins_item(
        db,
        "episode",
        lib,
        Some(season),
        "要塞夺还战",
        Some(1),
        Some(2),
    )
    .await;
    let movie = ins_item(db, "movie", lib, None, "某电影", None, None).await;
    ins_media(db, ep1, "u-ep1", 1430).await;
    ins_media(db, ep2, "u-ep2", 1425).await;
    ins_media(db, movie, "u-mov", 5400).await;
    (lib, series, season, ep1, ep2, movie)
}

fn find(rows: &[ItemRow], id: i64) -> &ItemRow {
    rows.iter().find(|r| r.id == id).expect("应含该 item")
}

/// list_episodes：季下两集，媒体时长/容器 + 季/剧层级均应在应用层补全。
#[tokio::test]
async fn list_episodes_backfills_media_and_chain() {
    let db = setup_db(&tmp_sqlite_dsn("episodes")).await;
    let (_lib, series, season, ep1, ep2, _movie) = seed(&db).await;

    let rows = ItemsStore::list_episodes(&db, season, 1).await.unwrap();
    assert_eq!(rows.len(), 2, "两集都应返回");
    assert_eq!(rows[0].id, ep1, "应按 episode_number 升序");

    for (id, dur) in [(ep1, 1430), (ep2, 1425)] {
        let e = find(&rows, id);
        assert_eq!(e.item_type, "Episode");
        assert_eq!(e.season_id, Some(season));
        assert_eq!(e.season_name.as_deref(), Some("第 1 季"));
        assert_eq!(e.series_id, Some(series));
        assert_eq!(e.series_name.as_deref(), Some("骸骨骑士"));
        assert_eq!(e.file_second, Some(dur), "时长应来自 media_source");
        assert_eq!(e.container.as_deref(), Some("mkv"));
        assert_eq!(e.path_type.as_deref(), Some("local"));
    }
}

/// get_episode：单集详情应组装出完整 ItemRow（媒体 + 季 + 剧 + uuid/path）。
#[tokio::test]
async fn get_episode_assembles_full_row() {
    let db = setup_db(&tmp_sqlite_dsn("getep")).await;
    let (_lib, series, season, ep1, _ep2, _movie) = seed(&db).await;

    let row = ItemsStore::get_episode(&db, ep1, 1).await.unwrap();
    let e = row.expect("episode 详情应存在");
    assert_eq!(e.item_type, "Episode");
    assert_eq!(e.season_id, Some(season));
    assert_eq!(e.season_name.as_deref(), Some("第 1 季"));
    assert_eq!(e.series_id, Some(series));
    assert_eq!(e.series_name.as_deref(), Some("骸骨骑士"));
    assert_eq!(e.file_second, Some(1430));
    assert_eq!(e.uuid.as_deref(), Some("u-ep1"));
    assert_eq!(e.path_type.as_deref(), Some("local"));
}

/// get_episode 对非 episode（movie）应返回 None（类型过滤仍生效）。
#[tokio::test]
async fn get_episode_rejects_non_episode() {
    let db = setup_db(&tmp_sqlite_dsn("notep")).await;
    let (_lib, _series, _season, _ep1, _ep2, movie) = seed(&db).await;
    assert!(
        ItemsStore::get_episode(&db, movie, 1)
            .await
            .unwrap()
            .is_none()
    );
}

/// list_active_sessions：仅「开播未看完且有进度」的条目入列，季/剧回溯 + play_ms 应补全。
#[tokio::test]
async fn list_active_sessions_filters_and_backfills() {
    let db = setup_db(&tmp_sqlite_dsn("sessions")).await;
    let (_lib, series, _season, ep1, ep2, movie) = seed(&db).await;
    // ep1：进行中（play_count>0、played=0、有进度）→ 入列。
    mark(&db, 1, ep1, 0, 1, 1_560_000_000, 0).await;
    // ep2：已看完 → 排除。
    mark(&db, 1, ep2, 1, 2, 1_425_000_000, 0).await;
    // movie：有进度但 play_count=0 → 排除（需 play_count>0）。
    mark(&db, 1, movie, 0, 0, 30_000_000, 0).await;

    let rows = ItemsStore::list_active_sessions(&db, 1).await.unwrap();
    assert_eq!(rows.len(), 1, "只有 ep1 处于进行中");
    let e = &rows[0];
    assert_eq!(e.id, ep1);
    assert_eq!(e.series_id, Some(series));
    assert_eq!(e.series_name.as_deref(), Some("骸骨骑士"));
    assert_eq!(e.play_ms, 156_000, "play_ms = ticks/10000");
    assert_eq!(e.file_second, Some(1430));
}

/// list_favorites：收藏项返回 + 时长补全 + total 正确；非收藏排除。
#[tokio::test]
async fn list_favorites_backfills_and_counts() {
    let db = setup_db(&tmp_sqlite_dsn("favs")).await;
    let (_lib, series, _season, ep1, ep2, movie) = seed(&db).await;
    mark(&db, 1, ep1, 0, 0, 0, 1).await; // 收藏 ep1
    mark(&db, 1, movie, 0, 0, 0, 1).await; // 收藏 movie
    mark(&db, 1, ep2, 0, 0, 0, 0).await; // 未收藏

    let r = ItemsStore::list_favorites(&db, 1, None, 50, 0)
        .await
        .unwrap();
    assert_eq!(r.total, 2);
    assert_eq!(r.items.len(), 2);
    let e = find(&r.items, ep1);
    assert_eq!(e.series_id, Some(series));
    assert_eq!(e.series_name.as_deref(), Some("骸骨骑士"));
    assert_eq!(e.file_second, Some(1430), "时长应经批取补全");
    let m = find(&r.items, movie);
    assert_eq!(m.file_second, Some(5400));
    assert!(!r.items.iter().any(|x| x.id == ep2));

    // 类型过滤：只要 movie → 仅 movie。
    let movies = ItemsStore::list_favorites(&db, 1, Some("Movie"), 50, 0)
        .await
        .unwrap();
    assert_eq!(movies.total, 1);
    assert_eq!(movies.items[0].id, movie);
}

/// NextUp（无 series_id）：anchor（最近播放集）看完 → 顺延其后第一个未看集。ep1 看完 → ep2。
#[tokio::test]
async fn list_next_up_finished_anchor_advances() {
    let db = setup_db(&tmp_sqlite_dsn("nextup")).await;
    let (_lib, series, _season, ep1, ep2, _movie) = seed(&db).await;
    mark(&db, 1, ep1, 1, 1, 1_430_000_000, 0).await; // ep1 已看完 = anchor

    let rows = ItemsStore::list_next_up(&db, 1, None, 20, 0).await.unwrap();
    assert_eq!(rows.len(), 1, "同剧只一条");
    assert_eq!(rows[0].id, ep2);
    assert_eq!(rows[0].series_name.as_deref(), Some("骸骨骑士"));
    assert_eq!(rows[0].file_second, Some(1425), "完整 ItemRow 应含媒体时长");
    assert_eq!(rows[0].episode_number, Some(2));

    // 有 series_id（下钻）：该剧所有未看完真实集 = ep2。
    let scoped = ItemsStore::list_next_up(&db, 1, Some(series), 20, 0)
        .await
        .unwrap();
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].id, ep2);
    assert_eq!(scoped[0].series_id, Some(series));
}

/// NextUp：anchor 尚未看完 → 续 anchor 本身（返回 ep1，带进度），不顺延 ep2。
#[tokio::test]
async fn list_next_up_unfinished_anchor_continues_it() {
    let db = setup_db(&tmp_sqlite_dsn("nextup_cont")).await;
    let (_lib, _series, _season, ep1, ep2, _movie) = seed(&db).await;
    mark(&db, 1, ep1, 0, 1, 600_000_000, 0).await; // ep1 看到一半 = anchor，未看完

    let rows = ItemsStore::list_next_up(&db, 1, None, 20, 0).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, ep1, "anchor 未看完应续 anchor");
    assert_eq!(rows[0].play_ms, 60_000);
    assert_ne!(rows[0].id, ep2);
}

/// NextUp 排除「从未开播的剧」；全库无人开播 → 空。
#[tokio::test]
async fn list_next_up_excludes_never_started() {
    let db = setup_db(&tmp_sqlite_dsn("nextup_never")).await;
    let lib = new_library(&db, "无人看").await;
    let (_series, season) = mk_show(&db, lib, "冷剧").await;
    let e1 = ins_ep(&db, lib, season, "冷01", 1, 1, false).await;
    ins_media(&db, e1, "u-cold", 1000).await; // 只有文件，无 user_item_data

    let rows = ItemsStore::list_next_up(&db, 1, None, 20, 0).await.unwrap();
    assert!(rows.is_empty(), "没开播过的剧不应出现在 NextUp");
}

/// NextUp 跳过虚拟集：ep1 看完，ep2 虚拟，ep3 真实未看 → 取 ep3。
#[tokio::test]
async fn list_next_up_skips_virtual() {
    let db = setup_db(&tmp_sqlite_dsn("nextup_virt")).await;
    let lib = new_library(&db, "有缺口").await;
    let (_series, season) = mk_show(&db, lib, "缺剧").await;
    let e1 = ins_ep(&db, lib, season, "缺01", 1, 1, false).await;
    let _e2v = ins_ep(&db, lib, season, "缺02", 1, 2, true).await;
    let e3 = ins_ep(&db, lib, season, "缺03", 1, 3, false).await;
    ins_media(&db, e1, "u-q01", 1000).await;
    ins_media(&db, e3, "u-q03", 1000).await;
    mark(&db, 1, e1, 1, 1, 1_000_000_000, 0).await; // anchor 看完

    let rows = ItemsStore::list_next_up(&db, 1, None, 20, 0).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, e3, "应跳过虚拟 缺02 取 缺03");
}

/// NextUp 带 SeriesId 下钻：返回「正在看的季」里 anchor 往后的**多条**未看集；没看过的下一季不返回。
#[tokio::test]
async fn list_next_up_scoped_returns_current_season_and_excludes_next() {
    let db = setup_db(&tmp_sqlite_dsn("nextup_scope")).await;
    let lib = new_library(&db, "下钻库").await;
    let (series, season1) = mk_show(&db, lib, "长剧").await;
    // 第 1 季：e1 看完(anchor)，e2/e3 未看。
    let e1 = ins_ep(&db, lib, season1, "长01x1", 1, 1, false).await;
    let e2 = ins_ep(&db, lib, season1, "长02x1", 1, 2, false).await;
    let e3 = ins_ep(&db, lib, season1, "长03x1", 1, 3, false).await;
    // 第 2 季（从没看过）：e4/e5 未看 → 不应返回。
    let season2 = ins_item(&db, "season", lib, Some(series), "长剧·季2", Some(2), None).await;
    let _e4 = ins_ep(&db, lib, season2, "长01x2", 2, 1, false).await;
    let _e5 = ins_ep(&db, lib, season2, "长02x2", 2, 2, false).await;
    mark(&db, 1, e1, 1, 1, 1_000_000_000, 0).await; // e1 看完 = anchor（在第 1 季）

    let rows = ItemsStore::list_next_up(&db, 1, Some(series), 30, 0)
        .await
        .unwrap();
    assert_eq!(
        rows.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![e2, e3],
        "只返回正在看的第 1 季里 anchor 往后的未看集（多条、按集序），排除未看的第 2 季"
    );
}

/// NextUp 带 SeriesId：正在看的季已看完、只有后面的季没看过 → 返回空（不跨进未看的季）。
#[tokio::test]
async fn list_next_up_scoped_current_season_done_returns_empty() {
    let db = setup_db(&tmp_sqlite_dsn("nextup_scope_done")).await;
    let lib = new_library(&db, "跨季库").await;
    let (series, season1) = mk_show(&db, lib, "跨季").await;
    // 第 1 季全部看完（anchor = 最后一集，S1E2）。
    let e1 = ins_ep(&db, lib, season1, "跨01x1", 1, 1, false).await;
    let e2 = ins_ep(&db, lib, season1, "跨02x1", 1, 2, false).await;
    // 第 2 季从没看过。
    let season2 = ins_item(&db, "season", lib, Some(series), "跨季·季2", Some(2), None).await;
    let _f1 = ins_ep(&db, lib, season2, "跨01x2", 2, 1, false).await;
    let _f2 = ins_ep(&db, lib, season2, "跨02x2", 2, 2, false).await;
    mark(&db, 1, e1, 1, 1, 1_000_000_000, 0).await;
    mark(&db, 1, e2, 1, 1, 1_000_000_000, 0).await; // S1 全看完 = anchor 在 S1E2

    let rows = ItemsStore::list_next_up(&db, 1, Some(series), 30, 0)
        .await
        .unwrap();
    assert!(
        rows.is_empty(),
        "正在看的季已看完，不应跨到没看过的第 2 季 → 空"
    );
}

/// NextUp 带 SeriesId 且整剧从未开播（无 anchor）→ 返回空，不回退第 1 集。
#[tokio::test]
async fn list_next_up_scoped_never_started_returns_empty() {
    let db = setup_db(&tmp_sqlite_dsn("nextup_scope_new")).await;
    let lib = new_library(&db, "新剧库").await;
    let (series, season) = mk_show(&db, lib, "新剧").await;
    let _e1 = ins_ep(&db, lib, season, "新01", 1, 1, false).await;
    let _e2 = ins_ep(&db, lib, season, "新02", 1, 2, false).await;
    // 无任何 user_item_data。

    let rows = ItemsStore::list_next_up(&db, 1, Some(series), 30, 0)
        .await
        .unwrap();
    assert!(rows.is_empty(), "没看过的剧没有「接下来」，不回退第 1 集");
}
#[tokio::test]
async fn list_next_up_orders_by_recent_first() {
    let db = setup_db(&tmp_sqlite_dsn("nextup_order")).await;
    let lib = new_library(&db, "多剧").await;
    // 每剧 ep1 看完(anchor)，ep2 未看 → 代表集 = ep2；anchor 时间 old<mid<new。
    let mut reps = Vec::new();
    for (name, ts) in [
        ("旧剧", "2026-01-01T00:00:01.000Z"),
        ("中剧", "2026-01-01T00:00:02.000Z"),
        ("新剧", "2026-01-01T00:00:03.000Z"),
    ] {
        let (_series, season) = mk_show(&db, lib, name).await;
        let e1 = ins_ep(&db, lib, season, &format!("{name}01"), 1, 1, false).await;
        let e2 = ins_ep(&db, lib, season, &format!("{name}02"), 1, 2, false).await;
        ins_media(&db, e2, &format!("u-{name}02"), 1000).await;
        mark_ts(&db, 1, e1, 1, 1, 1_000_000_000, ts).await;
        reps.push(e2);
    }

    let rows = ItemsStore::list_next_up(&db, 1, None, 20, 0).await.unwrap();
    assert_eq!(
        rows.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![reps[2], reps[1], reps[0]],
        "最近追的剧在前"
    );
}

/// NextUp 全量分页：start/limit 切片（skip 后取 limit），跨页不重叠不遗漏。
#[tokio::test]
async fn list_next_up_paginates_offset() {
    let db = setup_db(&tmp_sqlite_dsn("nextup_page")).await;
    let lib = new_library(&db, "多剧分页").await;
    let mut reps = Vec::new();
    for (name, ts) in [
        ("旧剧", "2026-01-01T00:00:01.000Z"),
        ("中剧", "2026-01-01T00:00:02.000Z"),
        ("新剧", "2026-01-01T00:00:03.000Z"),
    ] {
        let (_series, season) = mk_show(&db, lib, name).await;
        let e1 = ins_ep(&db, lib, season, &format!("{name}01"), 1, 1, false).await;
        let e2 = ins_ep(&db, lib, season, &format!("{name}02"), 1, 2, false).await;
        ins_media(&db, e2, &format!("u-{name}02"), 1000).await;
        mark_ts(&db, 1, e1, 1, 1, 1_000_000_000, ts).await;
        reps.push(e2);
    }
    // 全序 [新, 中, 旧] = [reps[2], reps[1], reps[0]]。
    let page0 = ItemsStore::list_next_up(&db, 1, None, 2, 0).await.unwrap();
    let page1 = ItemsStore::list_next_up(&db, 1, None, 2, 2).await.unwrap();
    assert_eq!(
        page0.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![reps[2], reps[1]],
        "第一页取最近两条"
    );
    assert_eq!(
        page1.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![reps[0]],
        "第二页从 start=2 续，与第一页不重叠"
    );
}

/// NextUp 带 SeriesId 下钻分页：skip/take 作用在集序升序的候选列表上。
#[tokio::test]
async fn list_next_up_scoped_paginates_offset() {
    let db = setup_db(&tmp_sqlite_dsn("nextup_scope_page")).await;
    let lib = new_library(&db, "下钻分页").await;
    let (series, season) = mk_show(&db, lib, "分页剧").await;
    let e1 = ins_ep(&db, lib, season, "分01", 1, 1, false).await;
    let e2 = ins_ep(&db, lib, season, "分02", 1, 2, false).await;
    let e3 = ins_ep(&db, lib, season, "分03", 1, 3, false).await;
    for e in [e2, e3] {
        ins_media(&db, e, &format!("u-{e}"), 1000).await;
    }
    mark(&db, 1, e1, 1, 1, 1_000_000_000, 0).await; // e1 看完 = anchor → 候选 [e2, e3]

    let page = ItemsStore::list_next_up(&db, 1, Some(series), 1, 1)
        .await
        .unwrap();
    assert_eq!(
        page.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![e3],
        "start=1 limit=1 → 只取候选第二条 e3"
    );
}
