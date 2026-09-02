//! Items 列表 / 详情 / 用户视图 / 续播 / 最新 / 剧集三件套 端点。

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::emby::{
    CollectionFolderView, EpisodeCardJson, IdKind, ItemDto, ItemImageFlags, ItemsResponse,
    LatestItemJson, MovieSeriesCardJson, NextUpJson, ResumeCardJson, SeasonCardJson, ViewsResponse,
    attach_media_sources, item_to_json,
};
use emrs_core::auth::AuthContext;
use emrs_core::stores::image_store::ImageTypeIds;
use emrs_core::stores::taxonomy_store::ItemTaxonomy;
use emrs_core::stores::{ItemRow, ItemsStore, ResumeEntry};

use super::{
    ItemsQuery, SeasonQuery, parse_generic_id, parse_genre_ids, parse_id, parse_item_ids,
    parse_person_ids, parse_studio_ids,
};
use crate::state::AppState;

/// 批量预取列表内所有 item 的图片行 id（一次查询，避免 N+1）。
async fn preload_image_flags(
    st: &AppState,
    items: &[ItemRow],
) -> std::collections::HashMap<i64, ImageTypeIds> {
    let mut ids: Vec<i64> = Vec::with_capacity(items.len() * 2);
    for i in items {
        ids.push(i.id);
        if let Some(sid) = i.series_id {
            ids.push(sid);
        }
    }
    ItemsStore::image_ids_batch(&st.db, &ids)
        .await
        .unwrap_or_default()
}

/// 批量预取列表内所有 item 的 genres + people（一次查询，避免 N+1）。
async fn preload_taxonomy(
    st: &AppState,
    items: &[ItemRow],
) -> std::collections::HashMap<i64, ItemTaxonomy> {
    let ids: Vec<i64> = items.iter().map(|i| i.id).collect();
    ItemsStore::taxonomy_batch(&st.db, &ids)
        .await
        .unwrap_or_default()
}

/// 批量预取列表内 folder 项（Season/Series）的子集计数（一次查询，避免 N+1）。
/// 供 `item_to_json` 填 `RecursiveItemCount` / `ChildCount` / `UserData.UnplayedItemCount`。
async fn preload_child_counts(
    st: &AppState,
    items: &[ItemRow],
    user_id: i64,
) -> std::collections::HashMap<i64, (i64, i64, Option<i64>)> {
    ItemsStore::child_counts_batch(&st.db, items, user_id)
        .await
        .unwrap_or_default()
}

/// 批量预取列表内 Episode 的视频分辨率（一次查询，避免 N+1）。
async fn preload_video_dims(
    st: &AppState,
    items: &[ItemRow],
) -> std::collections::HashMap<i64, (Option<i64>, Option<i64>)> {
    let ids: Vec<i64> = items.iter().map(|i| i.id).collect();
    ItemsStore::video_dims_batch(&st.db, &ids)
        .await
        .unwrap_or_default()
}

/// 用批量预取的图片 id + taxonomy + counts + dims 把一个 item 序列化为类型化 Emby DTO。
/// counts/dims 未预取的端点传空 map（per-item 查 None → 对应字段省略）。
fn item_json(
    st: &AppState,
    flags: &std::collections::HashMap<i64, ImageTypeIds>,
    tax: &std::collections::HashMap<i64, ItemTaxonomy>,
    counts: &std::collections::HashMap<i64, (i64, i64, Option<i64>)>,
    dims: &std::collections::HashMap<i64, (Option<i64>, Option<i64>)>,
    item: &ItemRow,
) -> ItemDto {
    item_to_json(
        &st.cfg.emby.server_id,
        item,
        &ItemImageFlags::from_batch(flags, item),
        tax.get(&item.id),
        counts.get(&item.id).copied(),
        dims.get(&item.id).copied(),
    )
}

/// 按 ItemId 加载完整 ItemRow（按 `item.type` 列分派查询）。
/// 仅被注释掉的 `/Items/{id}` 详情路由使用，恢复后取消该 allow。
#[allow(dead_code)]
async fn load_item(st: &AppState, user_id: i64, raw_id: &str) -> Option<ItemRow> {
    let id = parse_id(raw_id)?;
    load_item_by_id(st, user_id, id).await
}

/// 按已解析的 item.id 加载完整 ItemRow（按 `item.type` 列分派查询）。
async fn load_item_by_id(st: &AppState, user_id: i64, id: i64) -> Option<ItemRow> {
    let ty = ItemsStore::get_item_type(&st.db, id).await.ok().flatten()?;
    let row = match ty.as_str() {
        "episode" => ItemsStore::get_episode(&st.db, id, user_id).await,
        "season" => ItemsStore::get_season(&st.db, id, user_id).await,
        _ => ItemsStore::get_item(&st.db, id, user_id).await,
    };
    row.ok().flatten()
}

// /// GET /Items：列表（分 Movies + Series 两类）。
// pub(super) async fn items_list(
//     State(st): State<AppState>,
//     Query(q): Query<ItemsQuery>,
// ) -> Response {
//     let limit = q.limit.unwrap_or(100).min(200);
//     let start = q.start_index.unwrap_or(0);
//     let library_id = parent_library_id(&q.parent_id);
//
//     // 根据 include_item_types 选择查询
//     let types = q.include_item_types.as_deref().unwrap_or("");
//     let result = if types.contains("Series") {
//         ItemsStore::list_series_by_library(&st.db, library_id, limit, start).await
//     } else {
//         ItemsStore::list_movies_by_library(&st.db, library_id, limit, start).await
//     };
//
//     match result {
//         Ok(r) => {
//             let flags = preload_image_flags(&st, &r.items).await;
//             let items = r
//                 .items
//                 .iter()
//                 .map(|i| item_json(&st, &flags, i))
//                 .collect::<Vec<_>>();
//             axum::Json(ItemsResponse {
//                 items,
//                 total_record_count: r.total as usize,
//             })
//             .into_response()
//         }
//         Err(e) => {
//             tracing::error!(error = ?e, "items list failed");
//             (StatusCode::INTERNAL_SERVER_ERROR, "查询失败").into_response()
//         }
//     }
// }

// /// GET /Items/{id}：详情。
// pub(super) async fn item_by_id(
//     State(st): State<AppState>,
//     axum::Extension(ctx): axum::Extension<AuthContext>,
//     Path(id): Path<String>,
// ) -> Response {
//     match load_item(&st, ctx.user_id, &id).await {
//         Some(item) => {
//             let flags = preload_image_flags(&st, std::slice::from_ref(&item)).await;
//             let mut v = item_json(&st, &flags, &item);
//             attach_media_sources(
//                 &st.db,
//                 st.cfg.playback.signing_key.as_deref(),
//                 ctx.user_id,
//                 &item,
//                 &mut v,
//             )
//             .await;
//             axum::Json(v).into_response()
//         }
//         None => StatusCode::NOT_FOUND.into_response(),
//     }
// }

/// GET /Users/{user_id}/Views：用户媒体库视图（CollectionFolder）。
pub(super) async fn users_views(State(st): State<AppState>, Path(_user_id): Path<i64>) -> Response {
    let server_id = &st.cfg.emby.server_id;
    match ItemsStore::list_libraries(&st.db).await {
        Ok(views) => {
            let items: Vec<CollectionFolderView> = views
                .iter()
                .map(|v| CollectionFolderView::from_library(server_id, v))
                .collect();
            axum::Json(ViewsResponse {
                total_record_count: items.len(),
                items,
            })
            .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "query libraries failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "查询失败").into_response()
        }
    }
}

/// GET /Users/{user_id}/Items：用户 Items。
pub(super) async fn users_items(
    State(st): State<AppState>,
    Path(user_id): Path<i64>,
    Query(q): Query<ItemsQuery>,
) -> Response {
    let limit = q.limit.unwrap_or(100).min(200);
    let start = q.start_index.unwrap_or(0);

    // ParentId 目录导航：按类型前缀判型，避免 library.id 与 item.id 数值撞车时探测误判。
    // l-{n} → 库列表（library_id=lid）；i-{seriesId} → 季列表；i-{seasonId} → 集列表；
    // 裸数字/非法前缀 → 无法解析父级，返回空（裸数字不再兼容）。
    // （Infuse 等客户端用 /Users/{uid}/Items?ParentId={seriesId} 展开剧集树）
    let library_id = match q.parent_id.as_deref().map(crate::emby::parse_id) {
        // 无 ParentId → 全部（首页/全库视图）
        None => None,
        // 裸数字/非法前缀 → 无有效父级，返回空
        Some(None) => return render_media_cards(&st, Vec::new()).await,
        Some(Some((IdKind::Library, lid))) => Some(lid),
        Some(Some((IdKind::Item, pid))) => match ItemsStore::get_item_type(&st.db, pid).await {
            Ok(Some(t)) if t == "series" => {
                return match ItemsStore::list_seasons(&st.db, pid).await {
                    Ok(v) => render_season_cards(&st, user_id, v).await,
                    Err(e) => {
                        tracing::error!(error = %e, "list_seasons(parent) failed");
                        (StatusCode::INTERNAL_SERVER_ERROR, "查询失败").into_response()
                    }
                };
            }
            Ok(Some(t)) if t == "season" => {
                return match ItemsStore::list_episodes(&st.db, pid, user_id).await {
                    Ok(v) => render_episode_cards(&st, v).await,
                    Err(e) => {
                        tracing::error!(error = %e, "list_episodes(parent) failed");
                        (StatusCode::INTERNAL_SERVER_ERROR, "查询失败").into_response()
                    }
                };
            }
            // movie/episode/不存在 → 非文件夹父级，无子项，返回空
            _ => return render_media_cards(&st, Vec::new()).await,
        },
        // People/Genre/Studio 非合法 ParentId → 返回空
        Some(Some(_)) => return render_media_cards(&st, Vec::new()).await,
    };

    // 解析通用过滤参数
    let types = parse_include_item_types(q.include_item_types.as_deref());
    let is_played = q.is_played;
    let is_favorite = q.is_favorite == Some(true);

    // 收藏过滤：IsFavorite=true 或 Filters 含 IsFavorite 时只返回该用户的收藏
    let favorite_filter = is_favorite
        || q.filters
            .as_deref()
            .map(|f| f.split(',').any(|p| p.eq_ignore_ascii_case("IsFavorite")))
            .unwrap_or(false);
    // IsPlayed / IsUnplayed 过滤（兼容 Filters 语法）
    let is_played_filter = is_played.or_else(|| {
        q.filters.as_deref().and_then(|f| {
            if f.split(',').any(|p| p.eq_ignore_ascii_case("IsPlayed")) {
                Some(true)
            } else if f.split(',').any(|p| p.eq_ignore_ascii_case("IsUnplayed")) {
                Some(false)
            } else {
                None
            }
        })
    });

    let result = if favorite_filter {
        let item_types = q.include_item_types.as_deref();
        ItemsStore::list_favorites(&st.db, user_id, item_types, limit, start).await
    } else if let Some(list_ids) = q.list_item_ids.as_deref().map(parse_item_ids)
        && !list_ids.is_empty()
    {
        // ListItemIds：按指定 item id 集合返回（BoxSet/合集内容）
        ItemsStore::list_items_by_ids(&st.db, user_id, &list_ids).await
    } else if q
        .include_item_types
        .as_deref()
        .unwrap_or("")
        .split(',')
        .any(|t| t.eq_ignore_ascii_case("BoxSet"))
    {
        // BoxSet 合集：本实现无合集概念，返回空（避免误落到 movie 分支返回全部电影）
        Ok(emrs_core::stores::ItemsResult {
            items: vec![],
            total: 0,
        })
    } else if let Some(person_ids) = q.person_ids.as_deref().map(parse_person_ids)
        && !person_ids.is_empty()
    {
        ItemsStore::list_items_by_person(&st.db, user_id, &person_ids, &types, limit, start).await
    } else if let Some(studio_ids) = q.studio_ids.as_deref().map(parse_studio_ids)
        && !studio_ids.is_empty()
    {
        ItemsStore::list_items_by_studio(&st.db, user_id, &studio_ids, &types, limit, start).await
    } else if let Some(genre_ids) = q.genre_ids.as_deref().map(parse_genre_ids)
        && !genre_ids.is_empty()
    {
        ItemsStore::list_items_by_genre(&st.db, user_id, &genre_ids, &types, limit, start).await
    } else if q
        .include_item_types
        .as_deref()
        .unwrap_or("")
        .contains("Series")
    {
        ItemsStore::list_series_by_library(&st.db, user_id, library_id, limit, start).await
    } else {
        // 默认 Movie/Series 列表（支持 SearchTerm / SortBy / SortOrder / IsPlayed / Tags）
        ItemsStore::list_movies_series(
            &st.db,
            user_id,
            library_id,
            q.search_term.as_deref(),
            &types,
            is_played_filter,
            q.tags.as_deref(),
            q.sort_by.as_deref(),
            q.sort_order.as_deref(),
            limit,
            start,
        )
        .await
    };

    match result {
        Ok(r) => render_media_cards(&st, r.items).await,
        Err(e) => {
            tracing::error!(error = %e, "users_items failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "查询失败").into_response()
        }
    }
}

/// 渲染一批 Episode 行为精简 Episode 卡片（Resume / Episodes / Items 进入 season 分支）。
/// 仅预取图片标志：无 taxonomy / dims / counts（列表抓包不含这些重字段）。
async fn render_episode_cards(st: &AppState, items: Vec<ItemRow>) -> Response {
    let flags = preload_image_flags(st, &items).await;
    let sid = &st.cfg.emby.server_id;
    let out: Vec<EpisodeCardJson> = items
        .iter()
        .map(|i| EpisodeCardJson::from_row(sid, i, &ItemImageFlags::from_batch(&flags, i)))
        .collect();
    let total = out.len();
    axum::Json(ItemsResponse {
        items: out,
        total_record_count: total,
    })
    .into_response()
}

/// 渲染一批 Season 行为精简 Season 卡片（Seasons / Items 进入 series 分支）。
/// 预取图片标志 + 子集计数（RecursiveItemCount / ChildCount / UnplayedItemCount）。
async fn render_season_cards(st: &AppState, user_id: i64, items: Vec<ItemRow>) -> Response {
    let flags = preload_image_flags(st, &items).await;
    let counts = preload_child_counts(st, &items, user_id).await;
    let sid = &st.cfg.emby.server_id;
    let out: Vec<SeasonCardJson> = items
        .iter()
        .map(|i| {
            SeasonCardJson::from_row(
                sid,
                i,
                &ItemImageFlags::from_batch(&flags, i),
                counts.get(&i.id).copied(),
            )
        })
        .collect();
    let total = out.len();
    axum::Json(ItemsResponse {
        items: out,
        total_record_count: total,
    })
    .into_response()
}

/// 渲染一批 Movie/Series 行为精简海报卡（Items 根·库分支 / Similar）。
/// 仅预取图片标志。
async fn render_media_cards(st: &AppState, items: Vec<ItemRow>) -> Response {
    let flags = preload_image_flags(st, &items).await;
    let sid = &st.cfg.emby.server_id;
    let out: Vec<MovieSeriesCardJson> = items
        .iter()
        .map(|i| MovieSeriesCardJson::from_row(sid, i, &ItemImageFlags::from_batch(&flags, i)))
        .collect();
    let total = out.len();
    axum::Json(ItemsResponse {
        items: out,
        total_record_count: total,
    })
    .into_response()
}

/// 渲染一批 Episode 行为 NextUp 条目（含 People，唯一在列表保留 taxonomy 的端点）。
async fn render_next_up(st: &AppState, items: Vec<ItemRow>) -> Response {
    let flags = preload_image_flags(st, &items).await;
    let tax = preload_taxonomy(st, &items).await;
    let sid = &st.cfg.emby.server_id;
    let out: Vec<NextUpJson> = items
        .iter()
        .map(|i| {
            NextUpJson::from_row(
                sid,
                i,
                &ItemImageFlags::from_batch(&flags, i),
                &tax.get(&i.id).cloned().unwrap_or_default(),
            )
        })
        .collect();
    let total = out.len();
    axum::Json(ItemsResponse {
        items: out,
        total_record_count: total,
    })
    .into_response()
}

/// 渲染一批 Episode/Movie 行为 Resume 续看卡片（`/Users/{uid}/Items/Resume` 专用）。
/// 图片只查各 item 自身 Primary（一次 `image_primary_batch`）：`ImageTags` 仅自身 Primary 无回退，
/// `BackdropImageTags` 恒空数组，不输出任何上级剧集图片字段。
async fn render_resume_cards(st: &AppState, items: Vec<ResumeEntry>) -> Response {
    let ids: Vec<i64> = items.iter().map(|i| i.id).collect();
    let primary = ItemsStore::image_primary_batch(&st.db, &ids)
        .await
        .unwrap_or_default();
    let sid = &st.cfg.emby.server_id;
    let out: Vec<ResumeCardJson> = items
        .iter()
        .map(|i| ResumeCardJson::from_row(sid, i, primary.get(&i.id).copied()))
        .collect();
    let total = out.len();
    axum::Json(ItemsResponse {
        items: out,
        total_record_count: total,
    })
    .into_response()
}

/// GET /Users/{user_id}/Items/Resume：继续观看。
pub(super) async fn users_resume(
    State(st): State<AppState>,
    Path(user_id): Path<i64>,
    Query(q): Query<ItemsQuery>,
) -> Response {
    let limit = q.limit.unwrap_or(20).min(50);
    let start = q.start_index.unwrap_or(0).max(0);
    // ParentId 过滤：`l-{库}` 只留该库续看；`i-{n}` 命中剧集 series_id 或季 season_id；
    // 裸数字/其他前缀 → 无有效父级，返回空（与 users_items 一致）。
    let (library_id, parent_item) = match q.parent_id.as_deref().map(crate::emby::parse_id) {
        None => (None, None),
        Some(None) => return render_resume_cards(&st, Vec::new()).await,
        Some(Some((IdKind::Library, lid))) => (Some(lid), None),
        Some(Some((IdKind::Item, iid))) => (None, Some(iid)),
        Some(Some(_)) => return render_resume_cards(&st, Vec::new()).await,
    };
    match ItemsStore::list_resume(&st.db, user_id, library_id, parent_item, limit, start).await {
        Ok(items) => render_resume_cards(&st, items).await,
        Err(e) => {
            tracing::error!(error = %e, "list_resume failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "查询失败").into_response()
        }
    }
}

/// GET /Users/{user_id}/Items/Latest：最新入库。
/// `ParentId=l-{n}` → 仅返回该库最新条目；无 ParentId → 全库最新；
/// 其他前缀/裸数字非合法库父级 → 空（Latest 不支持 series/season 目录导航）。
pub(super) async fn users_latest(
    State(st): State<AppState>,
    Path(user_id): Path<i64>,
    Query(q): Query<ItemsQuery>,
) -> Response {
    let limit = q.limit.unwrap_or(20).min(50);
    let library_id = match q.parent_id.as_deref().map(crate::emby::parse_id) {
        None => None,
        Some(None) => return axum::Json(Vec::<LatestItemJson>::new()).into_response(),
        Some(Some((IdKind::Library, lid))) => Some(lid),
        Some(Some(_)) => return axum::Json(Vec::<LatestItemJson>::new()).into_response(),
    };
    match ItemsStore::list_latest(&st.db, user_id, library_id, limit).await {
        Ok(items) => {
            let flags = preload_image_flags(&st, &items).await;
            let tax = preload_taxonomy(&st, &items).await;
            let out = items
                .iter()
                .map(|i| {
                    LatestItemJson::from_row(
                        &st.cfg.emby.server_id,
                        i,
                        &ItemImageFlags::from_batch(&flags, i),
                        &tax.get(&i.id).cloned().unwrap_or_default(),
                    )
                })
                .collect::<Vec<_>>();
            axum::Json(out).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "list_latest failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "查询失败").into_response()
        }
    }
}

/// GET /Users/{user_id}/Items/{item_id}：用户 Item 详情。
/// `{item_id}` 按 kind 分流：`i-{id}`/裸数字 → item 详情；`p-{id}` → people 详情；
/// `l-/g-/s-` → 404（库/类型/工作室不走此路由）。
pub(super) async fn users_item_by_id(
    State(st): State<AppState>,
    axum::Extension(ctx): axum::Extension<AuthContext>,
    Path((_user_id, item_id)): Path<(i64, String)>,
) -> Response {
    match parse_generic_id(&item_id).map(|p| (p.kind, p.id)) {
        Some(("people", id)) => person_detail(&st, id).await,
        Some(("item", id)) => item_detail(&st, ctx, id).await,
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// 普通 item 详情（movie/series/season/episode）。
async fn item_detail(st: &AppState, ctx: AuthContext, item_id: i64) -> Response {
    match load_item_by_id(st, ctx.user_id, item_id).await {
        Some(item) => {
            let flags = preload_image_flags(st, std::slice::from_ref(&item)).await;
            let tax = preload_taxonomy(st, std::slice::from_ref(&item)).await;
            let counts = preload_child_counts(st, std::slice::from_ref(&item), ctx.user_id).await;
            let dims = preload_video_dims(st, std::slice::from_ref(&item)).await;
            let mut v = item_json(st, &flags, &tax, &counts, &dims, &item);
            attach_media_sources(
                &st.db,
                st.cfg.playback.signing_key.as_deref(),
                ctx.user_id,
                &item,
                &mut v,
            )
            .await;
            axum::Json(v).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Person 详情（`/Users/{uid}/Items/p-{id}`）：返回 People DTO。
async fn person_detail(st: &AppState, person_id: i64) -> Response {
    match ItemsStore::get_person(&st.db, person_id).await {
        Ok(Some(person)) => {
            // 人员头像图片行 id（parent_type='people'，复用 item_image 表）
            let primary_image_id =
                ItemsStore::get_image_path(&st.db, "people", person_id, "Primary", 0)
                    .await
                    .ok()
                    .flatten()
                    .filter(|(_, u)| !u.is_empty())
                    .map(|(img_id, _)| img_id);
            let v = crate::emby::person_to_json(&st.cfg.emby.server_id, &person, primary_image_id);
            axum::Json(v).into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(error = %e, person_id, "person detail failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "查询失败").into_response()
        }
    }
}

/// 解析 `IncludeItemTypes`（Emby 大写，逗号分隔）为 DB 小写类型白名单。
/// 未知/空类型忽略；`Movie/Series/Episode` 常见，其余类型透传小写。
fn parse_include_item_types(raw: Option<&str>) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for t in raw.unwrap_or("").split(',').map(|s| s.trim()) {
        if t.is_empty() {
            continue;
        }
        let lower = t.to_ascii_lowercase();
        let db_ty = match lower.as_str() {
            "movie" => "movie",
            "series" => "series",
            "season" => "season",
            "episode" => "episode",
            "video" => "movie",
            _ => continue,
        };
        if !out.contains(&db_ty) {
            out.push(db_ty);
        }
    }
    out
}

/// GET /Shows/NextUp：接下来播放。
pub(super) async fn shows_next_up(
    State(st): State<AppState>,
    axum::Extension(ctx): axum::Extension<AuthContext>,
    Query(q): Query<ItemsQuery>,
) -> Response {
    let user_id = q.user_id.unwrap_or(ctx.user_id);
    let limit = q.limit.unwrap_or(20).min(50);
    let start = q.start_index.unwrap_or(0).max(0);
    // SeriesId 过滤：须为 `i-{id}`；缺失 → 全量 NextUp，传了但解析失败（非 i- 前缀/裸数字）
    // → 记 warn 并忽略下钻，退化为全量（避免静默丢过滤条件排查无门）。
    let series_id = match q.series_id.as_deref() {
        None => None,
        Some(raw) => match parse_id(raw) {
            Some(id) => Some(id),
            None => {
                tracing::warn!(raw, "NextUp SeriesId 非法（期望 i-N 前缀），忽略下钻过滤");
                None
            }
        },
    };
    match ItemsStore::list_next_up(&st.db, user_id, series_id, limit, start).await {
        Ok(items) => render_next_up(&st, items).await,
        Err(e) => {
            tracing::error!(error = %e, "list_next_up failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "查询失败").into_response()
        }
    }
}

/// GET /Shows/{id}/Seasons：剧集的所有季。
pub(super) async fn shows_seasons(
    State(st): State<AppState>,
    axum::Extension(ctx): axum::Extension<AuthContext>,
    Path(id): Path<String>,
) -> Response {
    let Some(series_id) = parse_id(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if ItemsStore::get_item_type(&st.db, series_id)
        .await
        .ok()
        .flatten()
        .as_deref()
        != Some("series")
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    match ItemsStore::list_seasons(&st.db, series_id).await {
        Ok(items) => render_season_cards(&st, ctx.user_id, items).await,
        Err(e) => {
            tracing::error!(error = %e, "list_seasons failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "查询失败").into_response()
        }
    }
}

/// GET /Shows/{id}/Episodes：季的所有集。
/// 路径 `{id}` 是剧集，季 ID 通过 `SeasonId` 查询参数传入。
pub(super) async fn shows_episodes(
    State(st): State<AppState>,
    Path(_id): Path<String>,
    Query(q): Query<SeasonQuery>,
    axum::Extension(ctx): axum::Extension<AuthContext>,
) -> Response {
    let Some(season_id) = q.season_id.as_deref().and_then(parse_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if ItemsStore::get_item_type(&st.db, season_id)
        .await
        .ok()
        .flatten()
        .as_deref()
        != Some("season")
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let user_id = ctx.user_id;
    match ItemsStore::list_episodes(&st.db, season_id, user_id).await {
        Ok(items) => render_episode_cards(&st, items).await,
        Err(e) => {
            tracing::error!(error = %e, "list_episodes failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "查询失败").into_response()
        }
    }
}

/// GET /Items/{id}/Similar：相似推荐（Hills 详情页"相似"列表）。
/// 按共同 genre 数排序，回退同库；UserId/Limit 从 query 读取。
pub(super) async fn item_similar(
    State(st): State<AppState>,
    axum::Extension(ctx): axum::Extension<AuthContext>,
    Path(id): Path<String>,
    Query(q): Query<ItemsQuery>,
) -> Response {
    let Some(num_id) = parse_id(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let user_id = q.user_id.unwrap_or(ctx.user_id);
    let limit = q.limit.unwrap_or(20).min(50);
    match ItemsStore::list_similar(&st.db, user_id, num_id, limit).await {
        Ok(r) => render_media_cards(&st, r.items).await,
        Err(e) => {
            tracing::error!(error = %e, "list_similar failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "查询失败").into_response()
        }
    }
}
