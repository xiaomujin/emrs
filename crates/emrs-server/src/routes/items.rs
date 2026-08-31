//! Items / PlaybackInfo / Resume / Latest / NextUp / 剧集三件套 端点。
//!
//! 所有 DTO 输出对齐 Emby 协议，`PascalCase` 由 `#[serde]` 处理。
//! ItemId 带类型前缀（`i-`/`l-`/`p-`/`g-`/`s-`），DB 存裸 i64、前缀纯传输层；
//! 格式化/解析统一走 `emby::id`（裸数字不再兼容，一律视为非法）。
//!
//! 模块拆分：
//! - [`streaming`]：视频 302 / 代理流 / 本地 Range / 字幕
//! - [`sessions`]：播放进度上报
//! - [`user_data`]：收藏 / 已看 / 隐藏续看
//! - [`playback_info`]：PlaybackInfo + 图片代理
//! - [`list`]：Items 列表 / 详情 / 用户视图 / 续播 / 最新 / 剧集

use axum::Router;
use axum::routing::{get, post};
use serde::Deserialize;
use serde::de::{self, Visitor};

use emrs_core::emby;
use emrs_core::stores::ItemsStore;

use crate::state::AppState;

mod list;
mod playback_info;
mod sessions;
mod streaming;
mod user_data;

use list::*;
pub(crate) use playback_info::item_image;
use playback_info::{playback_info, playback_info_by_user};
use sessions::{report_playing, report_progress, report_stopped};
pub(crate) use streaming::serve_local_file;
pub use streaming::streaming_routes;
use user_data::{
    delete_favorite, hide_from_resume, mark_played, mark_played_delete, toggle_favorite,
};

/// Items 路由组（JSON API，受 Timeout 层约束）。
pub fn items_routes() -> Router<AppState> {
    Router::new()
        // Items 列表
        // .route("/Items", get(items_list))
        // .route("/Items/{id}", get(item_by_id))
        .route(
            "/Items/{id}/PlaybackInfo",
            get(playback_info).post(playback_info),
        )
        // Emby 带 userId 的 PlaybackInfo 别名（部分客户端用此路径）
        .route(
            "/Users/{user_id}/Items/{item_id}/PlaybackInfo",
            get(playback_info_by_user).post(playback_info_by_user),
        )
        // 用户 Items
        .route("/Users/{user_id}/Views", get(users_views))
        .route("/Users/{user_id}/Items", get(users_items))
        .route("/Users/{user_id}/Items/Resume", get(users_resume))
        .route("/Users/{user_id}/Items/Latest", get(users_latest))
        .route("/Users/{user_id}/Items/{item_id}", get(users_item_by_id))
        // 收藏
        .route(
            "/Users/{user_id}/FavoriteItems/{item_id}",
            post(toggle_favorite).delete(toggle_favorite),
        )
        .route(
            "/Users/{user_id}/FavoriteItems/{item_id}/Delete",
            post(delete_favorite),
        )
        // 已看
        .route(
            "/Users/{user_id}/PlayedItems/{item_id}",
            post(mark_played).delete(mark_played),
        )
        .route(
            "/Users/{user_id}/PlayedItems/{item_id}/Delete",
            post(mark_played_delete),
        )
        .route(
            "/Users/{user_id}/HideFromResume/{item_id}",
            post(hide_from_resume),
        )
        // 剧集
        .route("/Shows/NextUp", get(shows_next_up))
        .route("/Shows/{id}/Seasons", get(shows_seasons))
        .route("/Shows/{id}/Episodes", get(shows_episodes))
        // 相似推荐（Hills 详情页）
        .route("/Items/{id}/Similar", get(item_similar))
        // 会话进度
        .route("/Sessions/Playing", post(report_playing))
        .route("/Sessions/Playing/Progress", post(report_progress))
        .route("/Sessions/Playing/Stopped", post(report_stopped))
}

// ---------------------------------------------------------------------------
// Query params
// ---------------------------------------------------------------------------

/// 宽松 bool 反序列化：Emby 客户端常传空值（`Recursive=`）或大写变体（`True`/`False`）。
/// 空串/无法识别 → `None`；大小写不敏感识别 true/false。
pub fn deserialize_lenient_bool<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct LenientBool;
    impl<'de> Visitor<'de> for LenientBool {
        type Value = Option<bool>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a boolean or empty string")
        }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            match v.trim().to_ascii_lowercase().as_str() {
                "" => Ok(None),
                "true" | "1" | "yes" | "on" => Ok(Some(true)),
                "false" | "0" | "no" | "off" => Ok(Some(false)),
                _ => Err(E::custom(format!("invalid boolean value: {v}"))),
            }
        }
        fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
            Ok(Some(v))
        }
        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
    }
    deserializer.deserialize_any(LenientBool)
}

/// 宽松字符串反序列化：Emby 客户端常传空值（`parentid=`）。
/// 空串 → `None`；非空 → `Some(v)`（承载纯数字 ParentId）。
pub fn deserialize_lenient_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct LenientString;
    impl<'de> Visitor<'de> for LenientString {
        type Value = Option<String>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a string or empty value")
        }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            let t = v.trim();
            if t.is_empty() {
                Ok(None)
            } else {
                Ok(Some(t.to_string()))
            }
        }
        fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
            let t = v.trim();
            if t.is_empty() {
                Ok(None)
            } else {
                Ok(Some(t.to_string()))
            }
        }
        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
    }
    deserializer.deserialize_any(LenientString)
}

/// 宽松 i64 反序列化：Emby 客户端常传空值（`parentid=`）或非数字（`ParentId=abc`）。
/// 空串/无法识别 → `None`；合法数字 → `Some(n)`。
pub fn deserialize_lenient_i64<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct LenientI64;
    impl<'de> Visitor<'de> for LenientI64 {
        type Value = Option<i64>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("an integer or empty string")
        }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            let t = v.trim();
            if t.is_empty() {
                return Ok(None);
            }
            t.parse::<i64>()
                .map(Some)
                .map_err(|_| E::custom(format!("invalid integer value: {v}")))
        }
        fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
            Ok(Some(v))
        }
        fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
            i64::try_from(v)
                .map(Some)
                .map_err(|_| E::custom(format!("integer out of range: {v}")))
        }
        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
    }
    deserializer.deserialize_any(LenientI64)
}

/// 注意：lowercaseQuery 中间件会把 query key 统一小写，
/// 因此每个字段除 snake_case 名外还需全小写别名（Emby 客户端用 PascalCase）。
#[derive(Deserialize, Default)]
pub struct ItemsQuery {
    #[serde(default, deserialize_with = "deserialize_lenient_i64")]
    pub limit: Option<i64>,
    /// Emby 客户端几乎必传 UserId；master key 认证时 ctx.user_id=0，需以此参数为准。
    #[serde(
        alias = "userid",
        default,
        deserialize_with = "deserialize_lenient_i64"
    )]
    pub user_id: Option<i64>,
    #[serde(
        alias = "startindex",
        default,
        deserialize_with = "deserialize_lenient_i64"
    )]
    pub start_index: Option<i64>,
    /// 父目录/库 ID：纯数字字符串（item.id）。
    /// 用宽松字符串承载，避免空值解析失败返回 400。
    #[serde(
        alias = "parentid",
        default,
        deserialize_with = "deserialize_lenient_string"
    )]
    pub parent_id: Option<String>,
    #[serde(alias = "includeitemtypes")]
    pub include_item_types: Option<String>,
    #[serde(alias = "sortby")]
    pub sort_by: Option<String>,
    #[serde(alias = "sortorder")]
    pub sort_order: Option<String>,
    #[serde(default, deserialize_with = "deserialize_lenient_bool")]
    pub recursive: Option<bool>,
    #[serde(alias = "searchterm")]
    pub search_term: Option<String>,
    #[serde(
        alias = "isplayed",
        default,
        deserialize_with = "deserialize_lenient_bool"
    )]
    pub is_played: Option<bool>,
    #[serde(
        alias = "isfavorite",
        default,
        deserialize_with = "deserialize_lenient_bool"
    )]
    pub is_favorite: Option<bool>,
    #[serde(alias = "mediatypes")]
    pub media_types: Option<String>,
    /// Emby `Filters=IsFavorite`（逗号分隔集合，全小写接收）。
    #[serde(alias = "filters")]
    pub filters: Option<String>,
    pub ids: Option<String>,
    /// `SeriesId`：NextUp 按剧集过滤（`i-N` 前缀，宽松字符串承载）。
    #[serde(
        alias = "seriesid",
        default,
        deserialize_with = "deserialize_lenient_string"
    )]
    pub series_id: Option<String>,
    /// `PersonIds`：按人员过滤（`p-24` 或 `p-24,p-25` 逗号分隔；person 主页）。
    #[serde(
        alias = "personids",
        default,
        deserialize_with = "deserialize_lenient_string"
    )]
    pub person_ids: Option<String>,
    /// `StudioIds`：按工作室过滤（`5,6` 逗号分隔；studio 主页）。
    #[serde(
        alias = "studioids",
        default,
        deserialize_with = "deserialize_lenient_string"
    )]
    pub studio_ids: Option<String>,
    /// `GenreIds`：按类型过滤（`5,6` 逗号分隔；genre 主页）。
    #[serde(
        alias = "genreids",
        default,
        deserialize_with = "deserialize_lenient_string"
    )]
    pub genre_ids: Option<String>,
    /// `ListItemIds`：按指定 item id 集合过滤（逗号分隔；BoxSet/合集内容）。
    #[serde(
        alias = "listitemids",
        default,
        deserialize_with = "deserialize_lenient_string"
    )]
    pub list_item_ids: Option<String>,
    /// `Tags`：按标签过滤（逗号分隔，须全部命中；`/Tags` 端点数据源是 `tag` 规范表）。
    #[serde(default, deserialize_with = "deserialize_lenient_string")]
    pub tags: Option<String>,
}

/// `/Show/{id}/Episodes` 查询参数：季 ID 通过 `SeasonId` 传入（纯数字）。
#[derive(Deserialize, Default)]
pub struct SeasonQuery {
    #[serde(
        alias = "seasonid",
        default,
        deserialize_with = "deserialize_lenient_string"
    )]
    pub season_id: Option<String>,
}

// ---------------------------------------------------------------------------
// ID 解析辅助
// ---------------------------------------------------------------------------

/// 解析后的通用 ID：`类型前缀` + `数字 id`。
///
/// `i-/l-/p-/g-/s-{id}` 或裸数字（kind=`item`，旧客户端/缓存兼容）。
/// 前缀区分命名空间：`i-`item / `l-`library / `p-`people / `g-`genre / `s-`studio。
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedId {
    /// 类型：`item` / `library` / `people` / `genre` / `studio`。
    pub kind: &'static str,
    pub id: i64,
}

/// 通用 ID 解析：`{prefix}-{id}`，严格按前缀判型（裸数字不再兼容）。
/// 委托 [`emby::parse_id`]；前缀区分命名空间。
///
/// - `i-42` → `{ kind: "item", id: 42 }`；`l-2` → library；`p-24` → people；
///   `g-5` → genre；`s-7` → studio
/// - `42`（裸数字）→ None；`x-1` / `y-2023` / `-24` / `p-` / `24-3` / 空 → None
pub fn parse_generic_id(raw: &str) -> Option<ParsedId> {
    let (kind, id) = emby::parse_id(raw)?;
    Some(ParsedId {
        kind: kind_str(kind),
        id,
    })
}

/// `IdKind` → kind 字面量。
fn kind_str(k: emby::IdKind) -> &'static str {
    match k {
        emby::IdKind::Item => "item",
        emby::IdKind::Library => "library",
        emby::IdKind::People => "people",
        emby::IdKind::Genre => "genre",
        emby::IdKind::Studio => "studio",
        emby::IdKind::Image => "image",
    }
}

/// 解析 ItemId（`i-{id}`）为 item.id；裸数字/其他前缀返回 None。
fn parse_id(raw: &str) -> Option<i64> {
    match emby::parse_id(raw)? {
        (emby::IdKind::Item, id) => Some(id),
        _ => None,
    }
}

/// 解析 PersonId（`p-{id}`）为 people.id；裸数字/其他前缀返回 None。
pub fn parse_person_id(raw: &str) -> Option<i64> {
    match emby::parse_id(raw)? {
        (emby::IdKind::People, id) => Some(id),
        _ => None,
    }
}

/// 解析逗号分隔的 PersonIds（`p-24,p-25`），过滤非法项；全非法返回空 vec。
pub fn parse_person_ids(raw: &str) -> Vec<i64> {
    raw.split(',').filter_map(parse_person_id).collect()
}

/// 解析 StudioId（`s-{id}`）为 studio.id；裸数字/其他前缀返回 None。
fn parse_studio_id(raw: &str) -> Option<i64> {
    match emby::parse_id(raw)? {
        (emby::IdKind::Studio, id) => Some(id),
        _ => None,
    }
}

/// 解析逗号分隔的 StudioIds（`s-5,s-6`），过滤非法项；全非法返回空 vec。
pub fn parse_studio_ids(raw: &str) -> Vec<i64> {
    raw.split(',')
        .filter_map(|s| parse_studio_id(s.trim()))
        .collect()
}

/// 解析 GenreId（`g-{id}`）为 genre.id；裸数字/其他前缀返回 None。
fn parse_genre_id(raw: &str) -> Option<i64> {
    match emby::parse_id(raw)? {
        (emby::IdKind::Genre, id) => Some(id),
        _ => None,
    }
}

/// 解析逗号分隔的 GenreIds（`g-5,g-6`），过滤非法项；全非法返回空 vec。
pub fn parse_genre_ids(raw: &str) -> Vec<i64> {
    raw.split(',')
        .filter_map(|s| parse_genre_id(s.trim()))
        .collect()
}

/// 解析逗号分隔的 ItemIds（`i-5,i-6`，`ListItemIds` 参数），过滤非法项。
pub fn parse_item_ids(raw: &str) -> Vec<i64> {
    raw.split(',').filter_map(|s| parse_id(s.trim())).collect()
}

/// 解析 ItemId 为已校验存在的 item.id（movie/series/season/episode 统一粒度）。
/// 仅校验 item 存在且未删除；非法/已删/不存在返回 None（handler 回 404/400），
/// 避免往无外键约束的 user_item_data 写悬空行。
async fn resolve_item_id(st: &AppState, raw_id: &str) -> Option<i64> {
    let id = parse_id(raw_id)?;
    let exists = ItemsStore::item_exists(&st.db, id).await.ok()?;
    exists.then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_id_bare_rejected() {
        // 裸数字不再兼容：egress 始终发带前缀，裸数字一律非法
        assert_eq!(parse_generic_id("42"), None);
        assert_eq!(parse_generic_id("  7 "), None);
    }

    #[test]
    fn generic_id_prefix_kind() {
        assert_eq!(
            parse_generic_id("i-42"),
            Some(ParsedId {
                kind: "item",
                id: 42
            })
        );
        assert_eq!(
            parse_generic_id("l-2"),
            Some(ParsedId {
                kind: "library",
                id: 2
            })
        );
        assert_eq!(
            parse_generic_id("p-24"),
            Some(ParsedId {
                kind: "people",
                id: 24
            })
        );
        assert_eq!(
            parse_generic_id("g-5"),
            Some(ParsedId {
                kind: "genre",
                id: 5
            })
        );
        assert_eq!(
            parse_generic_id("s-7"),
            Some(ParsedId {
                kind: "studio",
                id: 7
            })
        );
    }

    #[test]
    fn generic_id_invalid() {
        assert_eq!(parse_generic_id(""), None);
        assert_eq!(parse_generic_id("42"), None, "裸数字不再兼容");
        assert_eq!(parse_generic_id("-24"), None, "前缀为空");
        assert_eq!(parse_generic_id("p-"), None, "缺数字");
        assert_eq!(parse_generic_id("p-0"), None, "id 必须 > 0");
        assert_eq!(parse_generic_id("24-3"), None, "数字前缀非法");
        assert_eq!(parse_generic_id("x-1"), None, "未知前缀");
        assert_eq!(parse_generic_id("y-2023"), None, "year 不在本方案");
    }

    #[test]
    fn parse_id_keeps_item_semantics() {
        assert_eq!(parse_id("42"), None, "裸数字不再兼容");
        assert_eq!(parse_id("i-42"), Some(42), "i- 前缀即 item");
        assert_eq!(parse_id("l-2"), None, "library 不算 item");
        assert_eq!(parse_id("p-24"), None, "people 不算 item");
        assert_eq!(parse_id("g-5"), None, "genre 不算 item");
        assert_eq!(parse_id("s-7"), None, "studio 不算 item");
        assert_eq!(parse_id("x-1"), None);
    }

    #[test]
    fn parse_person_id_strict() {
        assert_eq!(parse_person_id("p-24"), Some(24));
        assert_eq!(parse_person_id("24"), None, "裸数字不再兼容");
        assert_eq!(parse_person_id("i-24"), None, "item 不算 person");
        assert_eq!(parse_person_id("s-5"), None, "studio 不算 person");
        assert_eq!(parse_person_id("l-2"), None, "library 不算 person");
        assert_eq!(parse_person_id("abc"), None);
    }

    #[test]
    fn parse_person_ids_multiple() {
        assert_eq!(parse_person_ids("p-24,p-25"), vec![24, 25]);
        assert_eq!(parse_person_ids("p-24,abc,7"), vec![24], "裸 7 被过滤");
        assert_eq!(parse_person_ids(""), Vec::<i64>::new());
    }

    #[test]
    fn parse_genre_ids_strict() {
        assert_eq!(parse_genre_ids("g-5,g-6"), vec![5, 6]);
        assert_eq!(parse_genre_ids("5,6"), Vec::<i64>::new(), "裸数字不再兼容");
        assert_eq!(parse_genre_ids("g-5,abc,g-7"), vec![5, 7]);
        assert_eq!(parse_genre_ids(""), Vec::<i64>::new());
    }

    #[test]
    fn parse_studio_ids_strict() {
        assert_eq!(parse_studio_ids("s-5,s-6"), vec![5, 6]);
        assert_eq!(parse_studio_ids("5,6"), Vec::<i64>::new(), "裸数字不再兼容");
        assert_eq!(
            parse_studio_ids("s-5,g-7"),
            vec![5],
            "genre 前缀不算 studio"
        );
    }

    #[test]
    fn parse_item_ids_strict() {
        assert_eq!(parse_item_ids("i-5,i-6"), vec![5, 6]);
        assert_eq!(parse_item_ids("5,6"), Vec::<i64>::new(), "裸数字不再兼容");
        assert_eq!(parse_item_ids("i-5,p-7"), vec![5], "people 前缀不算 item");
    }
}
