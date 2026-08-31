//! Emby 路由：公开组 + 认证组 + 三重前缀挂载在 [`crate::app`]。

pub mod admin;
pub mod items;
pub mod public;
pub mod system;

pub use public::{public_routes, root_routes};

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};

use emrs_core::emby::{
    NameIdDto, NameIdTypeDto, SessionListEntryDto, TagDto, genre_id, person_id, studio_id,
};

use crate::state::AppState;

/// 204 空应答。
pub async fn no_content() -> impl IntoResponse {
    StatusCode::NO_CONTENT
}

/// 认证组路由（Sessions/空目录 stub + System/Info + Users/{id} + Items 系列 + Admin）。
pub fn authenticated_routes() -> Router<AppState> {
    Router::new()
        // Sessions 列表（空列表）；Playing/Progress/Stopped 由 items 模块提供真实实现
        .route("/Sessions", get(sessions))
        .route("/Sessions/Capabilities/Full", post(no_content))
        .route("/Sessions/Capabilities", post(no_content))
        .route("/Sessions/Playing/Ping", post(no_content))
        // Genres/Persons/Tags 从 genre/people 表读真数据
        .route("/Persons", get(list_persons))
        .route("/Genres", get(list_genres))
        .route("/Tags", get(list_tags))
        .route("/OfficialRatings", get(list_official_ratings))
        .route("/Years", get(list_years))
        .route("/Studios", get(list_studios))
        // 计数（匿名兼容读也走到这里）
        .route("/Items/Counts", get(items_counts))
        // System/Info（需认证；Public 版在公开组）
        .route("/System/Info", get(system::info))
        .route("/System/Info/Query", get(system::info))
        // Users
        .route("/Users/Me", get(public::users_me))
        .route("/Users/{user_id}", get(user_by_id))
        .route("/Users", get(users_list))
        // Items 系列：Items / PlaybackInfo / Resume / Latest / NextUp / 剧集 / 播放 / 进度
        .merge(items::items_routes())
        // Admin 仪表盘
        .merge(admin::admin_routes())
}

/// GET /Sessions：当前用户的进行中播放会话（NowPlayingItem + PlaybackPositionTicks）。
/// EMRS 无客户端会话注册表，从 user_item_data 派生"正在播放"条目作为会话列表。
/// 匿名兼容读（GET/HEAD 无 token）时无 AuthContext，返回空列表。
async fn sessions(
    State(st): State<AppState>,
    ctx: Option<axum::Extension<emrs_core::auth::AuthContext>>,
) -> Response {
    let Some(axum::Extension(ctx)) = ctx else {
        return axum::Json(Vec::<SessionListEntryDto>::new()).into_response();
    };
    let items = match emrs_core::stores::ItemsStore::list_active_sessions(&st.db, ctx.user_id).await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "sessions query failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    // 批量预取图片行 id，组装 NowPlayingItem DTO
    let mut ids: Vec<i64> = Vec::with_capacity(items.len() * 2);
    for i in &items {
        ids.push(i.id);
        if let Some(sid) = i.series_id {
            ids.push(sid);
        }
    }
    let flags = emrs_core::stores::ItemsStore::image_ids_batch(&st.db, &ids)
        .await
        .unwrap_or_default();
    let sessions: Vec<SessionListEntryDto> = items
        .iter()
        .map(|item| {
            let now_playing_item = emrs_core::emby::item_to_json(
                &st.cfg.emby.server_id,
                item,
                &emrs_core::emby::ItemImageFlags::from_batch(&flags, item),
                None,
                None,
                None,
            );
            let user_id = ctx.user_id.to_string();
            SessionListEntryDto::new(
                now_playing_item,
                format!("session-{}", item.id),
                &user_id,
                &ctx.username,
                &ctx.device,
                item.play_ms * 10_000,
            )
        })
        .collect();
    axum::Json(sessions).into_response()
}

/// 分页查询参数。
/// Hills 等客户端发 `Limit&`（空值）或 `StartIndex=0`，须 lenient 处理，否则 400。
#[derive(serde::Deserialize)]
struct PaginationQuery {
    #[serde(
        default,
        deserialize_with = "crate::routes::items::deserialize_lenient_i64"
    )]
    limit: Option<i64>,
    #[serde(
        alias = "startindex",
        default,
        deserialize_with = "crate::routes::items::deserialize_lenient_i64"
    )]
    start: Option<i64>,
    /// ParentId：分类页按库过滤（带前缀库 ID `l-{n}`；裸数字/其他前缀 → 空页，见 [`library_parent_filter`]）。
    #[serde(
        alias = "parentid",
        default,
        deserialize_with = "crate::routes::items::deserialize_lenient_string"
    )]
    parent_id: Option<String>,
}

/// 分类端点 ParentId → 库过滤 ID。
/// `l-{n}` → `Some(Some(id))`；未传/空值 → `Some(None)`（全库）；
/// 裸数字/非法/其他前缀 → `None`（调用方返回空页，与 Users/Items 导航语义一致）。
fn library_parent_filter(raw: Option<&str>) -> Option<Option<i64>> {
    match raw.map(emrs_core::emby::parse_id) {
        None => Some(None),
        Some(Some((emrs_core::emby::IdKind::Library, id))) => Some(Some(id)),
        Some(Some(_)) | Some(None) => None,
    }
}

/// ParentId 非法时的空列表应答（分类端点共用）。
fn empty_taxonomy_response() -> Response {
    axum::Json(emrs_core::emby::ItemsResponse::<NameIdTypeDto> {
        items: Vec::new(),
        total_record_count: 0,
    })
    .into_response()
}

/// /Genres 端点：从 genre 表返回真实数据。
async fn list_genres(
    State(st): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<PaginationQuery>,
) -> Response {
    let limit = q.limit.unwrap_or(100);
    let start = q.start.unwrap_or(0);
    let Some(library_id) = library_parent_filter(q.parent_id.as_deref()) else {
        return empty_taxonomy_response();
    };
    match emrs_core::stores::ItemsStore::list_genres(&st.db, library_id, limit, start).await {
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
            axum::Json(emrs_core::emby::ItemsResponse {
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
async fn list_persons(
    State(st): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<PaginationQuery>,
) -> Response {
    let limit = q.limit.unwrap_or(100);
    let start = q.start.unwrap_or(0);
    let Some(library_id) = library_parent_filter(q.parent_id.as_deref()) else {
        return empty_taxonomy_response();
    };
    match emrs_core::stores::ItemsStore::list_persons(&st.db, library_id, limit, start).await {
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
            axum::Json(emrs_core::emby::ItemsResponse {
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
    match emrs_core::stores::ItemsStore::list_tags(&st.db).await {
        Ok(tags) => {
            let items: Vec<TagDto> = tags
                .iter()
                .map(|tag| TagDto {
                    name: tag.clone(),
                    item_type: "Tag".into(),
                })
                .collect();
            let total = items.len();
            axum::Json(emrs_core::emby::ItemsResponse {
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
async fn list_studios(
    State(st): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<PaginationQuery>,
) -> Response {
    let limit = q.limit.unwrap_or(100);
    let start = q.start.unwrap_or(0);
    let Some(library_id) = library_parent_filter(q.parent_id.as_deref()) else {
        return empty_taxonomy_response();
    };
    match emrs_core::stores::ItemsStore::list_studios(&st.db, library_id, limit, start).await {
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
            axum::Json(emrs_core::emby::ItemsResponse {
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
async fn list_years(
    State(st): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<PaginationQuery>,
) -> Response {
    let limit = q.limit.unwrap_or(100);
    let start = q.start.unwrap_or(0);
    let Some(library_id) = library_parent_filter(q.parent_id.as_deref()) else {
        return empty_taxonomy_response();
    };
    match emrs_core::stores::ItemsStore::list_years(&st.db, library_id, limit, start).await {
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
            axum::Json(emrs_core::emby::ItemsResponse {
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
    axum::extract::Query(q): axum::extract::Query<PaginationQuery>,
) -> Response {
    let limit = q.limit.unwrap_or(100);
    let start = q.start.unwrap_or(0);
    let Some(library_id) = library_parent_filter(q.parent_id.as_deref()) else {
        return empty_taxonomy_response();
    };
    match emrs_core::stores::ItemsStore::list_official_ratings(&st.db, library_id, limit, start)
        .await
    {
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
            axum::Json(emrs_core::emby::ItemsResponse {
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

async fn items_counts(State(st): State<AppState>) -> Response {
    match emrs_core::stores::ItemsStore::item_counts(&st.db).await {
        Ok((movie_count, series_count, episode_count)) => {
            axum::Json(emrs_core::emby::ItemsCounts {
                movie_count,
                series_count,
                episode_count,
                item_count: movie_count + series_count + episode_count,
                ..Default::default()
            })
            .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "items counts query failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn user_by_id(
    axum::extract::Path(user_id): axum::extract::Path<i64>,
    axum::extract::State(state): axum::extract::State<AppState>,
) -> impl IntoResponse {
    match emrs_core::auth::AuthStore::find_user_by_id(&state.db, user_id).await {
        Ok(Some(u)) => axum::Json(user_dto(&state, &u)).into_response(),
        _ => axum::http::StatusCode::NOT_FOUND.into_response(),
    }
}

async fn users_list() -> impl IntoResponse {
    // Emby 兼容：返回空列表（用户枚举不开放）
    axum::Json(Vec::<serde_json::Value>::new())
}

/// Emby User DTO。
pub fn user_dto(state: &AppState, u: &emrs_core::auth::UserRow) -> emrs_core::emby::UserDto {
    emrs_core::emby::user_to_json(&state.cfg.emby.server_id, u)
}
