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
    path: String,
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

/// GET /admin/libraries：列出所有库。
pub(super) async fn list_libraries(State(st): State<AppState>) -> Response {
    // LEFT JOIN 取每个库首个挂载点的 path（按 sort_order, id 排序）。
    // 列表/详情必须回填 path，否则前端编辑弹窗会把 input.value 设成 JS undefined，
    // 保存时把字符串 "undefined" 写回 library_path（覆盖真实路径）。
    let rows = sqlx::query_as::<_, (i64, String, String, String, String)>(
        "SELECT l.id, l.name, COALESCE(lp.path, '') AS path, l.created_at, l.collection_type \
         FROM library l \
         LEFT JOIN library_path lp ON lp.id = (\
             SELECT id FROM library_path \
             WHERE library_id = l.id \
             ORDER BY sort_order, id LIMIT 1) \
         ORDER BY l.id",
    )
    .fetch_all(st.db.pool())
    .await;

    match rows {
        Ok(list) => {
            let items: Vec<Value> = list
                .into_iter()
                .map(|(id, name, path, created_at, collection_type)| {
                    json!({
                        "id": id,
                        "name": name,
                        "path": path,
                        "created_at": created_at,
                        "collection_type": collection_type,
                    })
                })
                .collect();
            axum::Json(json!({ "items": items })).into_response()
        }
        Err(e) => {
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
    if body.name.is_empty() || body.path.is_empty() {
        return (StatusCode::BAD_REQUEST, "name 和 path 不能为空").into_response();
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
    let path = normalize_library_path(&body.path).await;

    // 按 path 去重：同路径库已存在时直接返回已有 id（防重复建库导致扫描翻倍）
    if let Ok(Some(id)) = sqlx::query_scalar::<_, i64>(
        "SELECT lp.library_id FROM library_path lp \
         JOIN library l ON l.id = lp.library_id \
         WHERE lp.path = ? LIMIT 1",
    )
    .bind(&path)
    .fetch_optional(st.db.pool())
    .await
    {
        return axum::Json(json!({ "id": id, "name": body.name, "path": path, "existing": true }))
            .into_response();
    }

    match sqlx::query("INSERT INTO library (name, collection_type) VALUES (?, ?)")
        .bind(&body.name)
        .bind(body.collection_type.as_deref().unwrap_or("tvshows"))
        .execute(st.db.pool())
        .await
    {
        Ok(_) => {
            // sqlx Any 池下 last_insert_id 不可靠，回查取 id（三方言通用）
            let id: i64 = match sqlx::query_scalar::<_, i64>(
                "SELECT id FROM library WHERE name = ? \
                 ORDER BY id DESC LIMIT 1",
            )
            .bind(&body.name)
            .fetch_one(st.db.pool())
            .await
            {
                Ok(id) => id,
                Err(e) => {
                    tracing::error!(error = %e, "create_library: fetch id failed");
                    return (StatusCode::INTERNAL_SERVER_ERROR, "创建失败").into_response();
                }
            };
            // 写入 library_path 挂载点
            if let Err(e) = sqlx::query(
                "INSERT INTO library_path (library_id, path, path_type) VALUES (?, ?, 'local')",
            )
            .bind(id)
            .bind(&path)
            .execute(st.db.pool())
            .await
            {
                tracing::error!(error = %e, "create_library: insert library_path failed");
                return (StatusCode::INTERNAL_SERVER_ERROR, "创建失败").into_response();
            }
            axum::Json(json!({ "id": id, "name": body.name, "path": path })).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "create_library failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "创建失败").into_response()
        }
    }
}

/// GET /admin/libraries/{id}：查单个库。
pub(super) async fn get_library(State(st): State<AppState>, Path(id): Path<i64>) -> Response {
    let row = sqlx::query_as::<_, (i64, String, String, String, String)>(
        "SELECT l.id, l.name, COALESCE(lp.path, '') AS path, l.created_at, l.collection_type \
         FROM library l \
         LEFT JOIN library_path lp ON lp.id = (\
             SELECT id FROM library_path \
             WHERE library_id = l.id \
             ORDER BY sort_order, id LIMIT 1) \
         WHERE l.id = ? LIMIT 1",
    )
    .bind(id)
    .fetch_optional(st.db.pool())
    .await;

    match row {
        Ok(Some((id, name, path, created_at, collection_type))) => axum::Json(json!({
            "id": id,
            "name": name,
            "path": path,
            "created_at": created_at,
            "collection_type": collection_type,
        }))
        .into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(error = %e, "get_library failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// PUT /admin/libraries/{id}：更新库。
pub(super) async fn update_library(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    axum::extract::Json(body): axum::extract::Json<LibraryInput>,
) -> Response {
    let now = emrs_core::emby::format_time_now();
    // 先校验 collection_type（fail-fast，与 create_library 一致），再做路径 canonicalize。
    if let Some(ct) = &body.collection_type
        && !emrs_core::stores::is_valid_collection_type(ct)
    {
        return (
            StatusCode::BAD_REQUEST,
            format!("非法 collection_type: {ct}"),
        )
            .into_response();
    }
    let path = normalize_library_path(&body.path).await;
    match sqlx::query(
        "UPDATE library SET name = ?, collection_type = COALESCE(?, collection_type), updated_at = ? WHERE id = ?",
    )
    .bind(&body.name)
    .bind(&body.collection_type)
    .bind(&now)
    .bind(id)
    .execute(st.db.pool())
    .await
    {
        Ok(r) if r.rows_affected() > 0 => {
            // Update or insert library_path mount point
            let _ = sqlx::query(
                "UPDATE library_path SET path = ?, updated_at = ? \
                 WHERE library_id = ?",
            )
            .bind(&path)
            .bind(&now)
            .bind(id)
            .execute(st.db.pool())
            .await;
            StatusCode::OK.into_response()
        }
        Ok(_) => StatusCode::NOT_FOUND.into_response(),
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

/// GET /admin/media：列出所有媒体（分页）。
pub(super) async fn list_media(State(st): State<AppState>) -> Response {
    let rows = sqlx::query_as::<_, (String, Option<String>, Option<String>, Option<String>, i64)>(
        "SELECT ms.uuid, ms.name, \
                CASE ms.protocol WHEN 'file' THEN 'local' WHEN 'strm' THEN 'strm' WHEN 'webdavs' THEN 'webdav' ELSE ms.protocol END AS path_type, \
                COALESCE(ms.path, ms.remote_path) AS path_url, ms.item_id \
         FROM media_source ms \
         ORDER BY ms.created_at DESC LIMIT 200",
    )
    .fetch_all(st.db.pool())
    .await;

    match rows {
        Ok(list) => {
            let items: Vec<Value> = list
                .into_iter()
                .map(|(uuid, name, path_type, path_url, item_id)| {
                    json!({
                        "uuid": uuid,
                        "name": name,
                        "path_type": path_type,
                        "path_url": path_url,
                        "item_id": item_id,
                    })
                })
                .collect();
            axum::Json(json!({ "items": items })).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "list_media failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
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
