//! 应用状态。

use std::sync::Arc;

use emrs_core::cache::{Cache, CacheFacade};
use emrs_core::cloud::DriverRegistry;
use emrs_core::config::Config;
use emrs_core::db::Db;
use emrs_core::http_client::HttpClient;
use emrs_core::importer::pipeline::Pipeline;
use emrs_core::job::JobManager;
use emrs_core::playback::block_cache::BlockCache;
use emrs_core::watcher::LibraryWatcher;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Db>,
    pub cache: Arc<dyn Cache>,
    pub cfg: Arc<Config>,
    pub drivers: Arc<DriverRegistry>,
    /// 统一出网 HTTP 客户端（reqwest 连接池复用；图片代理等走此客户端）。
    pub http: Arc<HttpClient>,
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
