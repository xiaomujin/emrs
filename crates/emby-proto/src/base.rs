//! Emby 响应 DTO 共享 base：多接口复用的公共字段组，`#[serde(flatten)]` 注入父 struct。
//!
//! - [`ImageTagsDto`]：`ImageTags` 对象（Primary/Logo/Thumb，无则省略，全无则 `{}`）。
//! - [`BaseItemDto`]：item-like 响应（`ItemDto` /
//!   `LatestItemJson` / `CollectionFolderView`）
//!   共有的 11 个顶层字段，flatten 后直接出现在父对象顶层。
//!
//! 字段全 non-Option：避免 `flatten` × `skip_serializing_if` 交互；各子 struct 的
//! Option 变体字段（`overview` / `run_time_ticks` 等）留在自身。

use serde::Serialize;

use crate::ViewsUserData;

/// Emby `ImageTags` 对象。承载 Primary / Logo / Thumb / Banner（各自无则省略，全无则 `{}`）。
#[derive(Serialize, Default, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct ImageTagsDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumb: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub banner: Option<String>,
}

/// item-like 响应共有的 11 个顶层字段。
///
/// `#[serde(flatten)]` 进 `ItemDto` / `LatestItemJson` /
/// `CollectionFolderView`；序列化时这些字段直接出现在父对象顶层，
/// 与旧手写字段等价（键名/形状不变）。`item_type` 经 `rename="Type"` 输出 `Type`。
///
/// `Default`：除 `is_folder=false`/`item_type=""` 外全空/零；库视图等需在自身
/// `Default`/构造器里覆盖 `is_folder`/`item_type`/`primary_image_aspect_ratio`。
#[derive(Serialize, Default)]
#[serde(rename_all = "PascalCase")]
pub struct BaseItemDto {
    pub name: String,
    pub server_id: String,
    pub id: String,
    #[serde(rename = "Type")]
    pub item_type: String,
    pub is_folder: bool,
    pub date_created: String,
    pub user_data: ViewsUserData,
    pub primary_image_aspect_ratio: f64,
    pub image_tags: ImageTagsDto,
    pub backdrop_image_tags: Vec<String>,
    pub provider_ids: serde_json::Map<String, serde_json::Value>,
}

/// `{Name, Id}` 两字段身份 base。
///
/// `GenreItemDto` / `StudioDto`（item 子对象）与
/// 列表端点元素（[`NameIdTypeDto`]）共用。`PersonItemDto` 在此基础上扩展
/// `Role` / `Type` / `Character`。
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct NameIdDto {
    pub name: String,
    pub id: String,
}

/// `{Name, Id, Type}` 列表元素：`/Genres` `/Persons` `/Years` `/OfficialRatings`
/// `/Studios` 端点条目。flatten [`NameIdDto`] + `Type`。
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct NameIdTypeDto {
    #[serde(flatten)]
    pub name_id: NameIdDto,
    #[serde(rename = "Type")]
    pub item_type: String,
}

/// `{Name, Type}` 列表元素：`/Tags` 端点（无 Id）。
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct TagDto {
    pub name: String,
    #[serde(rename = "Type")]
    pub item_type: String,
}
