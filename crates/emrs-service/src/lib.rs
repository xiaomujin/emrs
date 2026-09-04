//! emrs-service：业务编排层（导入流水线 / 认证流程 / 后台任务），不依赖 HTTP 框架。
//! 归属判据：只有「跨多个 core / infra 能力的编排」才进 service；
//! 单步直连（一次 store 调用即可完成）的业务留在 server 路由层，不为分层而分层。
//!
//! 模块：
//! - [`importer`]：三阶段流水线 Scan→Probe→Scrape + Importer 门面
//! - [`auth`]：登录编排（验密 → 签发 token → 写登录事件）
//! - [`job`]：JobManager（后台任务状态表 + tokio::spawn 协作取消）

pub mod auth;
pub mod importer;
pub mod job;
