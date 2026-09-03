//! 数据导入：三阶段流水线 Scan→Probe→Scrape + Importer 门面。
//!
//! 元数据分离：扫描只落物理事实（不触网、不跑 ffprobe）；
//! Probe 消费 `media_source(status='pending')` 回填流信息；
//! Scrape 单一消费者处理 `item(scrape_status='pending')`（TMDB 匹配/按 ID 快路径）。
//! 状态全部落 DB，重启不丢任务。入口：`Importer::scan(path)` 同步扫描目录；
//! `Pipeline::start()` 启动后台轮询。
//!
//! 扫描器（[`emrs_infra::scanner`]）与 TMDB 刮削器（[`emrs_infra::tmdb`]）
//! 的实现在 emrs-infra，本模块只做编排。

pub mod pipeline;
pub mod stages;

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

use emrs_infra::http_client::Outbound;
use emrs_infra::scanner::{ScanStats, Scanner};
use emrs_infra::tmdb::TmdbScraper;
use emrs_infra::{db::Db, tmdb};

/// 导入器入口。
pub struct Importer {
    db: Arc<Db>,
    /// TMDB API key（空字符串表示不刮削）。
    tmdb_api_key: String,
    /// 出网配置（代理 + hosts 覆盖，TMDB 刮削用）。
    outbound: Arc<Outbound>,
}

impl Importer {
    pub fn new(db: Arc<Db>) -> Self {
        Self {
            db,
            tmdb_api_key: String::new(),
            outbound: Outbound::none(),
        }
    }

    /// 启用 TMDB 刮削的导入器（key 为空则扫描时不刮削）。
    pub fn with_tmdb_key(db: Arc<Db>, tmdb_api_key: String) -> Self {
        Self {
            db,
            tmdb_api_key,
            outbound: Outbound::none(),
        }
    }

    /// 启用 TMDB 刮削并指定出网配置（代理 / hosts）的导入器。
    pub fn with_tmdb_outbound(db: Arc<Db>, tmdb_api_key: String, outbound: Arc<Outbound>) -> Self {
        Self {
            db,
            tmdb_api_key,
            outbound,
        }
    }

    /// 扫描指定路径，导入媒体文件。
    pub async fn scan(&self, path: &Path) -> Result<ScanStats> {
        let scanner = Scanner::with_outbound(
            self.db.clone(),
            self.tmdb_api_key.clone(),
            self.outbound.clone(),
        );
        scanner.scan_path(path).await
    }

    /// 创建 TMDB 刮削器。
    pub fn tmdb_scraper(&self, api_key: &str) -> TmdbScraper {
        TmdbScraper::new(tmdb::TmdbConfig {
            api_key: api_key.to_string(),
            outbound: self.outbound.clone(),
            ..Default::default()
        })
    }
}
