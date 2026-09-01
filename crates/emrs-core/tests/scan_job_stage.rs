//! ScanStage scan_job 生命周期回归测试。
//!
//! 历史 bug：`update_scan_job_status` 的 UPDATE 占位符 7 个而 bind 8 个
//! （`deleted_items` 行漏了 `+ ?`），流水线此前从未消费过 scan_job 所以
//! 未暴露；scan_job 入队化后导致状态永远停留 pending、任务每 tick 重扫。
//! 本测试锁定 create → running → done 全链路。

use std::sync::Arc;

use emrs_core::db::Db;
use emrs_core::importer::stages::ScanStage;

#[tokio::test]
async fn scan_job_lifecycle_transitions() {
    let dir = std::env::temp_dir().join(format!("emrs-sjl-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dsn = format!(
        "sqlite:{}?mode=rwc",
        dir.join("p.db").to_string_lossy().replace('\\', "/")
    );
    let db = Arc::new(
        Db::connect(&emrs_core::config::StorageConfig {
            dsn,
            max_connections: 2,
        })
        .await
        .unwrap(),
    );
    db.migrate().await.unwrap();

    let stage = ScanStage::new(db.clone());
    let job = stage.create_scan_job(1, "probe").await.unwrap();

    // pending 可被消费
    assert_eq!(stage.pending_scan_jobs().await.len(), 1);

    // running：打 started_at
    stage.update_scan_job_status(job, "running", None).await;
    let (status, started): (String, Option<String>) =
        sqlx::query_as("SELECT status, started_at FROM scan_job WHERE id = ?")
            .bind(job)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(status, "running");
    assert!(started.is_some(), "running 应写入 started_at");

    // done：写 finished_at + 统计
    stage
        .update_scan_job_status(
            job,
            "done",
            Some(&emrs_core::importer::scanner::ScanStats {
                media: 3,
                ..Default::default()
            }),
        )
        .await;
    let (status, finished, updated): (String, Option<String>, i64) =
        sqlx::query_as("SELECT status, finished_at, updated_items FROM scan_job WHERE id = ?")
            .bind(job)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(status, "done");
    assert!(finished.is_some(), "done 应写入 finished_at");
    assert_eq!(updated, 3, "media 计数应写入 updated_items");

    // 终态后不再进入消费队列
    assert!(
        stage.pending_scan_jobs().await.is_empty(),
        "done 的 job 不应再被消费"
    );
}

/// 重扫去重回归：`item.title` 会被 Scrape 改写为 TMDB 标题，早期 movie/series
/// 按 title 去重导致重扫时文件名解析出的标题失配 → 同一文件重复入库
/// （movie 重复、series 连整棵 season/episode 树一起重复）。
/// 修复后以 media_source 物理路径（本地 path / strm remote_path）为复用锚。
#[tokio::test]
async fn rescan_after_title_change_does_not_duplicate_items() {
    use emrs_core::importer::scanner::Scanner;

    let dir = std::env::temp_dir().join(format!("emrs-rescan-{}-{}", std::process::id(), "t"));
    let _ = std::fs::remove_dir_all(&dir);
    let lib = dir.join("MediaLib");
    let season_dir = lib.join("Demo Show").join("Season 1");
    std::fs::create_dir_all(&season_dir).unwrap();
    // 一部电影 strm + 一集剧集 strm（http 直链，重扫解析出稳定 remote_path）。
    std::fs::write(
        lib.join("The Demo Movie 2020.strm"),
        b"http://x/movie.mp4\n",
    )
    .unwrap();
    std::fs::write(
        season_dir.join("Demo Show S01E01.strm"),
        b"http://x/s01e01.mp4\n",
    )
    .unwrap();

    let db = Arc::new(
        Db::connect(&emrs_core::config::StorageConfig {
            dsn: format!(
                "sqlite:{}?mode=rwc",
                dir.join("r.db").to_string_lossy().replace('\\', "/")
            ),
            max_connections: 2,
        })
        .await
        .unwrap(),
    );
    db.migrate().await.unwrap();

    let scanner = Scanner::new(db.clone(), String::new());

    // 首次扫描：movie(1) + series(1) + season(1) + episode(1) = 4 条目，2 media_source。
    scanner.scan_path(&lib).await.unwrap();
    let items_after_first: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM item")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let ms_after_first: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media_source")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(items_after_first, 4, "首扫应建 4 个条目");
    assert_eq!(ms_after_first, 2, "首扫应建 2 个 media_source");

    // 模拟 Scrape：把 movie / series 的 title 改写成 TMDB 标题（触发早期重复的根因条件）。
    sqlx::query("UPDATE item SET title = 'TMDB Renamed Title' WHERE type IN ('movie','series')")
        .execute(db.pool())
        .await
        .unwrap();

    // 重扫前：记录 movie 的 media_source 身份（id/uuid/created_at）。
    let ms_before: (i64, String, String) = sqlx::query_as(
        "SELECT ms.id, ms.uuid, ms.created_at FROM media_source ms \
         JOIN item i ON i.id = ms.item_id WHERE i.type = 'movie'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();

    // 再次扫描同一目录：应全部按物理路径复用既有条目，不新增。
    scanner.scan_path(&lib).await.unwrap();
    let items_after_second: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM item")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let ms_after_second: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media_source")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        items_after_second, items_after_first,
        "重扫（title 已被改写）不得重复入库条目"
    );
    assert_eq!(
        ms_after_second, ms_after_first,
        "重扫不得重复入库 media_source"
    );

    // 重扫后：同一路径的 media_source 应原样保留（id/uuid/created_at 不变），
    // 证明走的是「重复跳过保留旧行」而非「删除再插入」（后者会换新 uuid、改 id/created_at）。
    let ms_after: (i64, String, String) = sqlx::query_as(
        "SELECT ms.id, ms.uuid, ms.created_at FROM media_source ms \
         JOIN item i ON i.id = ms.item_id WHERE i.type = 'movie'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        ms_after, ms_before,
        "重复文件重扫必须保留旧 media_source，不得换新"
    );

    // 复用命中的条目仍保有 TMDB 标题（未按目录名/文件名回退覆盖）。
    let movie_title: String = sqlx::query_scalar("SELECT title FROM item WHERE type = 'movie'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(movie_title, "TMDB Renamed Title", "复用应保留刮削后的标题");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 重扫去重回归（source_dir 锚）：给「已刮削（title 被改写）的剧」新增一集文件后重扫，
/// 不得重复建 series/season，只新增那一集 episode。早期按 title 去重 + 按 episode 路径
/// 上溯都无法覆盖「新文件路径不在库中」的情形，改以 series.source_dir（剧集目录）等值命中。
#[tokio::test]
async fn add_episode_file_to_scraped_series_no_duplicate_series() {
    use emrs_core::importer::scanner::Scanner;

    let dir = std::env::temp_dir().join(format!("emrs-rescan-{}-add", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let lib = dir.join("MediaLib");
    let season_dir = lib.join("Demo Show").join("Season 1");
    std::fs::create_dir_all(&season_dir).unwrap();
    std::fs::write(
        season_dir.join("Demo Show S01E01.strm"),
        b"http://x/s01e01.mp4\n",
    )
    .unwrap();

    let db = Arc::new(
        Db::connect(&emrs_core::config::StorageConfig {
            dsn: format!(
                "sqlite:{}?mode=rwc",
                dir.join("a.db").to_string_lossy().replace('\\', "/")
            ),
            max_connections: 2,
        })
        .await
        .unwrap(),
    );
    db.migrate().await.unwrap();

    let scanner = Scanner::new(db.clone(), String::new());
    scanner.scan_path(&lib).await.unwrap();

    // 模拟刮削改写 series 标题（触发早期目录名失配的根因条件）。
    sqlx::query("UPDATE item SET title = 'TMDB Show Title' WHERE type = 'series'")
        .execute(db.pool())
        .await
        .unwrap();

    // 新增第二集文件到同一季目录，重扫。
    std::fs::write(
        season_dir.join("Demo Show S01E02.strm"),
        b"http://x/s01e02.mp4\n",
    )
    .unwrap();
    scanner.scan_path(&lib).await.unwrap();

    let count_type = |ty: &'static str| {
        let pool = db.pool().clone();
        async move {
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM item WHERE type = ?")
                .bind(ty)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    assert_eq!(count_type("series").await, 1, "新增集文件不得重复建 series");
    assert_eq!(count_type("season").await, 1, "新增集文件不得重复建 season");
    assert_eq!(count_type("episode").await, 2, "两集各一条 episode");

    // source_dir 已落到 series 上（目录锚生效）。
    let src_dir: Option<String> =
        sqlx::query_scalar("SELECT source_dir FROM item WHERE type = 'series'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(
        src_dir.as_deref().unwrap_or("").ends_with("Demo Show"),
        "series.source_dir 应为剧集目录，实际 {src_dir:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 第0季（Specials）落库回归：早期 `if season_number <= 0 { season_number = 1 }`
/// 把合法的第0季与「季号未知」哨兵混为一谈，导致本地 `S00` 目录下的实体文件
/// 全被并入第一季。修复后季目录结构（season_from_dir>=0）的 0 是合法 Specials 季，
/// 必须独立建 season_number=0 的季并挂载其下，绝不落到 season_number=1。
#[tokio::test]
async fn season_zero_folder_is_not_merged_into_season_one() {
    use emrs_core::importer::scanner::Scanner;

    let dir = std::env::temp_dir().join(format!("emrs-rescan-{}-s00", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let lib = dir.join("MediaLib");
    let s1 = lib.join("Demo Show").join("Season 1");
    let s0 = lib.join("Demo Show").join("S00");
    std::fs::create_dir_all(&s1).unwrap();
    std::fs::create_dir_all(&s0).unwrap();
    std::fs::write(s1.join("Demo Show S01E01.strm"), b"http://x/s01e01.mp4\n").unwrap();
    std::fs::write(s0.join("Demo Show S00E21.strm"), b"http://x/s00e21.mp4\n").unwrap();

    let db = Arc::new(
        Db::connect(&emrs_core::config::StorageConfig {
            dsn: format!(
                "sqlite:{}?mode=rwc",
                dir.join("s0.db").to_string_lossy().replace('\\', "/")
            ),
            max_connections: 2,
        })
        .await
        .unwrap(),
    );
    db.migrate().await.unwrap();

    let scanner = Scanner::new(db.clone(), String::new());
    scanner.scan_path(&lib).await.unwrap();

    // 两季独立：season_number=0 与 season_number=1 各一条。
    let season_nums: Vec<i64> = sqlx::query_scalar(
        "SELECT season_number FROM item WHERE type = 'season' ORDER BY season_number",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(season_nums, vec![0, 1], "S00 必须独立成第0季，不并入第一季");

    // S00E21 落在第0季之下（而非第一季）。
    let ep_season: Option<i64> = sqlx::query_scalar(
        "SELECT s.season_number FROM item e \
         JOIN item s ON s.id = e.parent_id \
         WHERE e.type = 'episode' AND e.episode_number = 21",
    )
    .fetch_optional(db.pool())
    .await
    .unwrap();
    assert_eq!(ep_season, Some(0), "S00E21 应归属第0季，实际 {ep_season:?}");

    let _ = std::fs::remove_dir_all(&dir);
}
