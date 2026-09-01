//! emrs-server 入口：默认启动 HTTP 服务，CLI 只指定配置文件路径。
//!
//! 媒体库导入不再走 CLI 子命令，统一在后台管理接口完成：
//!
//! ```bash
//! # 启动服务（默认配置 ./emrs.yml，缺失时自动生成）
//! emrs-server
//!
//! # 指定配置文件启动
//! emrs-server -c /etc/emrs/emrs.yml
//!
//! # 后台导入（需管理员 token）：
//! #   POST /admin/libraries            创建库（配置路径）
//! #   POST /admin/library/scan/start   触发异步扫描（不传 path 则扫全部库）
//! #   GET  /admin/library/scan/{id}    轮询扫描进度
//! ```
//!
//! # 后台导入示例
//!
//! ```bash
//! TOKEN=$(curl -s -X POST http://127.0.0.1:8086/admin/login \
//!   -H 'Content-Type: application/json' \
//!   -d '{"username":"admin","password":"admin123"}' | jq -r .token)
//!
//! # 配置库路径
//! curl -X POST http://127.0.0.1:8086/admin/libraries \
//!   -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
//!   -d '{"name":"电影","path":"/media/library"}'
//!
//! # 触发导入（后台 job）
//! curl -X POST http://127.0.0.1:8086/admin/library/scan/start \
//!   -H "Authorization: Bearer $TOKEN"
//! ```

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use emrs_core::cache::{self, CacheFacade};
use emrs_core::cloud::DriverRegistry;
use emrs_core::config::Config;
use emrs_core::db::Db;
use emrs_core::importer::pipeline::Pipeline;
use emrs_server::{AppState, router};

#[derive(Parser)]
#[command(
    name = "emrs-server",
    version,
    about = "Emby 兼容媒体服务器（默认启动 HTTP 服务）"
)]
struct Cli {
    /// 配置文件路径（默认: ./emrs.yml）
    #[arg(short, long, global = true, value_name = "FILE")]
    config: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // 日志：stdout（DEBUG）+ 文件每日滚动（INFO）
    let _guard = emrs_server::log::init_log();

    // 后台预装 ffmpeg/ffprobe（已安装则跳过；下载可能较慢，不阻塞启动）。
    tokio::task::spawn_blocking(|| {
        emrs_core::importer::probe::ensure_ffmpeg_binary();
    });

    let cli = Cli::parse();

    // 配置加载：支持 --config 指定路径，默认 ./emrs.yml
    let cfg = Arc::new(match &cli.config {
        Some(path) => Config::load_from(path).context("加载配置失败")?,
        None => Config::load().context("加载配置失败")?,
    });

    let db = Db::connect(&cfg.storage).await.context("数据库连接失败")?;
    db.migrate().await.context("数据库迁移失败")?;
    tracing::info!(dialect = ?db.dialect(), "数据库就绪");

    // 首次启动自动创建默认管理员
    match emrs_core::auth::AuthStore::ensure_default_admin(&db).await {
        Ok(Some(password)) => {
            tracing::info!("首次启动：已创建默认管理员 admin，初始密码：{password}（请尽快修改）");
        }
        Ok(None) => {}
        Err(e) => tracing::warn!(error = %e, "创建默认管理员失败"),
    }

    let db = Arc::new(db);

    let cache = cache::new_cache(&cfg.cache)?;
    tracing::info!(backend = cache.name(), "缓存就绪");

    let drivers = Arc::new(DriverRegistry::new(db.clone(), cfg.clone()));
    tracing::info!("driver 注册表就绪");

    // 统一出网配置：启动时加载一次 hosts（远程/文件/内联合并）+ 代理，供 TMDB 刮削与图片代理共用。
    let outbound = emrs_core::http_client::Outbound::from_config(&cfg.http).await;

    let http = Arc::new(emrs_core::http_client::HttpClient::new(&outbound));

    let jobs = Arc::new(emrs_core::job::JobManager::new());
    // 元数据分离：watch 只入队 scan_job，需持有流水线引用做即时唤醒，先建后绑
    let pipeline = Arc::new(Pipeline::new(
        db.clone(),
        cfg.pipeline.clone(),
        cfg.tmdb.api_key.clone(),
        outbound,
    ));
    let watcher = Arc::new(emrs_core::watcher::LibraryWatcher::with_pipeline(
        db.clone(),
        pipeline.clone(),
    ));
    let block_cache = Arc::new(emrs_core::playback::block_cache::BlockCache::new(
        emrs_core::playback::block_cache::BlockCacheConfig::default(),
    ));

    // 缓存门面（two-tier: moka L2 → redis L1 → DB）
    // L2 = 当前 cache 实例（memory/redis/valkey），L1 = None（单层即满足，多层时按需扩展）
    let two_tier = Arc::new(emrs_core::cache::TwoTierCache::new(cache.clone(), None));
    let cache_facade = Arc::new(CacheFacade::new(two_tier));
    // 启动预热（library:all / genre:all / settings），失败不阻断启动
    let cf_clone = cache_facade.clone();
    let db_clone = db.clone();
    tokio::spawn(async move {
        emrs_core::cache::preheat(&cf_clone, &db_clone).await;
        tracing::info!("缓存预热完成");
    });

    // 流水线启动（构造在 watcher 之前，见上）
    pipeline.start();

    let state = AppState {
        db,
        cache,
        cfg: cfg.clone(),
        drivers,
        http,
        jobs,
        watcher,
        block_cache,
        pipeline,
        cache_facade,
    };
    let app = router(state);

    // 使用 listen_port 库监听：IPv4/IPv6 双栈绑定、自动端口复用，返回标准库 listener
    let port = cfg.server.port;
    let std_listener =
        listen_port::listen_port(port).with_context(|| format!("监听端口 {port} 失败"))?;
    std_listener
        .set_nonblocking(true)
        .context("设置监听器非阻塞失败")?;
    let listener = tokio::net::TcpListener::from_std(std_listener).context("转换监听器失败")?;
    let addr = listener.local_addr().context("获取监听地址失败")?;
    tracing::info!(listen = %addr, "emrs-server 启动");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("服务退出")?;

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("收到退出信号，开始优雅关闭");
}
