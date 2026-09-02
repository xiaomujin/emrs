//! 目录扫描 + 数据库写入。
//!
//! 遍历库目录，识别 STRM 文件，按目录结构创建 `item`（movie/series/season/
//! episode 自引用树）/ `media_source` / `item_image` 记录，幂等 upsert。
//! 内嵌流信息存 `media_source.metadata` JSON；`external_subtitle` 只存外部字幕
//! （外挂附件）。库路径挂 `library` + `library_path`。
//!
//! 元数据分离约定：扫描只落物理事实——**不触网、不跑 ffprobe**。TMDB 刮削由
//! Scrape 阶段消费 `item.scrape_status='pending'` 完成；流信息由 Probe 阶段
//! 消费 `media_source.status='pending'`（file 协议）完成。外挂字幕关联依赖
//! 目录列举与文件名匹配，留在扫描期执行，与 ffprobe 无耦合。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::info;

use crate::db::Db;
use crate::http_client::Outbound;

use super::filename;
use super::nfo;
use super::probe;
use super::strm::parse_strm;
use crate::stores::StreamInfo;
mod scrape;

/// 扫描结果统计。
#[derive(Debug, Default)]
pub struct ScanStats {
    pub libraries: usize,
    pub movies: usize,
    pub series: usize,
    pub episodes: usize,
    pub media: usize,
    pub skipped: usize,
    pub errors: usize,
}

/// 批量元数据刮削（TMDB）统计。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ScrapeStats {
    /// 成功刮削（写入了 tmdb_id 及元数据）。
    pub scraped: usize,
    /// 跳过（已有 tmdb_id 且非 force，或未配置 TMDB key）。
    pub skipped: usize,
    /// 匹配不到（终态 none；保留基础信息）。
    pub none: usize,
    /// 失败（TMDB 请求异常：重试中或达上限转 failed）。
    pub failed: usize,
}

/// 单条刮削结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrapeOutcome {
    Scraped,
    Skipped,
    NotFound,
    Failed,
}

/// 媒体文件元数据（本地直扫时填充；STRM 走默认空值）。
#[derive(Debug, Clone, Default)]
pub struct MediaMeta {
    pub file_size: Option<i64>,
    pub file_second: Option<i64>,
    pub container: Option<String>,
    /// ffprobe 解析的流信息（序列化到 `media_source.metadata`）。
    pub streams: Vec<StreamInfo>,
    /// ffprobe 解析的章节（Emby `ChapterInfo` 形状，序列化到 `media_source.chapters`）。
    pub chapters: Vec<serde_json::Value>,
}

/// 将媒体元数据序列化为 `metadata` JSON（含 ffprobe 流信息）。
fn meta_metadata_json(meta: &MediaMeta) -> String {
    serde_json::to_string(&meta.streams).unwrap_or_else(|_| "[]".to_string())
}

/// 将章节序列化为 `chapters` JSON（Emby `ChapterInfo` 数组）。
fn meta_chapters_json(meta: &MediaMeta) -> String {
    serde_json::to_string(&meta.chapters).unwrap_or_else(|_| "[]".to_string())
}

/// 目录扫描器。
pub struct Scanner {
    db: Arc<Db>,
    /// TMDB API key（空字符串表示不刮削）。
    tmdb_api_key: String,
    /// 出网配置（代理 + hosts 覆盖，TMDB 刮削用）。
    outbound: Arc<Outbound>,
    /// TMDB 进程级限速（次/秒），透传给内部构造的 TmdbConfig。
    /// 默认 20；流水线消费路径用 pipeline.scrape_rate_limit_per_sec 覆盖。
    tmdb_rate_limit_per_sec: u32,
    /// 扫描写库节流：每处理 N 个文件让出一次写锁（0 关闭）。见 `scan_path`。
    yield_every_files: usize,
    /// 每让出一次的休眠毫秒。
    yield_ms: u64,
    /// 本次 scan 已处理文件计数（scan_path 进入时清零，写锁外无并发）。
    file_counter: std::sync::atomic::AtomicUsize,
}

/// 全局扫描互斥：scan job 与 watch 触发的扫描可能并发，
/// SELECT-then-INSERT 式 upsert 在并发下会产生重复行，必须串行化。
static SCAN_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

impl Scanner {
    /// 创建扫描器（无代理、无 hosts）。
    ///
    /// `tmdb_api_key` 为空时跳过 TMDB 刮削。
    pub fn new(db: Arc<Db>, tmdb_api_key: String) -> Self {
        Self::with_rate(db, tmdb_api_key, Outbound::none(), 20)
    }

    /// 创建扫描器并指定出网配置（代理 / hosts）。
    pub fn with_outbound(db: Arc<Db>, tmdb_api_key: String, outbound: Arc<Outbound>) -> Self {
        Self::with_rate(db, tmdb_api_key, outbound, 20)
    }

    /// 创建扫描器并指定出网配置与进程级限速（次/秒，0 不限速）。
    pub fn with_rate(
        db: Arc<Db>,
        tmdb_api_key: String,
        outbound: Arc<Outbound>,
        rate_limit_per_sec: u32,
    ) -> Self {
        Self {
            db,
            tmdb_api_key,
            outbound,
            tmdb_rate_limit_per_sec: rate_limit_per_sec,
            yield_every_files: 0,
            yield_ms: 0,
            file_counter: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// 设置扫描写库节流：每处理 `every` 个文件休眠 `ms` 毫秒让出写锁（0 关闭）。
    pub fn with_yield(mut self, every: usize, ms: u64) -> Self {
        self.yield_every_files = every;
        self.yield_ms = ms;
        self
    }

    /// 内部构造 TmdbConfig 的统一入口（key / 出网 / 限速三处收敛）。
    pub(super) fn tmdb_config(&self) -> crate::importer::tmdb::TmdbConfig {
        crate::importer::tmdb::TmdbConfig {
            api_key: self.tmdb_api_key.clone(),
            outbound: self.outbound.clone(),
            requests_per_second: self.tmdb_rate_limit_per_sec,
            ..Default::default()
        }
    }

    /// 扫描指定路径，创建/更新库记录并扫描媒体文件。
    pub async fn scan_path(&self, path: &Path) -> Result<ScanStats> {
        // 互斥：同一时刻只允许一个扫描（跨 scan job / watch / CLI import）
        let _guard = SCAN_MUTEX.lock().await;
        // 重置本次扫描的写节流计数（scan_path 持 SCAN_MUTEX，无并发写计数竞争）。
        self.file_counter
            .store(0, std::sync::atomic::Ordering::Relaxed);
        let canonical = std::fs::canonicalize(path)
            .with_context(|| format!("无法解析路径: {}", path.display()))?;
        let path_str = normalize_canonical_path(&canonical);

        // 1. 创建或获取 library + library_path 记录
        let library_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "Library".to_string());

        let library_id = self.upsert_library(&library_name, &path_str).await?;
        let mut stats = ScanStats {
            libraries: 1,
            ..Default::default()
        };

        // 2. 递归扫描目录
        self.scan_dir(&canonical, library_id, &mut stats).await?;

        Ok(stats)
    }

    /// 查询路径对应的 library_id（通过 library_path 表）。
    pub async fn library_id_for_path(&self, path: &Path) -> Result<i64> {
        let canonical = std::fs::canonicalize(path)
            .with_context(|| format!("无法解析路径: {}", path.display()))?;
        let path_str = normalize_canonical_path(&canonical);

        // 查找已存在的 library_path
        if let Some(id) = sqlx::query_scalar::<_, i64>(
            "SELECT lp.library_id FROM library_path lp \
             JOIN library l ON l.id = lp.library_id \
             WHERE lp.path = ? LIMIT 1",
        )
        .bind(&path_str)
        .fetch_optional(self.db.pool())
        .await?
        {
            return Ok(id);
        }

        // 不存在则创建
        let library_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "Library".to_string());
        self.upsert_library(&library_name, &path_str).await
    }

    /// 探测单个 media_source（调 ffprobe 写 `media_source.metadata` JSON）。
    /// 返回探测是否成功（成功含"ffprobe 正常执行但未解析出流"的空结果；
    /// 时长 `probe_duration` 是独立头部解析，不参与成败判定）。
    ///
    /// 时长回填顺序：原生头部解析（MP4 moov / MKV Duration，精确且零子进程开销）
    /// → ffprobe `format.duration`（fragmented MP4、TS/AVI/WMV 等容器头部解析
    /// 拿不到时的兜底）。
    pub async fn probe_media_source(&self, media_source_id: i64, file_path: &str) -> bool {
        let path = std::path::PathBuf::from(file_path);

        let native_second = probe::probe_duration(&path).await;
        let container = path
            .extension()
            .and_then(|e| e.to_str())
            .and_then(|e| probe::container_for(e))
            .map(|s| s.to_string());
        // 带失败原因的全量探测（流 + 章节），用于区分 'ok' / 'failed'
        let probed = probe::probe_media_checked(&path).await;

        // 更新 media_source 元数据（内嵌流只存 metadata JSON，不再写 external_subtitle）
        let (media, probe_ok, reason) = match probed {
            Ok(m) => (m, true, None),
            Err(reason) => (super::probe::ProbeMedia::default(), false, Some(reason)),
        };
        let file_second = native_second.or(media.format_duration);
        tracing::debug!(
            media_source_id,
            file_second,
            native_second,
            ffprobe_second = media.format_duration,
            "时长回填"
        );
        let meta = MediaMeta {
            file_size: media.format_size,
            file_second,
            container: container.clone(),
            streams: media.streams,
            chapters: media.chapters,
        };
        let metadata_json = meta_metadata_json(&meta);
        let chapters_json = meta_chapters_json(&meta);
        let now = crate::emby::format_time_now();
        // 探测成功 → 'ok'（可继续被删除检测覆盖）；失败 → 'failed' 终态
        // （播放链路不读 status，failed 行照常 DirectPlay；重扫或 admin force 可复位）。
        let status = if probe_ok { "ok" } else { "failed" };
        tracing::info!(
            media_source_id,
            status,
            reason = reason.as_deref().unwrap_or(""),
            "probe 完成"
        );
        let _ = sqlx::query(
            "UPDATE media_source SET status = ?, container = COALESCE(?, container), \
             file_size = COALESCE(?, file_size), file_duration = COALESCE(?, file_duration), \
             metadata = ?, chapters = ?, updated_at = ? WHERE id = ?",
        )
        .bind(status)
        .bind(container.as_deref())
        .bind(meta.file_size)
        .bind(meta.file_second)
        .bind(&metadata_json)
        .bind(&chapters_json)
        .bind(&now)
        .bind(media_source_id)
        .execute(self.db.pool())
        .await;
        probe_ok
    }

    /// 递归扫描目录。
    async fn scan_dir(&self, dir: &Path, library_id: i64, stats: &mut ScanStats) -> Result<()> {
        let mut entries = match tokio::fs::read_dir(dir).await {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(path = %dir.display(), error = %e, "无法读取目录");
                stats.errors += 1;
                return Ok(());
            }
        };

        // 收集子目录、strm 文件、视频源文件、nfo 文件、字幕文件
        let mut subdirs = Vec::new();
        let mut strm_files = Vec::new();
        let mut video_files = Vec::new();
        let mut nfo_files = Vec::new();
        let mut subtitle_files = Vec::new();

        while let Some(entry) = entries.next_entry().await? {
            let ft = entry.file_type().await?;
            let path = entry.path();
            if ft.is_dir() {
                subdirs.push(path);
            } else if ft.is_file() {
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if ext == "strm" {
                    strm_files.push(path);
                } else if ext == "nfo" {
                    nfo_files.push(path);
                } else if ext == "srt" || ext == "ass" || ext == "ssa" || ext == "vtt" {
                    subtitle_files.push(path);
                } else if probe::is_video_ext(&ext) {
                    video_files.push(path);
                }
            }
        }

        // 先处理 STRM；同目录同名（同 stem）时 STRM 优先，视频源跳过避免重复媒体
        if !strm_files.is_empty() {
            for strm_path in &strm_files {
                if let Err(e) = self
                    .process_strm(strm_path, library_id, &nfo_files, &subtitle_files, stats)
                    .await
                {
                    tracing::warn!(path = %strm_path.display(), error = %e, "处理 STRM 失败");
                    stats.errors += 1;
                }
            }
        }

        if !video_files.is_empty() {
            let strm_stems: HashSet<String> = strm_files
                .iter()
                .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(String::from))
                .collect();
            for video_path in &video_files {
                let stem = video_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("");
                if !stem.is_empty() && strm_stems.contains(stem) {
                    continue;
                }
                if let Err(e) = self
                    .process_video_file(video_path, library_id, &nfo_files, &subtitle_files, stats)
                    .await
                {
                    tracing::warn!(path = %video_path.display(), error = %e, "处理视频源失败");
                    stats.errors += 1;
                }
            }
        }

        // 递归子目录
        for subdir in subdirs {
            Box::pin(self.scan_dir(&subdir, library_id, stats)).await?;
        }

        Ok(())
    }

    /// 处理单个 STRM 文件（Movie 或 Series Episode）。
    async fn process_strm(
        &self,
        strm_path: &Path,
        library_id: i64,
        nfo_files: &[PathBuf],
        subtitle_files: &[PathBuf],
        stats: &mut ScanStats,
    ) -> Result<()> {
        let content = tokio::fs::read(strm_path)
            .await
            .with_context(|| format!("读取 STRM 失败: {}", strm_path.display()))?;

        let strm_dir = strm_path.parent().unwrap();
        let parsed = parse_strm(&content, strm_dir)?;

        // 多信号分类：文件名 SxxExx / 季目录 → 剧集
        let is_episode = self.classify_media(strm_path);

        // 文件名（不含扩展名）
        let file_stem = strm_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        // 去掉 .strm 后缀（如果 file_stem 以 .strm 结尾，可能有多层扩展名）
        let display_name = file_stem.trim_end_matches(".strm").to_string();

        if is_episode {
            self.process_episode(
                strm_path,
                library_id,
                &display_name,
                &parsed,
                &MediaMeta::default(),
                nfo_files,
                subtitle_files,
                stats,
            )
            .await?;
        } else {
            self.process_movie(
                strm_path,
                library_id,
                &display_name,
                &parsed,
                &MediaMeta::default(),
                nfo_files,
                subtitle_files,
                stats,
            )
            .await?;
        }

        Ok(())
    }

    /// 处理本地视频源文件（直接入库，path_type='local'，带大小/时长/容器元数据）。
    async fn process_video_file(
        &self,
        video_path: &Path,
        library_id: i64,
        nfo_files: &[PathBuf],
        subtitle_files: &[PathBuf],
        stats: &mut ScanStats,
    ) -> Result<()> {
        // 元数据：扫描期只取廉价事实（大小 / 扩展名推容器），时长与流信息
        // 由 Probe 阶段异步回填（本阶段不跑 ffprobe，保证目录遍历不被阻塞）。
        let file_size = tokio::fs::metadata(video_path)
            .await
            .map(|m| m.len() as i64)
            .ok();
        let container = video_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .and_then(|e| probe::container_for(&e))
            .map(String::from);

        // 归一化为 forward-slash 绝对路径（对齐 scan_path 的 path_str）
        let abs = std::fs::canonicalize(video_path).unwrap_or_else(|_| video_path.to_path_buf());
        let path_url = normalize_canonical_path(&abs);

        let parsed = super::strm::StrmPath {
            path_type: "local".to_string(),
            path_url,
        };
        let meta = MediaMeta {
            file_size,
            container,
            ..Default::default()
        };

        let file_stem = video_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        let display_name = file_stem.trim_end_matches(".strm").to_string();

        if self.classify_media(video_path) {
            self.process_episode(
                video_path,
                library_id,
                &display_name,
                &parsed,
                &meta,
                nfo_files,
                subtitle_files,
                stats,
            )
            .await?;
        } else {
            self.process_movie(
                video_path,
                library_id,
                &display_name,
                &parsed,
                &meta,
                nfo_files,
                subtitle_files,
                stats,
            )
            .await?;
        }
        Ok(())
    }

    /// 多信号媒体类型分类：文件名含明确的季/集标记（SxxExx / E01 / 第X集）或
    /// 父目录为季目录 → 视为剧集；否则视为电影。
    fn classify_media(&self, path: &Path) -> bool {
        // 信号 1：文件名强剧集标记
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            let parsed = filename::parse_filename(stem);
            if parsed.episode > 0 {
                return true;
            }
        }
        // 信号 2：父目录为季目录（Season N / S\d+）
        if let Some(parent) = path.parent()
            && let Some(dir_name) = parent.file_name().and_then(|n| n.to_str())
        {
            return filename::parse_season_folder(dir_name) >= 0;
        }
        false
    }

    /// 处理 Movie 类型 STRM。
    #[allow(clippy::too_many_arguments)]
    async fn process_movie(
        &self,
        strm_path: &Path,
        library_id: i64,
        title: &str,
        parsed: &super::strm::StrmPath,
        meta: &MediaMeta,
        nfo_files: &[PathBuf],
        subtitle_files: &[PathBuf],
        stats: &mut ScanStats,
    ) -> Result<()> {
        let now = crate::emby::format_time_now();
        let uuid = uuid::Uuid::new_v4().to_string();

        // 解析文件名，获取年份/provider ID
        let pn = filename::parse_filename(title);

        // 查找同 stem NFO
        let stem = strm_path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let nfo = nfo_files
            .iter()
            .find(|p| p.file_stem().and_then(|s| s.to_str()) == Some(stem))
            .and_then(|p| nfo::read_nfo(p));

        // 决定标题：NFO 标题 > 文件名（已去 provider 标签）
        let final_title = nfo
            .as_ref()
            .and_then(|n| n.title.as_deref())
            .unwrap_or(&pn.title)
            .to_string();
        let final_title = if final_title.is_empty() {
            title.to_string()
        } else {
            final_title
        };

        // 先复用/创建 item(type=movie)。重扫以文件物理路径为锚复用已有条目,
        // 避免 title 被刮削改写后按标题失配 → 同一文件重复入库。
        let item_id = match self
            .find_movie_item_by_path(library_id, &parsed.path_url)
            .await?
        {
            Some(id) => id,
            None => {
                self.upsert_item(library_id, "movie", None, &final_title, None, None)
                    .await?
            }
        };

        // NFO 元数据优先写入（高于 TMDB 刮削结果；NFO 已有的 id 不再覆盖）
        if let Some(ref nfo) = nfo {
            self.update_item_meta(
                item_id,
                &final_title,
                nfo.description.as_deref(),
                nfo.air_date.as_deref(),
                nfo.year,
                nfo.tmdb_id.as_deref(),
                nfo.imdb_id.as_deref(),
                nfo.tvdb_id.as_deref(),
                nfo.runtime,
                nfo.tagline.as_deref(),
                nfo.status.as_deref(),
                nfo.official_rating.as_deref(),
                nfo.community_rating,
            )
            .await;
        }

        // TMDB 刮削不在扫描期执行（元数据分离）：item 已带 scrape_status='pending'，
        // 由 Scrape 阶段后台消费；NFO 的 tmdb_id 已经 update_item_meta 写入，
        // Scrape 会走按 ID 快路径。

        // 落 media_source：同路径重复文件保留旧行不 churn，否则替换该条目既有源。
        let name = strm_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");
        let (media_source_id, reused) = self
            .upsert_media_source(item_id, name, parsed, meta, &now, &uuid)
            .await?;

        // 外挂附件：字幕（内嵌流已存 media_source.metadata，不再写 external_subtitle）。
        // 复用/新插都跑——attach 幂等，补齐新增的外挂字幕文件。
        self.attach_subtitles_by_stem(media_source_id, stem, subtitle_files)
            .await;

        // NFO 富元数据兜底：分类/制片/标签/演员/海报/背景（幂等，重扫不 churn）。
        if let Some(ref nfo) = nfo {
            self.apply_nfo_relations(item_id, nfo).await;
        }

        if reused {
            info!(title = %final_title, "跳过重复电影源（保留既有 media_source）");
            stats.skipped += 1;
        } else {
            info!(title = %final_title, uuid, "导入电影");
            stats.movies += 1;
            stats.media += 1;
        }
        self.throttle_scan_write().await;
        Ok(())
    }

    /// 处理剧集类型 STRM（Series → Season → Episode → Media）。
    #[allow(clippy::too_many_arguments)]
    async fn process_episode(
        &self,
        strm_path: &Path,
        library_id: i64,
        title: &str,
        parsed: &super::strm::StrmPath,
        meta: &MediaMeta,
        nfo_files: &[PathBuf],
        subtitle_files: &[PathBuf],
        stats: &mut ScanStats,
    ) -> Result<()> {
        let now = crate::emby::format_time_now();
        let uuid = uuid::Uuid::new_v4().to_string();

        // 解析文件名获取集号、季号、标题
        let pn = filename::parse_filename(title);

        // 解析目录结构：../Series Name/Season NN/Episode.strm
        // 或扁平结构：../Series Name/Series Name S2E01.mkv
        let parent = strm_path.parent().unwrap();
        let grandparent = parent.parent();
        let season_dir = parent.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let season_from_dir = filename::parse_season_folder(season_dir);

        // 若父目录是季目录 → series=祖父目录、season=目录名；
        // 否则父目录即剧名目录 → series=父目录、season=文件名解析。
        let (series_dir_path, series_name, mut season_number) = if season_from_dir >= 0 {
            let gp = grandparent.unwrap_or(parent);
            let series = gp
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("Unknown Series")
                .to_string();
            (gp, series, season_from_dir)
        } else {
            (parent, season_dir.to_string(), pn.season)
        };
        if season_from_dir < 0 && season_number <= 0 {
            // 仅扁平结构回退:此时 season_number 取自文件名 pn.season,0 是「未知」哨兵
            // (无 Option,无法区分真第0季),保守回退第1季。
            // 季目录结构(S00/Season 0)的 0 是合法 Specials 季,保留,不并入第一季。
            season_number = 1;
        }
        // series 磁盘目录锚（归一化绝对路径）：同一剧跨扫描稳定、新增集文件同目录，
        // 供 upsert_series_item 复用，替代会被刮削改写的 title。
        let series_dir = series_dir_path
            .canonicalize()
            .map(|p| normalize_canonical_path(&p))
            .unwrap_or_else(|_| series_dir_path.to_string_lossy().replace('\\', "/"));

        // 集号：优先文件名解析，回退纯数字
        let episode_number: i64 = if pn.episode > 0 {
            pn.episode
        } else {
            title.parse::<i64>().unwrap_or(1)
        };

        // 查找同 stem NFO
        let stem = strm_path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let nfo = nfo_files
            .iter()
            .find(|p| p.file_stem().and_then(|s| s.to_str()) == Some(stem))
            .and_then(|p| nfo::read_nfo(p));

        // 1. 创建/获取 Series item。以剧集磁盘目录 source_dir 为稳定身份锚复用，
        //    避免 series.title 被刮削改写后按目录名失配 → 重复建整棵
        //    series/season/episode；新增一集文件也命中同一目录，不再重复建 series。
        let series_item_id = self
            .upsert_series_item(library_id, &series_name, &series_dir)
            .await?;

        // NFO Series 元数据（从 strm 所在目录逐级向上找 tvshow.nfo）
        let series_nfo = self
            .find_tvshow_nfo(strm_path)
            .and_then(|p| nfo::read_nfo(&p));
        if let Some(ref snfo) = series_nfo {
            self.update_item_meta(
                series_item_id,
                &series_name,
                snfo.description.as_deref(),
                snfo.air_date.as_deref(),
                snfo.year,
                snfo.tmdb_id.as_deref(),
                snfo.imdb_id.as_deref(),
                snfo.tvdb_id.as_deref(),
                snfo.runtime,
                snfo.tagline.as_deref(),
                snfo.status.as_deref(),
                snfo.official_rating.as_deref(),
                snfo.community_rating,
            )
            .await;
            // Series 富元数据兜底：分类/制片/标签/演员/海报/背景。
            self.apply_nfo_relations(series_item_id, snfo).await;
        }

        // TMDB Series 刮削不在扫描期执行（元数据分离）：Scrape 阶段消费
        // series 的 pending 状态时按季集派生回填；NFO tmdb_id 已写入，走按 ID 快路径。

        // 2. 创建/获取 Season item
        let season_item_id = self
            .upsert_item(
                library_id,
                "season",
                Some(series_item_id),
                &format!("Season {season_number}"),
                Some(season_number),
                None,
            )
            .await?;

        // 3. 创建/获取 Episode item
        //    先尝试虚拟条目实体化：若 TMDB 刮削已创建 is_virtual=1 的占位集，
        //    本地文件命中后直接实体化（is_virtual=0），避免重复建条目。
        let episode_title = nfo
            .as_ref()
            .and_then(|n| n.title.as_deref())
            .unwrap_or(title)
            .to_string();
        let episode_item_id = if self
            .materialize_virtual_episode(season_item_id, episode_number, &episode_title)
            .await?
        {
            sqlx::query_scalar::<_, i64>(
                "SELECT id FROM item WHERE parent_id = ? AND type = 'episode' \
                 AND episode_number = ? AND is_virtual = 0 \
                 ORDER BY id DESC LIMIT 1",
            )
            .bind(season_item_id)
            .bind(episode_number)
            .fetch_one(self.db.pool())
            .await?
        } else {
            self.upsert_item(
                library_id,
                "episode",
                Some(season_item_id),
                &episode_title,
                Some(season_number),
                Some(episode_number),
            )
            .await?
        };

        // 合并集（E03-E04）：为 episode+1..=episode_end 创建虚拟占位集
        if pn.episode_end > pn.episode {
            let now_v = crate::emby::format_time_now();
            for ep_num in (pn.episode + 1)..=pn.episode_end {
                let _ = sqlx::query(
                    "INSERT INTO item (type, parent_id, scrape_status, title, \
                     season_number, episode_number, is_virtual, created_at, updated_at) \
                     VALUES ('episode', ?, 'scraped', ?, ?, ?, 1, ?, ?)",
                )
                .bind(season_item_id)
                .bind(&episode_title)
                .bind(season_number)
                .bind(ep_num)
                .bind(&now_v)
                .bind(&now_v)
                .execute(self.db.pool())
                .await;
            }
        }

        // 单集 NFO 元数据
        if let Some(ref en) = nfo {
            self.update_item_meta(
                episode_item_id,
                &episode_title,
                en.description.as_deref(),
                en.air_date.as_deref(),
                en.year,
                en.tmdb_id.as_deref(),
                en.imdb_id.as_deref(),
                en.tvdb_id.as_deref(),
                en.runtime,
                en.tagline.as_deref(),
                en.status.as_deref(),
                en.official_rating.as_deref(),
                en.community_rating,
            )
            .await;
        }

        // 4. 落 media_source：同路径重复文件保留旧行不 churn，否则替换该条目既有源。
        let name = strm_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");
        let (media_source_id, reused) = self
            .upsert_media_source(episode_item_id, name, parsed, meta, &now, &uuid)
            .await?;

        // 外挂附件：字幕（内嵌流已存 media_source.metadata，不再写 external_subtitle）。
        // attach 幂等，复用/新插都跑以补齐新增字幕文件。
        self.attach_subtitles_by_stem(media_source_id, stem, subtitle_files)
            .await;

        // 单集 NFO 富元数据兜底：分类/制片/标签/演员/海报（幂等）。
        if let Some(ref en) = nfo {
            self.apply_nfo_relations(episode_item_id, en).await;
        }

        if reused {
            info!(
                series_name,
                season_number, episode_number, "跳过重复剧集源（保留既有 media_source）"
            );
            stats.skipped += 1;
        } else {
            info!(series_name, season_number, episode_number, uuid, "导入剧集");
            if stats.series == 0 {
                stats.series += 1;
            }
            stats.episodes += 1;
            stats.media += 1;
        }
        self.throttle_scan_write().await;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // 辅助方法
    // -----------------------------------------------------------------------

    /// 扫描写库节流：每处理 `yield_every_files` 个文件，主动 `wal_checkpoint(TRUNCATE)`
    /// 把 WAL 刷回主库并截断（释放 WAL 增长、给等待中的读者/认证写一个干净窗口），
    /// 再休眠 `yield_ms` 让出 sqlite 写锁。`yield_every_files=0` 时直接返回（不节流）。
    ///
    /// 仅在持 SCAN_MUTEX 的扫描路径调用，file_counter 无跨任务竞争。
    async fn throttle_scan_write(&self) {
        if self.yield_every_files == 0 || self.yield_ms == 0 {
            return;
        }
        let n = self
            .file_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        if !n.is_multiple_of(self.yield_every_files) {
            return;
        }
        // checkpoint 失败不致命（WAL 会自行在增长阈值时 checkpoint），忽略错误。
        let _ = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(self.db.pool())
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(self.yield_ms)).await;
    }

    /// 创建或获取 library + library_path 记录。
    async fn upsert_library(&self, name: &str, path: &str) -> Result<i64> {
        let now = crate::emby::format_time_now();

        // 先按 library_path.path 查 library_path，找到则取 library_id。
        // 命中已存在库时**不改名**——库名由 admin 建库时用户指定，扫描只负责入库条目，
        // 绝不能用文件夹 basename 覆盖用户设定的库名。
        let existing_lib_id: Option<i64> =
            sqlx::query_scalar("SELECT library_id FROM library_path WHERE path = ? LIMIT 1")
                .bind(path)
                .fetch_optional(self.db.pool())
                .await?;

        if let Some(lib_id) = existing_lib_id {
            return Ok(lib_id);
        }

        // 新建 library（首次扫描该路径：CLI / watch / 手输路径，用文件夹名兜底命名）
        {
            sqlx::query("INSERT INTO library (name, created_at, updated_at) VALUES (?, ?, ?)")
                .bind(name)
                .bind(&now)
                .bind(&now)
                .execute(self.db.pool())
                .await?;

            let lib_id =
                sqlx::query_scalar::<_, i64>("SELECT id FROM library ORDER BY id DESC LIMIT 1")
                    .fetch_one(self.db.pool())
                    .await?;

            // 新建 library_path
            let path_type = if path.starts_with("http://") || path.starts_with("https://") {
                "strm"
            } else {
                "local"
            };
            sqlx::query(
                "INSERT INTO library_path (library_id, path, path_type, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(lib_id)
            .bind(path)
            .bind(path_type)
            .bind(&now)
            .bind(&now)
            .execute(self.db.pool())
            .await?;

            Ok(lib_id)
        }
    }

    /// 创建或获取 item 记录（type=movie/series/season/episode，parent_id 自引用树）。
    ///
    /// 唯一键：
    /// - movie: (library_id, title)
    /// - series: (library_id, title)
    /// - season: (parent_id, season_number)
    /// - episode: (parent_id, episode_number)
    #[allow(clippy::too_many_arguments)]
    async fn upsert_item(
        &self,
        library_id: i64,
        item_type: &str,
        parent_id: Option<i64>,
        title: &str,
        season_number: Option<i64>,
        episode_number: Option<i64>,
    ) -> Result<i64> {
        let now = crate::emby::format_time_now();

        // 查询已有行（按 type 区分唯一键）
        let existing: Option<i64> =
            match item_type {
                "movie" | "series" => sqlx::query_scalar(
                    "SELECT id FROM item WHERE library_id = ? AND type = ? AND title = ? LIMIT 1",
                )
                .bind(library_id)
                .bind(item_type)
                .bind(title)
                .fetch_optional(self.db.pool())
                .await?,
                "season" => {
                    sqlx::query_scalar(
                        "SELECT id FROM item WHERE parent_id = ? AND type = 'season' \
                     AND season_number = ? LIMIT 1",
                    )
                    .bind(parent_id.unwrap_or(0))
                    .bind(season_number.unwrap_or(0))
                    .fetch_optional(self.db.pool())
                    .await?
                }
                "episode" => {
                    sqlx::query_scalar(
                        "SELECT id FROM item WHERE parent_id = ? AND type = 'episode' \
                     AND episode_number = ? LIMIT 1",
                    )
                    .bind(parent_id.unwrap_or(0))
                    .bind(episode_number.unwrap_or(0))
                    .fetch_optional(self.db.pool())
                    .await?
                }
                _ => None,
            };

        if let Some(id) = existing {
            // 更新标题（season/episode 标题可能变化）
            if item_type == "episode" {
                sqlx::query("UPDATE item SET title = ?, updated_at = ? WHERE id = ?")
                    .bind(title)
                    .bind(&now)
                    .bind(id)
                    .execute(self.db.pool())
                    .await?;
            }
            Ok(id)
        } else {
            sqlx::query(
                "INSERT INTO item (type, parent_id, library_id, scrape_status, title, \
                 season_number, episode_number, created_at, updated_at) \
                 VALUES (?, ?, ?, 'pending', ?, ?, ?, ?, ?)",
            )
            .bind(item_type)
            .bind(parent_id)
            .bind(library_id)
            .bind(title)
            .bind(season_number)
            .bind(episode_number)
            .bind(&now)
            .bind(&now)
            .execute(self.db.pool())
            .await?;

            let id = sqlx::query_scalar::<_, i64>("SELECT id FROM item ORDER BY id DESC LIMIT 1")
                .fetch_one(self.db.pool())
                .await?;
            Ok(id)
        }
    }

    /// 重扫去重锚点:按媒体文件物理路径查已存在的 movie 条目。
    ///
    /// `item.title` 会被 Scrape 改写为 TMDB 标题,不能作跨扫描的稳定身份;
    /// `media_source.path`(本地)/`remote_path`(strm/直链)存的是文件路径,
    /// 同库同文件重扫必然命中,故以它为复用锚。命中返回其 item_id,
    /// 未命中(首次扫描该文件)返回 None,调用方回退到按 title 的 upsert。
    async fn find_movie_item_by_path(&self, library_id: i64, path: &str) -> Result<Option<i64>> {
        let id = sqlx::query_scalar::<_, i64>(
            "SELECT i.id FROM item i \
             JOIN media_source ms ON ms.item_id = i.id \
             WHERE i.library_id = ? AND i.type = 'movie' \
               AND (ms.path = ? OR ms.remote_path = ?) \
             ORDER BY i.id LIMIT 1",
        )
        .bind(library_id)
        .bind(path)
        .bind(path)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(id)
    }

    /// 创建或获取 series 条目,以剧集磁盘目录 `source_dir` 为稳定身份锚。
    ///
    /// series.title 会被 Scrape 改写为 TMDB 标题,不能作跨扫描去重键;而剧集在磁盘上的
    /// 目录对同一剧稳定、且新增一集文件仍落在同目录下。匹配顺序:
    /// 1. 按 (library_id, type='series', source_dir) 命中 → 复用;
    /// 2. 未命中再按 title 命中(兼容 source_dir 引入前的历史 NULL 行)→ 回填 source_dir 后复用;
    /// 3. 都未命中 → 新建并写入 source_dir。
    ///
    /// 注:并发重复由外层 SCAN_MUTEX 串行化 + SELECT-then-INSERT 保证(schema 无唯一约束兜底)。
    async fn upsert_series_item(
        &self,
        library_id: i64,
        series_name: &str,
        series_dir: &str,
    ) -> Result<i64> {
        let now = crate::emby::format_time_now();

        if let Some(id) = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM item WHERE library_id = ? AND type = 'series' AND source_dir = ? LIMIT 1",
        )
        .bind(library_id)
        .bind(series_dir)
        .fetch_optional(self.db.pool())
        .await?
        {
            return Ok(id);
        }

        // 兼容历史行:source_dir 列引入前建的 series 该列为 NULL,按 title 命中则回填并复用。
        if let Some(id) = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM item WHERE library_id = ? AND type = 'series' \
             AND source_dir IS NULL AND title = ? LIMIT 1",
        )
        .bind(library_id)
        .bind(series_name)
        .fetch_optional(self.db.pool())
        .await?
        {
            let _ = sqlx::query("UPDATE item SET source_dir = ?, updated_at = ? WHERE id = ?")
                .bind(series_dir)
                .bind(&now)
                .bind(id)
                .execute(self.db.pool())
                .await;
            return Ok(id);
        }

        sqlx::query(
            "INSERT INTO item (type, parent_id, library_id, scrape_status, title, source_dir, created_at, updated_at) \
             VALUES ('series', NULL, ?, 'pending', ?, ?, ?, ?)",
        )
        .bind(library_id)
        .bind(series_name)
        .bind(series_dir)
        .bind(&now)
        .bind(&now)
        .execute(self.db.pool())
        .await?;
        let id = sqlx::query_scalar::<_, i64>("SELECT id FROM item ORDER BY id DESC LIMIT 1")
            .fetch_one(self.db.pool())
            .await?;
        Ok(id)
    }

    /// 合并更新 item 元数据（COALESCE 语义：仅填充非空字段，不覆盖已有值）。
    #[allow(clippy::too_many_arguments)]
    async fn update_item_meta(
        &self,
        id: i64,
        title: &str,
        description: Option<&str>,
        date_air: Option<&str>,
        year: Option<i32>,
        tmdb_id: Option<&str>,
        imdb_id: Option<&str>,
        tvdb_id: Option<&str>,
        runtime: Option<i64>,
        tagline: Option<&str>,
        status: Option<&str>,
        official_rating: Option<&str>,
        community_rating: Option<f64>,
    ) {
        let now = crate::emby::format_time_now();
        // NFO `<runtime>` 是分钟，DB 约定存秒（×60，与 TMDB 刮削路径一致）。
        let runtime_secs = runtime.map(|m| m * 60);
        let sort = sort_title(title);
        let _ = sqlx::query(
            "UPDATE item SET \
             description = COALESCE(?, description), \
             date_air = COALESCE(?, date_air), \
             tmdb_id = COALESCE(?, tmdb_id), \
             imdb_id = COALESCE(?, imdb_id), \
             tvdb_id = COALESCE(?, tvdb_id), \
             runtime = COALESCE(?, runtime), \
             production_year = COALESCE(?, production_year), \
             sort_title = COALESCE(?, sort_title), \
             tagline = COALESCE(?, tagline), \
             status = COALESCE(?, status), \
             official_rating = COALESCE(?, official_rating), \
             community_rating = COALESCE(?, community_rating), \
             updated_at = ? WHERE id = ?",
        )
        .bind(description)
        .bind(date_air)
        .bind(tmdb_id)
        .bind(imdb_id)
        .bind(tvdb_id)
        .bind(runtime_secs)
        .bind(year.map(|y| y as i64))
        .bind(sort.as_str())
        .bind(tagline)
        .bind(status)
        .bind(official_rating)
        .bind(community_rating)
        .bind(&now)
        .bind(id)
        .execute(self.db.pool())
        .await;
    }

    /// 从媒体文件所在目录逐级向上查找 `tvshow.nfo`（剧集系列级元数据）。
    ///
    /// Kodi 约定 `tvshow.nfo` 放在系列根目录，而 strm / 视频常在 `系列/Season N/`
    /// 下——当前目录的 NFO 列表只有单集 nfo，故必须回溯祖先目录取最近命中的一层。
    fn find_tvshow_nfo(&self, media_path: &Path) -> Option<PathBuf> {
        let mut dir = media_path.parent()?;
        loop {
            let candidate = dir.join("tvshow.nfo");
            if candidate.is_file() {
                return Some(candidate);
            }
            dir = dir.parent()?;
        }
    }

    /// 按 media_source.uuid 查 media_source.id。
    async fn media_source_id_by_uuid(&self, uuid: &str) -> Option<i64> {
        sqlx::query_scalar("SELECT id FROM media_source WHERE uuid = ?")
            .bind(uuid)
            .fetch_optional(self.db.pool())
            .await
            .unwrap_or(None)
    }

    /// 为条目落一个媒体源（幂等，重复不churn）。返回 `(media_source.id, 是否复用旧行)`。
    ///
    /// 重复检测:该 item 下若已有**同物理路径**（`path`/`remote_path` = `parsed.path_url`）
    /// 的 media_source,视为同一文件重复扫描 → **原样保留旧行**（uuid、探测结果 status、
    /// 外挂字幕一律不动,不 UPDATE / 不删 / 不插）,返回 `(旧id, true)`。
    /// 否则替换该条目的既有源:删旧 media_source + 外部字幕,插入新行,返回 `(新id, false)`。
    ///
    /// 旧逻辑无论路径是否相同都 `DELETE WHERE item_id` 再 INSERT,导致每次重扫复位
    /// `status='pending'` 触发全量重探、更换 uuid、重挂字幕——本函数即修复此问题。
    async fn upsert_media_source(
        &self,
        item_id: i64,
        name: &str,
        parsed: &super::strm::StrmPath,
        meta: &MediaMeta,
        now: &str,
        uuid: &str,
    ) -> Result<(Option<i64>, bool)> {
        let existing: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM media_source WHERE item_id = ? AND (path = ? OR remote_path = ?) LIMIT 1",
        )
        .bind(item_id)
        .bind(&parsed.path_url)
        .bind(&parsed.path_url)
        .fetch_optional(self.db.pool())
        .await?;
        if existing.is_some() {
            return Ok((existing, true));
        }

        // 路径未命中（首次，或该条目既有源指向其它文件）→ 替换该条目的旧源与外部字幕。
        sqlx::query("DELETE FROM external_subtitle WHERE media_source_id IN (SELECT id FROM media_source WHERE item_id = ?)")
            .bind(item_id)
            .execute(self.db.pool())
            .await?;
        sqlx::query("DELETE FROM media_source WHERE item_id = ?")
            .bind(item_id)
            .execute(self.db.pool())
            .await?;

        // 本地文件源进 Probe 队列（ffprobe 回填流信息后置 ok/failed）；
        // http/strm 直链无本地文件可探，直接 ok。
        let ms_status = if parsed.path_type == "local" {
            "pending"
        } else {
            "ok"
        };
        let protocol = protocol_from_path_type(&parsed.path_type);
        sqlx::query(
            "INSERT INTO media_source (uuid, item_id, name, status, protocol, path, remote_path, container, file_size, file_duration, metadata, chapters, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(uuid)
        .bind(item_id)
        .bind(name)
        .bind(ms_status)
        .bind(&protocol)
        .bind(if parsed.path_type == "local" { Some(&parsed.path_url) } else { None })
        .bind(if parsed.path_type != "local" { Some(&parsed.path_url) } else { None })
        .bind(&meta.container)
        .bind(meta.file_size)
        .bind(meta.file_second)
        .bind(meta_metadata_json(meta))
        .bind(meta_chapters_json(meta))
        .bind(now)
        .bind(now)
        .execute(self.db.pool())
        .await?;
        Ok((self.media_source_id_by_uuid(uuid).await, false))
    }

    /// 按文件名前缀匹配并关联同目录字幕文件到 media_source（外部字幕）。
    async fn attach_subtitles_by_stem(
        &self,
        media_source_id: Option<i64>,
        stem: &str,
        subtitle_files: &[PathBuf],
    ) {
        let Some(media_source_id) = media_source_id else {
            return;
        };
        let now = crate::emby::format_time_now();

        for sub in subtitle_files {
            let name = sub.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.starts_with(stem) {
                continue;
            }
            let codec = sub
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let path_url = sub.to_string_lossy().replace('\\', "/");
            // 文件名强制字幕标记（如 Movie.eng.forced.srt）→ is_forced=1
            let is_forced = has_forced_marker(name);

            // 幂等：同 media_source_id + path 的外部字幕已存在则跳过
            let existing: Option<i64> = sqlx::query_scalar(
                "SELECT id FROM external_subtitle WHERE media_source_id = ? AND path = ? LIMIT 1",
            )
            .bind(media_source_id)
            .bind(&path_url)
            .fetch_optional(self.db.pool())
            .await
            .unwrap_or(None);
            if let Some(id) = existing {
                // 已存在：同步强制标记（文件名改动后重扫生效）
                let _ = sqlx::query(
                    "UPDATE external_subtitle SET is_forced = ?, updated_at = ? WHERE id = ?",
                )
                .bind(is_forced as i64)
                .bind(&now)
                .bind(id)
                .execute(self.db.pool())
                .await;
                continue;
            }
            let _ = sqlx::query(
                "INSERT INTO external_subtitle (media_source_id, codec, display_title, \
                 is_forced, path, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(media_source_id)
            .bind(&codec)
            .bind(name)
            .bind(is_forced as i64)
            .bind(&path_url)
            .bind(&now)
            .bind(&now)
            .execute(self.db.pool())
            .await;
        }
    }

    /// 幂等关联一张图片（primary / backdrop）到 item。
    async fn attach_image(&self, item_id: i64, image_type: &str, path_url: &str) {
        self.attach_image_for("item", item_id, image_type, path_url)
            .await;
    }

    /// 幂等关联一张图片到任意对象（`parent_type`：'item' / 'people'）。
    /// 同 (parent_type, parent_id, image_type) 已存在则更新 URL，否则插入。
    /// 批量关联 backdrop 图片（先删旧再插新，最多 `max_count` 张，按 vote_average 降序）。
    pub async fn attach_backdrops(
        &self,
        item_id: i64,
        backdrops: &[crate::importer::tmdb::TmdbImage],
        max_count: usize,
    ) {
        let _ = sqlx::query(
            "DELETE FROM item_image WHERE parent_type = 'item' AND parent_id = ? AND image_type = 'backdrop'",
        )
        .bind(item_id)
        .execute(self.db.pool())
        .await;

        let mut sorted: Vec<&crate::importer::tmdb::TmdbImage> = backdrops.iter().collect();
        sorted.sort_by(|a, b| {
            b.vote_average
                .unwrap_or(0.0)
                .partial_cmp(&a.vote_average.unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let now = crate::emby::format_time_now();
        for img in sorted.iter().take(max_count) {
            if img.file_path.is_empty() {
                continue;
            }
            let url = format!("https://image.tmdb.org/t/p/w1280{}", img.file_path);
            let _ = sqlx::query(
                "INSERT INTO item_image (parent_type, parent_id, image_type, path_type, path_url, created_at, updated_at) \
                 VALUES ('item', ?, 'backdrop', 'url', ?, ?, ?)",
            )
            .bind(item_id)
            .bind(&url)
            .bind(&now)
            .bind(&now)
            .execute(self.db.pool())
            .await;
        }
    }

    async fn attach_image_for(
        &self,
        parent_type: &str,
        parent_id: i64,
        image_type: &str,
        path_url: &str,
    ) {
        if path_url.is_empty() {
            return;
        }
        let now = crate::emby::format_time_now();
        let img_type_lower = image_type.to_ascii_lowercase();
        let existing: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM item_image WHERE parent_type = ? AND parent_id = ? \
             AND image_type = ? LIMIT 1",
        )
        .bind(parent_type)
        .bind(parent_id)
        .bind(&img_type_lower)
        .fetch_optional(self.db.pool())
        .await
        .unwrap_or(None);

        if let Some(id) = existing {
            let _ = sqlx::query("UPDATE item_image SET path_url = ? WHERE id = ?")
                .bind(path_url)
                .bind(id)
                .execute(self.db.pool())
                .await;
        } else {
            let _ = sqlx::query(
                "INSERT INTO item_image (parent_type, parent_id, image_type, path_type, path_url, created_at) \
                 VALUES (?, ?, ?, 'url', ?, ?)",
            )
            .bind(parent_type)
            .bind(parent_id)
            .bind(&img_type_lower)
            .bind(path_url)
            .bind(&now)
            .execute(self.db.pool())
            .await;
        }
    }
}

/// 外挂字幕文件名是否带强制字幕标记（分隔符包围的 forced 标签，
/// 如 `Movie.eng.forced.srt` / `Movie-forced-zh.srt`）。与外部字幕语言推断同用点分标签约定。
fn has_forced_marker(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    // 尾部 `.forced.{ext}` 先剥掉字幕扩展名，再补一个分隔符让末尾标签也能命中包围式匹配
    let body = ["srt", "ass", "ssa", "vtt"]
        .iter()
        .find_map(|ext| lower.strip_suffix(&format!(".{ext}")))
        .unwrap_or(&lower);
    let padded = format!("{body}.");
    padded.contains(".forced.") || padded.contains("-forced-") || padded.contains("_forced_")
}

/// 从旧 path_type 字符串映射到 media_source.protocol 值。
fn protocol_from_path_type(path_type: &str) -> String {
    match path_type {
        "local" => "file".to_string(),
        "url" => "url".to_string(),
        "strm" => "strm".to_string(),
        // 其他 kind（http/https）由播放层 http 直链驱动处理
        other => other.to_string(),
    }
}

/// Emby `SortName` 推导：把英文冠词（The/A/An）移到末尾（"The Matrix" → "Matrix, The"），
/// 中文/日文等无冠词标题原样返回。空标题按原样处理。
pub fn sort_title(title: &str) -> String {
    let t = title.trim();
    for (art, sep) in [("The ", ", The"), ("A ", ", A"), ("An ", ", An")] {
        if let Some(rest) = t.strip_prefix(art) {
            return format!("{}{}", rest.trim(), sep);
        }
    }
    t.to_string()
}

/// 归一化 `std::fs::canonicalize` 后的路径为干净的正斜杠绝对路径字符串。
///
/// Windows 上 `canonicalize` 返回带 `\\?\`（本地盘）或 `\\?\UNC\`（网络路径）
/// 前缀的 verbatim 路径，直接入库会变成 `//?/D:/...` 这种异常形式，且与
/// `media_source.path` 的归一化不一致。此处剥掉前缀并统一正斜杠，保证库路径
/// 与媒体路径可比对、可去重。admin 建库/改库与扫描器 `scan_path` 共用此函数。
pub fn normalize_canonical_path(canon: &Path) -> String {
    let s = canon.to_string_lossy();
    let stripped: String = match s.strip_prefix(r"\\?\UNC\") {
        // \\?\UNC\server\share -> \\server\share
        Some(rest) => format!(r"\\{}", rest),
        None => match s.strip_prefix(r"\\?\") {
            // \\?\D:\path -> D:\path
            Some(rest) => rest.to_string(),
            None => s.into_owned(),
        },
    };
    stripped.replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_filename_delegates_to_filename_module() {
        // 年份/集号提取由 filename.rs 承担；此处验证 classify_media 的多信号分类。
        let movie = Path::new("/lib/My Movie (2020).mkv");
        assert!(!scanner_classify(movie));

        let episode = Path::new("/lib/My Show/My Show S2E01.mkv");
        assert!(scanner_classify(episode));

        // 父目录为季目录 → 剧集
        let in_season = Path::new("/lib/My Show/Season 2/ep01.mkv");
        assert!(scanner_classify(in_season));
    }

    #[test]
    fn forced_marker_detected_from_subtitle_filename() {
        // 点分/横杠/下划线包围的 forced 标签；末尾标签先剥扩展名命中
        assert!(has_forced_marker("Movie.eng.forced.srt"));
        assert!(has_forced_marker("Movie.2020.1080p.forced.ass"));
        assert!(has_forced_marker("Movie-forced-zh.srt"));
        assert!(has_forced_marker("Movie_forced_.vtt"));
        assert!(has_forced_marker("Movie.FORCED.ssa"));
        // 非标签形式不误判：普通字幕/标题含 forced 子串（无分隔符包围）
        assert!(!has_forced_marker("Movie.srt"));
        assert!(!has_forced_marker("Movie.eng.srt"));
        assert!(!has_forced_marker("Unforced Love.chs.srt"));
        assert!(!has_forced_marker("forced.srt"));
    }

    #[test]
    fn normalize_canonical_strips_verbatim_prefix() {
        // Windows 本地盘 verbatim 前缀 \\?\ 被剥掉
        assert_eq!(
            normalize_canonical_path(Path::new(r"\\?\D:\media\movies")),
            "D:/media/movies"
        );
        // 无前缀路径仅做反斜杠 -> 正斜杠归一
        assert_eq!(
            normalize_canonical_path(Path::new(r"D:\media\movies")),
            "D:/media/movies"
        );
        // Windows UNC verbatim 前缀 \\?\UNC\ 还原为 \\ 形式
        assert_eq!(
            normalize_canonical_path(Path::new(r"\\?\UNC\server\share\dir")),
            "//server/share/dir"
        );
    }

    #[test]
    fn sort_title_moves_articles() {
        assert_eq!(sort_title("The Matrix"), "Matrix, The");
        assert_eq!(sort_title("A Quiet Place"), "Quiet Place, A");
        assert_eq!(sort_title("An American Werewolf"), "American Werewolf, An");
        assert_eq!(sort_title("The"), "The");
        assert_eq!(
            sort_title("  The Shawshank Redemption  "),
            "Shawshank Redemption, The"
        );
        // 中文/日文标题原样
        assert_eq!(sort_title("爱书的下克上"), "爱书的下克上");
        assert_eq!(sort_title("我心里危险的东西"), "我心里危险的东西");
    }

    fn scanner_classify(path: &Path) -> bool {
        // 复刻 classify_media 的判定（不依赖 self）
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            let parsed = filename::parse_filename(stem);
            if parsed.episode > 0 {
                return true;
            }
        }
        if let Some(parent) = path.parent()
            && let Some(dir_name) = parent.file_name().and_then(|n| n.to_str())
        {
            return filename::parse_season_folder(dir_name) >= 0;
        }
        false
    }
}
