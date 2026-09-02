//! emrs-server 侧 Emby 协议成型层：领域行 → Emby JSON。
//!
//! 协议原语（时间 / ID / 共享 DTO base / 响应壳）re-export 自 [`emby_proto`]
//! （与 emrs-core 的同名门面一致），本模块目录内是依赖领域数据的**成型**函数：
//!
//! - [`dto`]：`ItemRow` / `MediaSourceRow` → Emby JSON（item 详情 / PlaybackInfo）
//! - [`list_dto`]：列表端点卡片（Resume / NextUp / Seasons / Episodes / …）
//! - [`latest`]：`/Items/Latest` 轻量卡片
//! - [`views`]：`/Users/{id}/Views`（CollectionFolder 成型）
//! - [`session`]：会话 / 登录响应
//! - [`user`]：`/Users` 系列响应（UserDto）

pub use emby_proto::*;

mod dto;
mod latest;
mod list_dto;
mod session;
mod user;
mod views;
pub use dto::*;
pub use latest::*;
pub use list_dto::*;
pub use session::*;
pub use user::*;
pub use views::*;
