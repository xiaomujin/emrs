//! 数据导入：三阶段流水线 Scan→Probe→Scrape + 目录扫描 + TMDB 刮削。
//!
//! 元数据分离：扫描只落物理事实（不触网、不跑 ffprobe）；
//! Probe 消费 `media_source(status='pending')` 回填流信息；
//! Scrape 单一消费者处理 `item(scrape_status='pending')`（TMDB 匹配/按 ID 快路径）。
//! 状态全部落 DB，重启不丢任务。入口：`Importer::scan(path)` 同步扫描目录；
//! `Pipeline::start()` 启动后台轮询。

pub mod filename;
pub mod nfo;
pub mod pipeline;
pub mod probe;
pub mod scanner;
pub mod stages;
pub mod strm;
pub mod tmdb;

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

use crate::db::Db;

pub use scanner::Scanner;
pub use tmdb::TmdbScraper;

/// 导入器入口。
pub struct Importer {
    db: Arc<Db>,
    /// TMDB API key（空字符串表示不刮削）。
    tmdb_api_key: String,
    /// 可选 HTTP 代理地址（TMDB 刮削用）。为空则直连。
    tmdb_proxy_url: Option<String>,
}

impl Importer {
    pub fn new(db: Arc<Db>) -> Self {
        Self {
            db,
            tmdb_api_key: String::new(),
            tmdb_proxy_url: None,
        }
    }

    /// 启用 TMDB 刮削的导入器（key 为空则扫描时不刮削）。
    pub fn with_tmdb_key(db: Arc<Db>, tmdb_api_key: String) -> Self {
        Self {
            db,
            tmdb_api_key,
            tmdb_proxy_url: None,
        }
    }

    /// 启用 TMDB 刮削并指定请求代理的导入器。
    pub fn with_tmdb_proxy(db: Arc<Db>, tmdb_api_key: String, proxy_url: Option<String>) -> Self {
        Self {
            db,
            tmdb_api_key,
            tmdb_proxy_url: proxy_url,
        }
    }

    /// 扫描指定路径，导入媒体文件。
    pub async fn scan(&self, path: &Path) -> Result<scanner::ScanStats> {
        let scanner = Scanner::with_proxy(
            self.db.clone(),
            self.tmdb_api_key.clone(),
            self.tmdb_proxy_url.clone(),
        );
        scanner.scan_path(path).await
    }

    /// 创建 TMDB 刮削器。
    pub fn tmdb_scraper(&self, api_key: &str) -> TmdbScraper {
        TmdbScraper::new(tmdb::TmdbConfig {
            api_key: api_key.to_string(),
            proxy_url: self.tmdb_proxy_url.clone(),
            ..Default::default()
        })
    }
}
