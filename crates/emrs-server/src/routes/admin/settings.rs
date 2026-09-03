//! app_setting 读写端点。

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::state::AppState;

/// GET /admin/settings：读取全部 app_setting。
pub(super) async fn get_settings(State(st): State<AppState>) -> Response {
    match emrs_infra::stores::ItemsStore::list_settings(&st.db).await {
        Ok(rows) => {
            let settings: serde_json::Map<String, serde_json::Value> =
                rows.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
            axum::Json(json!({ "settings": settings })).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "get_settings failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// PUT /admin/settings：写入 app_setting（UPSERT）。
pub(super) async fn set_setting(
    State(st): State<AppState>,
    axum::extract::Json(body): axum::extract::Json<std::collections::HashMap<String, String>>,
) -> Response {
    let mut updated = 0;
    for (key, value) in &body {
        match emrs_infra::stores::ItemsStore::set_setting(&st.db, key, value).await {
            Ok(_) => updated += 1,
            Err(e) => {
                tracing::error!(error = %e, key, "set_setting failed");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        }
    }
    axum::Json(json!({ "updated": updated })).into_response()
}
