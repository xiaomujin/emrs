//! 后台 job 端点：扫描 / 刮削 / 流信息回填（探测）/ 目录监听。
//!
//! 元数据分离后的触发语义：
//! - `scan/start` 只**入队** `scan_job(pending)` 并唤醒流水线，实际扫描由
//!   Pipeline 的 scan 循环消费（单一消费者）；job 包装层轮询 DB 行进度。
//! - `scrape/start` 重置范围内条目为 `pending` 并唤醒 Scrape 阶段；
//!   未配置 TMDB key 时按旧契约把候选计为 skipped 后立即完成。

use std::path::PathBuf;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use emrs_infra::db::Db;
use emrs_infra::stores::scan_job_store;
use emrs_service::importer::stages::ScanStage;
use emrs_service::job::JobStatus;

use crate::state::AppState;

#[derive(Deserialize, Default)]
pub(super) struct ScanInput {
    /// 指定路径扫描；缺省扫描全部库根。
    path: Option<String>,
}

/// 查询所有库根（从 library_path 表，按 path 去重）。
async fn library_roots(st: &AppState) -> Vec<PathBuf> {
    let rows = sqlx::query_scalar::<_, String>("SELECT DISTINCT lp.path FROM library_path lp")
        .fetch_all(st.db.pool())
        .await
        .unwrap_or_default();
    rows.into_iter().map(PathBuf::from).collect()
}

/// POST /admin/library/scan/start：入队异步扫描（scan_job 化），返回 job id。
pub(super) async fn start_scan(
    State(st): State<AppState>,
    body: Option<axum::extract::Json<ScanInput>>,
) -> Response {
    let path = body.and_then(|axum::extract::Json(b)| b.path);

    // 解析要扫描的媒体库——扫描只作用于已登记的库，**绝不在扫描时新建库**（新建请到「媒体库」页）。
    // 按 library_id 去重：一库多挂载点只入队一次（Pipeline 消费时按库扫描其全部 path，避免重复扫描）。
    let libs: Vec<(i64, String)> = match path.as_deref().map(str::trim) {
        Some(p) if !p.is_empty() => {
            match sqlx::query_as::<_, (i64, String)>(
                "SELECT l.id, l.name FROM library l \
                 JOIN library_path lp ON lp.library_id = l.id \
                 WHERE lp.path = ? LIMIT 1",
            )
            .bind(p)
            .fetch_optional(st.db.pool())
            .await
            {
                Ok(Some(row)) => vec![row],
                Ok(None) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        "该路径未登记为媒体库，请先在「媒体库」页新建",
                    )
                        .into_response();
                }
                Err(e) => {
                    tracing::error!(error = %e, "start_scan: resolve library failed");
                    return (StatusCode::INTERNAL_SERVER_ERROR, "扫描入队失败").into_response();
                }
            }
        }
        _ => match sqlx::query_as::<_, (i64, String)>(
            "SELECT l.id, l.name FROM library l \
             WHERE l.id IN (SELECT DISTINCT library_id FROM library_path) \
             ORDER BY l.id",
        )
        .fetch_all(st.db.pool())
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                tracing::error!(error = %e, "start_scan: list libraries failed");
                return (StatusCode::INTERNAL_SERVER_ERROR, "扫描入队失败").into_response();
            }
        },
    };
    if libs.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "无可扫描的媒体库（先在「媒体库」页新建）",
        )
            .into_response();
    }

    // 入队：每库一行 scan_job(pending)；Pipeline scan 循环是唯一消费者。
    let stage = ScanStage::new(st.db.clone());
    let roots_display: Vec<String> = libs.iter().map(|(_, n)| n.clone()).collect();
    let mut job_rows: Vec<i64> = Vec::new();
    for (lid, _) in &libs {
        match stage.create_scan_job(*lid, "admin").await {
            Ok(id) => job_rows.push(id),
            Err(e) => {
                tracing::warn!(library_id = lid, error = %e, "扫描入队失败");
            }
        }
    }
    if job_rows.is_empty() {
        return (StatusCode::INTERNAL_SERVER_ERROR, "扫描入队失败").into_response();
    }
    st.pipeline.notify_scan();

    let db = st.db.clone();
    let jobs = st.jobs.clone();

    let job_id = st.jobs.spawn("scan", move |job_id| async move {
        let poll_rows = || async { scan_job_store::poll_status_batch(&db, &job_rows).await };

        let mut canceled = false;
        loop {
            let rows = poll_rows().await;
            let active = rows
                .iter()
                .filter(|r| r.status == "pending" || r.status == "running")
                .count();
            if active == 0 {
                break;
            }
            if jobs.is_cancelled(&job_id) {
                // 协作式取消：撤销尚未开跑的行；running 行由流水线跑完（粒度同旧实现）
                scan_job_store::cancel_pending_batch(&db, &job_rows).await;
                canceled = true;
                break;
            }
            let pending = rows.iter().filter(|r| r.status == "pending").count();
            jobs.set_progress(
                &job_id,
                format!("扫描中 running={} pending={}", active - pending, pending),
            );
            tokio::time::sleep(Duration::from_millis(800)).await;
        }

        // 终态汇总（media 对齐旧口径：updated_items 即扫描媒体数）
        let rows = poll_rows().await;
        let media: i64 = rows
            .iter()
            .filter(|r| r.status != "canceled")
            .map(|r| r.updated)
            .sum();
        let errors: i64 = rows.iter().filter(|r| r.status == "failed").count() as i64;

        Ok(json!({
            "roots": roots_display,
            "media": media,
            "errors": errors,
            "jobs": job_rows.len(),
            "canceled": canceled,
        }))
    });

    axum::Json(json!({ "job_id": job_id.to_string(), "kind": "scan" })).into_response()
}

/// GET /admin/library/scan/{id}：扫描 job 状态轮询。
pub(super) async fn get_scan_job(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    match uuid::Uuid::parse_str(&id) {
        Ok(uid) => match st.jobs.get(&uid) {
            Some(view) => {
                let mut body = view.to_json();
                // running 状态附带取消入口说明
                if view.status == JobStatus::Running {
                    body["cancel_hint"] = json!("DELETE /admin/library/scan/{id} 可取消");
                }
                axum::Json(body).into_response()
            }
            None => StatusCode::NOT_FOUND.into_response(),
        },
        Err(_) => (StatusCode::BAD_REQUEST, "job id 格式错误").into_response(),
    }
}

/// DELETE /admin/library/scan/{id}：请求取消扫描 job（协作式）。
/// 对齐 `cancel_hint`：仅 running 任务可取消；已完成/不存在返回 404。
pub(super) async fn cancel_scan_job(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let Ok(uid) = uuid::Uuid::parse_str(&id) else {
        return (StatusCode::BAD_REQUEST, "job id 格式错误").into_response();
    };
    match st.jobs.cancel(&uid) {
        true => StatusCode::NO_CONTENT.into_response(),
        false => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Deserialize, Default)]
pub(super) struct ScrapeInput {
    /// 指定库 ID；缺省刮削全部库。
    library_id: Option<i64>,
    /// 强制重新刮削（忽略已有 tmdb_id，全部重置为 pending）。
    #[serde(default)]
    force: bool,
}

/// 范围内条目状态分布（movie/series）。
async fn scrape_distribution(
    db: &Db,
    library_id: Option<i64>,
    only_missing_tmdb: bool,
) -> Vec<(String, i64)> {
    let mut sql = String::from(
        "SELECT scrape_status, COUNT(*) FROM item \
         WHERE type IN ('movie', 'series')",
    );
    if only_missing_tmdb {
        sql.push_str(" AND (tmdb_id IS NULL OR tmdb_id = '')");
    }
    if library_id.is_some() {
        sql.push_str(" AND library_id = ?");
    }
    sql.push_str(" GROUP BY scrape_status");
    let mut q = sqlx::query_as::<_, (String, i64)>(&sql);
    if let Some(lid) = library_id {
        q = q.bind(lid);
    }
    q.fetch_all(db.pool()).await.unwrap_or_default()
}

/// POST /admin/library/scrape/start：元数据刮削。force 时重置范围条目为 pending
/// 并等待 Scrape 阶段清空 backlog；未配置 TMDB key 时候选计 skipped 立即完成（旧契约）。
pub(super) async fn start_scrape(
    State(st): State<AppState>,
    body: Option<axum::extract::Json<ScrapeInput>>,
) -> Response {
    let (library_id, force) = match body {
        Some(axum::extract::Json(b)) => (b.library_id.filter(|&v| v > 0), b.force),
        None => (None, false),
    };
    let tmdb_configured = !st.cfg.tmdb.api_key.is_empty();
    let db = st.db.clone();
    let jobs = st.jobs.clone();

    // force：无论是否配置 key 都执行状态重置（未配置时条目滞留 pending，
    // 配置后由流水线自然消化——与旧行为一致）
    let mut reset_count = 0i64;
    if force {
        let mut q = sqlx::query(
            "UPDATE item SET scrape_status = 'pending', scrape_attempts = 0, updated_at = ? \
             WHERE type IN ('movie', 'series')",
        )
        .bind(crate::emby::format_time_now());
        if let Some(lid) = library_id {
            q = sqlx::query(
                "UPDATE item SET scrape_status = 'pending', scrape_attempts = 0, updated_at = ? \
                 WHERE type IN ('movie', 'series') AND library_id = ?",
            )
            .bind(crate::emby::format_time_now())
            .bind(lid);
        }
        match q.execute(db.pool()).await {
            Ok(r) => reset_count = r.rows_affected() as i64,
            Err(e) => {
                tracing::error!(error = %e, "scrape force 重置失败");
                return (StatusCode::INTERNAL_SERVER_ERROR, "重置失败").into_response();
            }
        }
        st.pipeline.notify_scrape();
    }

    let job_id = st.jobs.spawn("scrape", move |job_id| async move {
        // 未配置 key：沿用旧契约，候选计 skipped 后立即完成
        if !tmdb_configured {
            let rows = scrape_distribution(&db, library_id, !force).await;
            let skipped: i64 = rows.iter().map(|(_, c)| *c).sum();
            return Ok(json!({
                "library_id": library_id,
                "force": force,
                "reset": reset_count,
                "scraped": 0,
                "skipped": skipped,
                "none": 0,
                "failed": 0,
                "tmdb_configured": false,
            }));
        }

        // 已配置：非 force 仅上报分布快照（后台自动消化）；force 等 backlog 清空
        let snapshot = |rows: Vec<(String, i64)>| {
            let pick = |name: &str| {
                rows.iter()
                    .find(|(s, _)| s == name)
                    .map(|(_, c)| *c)
                    .unwrap_or(0)
            };
            json!({
                "pending": pick("pending"),
                "scraped": pick("scraped"),
                "none": pick("none"),
                "failed": pick("failed"),
            })
        };

        let base = json!({
            "library_id": library_id,
            "force": force,
            "reset": reset_count,
            "tmdb_configured": true,
        });

        if !force {
            let rows = scrape_distribution(&db, library_id, false).await;
            let pending_now = rows
                .iter()
                .find(|(s, _)| s == "pending")
                .map(|(_, c)| *c)
                .unwrap_or(0);
            jobs.set_progress(&job_id, format!("当前 pending {pending_now}"));
            return Ok(merge_json(base, snapshot(rows)));
        }

        loop {
            let rows = scrape_distribution(&db, library_id, false).await;
            let remaining = rows
                .iter()
                .filter(|(s, _)| s == "pending" || s == "scraping")
                .map(|(_, c)| *c)
                .sum::<i64>();
            if remaining == 0 {
                let final_dist = snapshot(rows);
                return Ok(merge_json(base, final_dist));
            }
            if jobs.is_cancelled(&job_id) {
                return Ok(merge_json(base, json!({ "canceled": true })));
            }
            jobs.set_progress(&job_id, format!("刮削中 剩余 {remaining}"));
            tokio::time::sleep(Duration::from_millis(800)).await;
        }
    });

    axum::Json(json!({ "job_id": job_id.to_string(), "kind": "scrape", "force": force }))
        .into_response()
}

/// 浅合并两个 JSON 对象（a 为底，b 覆盖同名键）。
fn merge_json(a: serde_json::Value, b: serde_json::Value) -> serde_json::Value {
    let mut out = a;
    if let (Some(map_a), Some(map_b)) = (out.as_object_mut(), b.as_object()) {
        for (k, v) in map_b {
            map_a.insert(k.clone(), v.clone());
        }
    }
    out
}

/// GET /admin/library/scrape/{id}：刮削 job 状态轮询。
pub(super) async fn get_scrape_job(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    match uuid::Uuid::parse_str(&id) {
        Ok(uid) => match st.jobs.get(&uid) {
            Some(view) => axum::Json(view.to_json()).into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        },
        Err(_) => (StatusCode::BAD_REQUEST, "job id 格式错误").into_response(),
    }
}

/// probe/start 查询参数。
#[derive(Debug, Default, Deserialize)]
pub(super) struct ProbeOpts {
    /// 强制回填：true 时重探所有本地视频，否则仅缺失流信息的。
    force: Option<String>,
}

impl ProbeOpts {
    fn force(&self) -> bool {
        self.force
            .as_deref()
            .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false)
    }
}

/// POST /admin/library/probe/start：对本地视频执行 ffprobe 探测并回填流信息。
/// 默认仅处理缺失 metadata 的视频；`?force=1` 强制回填所有本地视频。
/// （自动探测由 Probe 阶段消费 media_source.status='pending' 完成，此处是手动工具。）
pub(super) async fn start_probe(
    State(st): State<AppState>,
    Query(opts): Query<ProbeOpts>,
) -> Response {
    use emrs_infra::probe::{container_for, probe_duration, probe_media};

    // 缺失流信息/时长的本地视频：metadata 为 NULL / 空串 / "[]"（无 ffprobe 时的
    // 空结果），或时长缺失（fragmented MP4 等头部解析拿不到、需要 ffprobe 回填的容器）。
    let metadata_condition = if opts.force() {
        // 强制回填：查询所有本地视频（不删旧数据，探测后直接覆盖）
        String::new()
    } else {
        " AND (ms.metadata IS NULL OR ms.metadata = '' OR ms.metadata = '[]' \
         OR ms.file_duration IS NULL)"
            .to_string()
    };
    let sql = format!(
        "SELECT ms.uuid, COALESCE(ms.path, ms.remote_path) AS path_url FROM media_source ms \
         WHERE ms.protocol = 'file'{metadata_condition}"
    );
    let rows: Vec<(String, Option<String>)> =
        match sqlx::query_as(&sql).fetch_all(st.db.pool()).await {
            Ok(rows) => rows,
            Err(e) => {
                tracing::error!(error = %e, "probe: 查询待探测媒体失败");
                return (StatusCode::INTERNAL_SERVER_ERROR, "查询待探测媒体失败").into_response();
            }
        };
    let pending = rows.len();
    if pending == 0 {
        return axum::Json(json!({
            "job_id": null,
            "kind": "probe",
            "force": opts.force(),
            "pending": 0,
            "message": "没有需要回填流信息的本地视频",
        }))
        .into_response();
    }

    let db = st.db.clone();
    let jobs = st.jobs.clone();
    let force = opts.force();
    let job_id = st.jobs.spawn("probe", move |job_id| async move {
        let mut updated = 0usize;
        let mut missing = 0usize;
        let mut failed = 0usize;
        for (i, (uuid, path_url)) in rows.iter().enumerate() {
            if jobs.is_cancelled(&job_id) {
                break;
            }
            jobs.set_progress(&job_id, format!("探测 {i}/{}", rows.len()));

            let local = path_url
                .as_deref()
                .map(|s| s.strip_prefix("file://").unwrap_or(s))
                .unwrap_or("");
            let p = std::path::Path::new(local);
            if !p.exists() {
                missing += 1;
                continue;
            }
            let media = probe_media(p).await;
            // 原生头部解析优先，ffprobe format.duration 兜底（与 ProbeStage 同规则）
            let file_second = probe_duration(p).await.or(media.format_duration);
            let file_size = tokio::fs::metadata(p).await.map(|m| m.len() as i64).ok();
            let container = p
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase())
                .and_then(|e| container_for(e.as_str()))
                .map(String::from);
            let metadata = serde_json::to_string(&media.streams).unwrap_or_else(|_| "[]".to_string());
            let chapters_json =
                serde_json::to_string(&media.chapters).unwrap_or_else(|_| "[]".to_string());
            let now = crate::emby::format_time_now();

            let res = sqlx::query(
                "UPDATE media_source \
                 SET metadata = ?, chapters = ?, file_size = ?, file_duration = ?, container = ?, updated_at = ? \
                 WHERE uuid = ?",
            )
            .bind(&metadata)
            .bind(&chapters_json)
            .bind(file_size)
            .bind(file_second)
            .bind(&container)
            .bind(&now)
            .bind(uuid)
            .execute(db.pool())
            .await;
            match res {
                Ok(r) if r.rows_affected() > 0 => updated += 1,
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(uuid, error = %e, "probe: 回填 file_metadata 失败");
                    failed += 1;
                }
            }
        }
        Ok(json!({
            "kind": "probe",
            "force": force,
            "pending": pending,
            "updated": updated,
            "missing": missing,
            "failed": failed,
        }))
    });

    axum::Json(json!({ "job_id": job_id.to_string(), "kind": "probe", "force": force, "pending": pending }))
        .into_response()
}

/// GET /admin/library/probe/{id}：流信息回填 job 状态轮询。
pub(super) async fn get_probe_job(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    match uuid::Uuid::parse_str(&id) {
        Ok(uid) => match st.jobs.get(&uid) {
            Some(view) => axum::Json(view.to_json()).into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        },
        Err(_) => (StatusCode::BAD_REQUEST, "job id 格式错误").into_response(),
    }
}

#[derive(Deserialize)]
pub(super) struct WatchInput {
    /// 监听根；缺省监听全部库根。
    roots: Option<Vec<String>>,
}

/// POST /admin/library/watch/start：开始目录监听。
pub(super) async fn start_watch(
    State(st): State<AppState>,
    body: Option<axum::extract::Json<WatchInput>>,
) -> Response {
    let roots: Vec<PathBuf> = match body.and_then(|axum::extract::Json(b)| b.roots) {
        Some(list) if !list.is_empty() => list.into_iter().map(PathBuf::from).collect(),
        _ => library_roots(&st).await,
    };
    if roots.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "无可监听的库根（先创建库或传 roots）",
        )
            .into_response();
    }

    match st.watcher.start(roots).await {
        Ok((ok, failed)) => axum::Json(json!({ "watching": ok, "failed": failed })).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "start_watch failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "启动监听失败").into_response()
        }
    }
}

/// GET /admin/library/watch：监听状态。
pub(super) async fn watch_status(State(st): State<AppState>) -> Response {
    axum::Json(st.watcher.status().await).into_response()
}

/// DELETE /admin/library/watch：停止监听。
pub(super) async fn stop_watch(State(st): State<AppState>) -> Response {
    st.watcher.stop().await;
    StatusCode::NO_CONTENT.into_response()
}
