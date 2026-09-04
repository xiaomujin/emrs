//! 列表端点精简 DTO：每个列表端点只返回真实客户端（`emby_json/` 抓包）需要的字段，
//! 不再复用详情页的胖 [`super::dto::ItemDto`]。
//!
//! 裁剪基准：**以精简抓包为基线 + 零成本字段保留**——判据是 DB 成本而非严格必要性。
//! 凡随 ItemRow 载入、或由 `preload_image_flags` / `preload_child_counts` 预取、
//! 或可由 provider id 直接算出的字段（`ProviderIds` / `RunTimeTicks` / `ProductionYear`
//! / `DateCreated` / 图片标记）保留；需 `taxonomy_batch` 这一次额外查询才有的重字段
//! （`People` / `Genres` / `GenreItems` / `Studios` / `Tags`）从除 NextUp（需 People）
//! 与详情页外的所有列表移除。
//!
//! 与 [`super::latest::LatestItemJson`] 同构：`#[serde(flatten)] base: BaseItemDto` +
//! 端点专属字段、`from_row` 纯函数、零 DB 查询。字段集完全相同的端点共用一个结构体。
//!
//! 结构体 ↔ 端点（卡片分散在子模块，此处统一 re-export）：
//! - [`EpisodeCardJson`]：Shows/Episodes / Items 进入 season 分支。
//! - [`ResumeCardJson`]：`/Users/{uid}/Items/Resume`（图片各自独立、无回退）。
//! - [`SeasonCardJson`]：Shows/Seasons / Items 进入 series 分支。
//! - [`MovieSeriesCardJson`]：Items 根·库分支（Movie+Series 混合）/ Similar。
//! - [`NextUpJson`]：Shows/NextUp（唯一在列表里保留 People 的端点）。

mod cards;
mod next_up;
mod resume;
mod shared;

pub use cards::{EpisodeCardJson, MovieSeriesCardJson, SeasonCardJson};
pub use next_up::NextUpJson;
pub use resume::ResumeCardJson;