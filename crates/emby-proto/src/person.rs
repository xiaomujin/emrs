//! Person 相关静态形状 DTO（裁定 C11 静态轨）：字段集固定、与 item 类型无关。
//!
//! 构造逻辑（依赖 `PersonBrief`/`PersonRow` 等领域行）留在消费方 server 侧，
//! 本模块只承载可序列化的协议形状。

use serde::Serialize;

use super::base::{ImageTagsDto, NameIdDto, NameIdTypeDto};

/// Emby Person 详情 DTO（`/Users/{uid}/Items/p-{id}`）。
///
/// flatten [`NameIdTypeDto`]（Name/Id/Type）。`primary_image_id`（头像图片行 id）
/// 存在时 `ImageTags.Primary` = `img-{图片行 id}`；`PremiereDate`/`ProductionYear`
/// 仅 birthday 存在时发（`ProductionYear` 取 birthday 前 4 位 parse，失败→0）；
/// `Overview` 仅 description 存在时发。
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct PersonDetailDto {
    #[serde(flatten)]
    pub name_id_type: NameIdTypeDto,
    pub server_id: String,
    pub production_locations: Vec<String>,
    pub provider_ids: serde_json::Map<String, serde_json::Value>,
    pub image_tags: ImageTagsDto,
    pub backdrop_image_tags: Vec<String>,
    pub primary_image_aspect_ratio: f64,
    pub date_created: String,
    pub date_modified: String,
    pub external_urls: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub premiere_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub production_year: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overview: Option<String>,
}

/// Emby `People` 元素（演职员）。`Character` / `PrimaryImageTag` 仅当存在时输出。
/// flatten [`NameIdDto`]（Name/Id）+ `Role` + `Type` + 可选 `Character` / `PrimaryImageTag`。
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct PersonItemDto {
    #[serde(flatten)]
    pub name_id: NameIdDto,
    pub role: String,
    #[serde(rename = "Type")]
    pub item_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub character: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_image_tag: Option<String>,
}

/// Emby `ExternalUrls` 元素 `{Name, Url}`（Movie/Series 外链）。
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ExternalUrlDto {
    pub name: String,
    pub url: String,
}

/// `RequiredHttpHeaders`（恒空对象 `{}`）。
#[derive(Serialize, Default)]
pub struct RequiredHttpHeaders {}
