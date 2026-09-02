//! 配置加载：`emrs.yml`（存在时读取，不存在时自动创建默认配置并退出）。
//!
//! # 首次启动
//! 当 `emrs.yml` 不存在时，自动从内嵌的默认配置创建并提示用户修改后重启。
//!
//! # 密钥
//! 敏感密钥一律从配置文件读取（**不读取环境变量**）：
//! - `playback.signing_key`：播放票据签名密钥（必填，否则无法签发播放票据）
//! - `tmdb.api_key`：TMDB v3 API key（为空则刮削静默跳过）
//!
//! # 各后端 DSN 样例
//! - sqlite: `sqlite://data/emrs.db?mode=rwc`（mode=rwc 表示不存在则建库）
//! - mysql: `mysql://root:root@127.0.0.1:3306/emrs`
//! - postgres: `postgres://postgres:root@127.0.0.1:5432/emrs`

use std::io::Write;
use std::path::Path;

use anyhow::Context;
use rust_embed::Embed;
use serde::Deserialize;

/// 默认配置文件路径。
pub const DEFAULT_CONFIG_PATH: &str = "emrs.yml";

/// 内嵌资源目录（编译时打包进二进制）。
#[derive(Embed)]
#[folder = "resource"]
struct Resource;

// ---------------------------------------------------------------------------
// 配置结构体
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct Config {
    pub server: ServerConfig,
    pub storage: StorageConfig,
    pub cache: CacheConfig,
    pub emby: EmbyConfig,
    pub playback: PlaybackConfig,
    pub cloud: CloudConfig,
    pub tmdb: TmdbConfig,
    pub http: HttpConfig,
    pub pipeline: PipelineConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// 监听端口
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    /// 数据库 DSN（sqlite/mysql/postgres）
    pub dsn: String,
    pub max_connections: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CacheConfig {
    pub backend: CacheBackend,
    /// redis/valkey 时必填；memory 时忽略
    pub url: Option<String>,
    pub default_ttl_secs: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheBackend {
    #[default]
    Memory,
    Redis,
    Valkey,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct EmbyConfig {
    /// Emby ServerId（客户端展示用）
    pub server_id: String,
    /// 服务器名称（客户端展示用）
    pub server_name: String,
    /// 主 API key（管理员级兜底凭证）
    pub api_key: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PlaybackConfig {
    /// 播放票据签名密钥：从配置文件读取（必填，否则无法签发播放票据）
    pub signing_key: Option<String>,
    /// 播放票据有效期（秒）
    pub ticket_ttl_secs: i64,
    /// 302 直链结果缓存上限（秒）
    pub redirect_cache_ttl_secs: u64,
    /// strm/http 直链播放期缺流信息时异步 ffprobe 回填总开关（默认 true）。
    /// Probe 阶段只探测本地 file 源，strm 由播放请求命中时后台回填
    /// `media_source.metadata`/`file_duration`，当前请求不阻塞。
    pub strm_probe_backfill: bool,
    /// 单次 strm 后台 ffprobe 超时（秒，默认 30）。远端 URL 可能挂起，必须兜底。
    pub strm_probe_timeout_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CloudConfig {
    /// 启用的 driver 列表（当前仅 http 直链）
    pub drivers: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct TmdbConfig {
    /// TMDB v3 API key（为空则刮削静默跳过）：从配置文件读取，不再读取环境变量
    pub api_key: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct HttpConfig {
    /// 外部请求代理地址，如 `http://127.0.0.1:7890`。
    /// 设置后 TMDB 刮削和图片代理下载均通过此代理发起。
    /// 为空时不使用代理（直接连接）。
    /// 注意：与下方 hosts 覆盖互斥生效——走代理的请求不应用本地 hosts。
    pub proxy_url: Option<String>,
    /// hosts 远程拉取地址（如 gh-proxy 镜像的 CheckTMDB hosts）。
    /// 非空则启动时拉取一份标准 hosts 文本，解析后覆盖 TMDB/图片域名到指定 IP。
    /// 拉取成功写回 `hosts_file` 作离线兜底；失败回退读 `hosts_file`。为空则不联网。
    pub hosts_url: Option<String>,
    /// 本地 hosts 文件：既是远程拉取的缓存落点，也是纯离线/兜底来源。
    pub hosts_file: Option<String>,
    /// 内联 hosts 行（每行 `IP host`），优先级最高——同名 host 覆盖远程/文件。
    pub hosts_inline: Vec<String>,
    /// 是否本机代理获取图片（`/Items/{id}/Images`），默认 false。
    /// false：301 重定向到图片原始 URL，客户端直连上游，省本机带宽；
    /// true：本机下载后返回字节流（支持 maxWidth/maxHeight/quality 缩放，客户端访问不了图源时开启）。
    pub image_proxy: bool,
}

/// 四阶段流水线配置。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PipelineConfig {
    /// 是否启动后台四阶段轮询（默认 true）
    pub enabled: bool,
    /// probe 阶段并发度（默认 2，同时在飞的 ffprobe 进程数上限）
    pub probe_concurrency: usize,
    /// scrape 阶段批量大小（默认 4；消费循环串行 + TMDB 全局限速控制节奏）
    pub scrape_concurrency: usize,
    /// 轮询间隔（秒，默认 10）
    pub poll_interval_secs: u64,
    /// 刮削失败最大尝试次数，超过转 failed 终态（默认 5）。
    /// 匹配不到（none）不计数重试；网络/API 异常按 attempts 退避。
    pub scrape_retry_max_attempts: u64,
    /// TMDB 全局限速（次/秒，默认 3；进程级共享，覆盖 season/person/images 等附属请求）。
    /// 0 表示不限速。传给 TmdbConfig.requests_per_second。
    pub scrape_rate_limit_per_sec: u32,
    /// 删除检测兜底间隔（秒，默认 3600）；扫描完成后必触发一次。
    pub delete_check_interval_secs: u64,
    /// 扫描写库节流：每处理 N 个媒体文件让出一次写锁（默认 200，0 关闭）。
    /// 大库首扫时避免扫描任务长时间独占 sqlite 写锁，饿死 HTTP 认证/读。
    pub scan_yield_every_files: usize,
    /// 扫描每让出一次的休眠毫秒（默认 50，配合 `scan_yield_every_files`）。
    pub scan_yield_ms: u64,
    /// Probe 批次之间让出写锁的毫秒（默认 20，0 关闭）。
    pub probe_yield_ms: u64,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            probe_concurrency: 2,
            scrape_concurrency: 4,
            poll_interval_secs: 10,
            scrape_retry_max_attempts: 5,
            scrape_rate_limit_per_sec: 20,
            delete_check_interval_secs: 3600,
            scan_yield_every_files: 200,
            scan_yield_ms: 50,
            probe_yield_ms: 20,
        }
    }
}

// ---------------------------------------------------------------------------
// Default 实现
// ---------------------------------------------------------------------------

impl Default for ServerConfig {
    fn default() -> Self {
        Self { port: 8080 }
    }
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            proxy_url: None,
            hosts_url: Some("https://gh-proxy.com/https://raw.githubusercontent.com/cnwikee/CheckTMDB/refs/heads/main/Tmdb_host_ipv4".into()),
            hosts_file: Some("data/tmdb_hosts.txt".into()),
            hosts_inline: vec![],
            image_proxy: false,
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            dsn: "sqlite://data/emrs.db?mode=rwc".into(),
            max_connections: 8,
        }
    }
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            backend: CacheBackend::default(),
            url: None,
            default_ttl_secs: 3600,
        }
    }
}

impl Default for EmbyConfig {
    fn default() -> Self {
        Self {
            server_id: "emrs".into(),
            server_name: "EMRS".into(),
            api_key: String::new(),
        }
    }
}

impl Default for PlaybackConfig {
    fn default() -> Self {
        Self {
            signing_key: Some("emrs_signing_key".into()),
            ticket_ttl_secs: 3600,
            redirect_cache_ttl_secs: 3 * 3600,
            strm_probe_backfill: true,
            strm_probe_timeout_secs: 30,
        }
    }
}

impl Default for CloudConfig {
    fn default() -> Self {
        Self {
            drivers: vec!["http".into()],
        }
    }
}

// ---------------------------------------------------------------------------
// 加载逻辑
// ---------------------------------------------------------------------------

impl Config {
    /// 从当前目录的 `emrs.yml` 加载配置。
    ///
    /// 文件不存在时自动创建默认配置并返回错误提示，由调用方决定是否退出。
    pub fn load() -> anyhow::Result<Self> {
        Self::load_from(DEFAULT_CONFIG_PATH)
    }

    /// 从指定路径加载配置。
    ///
    /// - 默认路径不存在时自动创建默认配置并返回引导提示；
    /// - 显式指定的路径不存在时直接报错，不自动生成。
    pub fn load_from(config_path: &str) -> anyhow::Result<Self> {
        ensure_config_exists(config_path)?;

        let content = std::fs::read_to_string(config_path)
            .with_context(|| format!("读取配置文件 {config_path} 失败"))?;
        let cfg: Config = serde_yaml::from_str(&content)
            .with_context(|| format!("解析配置文件 {config_path} 失败"))?;
        Ok(cfg)
    }

    /// 供测试：从内嵌 YAML 字符串构建（不读磁盘、不创建文件）。
    pub fn from_yaml_str(yaml: &str) -> anyhow::Result<Self> {
        let cfg: Config = serde_yaml::from_str(yaml)?;
        Ok(cfg)
    }
}

/// 确保配置文件存在。
///
/// 仅默认路径缺失时从内嵌默认配置自动生成并返回引导提示；
/// 显式指定的路径缺失时返回错误，不自动建文件。
fn ensure_config_exists(path: &str) -> anyhow::Result<()> {
    if Path::new(path).exists() {
        return Ok(());
    }
    if path != DEFAULT_CONFIG_PATH {
        anyhow::bail!("配置文件不存在: {path}");
    }
    let embedded = Resource::get("default.yml")
        .ok_or_else(|| anyhow::anyhow!("内嵌默认配置 default.yml 不存在"))?;
    std::fs::File::create(path)
        .and_then(|mut f| f.write_all(embedded.data.as_ref()))
        .with_context(|| format!("创建默认配置文件 {path} 失败"))?;
    anyhow::bail!("默认配置文件 {path} 已创建，请修改配置后重新启动");
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let cfg = Config::default();
        assert_eq!(cfg.server.port, 8080);
        assert_eq!(cfg.cache.backend, CacheBackend::Memory);
        assert!(cfg.storage.dsn.starts_with("sqlite:"));
        assert!(cfg.cloud.drivers.contains(&"http".to_string()));
    }

    #[test]
    fn yaml_overrides_defaults() {
        let yaml = r#"
server:
  port: 9000

storage:
  dsn: "mysql://root:root@127.0.0.1:3306/emrs"
  max_connections: 16

cache:
  backend: "valkey"
  url: "redis://127.0.0.1:6379"
  default_ttl_secs: 60

emby:
  server_id: "srv-1"
  api_key: "k-1"

playback:
  ticket_ttl_secs: 120

cloud:
  drivers:
    - "http"
"#;
        let cfg = Config::from_yaml_str(yaml).unwrap();
        assert_eq!(cfg.server.port, 9000);
        assert_eq!(cfg.storage.dsn, "mysql://root:root@127.0.0.1:3306/emrs");
        assert_eq!(cfg.storage.max_connections, 16);
        assert_eq!(cfg.cache.backend, CacheBackend::Valkey);
        assert_eq!(cfg.cache.url.as_deref(), Some("redis://127.0.0.1:6379"));
        assert_eq!(cfg.emby.server_id, "srv-1");
        assert_eq!(cfg.playback.ticket_ttl_secs, 120);
        assert_eq!(cfg.cloud.drivers, vec!["http".to_string()]);
    }

    #[test]
    fn secrets_can_come_from_yaml() {
        // signing_key / tmdb.api_key 均从配置文件读取，不读环境变量
        let yaml = r#"
playback:
  signing_key: "from-yaml"
tmdb:
  api_key: "tmdb-from-yaml"
"#;
        let cfg = Config::from_yaml_str(yaml).unwrap();
        assert_eq!(cfg.playback.signing_key.as_deref(), Some("from-yaml"));
        assert_eq!(cfg.tmdb.api_key, "tmdb-from-yaml");
    }

    #[test]
    fn secrets_ignore_env() {
        // 即使存在同名环境变量也不覆盖配置文件值
        let yaml = r#"
playback:
  signing_key: "from-yaml"
"#;
        temp_env::with_var("EMRS_SIGNING_KEY", Some("from-env"), || {
            let cfg = Config::from_yaml_str(yaml).unwrap();
            assert_eq!(cfg.playback.signing_key.as_deref(), Some("from-yaml"));
        });
    }

    #[test]
    fn tmdb_api_key_from_yaml() {
        let yaml = r#"
tmdb:
  api_key: "from-yaml"
"#;
        let cfg = Config::from_yaml_str(yaml).unwrap();
        assert_eq!(cfg.tmdb.api_key, "from-yaml");
    }

    #[test]
    fn embedded_default_is_valid() {
        // 验证内嵌的 default.yml 可以被正确解析
        let embedded = Resource::get("default.yml").unwrap();
        let content = std::str::from_utf8(embedded.data.as_ref()).unwrap();
        let cfg: Config = serde_yaml::from_str(content).unwrap();
        assert_eq!(cfg.server.port, 8086);
        assert_eq!(cfg.cache.backend, CacheBackend::Memory);
    }
}
