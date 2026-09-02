//! Taxonomy 域：分类聚合端点 + 库计数（从规范表返回真数据）。
//!
//! `Genres / Persons / Tags / Studios / Years / OfficialRatings` 支持按库 ParentId（`l-{n}`）
//! 过滤与分页；非法 ParentId → 空页（语义见 [`crate::routes::params::library_parent_filter`]）。
//! `Items/Counts` 返回 Movie/Series/Episode 计数。全部走认证 + Timeout。

use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::emby::{
    ItemsResponse, NameIdDto, NameIdTypeDto, TagDto, genre_id, person_id, studio_id,
};
use crate::routes::params::{PaginationQuery, library_parent_filter};
use crate::state::AppState;
use emrs_core::stores::ItemsStore;

/// 认证组：分类聚合端点 + Items/Counts。
pub fn authenticated() -> Router<AppState> {
    Router::new()
        .route("/Persons", get(list_persons))
        .route("/Genres", get(list_genres))
        .route("/Tags", get(list_tags))
        .route("/OfficialRatings", get(list_official_ratings))
        .route("/Years", get(list_years))
        .route("/Studios", get(list_studios))
        // 计数（匿名兼容读也走到这里）
        .route("/Items/Counts", get(items_counts))
}

/// ParentId 非法时的空列表应答（分类端点共用）。
fn empty_taxonomy_response() -> Response {
    axum::Json(ItemsResponse::<NameIdTypeDto> {
        items: Vec::new(),
        total_record_count: 0,
    })
    .into_response()
}

/// /Genres 端点：从 genre 表返回真实数据。
async fn list_genres(State(st): State<AppState>, Query(q): Query<PaginationQuery>) -> Response {
    let limit = q.limit.unwrap_or(100);
    let start = q.start.unwrap_or(0);
    let Some(library_id) = library_parent_filter(q.parent_id.as_deref()) else {
        return empty_taxonomy_response();
    };
    match ItemsStore::list_genres(&st.db, library_id, limit, start).await {
        Ok(result) => {
            let items: Vec<NameIdTypeDto> = result
                .items
                .iter()
                .map(|item| NameIdTypeDto {
                    name_id: NameIdDto {
                        name: item.title.clone(),
                        id: genre_id(item.id),
                    },
                    item_type: "Genre".into(),
                })
                .collect();
            axum::Json(ItemsResponse {
                items,
                total_record_count: result.total as usize,
            })
            .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "genres query failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// /Persons 端点：从 people 表返回真实数据。
async fn list_persons(State(st): State<AppState>, Query(q): Query<PaginationQuery>) -> Response {
    let limit = q.limit.unwrap_or(100);
    let start = q.start.unwrap_or(0);
    let Some(library_id) = library_parent_filter(q.parent_id.as_deref()) else {
        return empty_taxonomy_response();
    };
    match ItemsStore::list_persons(&st.db, library_id, limit, start).await {
        Ok(result) => {
            let items: Vec<NameIdTypeDto> = result
                .items
                .iter()
                .map(|item| NameIdTypeDto {
                    name_id: NameIdDto {
                        name: item.title.clone(),
                        id: person_id(item.id),
                    },
                    item_type: "Person".into(),
                })
                .collect();
            axum::Json(ItemsResponse {
                items,
                total_record_count: result.total as usize,
            })
            .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "persons query failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// /Tags 端点：从 `tag` 规范表返回（刮削时 TMDB keywords 写入）。
async fn list_tags(State(st): State<AppState>) -> Response {
    match ItemsStore::list_tags(&st.db).await {
        Ok(tags) => {
            let items: Vec<TagDto> = tags
                .iter()
                .map(|tag| TagDto {
                    name: tag.clone(),
                    item_type: "Tag".into(),
                })
                .collect();
            let total = items.len();
            axum::Json(ItemsResponse {
                items,
                total_record_count: total,
            })
            .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "tags query failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// /Studios 端点：从 `studio` 规范表返回（Id 为表主键，与 item `Studios[]` 一致）。
async fn list_studios(State(st): State<AppState>, Query(q): Query<PaginationQuery>) -> Response {
    let limit = q.limit.unwrap_or(100);
    let start = q.start.unwrap_or(0);
    let Some(library_id) = library_parent_filter(q.parent_id.as_deref()) else {
        return empty_taxonomy_response();
    };
    match ItemsStore::list_studios(&st.db, library_id, limit, start).await {
        Ok(r) => {
            let items: Vec<NameIdTypeDto> = r
                .items
                .iter()
                .map(|row| NameIdTypeDto {
                    name_id: NameIdDto {
                        name: row.title.clone(),
                        id: studio_id(row.id),
                    },
                    item_type: "Studio".into(),
                })
                .collect();
            axum::Json(ItemsResponse {
                items,
                total_record_count: r.total as usize,
            })
            .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "studios query failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// GET /Years：从 item.date_air 聚合年份（真数据）。
async fn list_years(State(st): State<AppState>, Query(q): Query<PaginationQuery>) -> Response {
    let limit = q.limit.unwrap_or(100);
    let start = q.start.unwrap_or(0);
    let Some(library_id) = library_parent_filter(q.parent_id.as_deref()) else {
        return empty_taxonomy_response();
    };
    match ItemsStore::list_years(&st.db, library_id, limit, start).await {
        Ok(result) => {
            let items: Vec<NameIdTypeDto> = result
                .items
                .iter()
                .map(|item| NameIdTypeDto {
                    name_id: NameIdDto {
                        name: item.title.clone(),
                        id: item.id.to_string(),
                    },
                    item_type: "Year".into(),
                })
                .collect();
            axum::Json(ItemsResponse {
                items,
                total_record_count: result.total as usize,
            })
            .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "years query failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// GET /OfficialRatings：从 item.official_rating 聚合分级（真数据）。
async fn list_official_ratings(
    State(st): State<AppState>,
    Query(q): Query<PaginationQuery>,
) -> Response {
    let limit = q.limit.unwrap_or(100);
    let start = q.start.unwrap_or(0);
    let Some(library_id) = library_parent_filter(q.parent_id.as_deref()) else {
        return empty_taxonomy_response();
    };
    match ItemsStore::list_official_ratings(&st.db, library_id, limit, start).await {
        Ok(result) => {
            let items: Vec<NameIdTypeDto> = result
                .items
                .iter()
                .map(|item| NameIdTypeDto {
                    name_id: NameIdDto {
                        name: item.title.clone(),
                        id: item.id.to_string(),
                    },
                    item_type: "Rating".into(),
                })
                .collect();
            axum::Json(ItemsResponse {
                items,
                total_record_count: result.total as usize,
            })
            .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "official ratings query failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// GET /Items/Counts：Movie/Series/Episode 计数。
async fn items_counts(State(st): State<AppState>) -> Response {
    match ItemsStore::item_counts(&st.db).await {
        Ok((movie_count, series_count, episode_count)) => axum::Json(crate::emby::ItemsCounts {
            movie_count,
            series_count,
            episode_count,
            item_count: movie_count + series_count + episode_count,
            ..Default::default()
        })
        .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "items counts query failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
