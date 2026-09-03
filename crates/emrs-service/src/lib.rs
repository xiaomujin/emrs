//! emrs-service：业务编排层（导入流水线 / 后台任务），不依赖 HTTP 框架。
//!
//! 模块：
//! - [`importer`]：三阶段流水线 Scan→Probe→Scrape + Importer 门面
//! - [`job`]：JobManager（后台任务状态表 + tokio::spawn 协作取消）

pub mod importer;
pub mod job;
