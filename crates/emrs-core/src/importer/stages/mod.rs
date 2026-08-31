//! 流水线阶段实现：Scan → Probe → Scrape（Identify 已并入 Scrape 单消费者）。
//!
//! 每个阶段是独立轮询的 tokio 任务（由 [`super::pipeline::Pipeline`] 启动），
//! 状态全部落 DB（`item.scrape_status` / `media_source.status` / `scan_job`），重启不丢任务。
//! 阶段实现委托各能力模块（[`super::scanner`] / [`super::probe`] / [`super::tmdb`]）。

mod probe;
mod scan;
mod scrape;

pub use probe::ProbeStage;
pub use scan::ScanStage;
pub use scrape::ScrapeStage;
