//! 库管理 + 媒体管理 + 人工裁决/手动识别端点。

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use emrs_core::importer::scanner::normalize_canonical_path;

use crate::state::AppState;

#[derive(Deserialize)]
pub(super) struct LibraryInput {
    name: String,
    /// 同一媒体库的多个物理挂载点（至少一个）。
    #[serde(default)]
    paths: Vec<String>,
    collection_type: Option<String>,
}

/// 归一化库路径：与扫描器 [`Scanner::scan_path`] 保持一致——canonicalize 后剥掉
/// Windows verbatim 前缀（`\\?\` / `\\?\UNC\`）并统一正斜杠，避免存成 `//?/D:/...`
/// 这种异常形式；canonicalize 失败（路径暂不存在）时退化为反斜杠转正斜杠，保证去重可比。
///
/// 异步：`tokio::fs::canonicalize` 内部走 spawn_blocking，避免在 axum handler 中
/// 同步阻塞 tokio worker（慢盘/网络挂载时尤为关键）。
async fn normalize_library_path(path: &str) -> String {
    let p = path.trim();
    match tokio::fs::canonicalize(p).await {
        Ok(c) => normalize_canonical_path(&c),
        Err(_) => p.replace('\\', "/"),
    }
}

/// 解析有效原始路径列表：逐项去首尾空白并丢弃空串。不做归一化
/// （归一化统一交给 [`normalize_paths`]，那里按归一值去重）。
fn resolve_raw_paths(input: &LibraryInput) -> Vec<String> {
    input
        .paths
        .iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

/// 逐个归一化并按归一值去重（保序）。返回归一化后的非空路径列表。
async fn normalize_paths(raw: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(raw.len());
    for p in raw {
        let n = normalize_library_path(p).await;
        if !n.is_empty() && !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

/// 查一个库的全部挂载点路径（按 sort_order, id 排序）。
async fn fetch_library_paths(
    pool: &sqlx::AnyPool,
    library_id: i64,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        "SELECT path FROM library_path WHERE library_id = ? ORDER BY sort_order, id",
    )
    .bind(library_id)
    .fetch_all(pool)
    .await
}

/// 批量查多个库的挂载点，返回 `library_id -> [path...]`（各自按 sort_order, id 排序）。
async fn fetch_paths_by_library(
    pool: &sqlx::AnyPool,
) -> Result<std::collections::HashMap<i64, Vec<String>>, sqlx::Error> {
    let rows = sqlx::query_as::<_, (i64, String)>(
        "SELECT library_id, path FROM library_path ORDER BY library_id, sort_order, id",
    )
    .fetch_all(pool)
    .await?;
    let mut map: std::collections::HashMap<i64, Vec<String>> = std::collections::HashMap::new();
    for (lib_id, path) in rows {
        map.entry(lib_id).or_default().push(path);
    }
    Ok(map)
}

/// 向 library_path 写入一批挂载点（按列表顺序赋 sort_order，path_type 固定 local）。
/// 供 create 复用；update 走事务内先 DELETE 再本函数重插。
async fn insert_library_paths(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    library_id: i64,
    paths: &[String],
    now: &str,
) -> Result<(), sqlx::Error> {
    for (i, p) in paths.iter().enumerate() {
        sqlx::query(
            "INSERT INTO library_path (library_id, path, path_type, sort_order, updated_at) \
             VALUES (?, ?, 'local', ?, ?)",
        )
        .bind(library_id)
        .bind(p)
        .bind(i as i64)
        .bind(now)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// GET /admin/libraries：列出所有库。
pub(super) async fn list_libraries(State(st): State<AppState>) -> Response {
    // 返回全部挂载点 paths（按 sort_order, id 排序）。
    // 一次批取所有 library_path 分组，避免逐库查询的 N+1。
    let lib_rows = sqlx::query_as::<_, (i64, String, String, String)>(
        "SELECT id, name, created_at, collection_type FROM library ORDER BY id",
    )
    .fetch_all(st.db.pool())
    .await;
    let paths_map = fetch_paths_by_library(st.db.pool()).await;

    match (lib_rows, paths_map) {
        (Ok(list), Ok(map)) => {
            let items: Vec<Value> = list
                .into_iter()
                .map(|(id, name, created_at, collection_type)| {
                    let paths = map.get(&id).cloned().unwrap_or_default();
                    json!({
                        "id": id,
                        "name": name,
                        "paths": paths,
                        "created_at": created_at,
                        "collection_type": collection_type,
                    })
                })
                .collect();
            axum::Json(json!({ "items": items })).into_response()
        }
        (Err(e), _) | (_, Err(e)) => {
            tracing::error!(error = %e, "list_libraries failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// POST /admin/libraries：创建库。
pub(super) async fn create_library(
    State(st): State<AppState>,
    axum::extract::Json(body): axum::extract::Json<LibraryInput>,
) -> Response {
    let raw = resolve_raw_paths(&body);
    if body.name.is_empty() || raw.is_empty() {
        return (StatusCode::BAD_REQUEST, "name 和至少一个 path 不能为空").into_response();
    }
    if let Some(ct) = &body.collection_type
        && !emrs_core::stores::is_valid_collection_type(ct)
    {
        return (
            StatusCode::BAD_REQUEST,
            format!("非法 collection_type: {ct}"),
        )
            .into_response();
    }
    let paths = normalize_paths(&raw).await;
    if paths.is_empty() {
        return (StatusCode::BAD_REQUEST, "路径非法（归一化后为空）").into_response();
    }

    // 单路径去重：同路径库已存在时直接返回已有 id（防重复建库导致扫描翻倍）。
    // 多路径新建不去重——用户显式声明一个新库的多挂载点，语义上应新建。
    if paths.len() == 1
        && let Ok(Some(id)) = sqlx::query_scalar::<_, i64>(
            "SELECT lp.library_id FROM library_path lp \
             JOIN library l ON l.id = lp.library_id \
             WHERE lp.path = ? LIMIT 1",
        )
        .bind(&paths[0])
        .fetch_optional(st.db.pool())
        .await
    {
        return axum::Json(json!({ "id": id, "name": body.name, "existing": true }))
            .into_response();
    }

    let now = emrs_core::emby::format_time_now();
    let result: Result<i64, sqlx::Error> = async {
        let mut tx = st.db.pool().begin().await?;
        sqlx::query("INSERT INTO library (name, collection_type) VALUES (?, ?)")
            .bind(&body.name)
            .bind(body.collection_type.as_deref().unwrap_or("tvshows"))
            .execute(&mut *tx)
            .await?;
        // sqlx Any 池下 last_insert_id 不可靠，回查取 id（三方言通用）
        let id: i64 = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM library WHERE name = ? ORDER BY id DESC LIMIT 1",
        )
        .bind(&body.name)
        .fetch_one(&mut *tx)
        .await?;
        insert_library_paths(&mut tx, id, &paths, &now).await?;
        tx.commit().await?;
        Ok(id)
    }
    .await;

    match result {
        Ok(id) => {
            axum::Json(json!({ "id": id, "name": body.name, "paths": paths })).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "create_library failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "创建失败").into_response()
        }
    }
}

/// GET /admin/libraries/{id}：查单个库（含全部挂载点 paths）。
pub(super) async fn get_library(State(st): State<AppState>, Path(id): Path<i64>) -> Response {
    let row = sqlx::query_as::<_, (i64, String, String, String)>(
        "SELECT id, name, created_at, collection_type FROM library WHERE id = ? LIMIT 1",
    )
    .bind(id)
    .fetch_optional(st.db.pool())
    .await;

    match row {
        Ok(Some((lid, name, created_at, collection_type))) => {
            let paths = fetch_library_paths(st.db.pool(), lid)
                .await
                .unwrap_or_default();
            axum::Json(json!({
                "id": lid,
                "name": name,
                "paths": paths,
                "created_at": created_at,
                "collection_type": collection_type,
            }))
            .into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(error = %e, "get_library failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// PUT /admin/libraries/{id}：更新库（名称/类型 + 全量重设挂载点集合）。
pub(super) async fn update_library(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    axum::extract::Json(body): axum::extract::Json<LibraryInput>,
) -> Response {
    // 先校验（fail-fast，与 create_library 一致），再做路径 canonicalize。
    if body.name.is_empty() {
        return (StatusCode::BAD_REQUEST, "name 不能为空").into_response();
    }
    if let Some(ct) = &body.collection_type
        && !emrs_core::stores::is_valid_collection_type(ct)
    {
        return (
            StatusCode::BAD_REQUEST,
            format!("非法 collection_type: {ct}"),
        )
            .into_response();
    }
    let raw = resolve_raw_paths(&body);
    if raw.is_empty() {
        return (StatusCode::BAD_REQUEST, "至少需要一个 path").into_response();
    }
    let paths = normalize_paths(&raw).await;
    if paths.is_empty() {
        return (StatusCode::BAD_REQUEST, "路径非法（归一化后为空）").into_response();
    }

    let now = emrs_core::emby::format_time_now();
    let result: Result<bool, sqlx::Error> = async {
        let mut tx = st.db.pool().begin().await?;
        let r = sqlx::query(
            "UPDATE library SET name = ?, collection_type = COALESCE(?, collection_type), updated_at = ? WHERE id = ?",
        )
        .bind(&body.name)
        .bind(&body.collection_type)
        .bind(&now)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        if r.rows_affected() == 0 {
            // 库不存在：回滚（无副作用）
            return Ok(false);
        }
        // 全量重设挂载点：无外部引用 library_path.id，删除重插最稳妥（保留库内路径唯一性由 sort_order 序号保证）。
        sqlx::query("DELETE FROM library_path WHERE library_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        insert_library_paths(&mut tx, id, &paths, &now).await?;
        tx.commit().await?;
        Ok(true)
    }
    .await;

    match result {
        Ok(true) => StatusCode::OK.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(error = %e, "update_library failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// DELETE /admin/libraries/{id}：物理删除库及全部关联数据。
///
/// 级联清理（事务内）：external_subtitle → media_source → item 关联表/图片/
/// 用户进度 → item → library_path → scan_job → library。
pub(super) async fn delete_library(State(st): State<AppState>, Path(id): Path<i64>) -> Response {
    let delete = async {
        let mut tx = st.db.pool().begin().await?;
        sqlx::query(
            "DELETE FROM external_subtitle WHERE media_source_id IN \
             (SELECT id FROM media_source WHERE item_id IN (SELECT id FROM item WHERE library_id = ?))",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "DELETE FROM media_source WHERE item_id IN (SELECT id FROM item WHERE library_id = ?)",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "DELETE FROM item_genre WHERE item_id IN (SELECT id FROM item WHERE library_id = ?)",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "DELETE FROM item_people WHERE item_id IN (SELECT id FROM item WHERE library_id = ?)",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "DELETE FROM item_studio WHERE item_id IN (SELECT id FROM item WHERE library_id = ?)",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "DELETE FROM item_tag WHERE item_id IN (SELECT id FROM item WHERE library_id = ?)",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "DELETE FROM item_image WHERE parent_type = 'item' AND parent_id IN \
             (SELECT id FROM item WHERE library_id = ?)",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "DELETE FROM user_item_data WHERE item_id IN (SELECT id FROM item WHERE library_id = ?)",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM item WHERE library_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM library_path WHERE library_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM scan_job WHERE library_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        let res = sqlx::query("DELETE FROM library WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        anyhow::Ok(res.rows_affected() > 0)
    };

    match delete.await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(error = %e, "delete_library failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Deserialize, Default)]
pub(super) struct TreeChildrenQuery {
    /// 展开某媒体库顶层（movie/series）。与 `parent_id` 二选一。
    library_id: Option<i64>,
    /// 展开某条目下一层（series→season / season→episode）。与 `library_id` 二选一。
    parent_id: Option<i64>,
}

/// 把一个树节点拼成统一 JSON。`media` 仅 episode 叶子有值（首源路径）。
/// `has_children` 由类型派生：series（有季）/ season（有集）可展开，movie/episode 为叶子。
fn tree_node(
    id: i64,
    title: String,
    item_type: &str,
    season_number: Option<i64>,
    episode_number: Option<i64>,
    is_virtual: bool,
    media: Option<Value>,
) -> Value {
    let has_children = matches!(item_type, "series" | "season");
    json!({
        "id": id,
        "title": title,
        "type": item_type,
        "has_children": has_children,
        "season_number": season_number,
        "episode_number": episode_number,
        "is_virtual": is_virtual,
        "media": media,
    })
}

/// GET /admin/tree/children?library_id=N | ?parent_id=M：后台媒体树单层懒加载。
///
/// - `library_id` → 该库顶层条目（movie/series），series 可展开（`has_children`）。
/// - `parent_id` → 按父类型下钻：series→season 列表；season→episode 列表（批取首源路径）；
///   movie/episode/不存在 → 空。
///
/// 方言安全：`?` 占位符、不用 `||` 拼接、`is_virtual` 按 i64 读再转 bool。
pub(super) async fn list_tree_children(
    State(st): State<AppState>,
    Query(q): Query<TreeChildrenQuery>,
) -> Response {
    let pool = st.db.pool();

    // 分支 1：库顶层 movie/series。
    if let Some(lib) = q.library_id {
        let rows = sqlx::query_as::<_, (i64, String, String)>(
            "SELECT id, title, type FROM item \
             WHERE library_id = ? AND type IN ('movie','series') ORDER BY title",
        )
        .bind(lib)
        .fetch_all(pool)
        .await;
        return match rows {
            Ok(list) => {
                let items: Vec<Value> = list
                    .into_iter()
                    .map(|(id, title, ty)| tree_node(id, title, &ty, None, None, false, None))
                    .collect();
                axum::Json(json!({ "items": items })).into_response()
            }
            Err(e) => {
                tracing::error!(error = %e, "list_tree_children(library) failed");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        };
    }

    // 分支 2：按父条目下钻。
    let Some(parent) = q.parent_id else {
        return (
            StatusCode::BAD_REQUEST,
            "library_id 或 parent_id 必须提供其一",
        )
            .into_response();
    };
    let parent_type = sqlx::query_scalar::<_, String>("SELECT type FROM item WHERE id = ? LIMIT 1")
        .bind(parent)
        .fetch_optional(pool)
        .await;
    let parent_type = match parent_type {
        Ok(Some(t)) => t,
        Ok(None) => return axum::Json(json!({ "items": [] })).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "list_tree_children: query parent type failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    match parent_type.as_str() {
        "series" => {
            let rows = sqlx::query_as::<_, (i64, String, Option<i64>, i64)>(
                "SELECT id, title, season_number, is_virtual FROM item \
                 WHERE parent_id = ? AND type = 'season' ORDER BY season_number, id",
            )
            .bind(parent)
            .fetch_all(pool)
            .await;
            match rows {
                Ok(list) => {
                    let items: Vec<Value> = list
                        .into_iter()
                        .map(|(id, title, season_number, is_virtual)| {
                            tree_node(
                                id,
                                title,
                                "season",
                                season_number,
                                None,
                                is_virtual == 1,
                                None,
                            )
                        })
                        .collect();
                    axum::Json(json!({ "items": items })).into_response()
                }
                Err(e) => {
                    tracing::error!(error = %e, "list_tree_children(seasons) failed");
                    StatusCode::INTERNAL_SERVER_ERROR.into_response()
                }
            }
        }
        "season" => {
            let rows = sqlx::query_as::<_, (i64, String, Option<i64>, i64)>(
                "SELECT id, title, episode_number, is_virtual FROM item \
                 WHERE parent_id = ? AND type = 'episode' ORDER BY episode_number, id",
            )
            .bind(parent)
            .fetch_all(pool)
            .await;
            let list = match rows {
                Ok(l) => l,
                Err(e) => {
                    tracing::error!(error = %e, "list_tree_children(episodes) failed");
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
            };
            // 单表 IN 批取各集首源（id 最小）路径，避免 N+1。
            let ep_ids: Vec<i64> = list.iter().map(|(id, _, _, _)| *id).collect();
            let media_map = match fetch_episode_media(pool, &ep_ids).await {
                Ok(m) => m,
                Err(e) => {
                    tracing::error!(error = %e, "list_tree_children(episode media) failed");
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
            };
            let items: Vec<Value> = list
                .into_iter()
                .map(|(id, title, episode_number, is_virtual)| {
                    let media = media_map.get(&id).map(|(name, path_type, path_url)| {
                        json!({ "name": name, "path_type": path_type, "path_url": path_url })
                    });
                    tree_node(
                        id,
                        title,
                        "episode",
                        None,
                        episode_number,
                        is_virtual == 1,
                        media,
                    )
                })
                .collect();
            axum::Json(json!({ "items": items })).into_response()
        }
        // movie / episode 无 item 子级。
        _ => axum::Json(json!({ "items": [] })).into_response(),
    }
}

/// 批取一批 episode 的首个 `media_source`（同 item 下 id 最小者）的展示字段。
/// 返回 `item_id → (name, path_type, path_url)`。`path_type` 复刻协议归一
/// （file→local / strm→strm / webdavs→webdav）；`path_url` = `COALESCE(path, remote_path)`。
async fn fetch_episode_media(
    pool: &sqlx::AnyPool,
    item_ids: &[i64],
) -> Result<
    std::collections::HashMap<i64, (Option<String>, Option<String>, Option<String>)>,
    sqlx::Error,
> {
    let mut out = std::collections::HashMap::new();
    if item_ids.is_empty() {
        return Ok(out);
    }
    let placeholders = std::iter::repeat_n("?", item_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT item_id, name, \
                CASE protocol WHEN 'file' THEN 'local' WHEN 'strm' THEN 'strm' \
                              WHEN 'webdavs' THEN 'webdav' ELSE protocol END AS path_type, \
                COALESCE(path, remote_path) AS path_url \
         FROM media_source WHERE item_id IN ({placeholders}) ORDER BY item_id, id"
    );
    let mut q = sqlx::query_as::<_, (i64, Option<String>, Option<String>, Option<String>)>(&sql);
    for id in item_ids {
        q = q.bind(id);
    }
    let rows = q.fetch_all(pool).await?;
    for (item_id, name, path_type, path_url) in rows {
        // 已按 item_id, id 升序，保留首源（id 最小）。
        out.entry(item_id).or_insert((name, path_type, path_url));
    }
    Ok(out)
}

#[derive(Deserialize, Default)]
pub(super) struct ItemsByStatusQuery {
    scrape_status: Option<String>,
}

/// GET /admin/library/items?scrape_status=none：列出指定状态的 item。
/// 状态词表：pending / scraping / scraped / none / failed。
pub(super) async fn list_items_by_scrape_status(
    State(st): State<AppState>,
    Query(q): Query<ItemsByStatusQuery>,
) -> Response {
    let status = q.scrape_status.as_deref().unwrap_or("pending");
    let rows = sqlx::query_as::<_, (i64, String, String, Option<String>, Option<String>)>(
        "SELECT id, title, type, scrape_status, tmdb_id FROM item \
         WHERE scrape_status = ? ORDER BY id LIMIT 200",
    )
    .bind(status)
    .fetch_all(st.db.pool())
    .await;

    match rows {
        Ok(list) => {
            let items: Vec<Value> = list
                .into_iter()
                .map(|(id, title, item_type, scrape_status, tmdb_id)| {
                    json!({
                        "id": id,
                        "title": title,
                        "type": item_type,
                        "scrape_status": scrape_status,
                        "tmdb_id": tmdb_id,
                    })
                })
                .collect();
            axum::Json(json!({ "items": items })).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "list_items_by_scrape_status failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Deserialize)]
pub(super) struct ManualIdentifyInput {
    tmdb_id: String,
    /// 指定后是否清除旧元数据（genre/people/studio 关联、图片与文本字段）。
    /// 默认 true——手动指定通常意味着旧匹配有误；传 false 保留全部旧关联仅换 ID。
    #[serde(default)]
    clear_metadata: Option<bool>,
}

/// POST /admin/library/items/{id}/identify：手动指定 tmdb_id（对齐设计方案 §手动指定）。
///
/// 写入 `scrape_status='pending'`（attempts 归零）并唤醒 Scrape 阶段按 ID 快路径重刮。
/// 仅 movie/series 可手动识别——季/集由父级派生，不接受直接指定。
pub(super) async fn manual_identify_item(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    axum::extract::Json(body): axum::extract::Json<ManualIdentifyInput>,
) -> Response {
    let now = emrs_core::emby::format_time_now();
    // 仅顶层类型可手动识别（子级随父级派生）
    let affected = sqlx::query(
        "UPDATE item SET tmdb_id = ?, imdb_id = NULL, tvdb_id = NULL, \
         scrape_status = 'pending', scrape_attempts = 0, updated_at = ? \
         WHERE id = ? AND type IN ('movie', 'series')",
    )
    .bind(&body.tmdb_id)
    .bind(&now)
    .bind(id)
    .execute(st.db.pool())
    .await;

    match affected {
        Ok(r) if r.rows_affected() > 0 => {}
        Ok(_) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(error = %e, "manual_identify_item failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }

    if body.clear_metadata.unwrap_or(true) {
        // 派生关联表物理删除
        let _ = sqlx::query("DELETE FROM item_genre WHERE item_id = ?")
            .bind(id)
            .execute(st.db.pool())
            .await;
        let _ = sqlx::query("DELETE FROM item_people WHERE item_id = ?")
            .bind(id)
            .execute(st.db.pool())
            .await;
        let _ = sqlx::query("DELETE FROM item_studio WHERE item_id = ?")
            .bind(id)
            .execute(st.db.pool())
            .await;
        let _ = sqlx::query("DELETE FROM item_tag WHERE item_id = ?")
            .bind(id)
            .execute(st.db.pool())
            .await;
        let _ = sqlx::query("DELETE FROM item_image WHERE parent_type = 'item' AND parent_id = ?")
            .bind(id)
            .execute(st.db.pool())
            .await;
        let _ = sqlx::query(
            "UPDATE item SET description = NULL, tagline = NULL, end_date = NULL, \
             runtime = NULL, community_rating = NULL, official_rating = NULL, \
             updated_at = ? WHERE id = ?",
        )
        .bind(&now)
        .bind(id)
        .execute(st.db.pool())
        .await;
    }

    // 唤醒 Scrape 阶段立即消费（按 TMDB ID 快路径）
    st.pipeline.notify_scrape();
    axum::Json(json!({ "id": id, "tmdb_id": body.tmdb_id, "scrape_status": "pending" }))
        .into_response()
}
