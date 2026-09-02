//! 媒体库 Store 层：按领域拆分的查询模块。
//!
//! 对外保持 `ItemsStore` / `PlaybackStore` 聚合入口与 `ItemRow` / `MediaSourceRow`
//! / `ItemsResult` / `LibraryView` 类型不变，路由层（`items.rs`）无需改动。
//! 内部 SQL 全部迁移到新表（`item` / `media_source` / `external_subtitle` /
//! `user_item_data` / `item_image` / `genre` / `people`）。
//!
//! 模块拆分：
//! - [`library_store`]：library + library_path 聚合 CRUD
//! - [`item_store`]：item 多态查询（Items/Resume/Latest/NextUp/Seasons/Episodes）
//! - [`media_store`]：media_source / external_subtitle 读写、get_playback_info 组装
//! - [`image_store`]：item_image（primary A 类唯一、图片标签）
//! - [`taxonomy_store`]：genre / people + item_genre / item_people（分类/规范数据）
//! - [`user_data_store`]：user_item_data UPSERT、收藏 / 已看 / 进度

pub mod image_store;
pub mod item_store;
pub mod library_store;
pub mod media_store;
pub mod settings_store;
pub mod taxonomy_store;
pub mod user_data_store;

pub use library_store::{COLLECTION_TYPES, is_valid_collection_type};
pub use media_store::StreamInfo;

use crate::db::Db;

/// 媒体库视图（Emby `/Users/{id}/Views` 的 CollectionFolder 数据源）。
///
/// 精简为库本身字段，不再伪造 `ItemRow`；`collection_type` 由管理员
/// 在建库/编辑时设置（默认 `"tvshows"`），合法值参见 [`COLLECTION_TYPES`]。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct LibraryView {
    pub id: i64,
    pub name: String,
    pub created_at: String,
    pub updated_at: String,
    pub collection_type: String,
}

/// Emby Items 基础行。
///
/// 字段对齐旧 `items.rs`，路由层 `item_to_json` 直接消费；新表查询在 SQL 层
/// 将 `item.type` / `media_source.*` / `user_item_data.*` 映射到这些列。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ItemRow {
    pub id: i64,
    /// 所属媒体库（`item.library_id`）。Movie/Series 的 Emby `ParentId` 用它。
    pub library_id: Option<i64>,
    pub item_type: String,
    pub title: String,
    pub description: Option<String>,
    pub date_air: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub container: Option<String>,
    pub file_second: Option<i64>,
    pub uuid: Option<String>,
    pub name: Option<String>,
    pub path_type: Option<String>,
    pub path_url: Option<String>,
    pub play_ms: i64,
    /// 布尔以 0/1 整数承载：sqlx Any 下 SQLite INTEGER 映射 BIGINT，bool 解码会失败。
    pub is_complete: i64,
    /// 播放次数（`user_item_data.play_count`，开播时递增）。
    pub play_count: i64,
    pub is_favorite: i64,
    pub season_number: Option<i64>,
    pub episode_number: Option<i64>,
    pub series_id: Option<i64>,
    pub series_name: Option<String>,
    pub season_id: Option<i64>,
    /// 所属季标题（仅 episode 行：`season_item.title`）。
    pub season_name: Option<String>,
    /// 虚拟条目标记（1 = 虚拟占位，LocationType=Virtual；0 = 真实文件）。
    pub is_virtual: i64,
    pub tmdb_id: Option<String>,
    pub imdb_id: Option<String>,
    pub tvdb_id: Option<String>,
    /// TMDB 用户评分（`vote_average`），Emby `CommunityRating`。
    pub community_rating: Option<f64>,
    /// 分级（TMDB release_dates/content_ratings），Emby `OfficialRating`。
    pub official_rating: Option<String>,
    /// 短标语（电影 TMDB `tagline`），Emby `Tagline`。
    pub tagline: Option<String>,
    /// 排序标题（刮削时由标题推导），Emby `SortName`。
    pub sort_title: Option<String>,
    /// 结束日期（系列停播日），Emby `EndDate`。
    pub end_date: Option<String>,
    /// 制作状态（released/canceled/in production），Emby `Status`。
    pub status: Option<String>,
    /// 制作年份（刮削时由首播日期推导）。
    pub production_year: Option<i64>,
}

/// Items 查询结果。
#[derive(Debug, Clone)]
pub struct ItemsResult {
    pub items: Vec<ItemRow>,
    pub total: i64,
}

/// Resume（继续观看）精简行：只含 `/Users/{uid}/Items/Resume` 卡片实际消费的字段。
///
/// 由 [`item_store::list_resume`] 通过「两表筛选 + 三次单表 `IN` 批取」在应用层组装，
/// 不复用胖 [`ItemRow`]（省掉 description/container/path/taxonomy 等 Resume 用不到的列）。
#[derive(Debug, Clone)]
pub struct ResumeEntry {
    pub id: i64,
    /// Emby 类型（`Movie` / `Episode`）。
    pub item_type: String,
    pub title: String,
    pub created_at: String,
    /// 进度毫秒（`playback_position_ticks / 10000`）。
    pub play_ms: i64,
    /// 布尔以 0/1 承载（sqlx Any 约定）。
    pub is_complete: i64,
    pub play_count: i64,
    pub is_favorite: i64,
    /// 时长秒（来自 `media_source.file_duration`；虚拟/无源 → None）。
    pub file_second: Option<i64>,
    pub production_year: Option<i64>,
    pub date_air: Option<String>,
    pub season_number: Option<i64>,
    pub episode_number: Option<i64>,
    pub series_id: Option<i64>,
    pub series_name: Option<String>,
    pub season_id: Option<i64>,
    pub season_name: Option<String>,
    pub tmdb_id: Option<String>,
    pub imdb_id: Option<String>,
    pub tvdb_id: Option<String>,
}

/// PlaybackInfo 媒体源行。
///
/// 对齐旧 `items.rs::MediaSourceRow`；新表 `media_source` + `external_subtitle`
/// 在 SQL 层映射到这些字段，路由层 `media_sources_json` / `media_streams_json`
/// 直接消费（`file_metadata` / `file_chapters` 现读 `media_source.metadata` /
/// `media_source.chapters`）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MediaSourceRow {
    pub uuid: Option<String>,
    pub name: Option<String>,
    pub file_size: Option<i64>,
    pub file_second: Option<i64>,
    pub file_container: Option<String>,
    pub path_type: Option<String>,
    pub path_url: Option<String>,
    pub file_metadata: Option<String>,
    pub file_chapters: Option<String>,
    pub item_id: i64,
    /// media_source 自增 id（查询外部字幕 / external_subtitle 用）。
    pub media_id: Option<i64>,
}

/// user_item_data 行（UserData DTO 成型用）。
///
/// 收藏 / 已看 / 进度端点写操作后回读实际字段，避免返回固定假数据。
/// 布尔以 0/1 整数承载：sqlx Any 下 SQLite INTEGER 映射 BIGINT，bool 解码会失败。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UserItemData {
    pub played: i64,
    pub play_count: i64,
    pub playback_position_ticks: Option<i64>,
    pub last_played_date: Option<String>,
    pub is_favorite: i64,
}

/// 媒体库查询入口（聚合各 store 的便捷门面）。
///
/// 方法委托到 [`item_store`] / [`media_store`] / [`image_store`] / [`library_store`] / [`taxonomy_store`] / [`user_data_store`]。
pub struct ItemsStore;

impl ItemsStore {
    pub async fn list_movies(
        db: &Db,
        user_id: i64,
        limit: i64,
        start: i64,
    ) -> anyhow::Result<ItemsResult> {
        item_store::list_movies(db, user_id, limit, start).await
    }

    pub async fn list_movies_by_library(
        db: &Db,
        user_id: i64,
        library_id: Option<i64>,
        limit: i64,
        start: i64,
    ) -> anyhow::Result<ItemsResult> {
        item_store::list_movies_by_library(db, user_id, library_id, limit, start).await
    }

    pub async fn list_series(
        db: &Db,
        user_id: i64,
        limit: i64,
        start: i64,
    ) -> anyhow::Result<ItemsResult> {
        item_store::list_series(db, user_id, limit, start).await
    }

    pub async fn list_series_by_library(
        db: &Db,
        user_id: i64,
        library_id: Option<i64>,
        limit: i64,
        start: i64,
    ) -> anyhow::Result<ItemsResult> {
        item_store::list_series_by_library(db, user_id, library_id, limit, start).await
    }

    /// Movie/Series 统一查询（搜索 + 排序 + 已看过滤 + 库过滤）。
    #[allow(clippy::too_many_arguments)]
    pub async fn list_movies_series(
        db: &Db,
        user_id: i64,
        library_id: Option<i64>,
        search_term: Option<&str>,
        item_types: &[&str],
        is_played: Option<bool>,
        tags: Option<&str>,
        sort_by: Option<&str>,
        sort_order: Option<&str>,
        limit: i64,
        start: i64,
    ) -> anyhow::Result<ItemsResult> {
        item_store::list_movies_series(
            db,
            user_id,
            library_id,
            search_term,
            item_types,
            is_played,
            tags,
            sort_by,
            sort_order,
            limit,
            start,
        )
        .await
    }

    /// 按 Genre 过滤 Items（`GenreIds` 参数，genre 主页）。
    pub async fn list_items_by_genre(
        db: &Db,
        user_id: i64,
        genre_ids: &[i64],
        item_types: &[&str],
        limit: i64,
        start: i64,
    ) -> anyhow::Result<ItemsResult> {
        item_store::list_items_by_genre(db, user_id, genre_ids, item_types, limit, start).await
    }

    pub async fn list_libraries(db: &Db) -> anyhow::Result<Vec<LibraryView>> {
        library_store::list_libraries(db).await
    }

    pub async fn get_item(db: &Db, id: i64, user_id: i64) -> anyhow::Result<Option<ItemRow>> {
        item_store::get_item(db, id, user_id).await
    }

    pub async fn get_item_type(db: &Db, id: i64) -> anyhow::Result<Option<String>> {
        item_store::get_item_type(db, id).await
    }

    /// item 是否存在（存在性校验，避免往 user_item_data 写悬空行）。
    pub async fn item_exists(db: &Db, id: i64) -> anyhow::Result<bool> {
        item_store::item_exists(db, id).await
    }

    pub async fn get_season(
        db: &Db,
        season_id: i64,
        user_id: i64,
    ) -> anyhow::Result<Option<ItemRow>> {
        item_store::get_season(db, season_id, user_id).await
    }

    pub async fn get_episode(
        db: &Db,
        episode_id: i64,
        user_id: i64,
    ) -> anyhow::Result<Option<ItemRow>> {
        item_store::get_episode(db, episode_id, user_id).await
    }

    pub async fn list_resume(
        db: &Db,
        user_id: i64,
        library_id: Option<i64>,
        parent_item: Option<i64>,
        limit: i64,
        start: i64,
    ) -> anyhow::Result<Vec<ResumeEntry>> {
        item_store::list_resume(db, user_id, library_id, parent_item, limit, start).await
    }

    /// `/Sessions` 数据源：进行中播放的 item。
    pub async fn list_active_sessions(db: &Db, user_id: i64) -> anyhow::Result<Vec<ItemRow>> {
        item_store::list_active_sessions(db, user_id).await
    }

    pub async fn list_latest(
        db: &Db,
        user_id: i64,
        library_id: Option<i64>,
        limit: i64,
    ) -> anyhow::Result<Vec<ItemRow>> {
        item_store::list_latest(db, user_id, library_id, limit).await
    }

    pub async fn list_next_up(
        db: &Db,
        user_id: i64,
        series_id: Option<i64>,
        limit: i64,
        start: i64,
    ) -> anyhow::Result<Vec<ItemRow>> {
        item_store::list_next_up(db, user_id, series_id, limit, start).await
    }

    pub async fn list_seasons(db: &Db, series_id: i64) -> anyhow::Result<Vec<ItemRow>> {
        item_store::list_seasons(db, series_id).await
    }

    pub async fn list_episodes(
        db: &Db,
        season_id: i64,
        user_id: i64,
    ) -> anyhow::Result<Vec<ItemRow>> {
        item_store::list_episodes(db, season_id, user_id).await
    }

    pub async fn get_image_path(
        db: &Db,
        parent_type: &str,
        relation_id: i64,
        image_type: &str,
        index: i64,
    ) -> anyhow::Result<Option<(i64, String)>> {
        image_store::get_image_path(db, parent_type, relation_id, image_type, index).await
    }

    /// 批量查询多个 item 各类型图片的行 id（Primary / Backdrop / Logo / Thumb / Banner），避免列表端点 N+1。
    pub async fn image_ids_batch(
        db: &Db,
        relation_ids: &[i64],
    ) -> anyhow::Result<std::collections::HashMap<i64, image_store::ImageTypeIds>> {
        image_store::image_ids_batch(db, relation_ids).await
    }

    /// 批量查询多个 item 的 Primary 图片行 id（`parent_id → 图片行 id`）。
    /// 只需主图的列表（如 Resume）用它，避免拉回无用图片类型。
    pub async fn image_primary_batch(
        db: &Db,
        relation_ids: &[i64],
    ) -> anyhow::Result<std::collections::HashMap<i64, i64>> {
        image_store::image_primary_batch(db, relation_ids).await
    }

    /// 批量查询 folder 项（Season/Series）的子集计数
    /// `(recursive_total, unplayed, season_count)`，
    /// 供 `item_to_json` 填 `RecursiveItemCount` / `ChildCount` / `UserData.UnplayedItemCount`。
    /// `season_count` 仅 Series 有值（直接子季数 → `ChildCount`）。
    pub async fn child_counts_batch(
        db: &Db,
        items: &[ItemRow],
        user_id: i64,
    ) -> anyhow::Result<std::collections::HashMap<i64, (i64, i64, Option<i64>)>> {
        item_store::child_counts_batch(db, items, user_id).await
    }

    pub async fn get_parent_image_path(
        db: &Db,
        item_type: &str,
        id: i64,
        image_type: &str,
        index: i64,
    ) -> anyhow::Result<Option<String>> {
        image_store::get_parent_image_path(db, item_type, id, image_type, index).await
    }

    /// 按指定 item id 集合过滤（`ListItemIds` 参数，BoxSet/合集内容）。
    pub async fn list_items_by_ids(
        db: &Db,
        user_id: i64,
        item_ids: &[i64],
    ) -> anyhow::Result<ItemsResult> {
        item_store::list_items_by_ids(db, user_id, item_ids).await
    }

    pub async fn get_playback_info(
        db: &Db,
        item_id: i64,
    ) -> anyhow::Result<Option<MediaSourceRow>> {
        media_store::get_playback_info(db, item_id).await
    }

    /// 批量查询多个 item 的视频分辨率 `(width, height)`（解析 media_source.metadata），
    /// 供 `item_to_json` 填 Episode 顶层 `Width` / `Height`。
    pub async fn video_dims_batch(
        db: &Db,
        item_ids: &[i64],
    ) -> anyhow::Result<std::collections::HashMap<i64, (Option<i64>, Option<i64>)>> {
        media_store::video_dims_batch(db, item_ids).await
    }

    /// 多版本 PlaybackInfo：返回 item 的所有 media_source 行。
    pub async fn list_media_sources(db: &Db, item_id: i64) -> anyhow::Result<Vec<MediaSourceRow>> {
        media_store::list_media_sources(db, item_id).await
    }

    pub async fn list_favorites(
        db: &Db,
        user_id: i64,
        video_type: Option<&str>,
        limit: i64,
        start: i64,
    ) -> anyhow::Result<ItemsResult> {
        item_store::list_favorites(db, user_id, video_type, limit, start).await
    }

    /// 查询所有 Genres（`/Genres` 端点数据源）。
    pub async fn list_genres(
        db: &Db,
        library_id: Option<i64>,
        limit: i64,
        start: i64,
    ) -> anyhow::Result<ItemsResult> {
        taxonomy_store::list_genres(db, library_id, limit, start).await
    }

    /// 查询所有 People（`/Persons` 端点数据源）。
    pub async fn list_persons(
        db: &Db,
        library_id: Option<i64>,
        limit: i64,
        start: i64,
    ) -> anyhow::Result<ItemsResult> {
        taxonomy_store::list_persons(db, library_id, limit, start).await
    }

    /// 查询所有年份（`/Years` 端点数据源）。
    pub async fn list_years(
        db: &Db,
        library_id: Option<i64>,
        limit: i64,
        start: i64,
    ) -> anyhow::Result<ItemsResult> {
        taxonomy_store::list_years(db, library_id, limit, start).await
    }

    /// 查询所有分级（`/OfficialRatings` 端点数据源）。
    pub async fn list_official_ratings(
        db: &Db,
        library_id: Option<i64>,
        limit: i64,
        start: i64,
    ) -> anyhow::Result<ItemsResult> {
        taxonomy_store::list_official_ratings(db, library_id, limit, start).await
    }

    /// `/Items/Counts` 数据源：各类型计数。
    pub async fn item_counts(db: &Db) -> anyhow::Result<(i64, i64, i64)> {
        taxonomy_store::item_counts(db).await
    }

    /// 查询单个 People（`/Users/{uid}/Items/p-{id}` 详情数据源）。
    pub async fn get_person(db: &Db, id: i64) -> anyhow::Result<Option<taxonomy_store::PersonRow>> {
        taxonomy_store::get_person(db, id).await
    }

    /// 按 People 过滤 Items（`PersonIds` 参数，person 主页）。
    pub async fn list_items_by_person(
        db: &Db,
        user_id: i64,
        person_ids: &[i64],
        item_types: &[&str],
        limit: i64,
        start: i64,
    ) -> anyhow::Result<ItemsResult> {
        item_store::list_items_by_person(db, user_id, person_ids, item_types, limit, start).await
    }

    /// 查询所有 Tags（`/Tags` 端点数据源）。
    /// 查询所有 Tags（`/Tags` 端点数据源）：`tag` 规范表（刮削时 TMDB keywords 写入）。
    pub async fn list_tags(db: &Db) -> anyhow::Result<Vec<String>> {
        taxonomy_store::list_tags(db).await
    }

    /// 查询所有 Studios（`/Studios` 端点数据源）。
    /// 从 `studio` 规范表读取（刮削时 TMDB production_companies 写入）。
    pub async fn list_studios(
        db: &Db,
        library_id: Option<i64>,
        limit: i64,
        start: i64,
    ) -> anyhow::Result<ItemsResult> {
        taxonomy_store::list_studios(db, library_id, limit, start).await
    }

    /// 按 Studio 过滤 Items（`StudioIds` 参数，studio 主页）。
    pub async fn list_items_by_studio(
        db: &Db,
        user_id: i64,
        studio_ids: &[i64],
        item_types: &[&str],
        limit: i64,
        start: i64,
    ) -> anyhow::Result<ItemsResult> {
        item_store::list_items_by_studio(db, user_id, studio_ids, item_types, limit, start).await
    }

    /// `/Items/{id}/Similar`：相似推荐（按共同 genre 数降序，回退同库）。
    pub async fn list_similar(
        db: &Db,
        user_id: i64,
        item_id: i64,
        limit: i64,
    ) -> anyhow::Result<ItemsResult> {
        item_store::list_similar(db, user_id, item_id, limit).await
    }

    /// 批量查询多个 item 的 genres + people（供 `item_to_json` / Latest 附加字段）。
    pub async fn taxonomy_batch(
        db: &Db,
        item_ids: &[i64],
    ) -> anyhow::Result<std::collections::HashMap<i64, taxonomy_store::ItemTaxonomy>> {
        taxonomy_store::taxonomy_batch(db, item_ids).await
    }

    /// 读取 app_setting（单个 key）。委托 [`settings_store`]。
    pub async fn get_setting(db: &Db, key: &str) -> anyhow::Result<Option<String>> {
        settings_store::get_setting(db, key).await
    }

    /// 写入 app_setting（UPSERT）。委托 [`settings_store`]。
    pub async fn set_setting(db: &Db, key: &str, value: &str) -> anyhow::Result<()> {
        settings_store::set_setting(db, key, value).await
    }

    /// 读取全部 app_setting。委托 [`settings_store`]。
    pub async fn list_settings(db: &Db) -> anyhow::Result<Vec<(String, String)>> {
        settings_store::list_settings(db).await
    }
}

/// 播放进度 / 收藏 / 已看门面（委托到 [`user_data_store`]）。
pub struct PlaybackStore;

impl PlaybackStore {
    pub async fn upsert_progress(
        db: &Db,
        user_id: i64,
        item_id: i64,
        play_ms: i64,
        is_complete: bool,
    ) -> anyhow::Result<()> {
        user_data_store::upsert_progress(db, user_id, item_id, play_ms, is_complete).await
    }

    /// 开始播放：`play_count` +1（Resume 的"看过/正在看"标记）。
    pub async fn mark_started(db: &Db, user_id: i64, item_id: i64) -> anyhow::Result<()> {
        user_data_store::mark_started(db, user_id, item_id).await
    }

    pub async fn toggle_favorite(
        db: &Db,
        user_id: i64,
        item_id: i64,
        favorite: bool,
    ) -> anyhow::Result<()> {
        user_data_store::toggle_favorite(db, user_id, item_id, favorite).await
    }

    pub async fn mark_played(
        db: &Db,
        user_id: i64,
        item_id: i64,
        played: bool,
    ) -> anyhow::Result<()> {
        user_data_store::mark_played(db, user_id, item_id, played).await
    }

    /// 读取 user_item_data 行（无记录返回 None）。
    pub async fn get_user_data(
        db: &Db,
        user_id: i64,
        item_id: i64,
    ) -> anyhow::Result<Option<UserItemData>> {
        user_data_store::get_user_data(db, user_id, item_id).await
    }
}
