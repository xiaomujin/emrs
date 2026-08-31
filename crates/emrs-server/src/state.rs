//! 应用状态。

use std::sync::Arc;

use emrs_core::cache::{Cache, CacheFacade};
use emrs_core::cloud::DriverRegistry;
use emrs_core::config::Config;
use emrs_core::db::Db;
use emrs_core::importer::pipeline::Pipeline;
use emrs_core::job::JobManager;
use emrs_core::playback::block_cache::BlockCache;
use emrs_core::playback::proxy::ProxyClient;
use emrs_core::watcher::LibraryWatcher;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Db>,
    pub cache: Arc<dyn Cache>,
    pub cfg: Arc<Config>,
    pub drivers: Arc<DriverRegistry>,
    /// 代理流转发客户端（reqwest 连接池复用）。
    pub proxy: Arc<ProxyClient>,
    /// 后台任务管理（扫描/刮削/探测 job）。
    pub jobs: Arc<JobManager>,
    /// 库目录监听（notify）。
    pub watcher: Arc<LibraryWatcher>,
    /// 分块缓存（磁盘块，热点加速）。
    pub block_cache: Arc<BlockCache>,
    /// 四阶段流水线（Scan→Probe→Identify→Scrape 后台轮询）。
    pub pipeline: Arc<Pipeline>,
    /// 缓存门面（two-tier: moka L2 → redis L1 → DB）。
    pub cache_facade: Arc<CacheFacade>,
}
