//! Emby 协议 DTO 成型模块：领域行 → Emby JSON（仍在 emrs-server，因成型依赖 `emrs_infra::stores` 行类型）。
//!
//! 从 HTTP 路由层下沉为独立成型模块，不依赖 axum / `AppState`；
//! 调用方传入 `server_id` + `signing_key`。
//!
//! 子模块（依赖顺序：`shared` / `person` / `media_source` 为叶子，`item` 依赖前三者）：
//! - [`shared`]：跨 DTO 复用的叶子辅助（图片 flags / provider ids / user data / 日期补全）
//! - [`person`]：Person 详情与 People 元素成型
//! - [`media_source`]：MediaSource / MediaStream 成型（含 DB 查询与播放票据签发）
//! - [`item`]：ItemDto 详情成型

mod item;
mod media_source;
mod person;
mod shared;

pub use item::{ItemDto, attach_media_sources, item_to_json};
pub use media_source::{MediaSourceDto, MediaStreamDto, media_sources_json};
pub use person::person_to_json;
pub use shared::{GenreItemDto, ItemImageFlags, StudioDto};

pub(crate) use person::person_item_dto;
pub(crate) use shared::{
    emby_date, item_user_data, provider_ids, provider_ids_map,
};