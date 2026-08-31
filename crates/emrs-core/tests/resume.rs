//! Resume（继续观看）「同剧只一条 · 时间 anchor 往后取首个未看可播集」的集成测试。
//!
//! 回归验证 `item_store::list_resume` 的新语义：
//! - 每剧代表集 = 该剧「时间最近播放集（anchor，`play_count>0` 中 `updated_at` 最大）」往后、
//!   集序 `(season_number, episode_number) >=` anchor 的第一个「未看且非虚拟」集；
//! - anchor 未看完 → 续 anchor（带进度）；anchor 看完 → 顺延下一未开播集（位置 0）；
//! - 全剧看完 / 其后无可播集 → 该剧不出现；从未开播的剧 → 不出现；
//! - 虚拟缺失集（`is_virtual=1`）跳过；
//! - 分页作用在「去重合并」之后；图片各取自身、无回退。
//!
//! 所有时间用显式 ISO 字符串（字典序==时间序），保证 anchor 确定。

use emrs_core::config::StorageConfig;
use emrs_core::emby::ResumeCardJson;
use emrs_core::stores::{ItemsStore, ResumeEntry};

fn tmp_sqlite_dsn(tag: &str) -> String {
    let dir = std::env::temp_dir().join(format!("emrs-resume-{tag}-{}", std::process::id()));
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

const USER: i64 = 1;

/// 插入一个 item（type/library/parent/title + 季号/集号 + 是否虚拟），返回 id。
#[allow(clippy::too_many_arguments)]
async fn ins(
    db: &emrs_core::db::Db,
    ty: &str,
    library_id: i64,
    parent_id: Option<i64>,
    title: &str,
    season_number: Option<i64>,
    episode_number: Option<i64>,
    is_virtual: bool,
) -> i64 {
    sqlx::query(
        "INSERT INTO item (type, library_id, parent_id, title, season_number, episode_number, is_virtual) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(ty)
    .bind(library_id)
    .bind(parent_id)
    .bind(title)
    .bind(season_number)
    .bind(episode_number)
    .bind(is_virtual as i64)
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

async fn new_library(db: &emrs_core::db::Db, name: &str) -> i64 {
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

async fn ins_media(db: &emrs_core::db::Db, item_id: i64, uuid: &str, dur: i64) {
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

/// 用户进度行：显式 `updated_at` 控制 recency（anchor 依据）。
#[allow(clippy::too_many_arguments)]
async fn uid(
    db: &emrs_core::db::Db,
    item_id: i64,
    played: i64,
    play_count: i64,
    ticks: i64,
    updated_at: &str,
) {
    sqlx::query(
        "INSERT INTO user_item_data (user_id, item_id, played, play_count, playback_position_ticks, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(USER)
    .bind(item_id)
    .bind(played)
    .bind(play_count)
    .bind(ticks)
    .bind(updated_at)
    .execute(db.pool())
    .await
    .unwrap();
}

/// 看到一半（played=0, play_count=1）。
async fn in_progress(db: &emrs_core::db::Db, item_id: i64, ticks: i64, ts: &str) {
    uid(db, item_id, 0, 1, ticks, ts).await;
}
/// 已看完（played=1, play_count=1）。
async fn done(db: &emrs_core::db::Db, item_id: i64, ts: &str) {
    uid(db, item_id, 1, 1, 999_000_000, ts).await;
}

async fn ins_image(db: &emrs_core::db::Db, parent_id: i64, image_type: &str) -> i64 {
    sqlx::query(
        "INSERT INTO item_image (parent_type, parent_id, image_type, path_url) \
         VALUES ('item', ?, ?, ?)",
    )
    .bind(parent_id)
    .bind(image_type)
    .bind(format!("https://img/{parent_id}-{image_type}.jpg"))
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query_scalar(
        "SELECT id FROM item_image WHERE parent_type='item' AND parent_id=? AND image_type=?",
    )
    .bind(parent_id)
    .bind(image_type)
    .fetch_one(db.pool())
    .await
    .unwrap()
}

/// 造 series→season 链，返回 (series_id, season_id)。`series` 用唯一前缀避免标题冲突。
async fn mk_show(db: &emrs_core::db::Db, library_id: i64, name: &str) -> (i64, i64) {
    let series = ins(
        db,
        "series",
        library_id,
        None,
        &format!("{name}·剧"),
        None,
        None,
        false,
    )
    .await;
    let season = ins(
        db,
        "season",
        library_id,
        Some(series),
        &format!("{name}·季"),
        Some(1),
        None,
        false,
    )
    .await;
    (series, season)
}

/// 在某季下造一集（真实/虚拟，带季号集号）。
async fn mk_ep(
    db: &emrs_core::db::Db,
    library_id: i64,
    season_id: i64,
    name: &str,
    snum: i64,
    enum_: i64,
    virtual_: bool,
) -> i64 {
    ins(
        db,
        "episode",
        library_id,
        Some(season_id),
        name,
        Some(snum),
        Some(enum_),
        virtual_,
    )
    .await
}

fn ids(rows: &[ResumeEntry]) -> Vec<i64> {
    rows.iter().map(|r| r.id).collect()
}

/// anchor 由「时间」决定（非集号）：E01、E03 各看到一半且 E03 时间更近 → 只出 E03；E01 在 anchor 之前不补。
#[tokio::test]
async fn anchor_by_time_not_episode_number() {
    let db = setup_db(&tmp_sqlite_dsn("anchor_time")).await;
    let lib = new_library(&db, "L1").await;
    let (_, season) = mk_show(&db, lib, "剧A").await;
    let e1 = mk_ep(&db, lib, season, "A01", 1, 1, false).await;
    let _e2 = mk_ep(&db, lib, season, "A02", 1, 2, false).await;
    let e3 = mk_ep(&db, lib, season, "A03", 1, 3, false).await;

    in_progress(&db, e1, 500_000_000, "2026-01-01T00:00:01.000Z").await; // 更早
    in_progress(&db, e3, 300_000_000, "2026-01-01T00:00:09.000Z").await; // 时间最近 = anchor

    let rows = ItemsStore::list_resume(&db, USER, None, None, 50, 0)
        .await
        .unwrap();
    assert_eq!(
        ids(&rows),
        vec![e3],
        "应只出 anchor(E03) 续播；E01/E02 不在"
    );
    let r = &rows[0];
    assert_eq!(r.item_type, "Episode");
    assert_eq!(r.play_ms, 30_000, "play_ms = ticks/10000");
    assert_eq!(r.episode_number, Some(3));
}

/// anchor 已看完 → 顺延其后第一个未开播真实集（位置 0）。
#[tokio::test]
async fn finished_anchor_advances_to_next_unwatched() {
    let db = setup_db(&tmp_sqlite_dsn("advance")).await;
    let lib = new_library(&db, "L2").await;
    let (_, season) = mk_show(&db, lib, "剧B").await;
    let e1 = mk_ep(&db, lib, season, "B01", 1, 1, false).await;
    let e2 = mk_ep(&db, lib, season, "B02", 1, 2, false).await;
    ins_media(&db, e2, "ub02", 1400).await;

    done(&db, e1, "2026-01-01T00:00:05.000Z").await; // anchor 看完

    let rows = ItemsStore::list_resume(&db, USER, None, None, 50, 0)
        .await
        .unwrap();
    assert_eq!(ids(&rows), vec![e2]);
    let r = &rows[0];
    assert_eq!(r.play_ms, 0, "从未开播 → 从头");
    assert_eq!(r.file_second, Some(1400), "时长来自 media_source");
}

/// 全剧看完 → 该剧不出现。
#[tokio::test]
async fn all_watched_series_excluded() {
    let db = setup_db(&tmp_sqlite_dsn("done_all")).await;
    let lib = new_library(&db, "L3").await;
    let (_, season) = mk_show(&db, lib, "剧C").await;
    let e1 = mk_ep(&db, lib, season, "C01", 1, 1, false).await;
    let e2 = mk_ep(&db, lib, season, "C02", 1, 2, false).await;
    done(&db, e1, "2026-01-01T00:00:01.000Z").await;
    done(&db, e2, "2026-01-01T00:00:02.000Z").await; // anchor=E02，其后无未看集

    let rows = ItemsStore::list_resume(&db, USER, None, None, 50, 0)
        .await
        .unwrap();
    assert!(rows.is_empty(), "全看完的剧不应出现");
}

/// 虚拟缺失集跳过：anchor 后是虚拟集 + 真实未看集 → 取真实集。
#[tokio::test]
async fn skips_virtual_episode() {
    let db = setup_db(&tmp_sqlite_dsn("virtual")).await;
    let lib = new_library(&db, "L4").await;
    let (_, season) = mk_show(&db, lib, "剧D").await;
    let e1 = mk_ep(&db, lib, season, "D01", 1, 1, false).await;
    let e2v = mk_ep(&db, lib, season, "D02", 1, 2, true).await; // 虚拟，不可播
    let e3 = mk_ep(&db, lib, season, "D03", 1, 3, false).await;
    done(&db, e1, "2026-01-01T00:00:05.000Z").await; // anchor 看完

    let rows = ItemsStore::list_resume(&db, USER, None, None, 50, 0)
        .await
        .unwrap();
    assert_eq!(ids(&rows), vec![e3], "应跳过虚拟 D02 取 D03");
    assert!(!ids(&rows).contains(&e2v));
}

/// 从未开播的剧（无任何 user_item_data）→ Resume 不出现。
#[tokio::test]
async fn never_started_series_excluded() {
    let db = setup_db(&tmp_sqlite_dsn("never")).await;
    let lib = new_library(&db, "L5").await;
    let (_, season) = mk_show(&db, lib, "剧E").await;
    let _e1 = mk_ep(&db, lib, season, "E01", 1, 1, false).await; // 只有文件，无播放记录
    ins_media(&db, _e1, "ue01", 1000).await;

    let rows = ItemsStore::list_resume(&db, USER, None, None, 50, 0)
        .await
        .unwrap();
    assert!(rows.is_empty(), "没开播过的剧不应进继续观看");
}

/// 电影：看到一半出现、看完不出现。
#[tokio::test]
async fn movie_in_progress_only() {
    let db = setup_db(&tmp_sqlite_dsn("movie")).await;
    let lib = new_library(&db, "L6").await;
    let m_open = ins(&db, "movie", lib, None, "电影·看一半", None, None, false).await;
    let m_done = ins(&db, "movie", lib, None, "电影·已看完", None, None, false).await;
    ins_media(&db, m_open, "mo", 5400).await;
    in_progress(&db, m_open, 30_000_000, "2026-01-01T00:00:01.000Z").await;
    done(&db, m_done, "2026-01-01T00:00:02.000Z").await;

    let rows = ItemsStore::list_resume(&db, USER, None, None, 50, 0)
        .await
        .unwrap();
    assert_eq!(ids(&rows), vec![m_open]);
    let r = &rows[0];
    assert_eq!(r.item_type, "Movie");
    assert_eq!(r.series_id, None);
    assert_eq!(r.file_second, Some(5400));
    assert_eq!(r.play_ms, 3_000);
}

/// 去重 + 按时间排序 + 去重后分页：三剧各两条在播，anchor 时间 D>C>B>A，limit 分页落在去重结果上。
#[tokio::test]
async fn dedup_then_sort_then_paginate() {
    let db = setup_db(&tmp_sqlite_dsn("paginate")).await;
    let lib = new_library(&db, "L7").await;
    // 每剧 E01 早、E02 晚（anchor=E02），anchor 时间 B/C/D 递增。
    let mut rep = Vec::new();
    for (name, ts) in [
        ("剧A", "2026-01-01T00:00:01.000Z"),
        ("剧B", "2026-01-01T00:00:02.000Z"),
        ("剧C", "2026-01-01T00:00:03.000Z"),
    ] {
        let (_, season) = mk_show(&db, lib, name).await;
        let e1 = mk_ep(&db, lib, season, &format!("{name}01"), 1, 1, false).await;
        let e2 = mk_ep(&db, lib, season, &format!("{name}02"), 1, 2, false).await;
        in_progress(&db, e1, 100_000_000, "2025-01-01T00:00:00.000Z").await; // 旧，非 anchor
        in_progress(&db, e2, 200_000_000, ts).await; // anchor
        rep.push(e2); // 每剧代表集 = E02
    }

    // 全量：三条，按 anchor 时间倒序 = [C, B, A]。
    let all = ItemsStore::list_resume(&db, USER, None, None, 50, 0)
        .await
        .unwrap();
    assert_eq!(ids(&all), vec![rep[2], rep[1], rep[0]], "去重后按时间倒序");

    // 第 1 页 2 条 = [C, B]；第 2 页 = [A]。
    let p1 = ItemsStore::list_resume(&db, USER, None, None, 2, 0)
        .await
        .unwrap();
    assert_eq!(ids(&p1), vec![rep[2], rep[1]]);
    let p2 = ItemsStore::list_resume(&db, USER, None, None, 2, 2)
        .await
        .unwrap();
    assert_eq!(ids(&p2), vec![rep[0]]);
}

/// 图片规则：代表集只返回自身 Primary，无回退；`BackdropImageTags` 恒空；不含上级剧集图片字段。
#[tokio::test]
async fn resume_card_images_own_only_no_fallback() {
    let db = setup_db(&tmp_sqlite_dsn("image")).await;
    let lib = new_library(&db, "L8").await;
    let (series, season) = mk_show(&db, lib, "剧F").await;
    let e1 = mk_ep(&db, lib, season, "F01", 1, 1, false).await;
    in_progress(&db, e1, 100_000_000, "2026-01-01T00:00:01.000Z").await;
    ins_media(&db, e1, "uf01", 1430).await;
    ins_image(&db, series, "primary").await;
    ins_image(&db, series, "backdrop").await;
    ins_image(&db, series, "logo").await;
    let own = ins_image(&db, e1, "primary").await;

    let rows = ItemsStore::list_resume(&db, USER, None, None, 50, 0)
        .await
        .unwrap();
    let primary = ItemsStore::image_primary_batch(&db, &ids(&rows))
        .await
        .unwrap();
    let card = serde_json::to_value(ResumeCardJson::from_row(
        "srv",
        &rows[0],
        primary.get(&e1).copied(),
    ))
    .unwrap();

    assert_eq!(card["ImageTags"]["Primary"], format!("img-{own}"));
    assert_eq!(card["BackdropImageTags"], serde_json::json!([]));
    assert_eq!(card["RunTimeTicks"], 1430_i64 * 10_000_000);
    assert!(card["ImageTags"].get("Logo").is_none());
    for f in [
        "SeriesPrimaryImageTag",
        "ParentBackdropImageTags",
        "ParentLogoImageTag",
        "ParentThumbImageTag",
    ] {
        assert!(
            !card.as_object().unwrap().contains_key(f),
            "不应含上级剧集图片字段 {f}"
        );
    }
}

/// ParentId 过滤：`library_id` 按所属库；`parent_item`（命中 series_id 或 season_id）按剧集/季下钻。
/// 电影无 series/season → 带 `parent_item` 时被排除，但受库过滤约束。
#[tokio::test]
async fn parent_id_filter() {
    let db = setup_db(&tmp_sqlite_dsn("parentid")).await;
    let l1 = new_library(&db, "PA").await;
    let l2 = new_library(&db, "PB").await;

    // L1：剧 A（E01 看到一半 = 代表集）+ 一部电影（看到一半）。
    let (series_a, season_a) = mk_show(&db, l1, "剧A").await;
    let ep_a = mk_ep(&db, l1, season_a, "A01", 1, 1, false).await;
    in_progress(&db, ep_a, 100_000_000, "2026-01-01T00:00:01.000Z").await;
    let movie = ins(&db, "movie", l1, None, "电影X", None, None, false).await;
    in_progress(&db, movie, 100_000_000, "2026-01-01T00:00:02.000Z").await;

    // L2：剧 B（E01 看到一半 = 代表集）。
    let (series_b, _season_b) = mk_show(&db, l2, "剧B").await;
    let ep_b = mk_ep(&db, l2, _season_b, "B01", 1, 1, false).await;
    in_progress(&db, ep_b, 100_000_000, "2026-01-01T00:00:03.000Z").await;

    let sorted = |rows: Vec<ResumeEntry>| {
        let mut v = ids(&rows);
        v.sort_unstable();
        v
    };

    // 全量：3 条（ep_a, movie, ep_b）。
    let all = sorted(
        ItemsStore::list_resume(&db, USER, None, None, 50, 0)
            .await
            .unwrap(),
    );
    assert_eq!(all, {
        let mut v = vec![ep_a, ep_b, movie];
        v.sort_unstable();
        v
    });

    // 按库 L1：只 ep_a + movie。
    let by_l1 = sorted(
        ItemsStore::list_resume(&db, USER, Some(l1), None, 50, 0)
            .await
            .unwrap(),
    );
    assert_eq!(by_l1, {
        let mut v = vec![ep_a, movie];
        v.sort_unstable();
        v
    });

    // 按库 L2：只 ep_b。
    let by_l2 = sorted(
        ItemsStore::list_resume(&db, USER, Some(l2), None, 50, 0)
            .await
            .unwrap(),
    );
    assert_eq!(by_l2, vec![ep_b]);

    // 按剧集下钻（ParentId=series_a）：只 ep_a，电影排除。
    let by_series = sorted(
        ItemsStore::list_resume(&db, USER, None, Some(series_a), 50, 0)
            .await
            .unwrap(),
    );
    assert_eq!(by_series, vec![ep_a]);

    // 按季下钻（ParentId=season_a）：命中 ep_a 的 season_id。
    let by_season = sorted(
        ItemsStore::list_resume(&db, USER, None, Some(season_a), 50, 0)
            .await
            .unwrap(),
    );
    assert_eq!(by_season, vec![ep_a]);

    // 无匹配剧集（series_b 下只有 ep_b，在 L2）：验证隔离。
    let by_series_b = sorted(
        ItemsStore::list_resume(&db, USER, None, Some(series_b), 50, 0)
            .await
            .unwrap(),
    );
    assert_eq!(by_series_b, vec![ep_b]);
}

/// top-K 惰性展开：小 `limit` 须跳过 recency 高但「全看完无代表集」的剧，继续往后取有代表集的剧。
/// 若把 LIMIT 直接压进选集 SQL（不展开），前 K 名无代表 → 会错误返回空。
#[tokio::test]
async fn frontier_top_k_skips_no_rep_series() {
    let db = setup_db(&tmp_sqlite_dsn("topk")).await;
    let lib = new_library(&db, "多剧topk").await;
    // 5 剧，recency 由 E01 的 done 时间定：T0>T1>T2>T3>T4。
    // T0/T1（recency 最高）两集全看完 → 无代表集；T2/T3/T4 有未看 E02 → 有代表集。
    let ts = |i: i64| format!("2026-01-01T00:00:0{i}.000Z");
    let mut reps = Vec::new();
    for i in 0..5 {
        let (_, season) = mk_show(&db, lib, &format!("剧T{i}")).await;
        let e1 = mk_ep(&db, lib, season, &format!("T{i}01"), 1, 1, false).await;
        let e2 = mk_ep(&db, lib, season, &format!("T{i}02"), 1, 2, false).await;
        let anchor_ts = ts(5 - i);
        done(&db, e1, &anchor_ts).await;
        if i < 2 {
            done(&db, e2, &anchor_ts).await; // 全看完 → 该剧不出代表
        } else {
            reps.push(e2); // E02 未看 → 代表集
        }
    }
    // limit=2：跳过 T0/T1（无代表），返回 [T2 代表, T3 代表]（recency 降序的前两个有代表集者）。
    let rows = ItemsStore::list_resume(&db, USER, None, None, 2, 0)
        .await
        .unwrap();
    assert_eq!(
        ids(&rows),
        vec![reps[0], reps[1]],
        "top-K 须跳过无代表集的剧"
    );

    // start=2：第三有代表集者 T4。
    let p2 = ItemsStore::list_resume(&db, USER, None, None, 2, 2)
        .await
        .unwrap();
    assert_eq!(ids(&p2), vec![reps[2]], "跨页仍从有代表集的剧续");
}
