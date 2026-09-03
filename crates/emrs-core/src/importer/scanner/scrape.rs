//! TMDB 刮削：`Scanner` 的 scrape_movie / scrape_tv / 虚拟条目生成。
//!
//! 从 `scanner/mod.rs` 拆出（scan 与 scrape 分离）。作为 `impl Scanner` 的子模块，
//! 可访问 `Scanner` 的私有字段（`db` / `tmdb_api_key` / `tmdb_proxy_url`）与
//! scan 侧的私有 helper（`attach_image`）。
//!
//! 元数据分离后的消费拓扑：批量整库刮削入口（scrape_library）已移除，
//! Scrape 阶段（stages/scrape.rs）是唯一消费者——pending 逐条走
//! scrape_movie/scrape_tv（搜索）或 *_by_tmdb（按 ID 快路径）。

use anyhow::Result;
use tracing::info;

use crate::importer::nfo::Nfo;
use crate::importer::tmdb::{
    Credits, EpisodeBrief, MovieDetail, SeasonBrief, TmdbMovie, TmdbScraper, TmdbTv, TvDetail,
    best_logo, extract_year,
};
use crate::stores::item_store;
use crate::stores::taxonomy_store;

use super::{Scanner, ScrapeOutcome, sort_title};

/// `upsert_people` 查询已有行用的元组：(id, description, birthday, deathday)。
type ExistingPerson = (i64, Option<String>, Option<String>, Option<String>);

impl Scanner {
    /// TMDB 刮削电影：查 `item` 行的 tmdb_id，为空（或 `force`）时按标题+年份
    /// 查询并写入文本元数据与海报。返回刮削结果。
    pub async fn scrape_movie(
        &self,
        item_id: i64,
        title: &str,
        year: Option<i32>,
        force: bool,
    ) -> ScrapeOutcome {
        if self.tmdb_api_key.is_empty() {
            return ScrapeOutcome::Skipped;
        }
        // 已有 tmdb_id 且非 force 则跳过（避免重复请求）
        if !force && self.has_tmdb_id(item_id).await {
            return ScrapeOutcome::Skipped;
        }

        let scraper = TmdbScraper::new(self.tmdb_config());
        let info = match scraper.search_movie(title, year).await {
            Ok(Some(info)) => info,
            Ok(None) => {
                info!(title, "TMDB 未找到电影");
                return ScrapeOutcome::NotFound;
            }
            Err(e) => {
                tracing::warn!(title, error = %e, "TMDB 电影刮削失败");
                return ScrapeOutcome::Failed;
            }
        };

        // 抓取完整详情（tagline/runtime/imdb_id/genres/credits/分级/关键词）。失败不阻断基础元数据写入。
        let detail = scraper.get_movie(info.id).await;
        self.apply_movie(&scraper, item_id, &info, &detail, title)
            .await
    }

    /// 按 TMDB ID 快路径：NFO / 手动识别已提供 tmdb_id 的行直接拉详情回填
    /// （不走标题搜索，对齐设计方案 §刮削"已有 tmdb_id：直接按 ID 拉取详情"）。
    /// 详情抓取失败按 Failed 处理，由 Scrape 阶段计数退避重试。
    pub async fn scrape_movie_by_tmdb(
        &self,
        item_id: i64,
        tmdb_id: i64,
        title_for_sort: &str,
    ) -> ScrapeOutcome {
        if self.tmdb_api_key.is_empty() {
            return ScrapeOutcome::Skipped;
        }
        let scraper = TmdbScraper::new(self.tmdb_config());
        match scraper.get_movie(tmdb_id).await {
            Err(e) => {
                tracing::warn!(item_id, tmdb_id, error = %e, "TMDB 电影详情拉取失败");
                ScrapeOutcome::Failed
            }
            Ok(detail) => {
                // 以详情合成搜索结果同形的 info，复用同一落库主体
                let info = TmdbMovie {
                    id: detail.id,
                    title: detail.title.clone(),
                    original_title: detail.original_title.clone(),
                    overview: detail.overview.clone().unwrap_or_default(),
                    release_date: detail.release_date.clone().unwrap_or_default(),
                    runtime: detail.runtime,
                    imdb_id: detail.imdb_id.clone(),
                    poster_path: detail.poster_path.clone(),
                    backdrop_path: detail.backdrop_path.clone(),
                };
                self.apply_movie(&scraper, item_id, &info, &Ok(detail), title_for_sort)
                    .await
            }
        }
    }

    /// 电影元数据落库主体（搜索路径与按 ID 路径共用）。
    /// `title` 为本地/NFO 清洗名，仅用于 SortName 推导与日志；正文以 TMDB 数据为准。
    /// 详情 `Err` 不阻断——沿用搜索结果字段写基础信息。
    async fn apply_movie(
        &self,
        scraper: &TmdbScraper,
        item_id: i64,
        info: &TmdbMovie,
        detail: &Result<MovieDetail, anyhow::Error>,
        title: &str,
    ) -> ScrapeOutcome {
        let (imdb_id, tagline, runtime, genres, credits, status, community_rating, official_rating) =
            match &detail {
                Ok(d) => (
                    d.imdb_id.clone(),
                    d.tagline.clone(),
                    d.runtime,
                    d.genres.clone(),
                    d.credits.clone(),
                    d.status.clone(),
                    d.vote_average,
                    d.release_dates.as_ref().and_then(|r| r.certification()),
                ),
                Err(e) => {
                    tracing::warn!(title, tmdb_id = info.id, error = %e, "TMDB 电影详情抓取失败");
                    (
                        None,
                        None,
                        None,
                        Vec::new(),
                        Credits::default(),
                        None,
                        None,
                        None,
                    )
                }
            };

        let now = crate::emby::format_time_now();
        let date_air = if info.release_date.is_empty() {
            None
        } else {
            Some(info.release_date.as_str())
        };
        let production_year = info
            .release_date
            .get(..4)
            .and_then(|y| y.parse::<i64>().ok());
        let sort_title = sort_title(title);
        if let Err(e) = sqlx::query(
            "UPDATE item SET tmdb_id = ?, title = COALESCE(?, title), \
             description = ?, date_air = ?, imdb_id = COALESCE(?, imdb_id), \
             tagline = COALESCE(?, tagline), runtime = COALESCE(?, runtime), \
             status = COALESCE(?, status), community_rating = COALESCE(?, community_rating), \
             official_rating = COALESCE(?, official_rating), \
             production_year = COALESCE(?, production_year), sort_title = COALESCE(?, sort_title), \
             scrape_status = 'scraped', updated_at = ? WHERE id = ?",
        )
        .bind(info.id.to_string())
        .bind(&info.title)
        .bind(&info.overview)
        .bind(date_air)
        .bind(&imdb_id)
        .bind(tagline.as_deref())
        .bind(runtime.map(|m| m * 60))
        .bind(status.as_deref())
        .bind(community_rating)
        .bind(official_rating.as_deref())
        .bind(production_year)
        .bind(sort_title.as_str())
        .bind(&now)
        .bind(item_id)
        .execute(self.db.pool())
        .await
        {
            tracing::warn!(title, error = %e, "TMDB 电影刮削结果写入失败");
            return ScrapeOutcome::Failed;
        }
        info!(title, tmdb_id = info.id, "TMDB 电影刮削成功");

        // 分类 / 演职员（genre / people 规范表 + 关联表）
        for g in &genres {
            let gid = self.upsert_genre(&g.id.to_string(), &g.name).await;
            self.link_genre(item_id, gid).await;
        }
        self.link_credits(scraper, item_id, &credits).await;
        // 制片公司 / 关键词（studio / tag 规范表 + 关联）
        if let Ok(d) = &detail {
            self.link_studios(item_id, &d.production_companies).await;
            self.link_tags(item_id, d.keywords.as_ref()).await;
        }

        // 图片全集（logo 语言挑选从这里拿）；失败不阻断主流程。
        let imgs = scraper.get_images("movie", info.id).await.ok();

        // 海报（搜索结果的 poster 指向 image.tmdb.org）
        if let Some(poster) = info.poster_path.as_deref() {
            let url = format!("https://image.tmdb.org/t/p/w500{poster}");
            self.attach_image(item_id, "primary", &url).await;
        }
        // 顶部大图 Backdrop（多张取 vote 最高的前 10）+ Thumb（列表缩略，复用主 backdrop）。
        if let Some(ref imgs) = imgs {
            self.attach_backdrops(item_id, &imgs.backdrops, 10).await;
        } else if let Some(backdrop) = info.backdrop_path.as_deref() {
            self.attach_image(
                item_id,
                "backdrop",
                &format!("https://image.tmdb.org/t/p/w1280{backdrop}"),
            )
            .await;
        }
        if let Some(backdrop) = info.backdrop_path.as_deref() {
            self.attach_image(
                item_id,
                "thumb",
                &format!("https://image.tmdb.org/t/p/w780{backdrop}"),
            )
            .await;
        }
        // Logo（清晰标题字标，Emby ImageType.Logo）：按 zh → en → 任意挑最佳。
        if let Some(l) = imgs.as_ref().and_then(|i| best_logo(&i.logos, "zh")) {
            self.attach_image(
                item_id,
                "logo",
                &format!("https://image.tmdb.org/t/p/original{}", l.file_path),
            )
            .await;
        }
        ScrapeOutcome::Scraped
    }

    /// TMDB 刮削剧集（Series item）：按剧名查询并写入 tmdb_id、元数据与海报。
    pub async fn scrape_tv(&self, item_id: i64, series_name: &str, force: bool) -> ScrapeOutcome {
        if self.tmdb_api_key.is_empty() {
            return ScrapeOutcome::Skipped;
        }
        // 已有 tmdb_id 且非 force 则跳过（剧集按季/集多次 upsert 同一 Series 行，避免重复请求）
        if !force && self.has_tmdb_id(item_id).await {
            return ScrapeOutcome::Skipped;
        }

        let scraper = TmdbScraper::new(self.tmdb_config());
        let info = match scraper
            .search_tv(series_name, extract_year(series_name))
            .await
        {
            Ok(Some(info)) => info,
            Ok(None) => {
                info!(series_name, "TMDB 未找到剧集");
                return ScrapeOutcome::NotFound;
            }
            Err(e) => {
                tracing::warn!(series_name, error = %e, "TMDB 剧集刮削失败");
                return ScrapeOutcome::Failed;
            }
        };

        // 抓取剧集完整详情（含外键 + 各季 + 演职员）。失败不阻断基础元数据写入，仅记录。
        let detail = scraper.get_tv(info.id).await;
        self.apply_tv(&scraper, item_id, &info, &detail, series_name)
            .await
    }

    /// 按 TMDB ID 快路径（剧集）：NFO / 手动识别已提供 tmdb_id 时直接拉详情回填，
    /// 并派生季/集同步与虚拟条目生成。详情抓取失败按 Failed 处理。
    pub async fn scrape_tv_by_tmdb(
        &self,
        item_id: i64,
        tmdb_id: i64,
        series_name: &str,
    ) -> ScrapeOutcome {
        if self.tmdb_api_key.is_empty() {
            return ScrapeOutcome::Skipped;
        }
        let scraper = TmdbScraper::new(self.tmdb_config());
        match scraper.get_tv(tmdb_id).await {
            Err(e) => {
                tracing::warn!(item_id, tmdb_id, error = %e, "TMDB 剧集详情拉取失败");
                ScrapeOutcome::Failed
            }
            Ok(detail) => {
                // 以详情合成搜索结果同形的 info，复用同一落库主体
                let info = TmdbTv {
                    id: detail.id,
                    name: detail.name.clone(),
                    original_name: detail.original_name.clone(),
                    overview: detail.overview.clone().unwrap_or_default(),
                    first_air_date: detail.first_air_date.clone().unwrap_or_default(),
                    runtime: detail
                        .episode_run_time
                        .as_ref()
                        .and_then(|v| v.first())
                        .copied(),
                    imdb_id: detail.external_ids.imdb_id.clone(),
                    tvdb_id: detail.external_ids.tvdb_id,
                    poster_path: detail.poster_path.clone(),
                    backdrop_path: detail.backdrop_path.clone(),
                };
                self.apply_tv(&scraper, item_id, &info, &Ok(detail), series_name)
                    .await
            }
        }
    }

    /// 剧集元数据落库主体：搜索路径与按 ID 路径共用；尾部派生季/集元数据同步、
    /// 图片、虚拟条目生成。详情 `Err` 不阻断——沿用搜索结果字段写基础信息。
    async fn apply_tv(
        &self,
        scraper: &TmdbScraper,
        item_id: i64,
        info: &TmdbTv,
        detail: &Result<TvDetail, anyhow::Error>,
        series_name: &str,
    ) -> ScrapeOutcome {
        let (
            imdb_id,
            tvdb_id,
            genres,
            credits,
            tagline,
            status,
            community_rating,
            end_date,
            runtime,
            official_rating,
        ) = match &detail {
            Ok(d) => (
                d.external_ids.imdb_id.clone(),
                d.external_ids.tvdb_id.map(|v| v.to_string()),
                d.genres.clone(),
                d.credits.clone(),
                d.tagline.clone(),
                d.status.clone(),
                d.vote_average,
                d.last_air_date.clone(),
                d.episode_run_time.as_ref().and_then(|v| v.first()).copied(),
                d.content_ratings.as_ref().and_then(|r| r.rating()),
            ),
            Err(e) => {
                tracing::warn!(series_name, tmdb_id = info.id, error = %e, "TMDB 剧集详情抓取失败");
                (
                    None,
                    None,
                    Vec::new(),
                    Credits::default(),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
            }
        };

        let now = crate::emby::format_time_now();
        let date_air = if info.first_air_date.is_empty() {
            None
        } else {
            Some(info.first_air_date.as_str())
        };
        let production_year = info
            .first_air_date
            .get(..4)
            .and_then(|y| y.parse::<i64>().ok());
        let sort_title = sort_title(series_name);
        if let Err(e) = sqlx::query(
            "UPDATE item SET tmdb_id = ?, title = COALESCE(?, title), \
             description = ?, date_air = ?, imdb_id = COALESCE(?, imdb_id), tvdb_id = COALESCE(?, tvdb_id), \
             tagline = COALESCE(?, tagline), status = COALESCE(?, status), \
             community_rating = COALESCE(?, community_rating), end_date = COALESCE(?, end_date), \
             runtime = COALESCE(?, runtime), \
             official_rating = COALESCE(?, official_rating), \
             production_year = COALESCE(?, production_year), sort_title = COALESCE(?, sort_title), \
             scrape_status = 'scraped', updated_at = ? WHERE id = ?",
        )
        .bind(info.id.to_string())
        .bind(&info.name)
        .bind(&info.overview)
        .bind(date_air)
        .bind(&imdb_id)
        .bind(&tvdb_id)
        .bind(tagline.as_deref())
        .bind(status.as_deref())
        .bind(community_rating)
        .bind(end_date.as_deref())
        .bind(runtime.map(|r| r * 60))
        .bind(official_rating.as_deref())
        .bind(production_year)
        .bind(sort_title.as_str())
        .bind(&now)
        .bind(item_id)
        .execute(self.db.pool())
        .await
        {
            tracing::warn!(series_name, error = %e, "TMDB 剧集刮削结果写入失败");
            return ScrapeOutcome::Failed;
        }
        info!(series_name, tmdb_id = info.id, "TMDB 剧集刮削成功");

        // 分类 / 演职员（genre / people 规范表 + 关联表）
        for g in &genres {
            let gid = self.upsert_genre(&g.id.to_string(), &g.name).await;
            self.link_genre(item_id, gid).await;
        }
        self.link_credits(scraper, item_id, &credits).await;
        // 制片公司 / 关键词（studio / tag 规范表 + 关联）
        if let Ok(d) = &detail {
            self.link_studios(item_id, &d.production_companies).await;
            self.link_tags(item_id, d.keywords.as_ref()).await;
        }

        // 同步季/集元数据（简介 / 日期）。仅当详情抓取成功且本地已有对应行时更新，
        // 失败不阻断整体刮削（用户可能没扫出对应季/集）。
        if let Ok(detail) = &detail
            && let Some(seasons) = &detail.seasons
        {
            for season in seasons {
                self.update_season_meta(item_id, season.season_number, season)
                    .await;
                if let Err(e) = self
                    .scrape_episodes(scraper, item_id, info.id, season.season_number, series_name)
                    .await
                {
                    tracing::warn!(
                        series_name,
                        tmdb_series_id = info.id,
                        season_number = season.season_number,
                        error = %e,
                        "TMDB 剧集列表刮削失败"
                    );
                }
            }
        }

        // 图片全集（logo 语言挑选从这里拿）；失败不阻断主流程。
        let imgs = scraper.get_images("tv", info.id).await.ok();

        if let Some(poster) = info.poster_path.as_deref() {
            let url = format!("https://image.tmdb.org/t/p/w500{poster}");
            self.attach_image(item_id, "primary", &url).await;
        }
        // 顶部大图 Backdrop（多张取 vote 最高的前 10）+ Thumb（列表缩略，复用主 backdrop）。
        if let Some(ref imgs) = imgs {
            self.attach_backdrops(item_id, &imgs.backdrops, 10).await;
        } else if let Some(backdrop) = info.backdrop_path.as_deref() {
            self.attach_image(
                item_id,
                "backdrop",
                &format!("https://image.tmdb.org/t/p/w1280{backdrop}"),
            )
            .await;
        }
        if let Some(backdrop) = info.backdrop_path.as_deref() {
            self.attach_image(
                item_id,
                "thumb",
                &format!("https://image.tmdb.org/t/p/w780{backdrop}"),
            )
            .await;
        }
        // Logo（清晰标题字标，Emby ImageType.Logo）：按 zh → en → 任意挑最佳。
        if let Some(l) = imgs.as_ref().and_then(|i| best_logo(&i.logos, "zh")) {
            self.attach_image(
                item_id,
                "logo",
                &format!("https://image.tmdb.org/t/p/original{}", l.file_path),
            )
            .await;
        }

        // 虚拟条目生成：对比 TMDB 季集全集 vs 本地 item 树，缺失集建 is_virtual=true
        if let Err(e) = self.generate_virtual_entries(item_id).await {
            tracing::warn!(series_name, series_id = item_id, error = %e, "虚拟条目生成失败");
        }

        ScrapeOutcome::Scraped
    }

    /// 把 TMDB 季信息写入 `item`(season)（仅当本地已有该季行；title/description/date_air 为空不覆盖），
    /// 并为季写入自身海报（Primary），避免客户端回退到剧海报。
    async fn update_season_meta(
        &self,
        series_item_id: i64,
        season_number: i64,
        season: &SeasonBrief,
    ) {
        let _ = sqlx::query(
            "UPDATE item SET title = COALESCE(?, title), \
             description = COALESCE(?, description), \
             date_air = COALESCE(?, date_air), \
             community_rating = COALESCE(?, community_rating), updated_at = ? \
             WHERE parent_id = ? AND type = 'season' AND season_number = ?",
        )
        .bind(season.name.as_deref().filter(|s| !s.is_empty()))
        .bind(season.overview.as_deref())
        .bind(season.air_date.as_deref())
        .bind(season.vote_average)
        .bind(crate::emby::format_time_now())
        .bind(series_item_id)
        .bind(season_number)
        .execute(self.db.pool())
        .await;

        if let Some(poster) = season.poster_path.as_deref().filter(|s| !s.is_empty()) {
            let season_id: Option<i64> = sqlx::query_scalar(
                "SELECT id FROM item WHERE parent_id = ? AND type = 'season' \
                 AND season_number = ? LIMIT 1",
            )
            .bind(series_item_id)
            .bind(season_number)
            .fetch_optional(self.db.pool())
            .await
            .ok()
            .flatten();
            if let Some(season_id) = season_id {
                let url = format!("https://image.tmdb.org/t/p/w500{poster}");
                self.attach_image(season_id, "primary", &url).await;
            }
        }
    }

    /// 抓取某季剧集列表并同步 `item`(episode) 元数据（标题/简介/日期）。
    /// 仅更新本地已存在的集（按 episode_number 匹配）；无对应集时静默跳过。
    async fn scrape_episodes(
        &self,
        scraper: &TmdbScraper,
        series_item_id: i64,
        tmdb_series_id: i64,
        season_number: i64,
        series_name: &str,
    ) -> Result<()> {
        let season = scraper.get_season(tmdb_series_id, season_number).await?;
        let now = crate::emby::format_time_now();
        let mut updated = 0usize;
        for ep in &season.episodes {
            let Some(title) = ep.name.as_deref() else {
                continue;
            };
            // 本地集 id（同一集可多源，取 id 最大的一行；仅用于挂图/演职员）
            let episode_id: Option<i64> = sqlx::query_scalar(
                "SELECT id FROM item WHERE parent_id IN \
                 (SELECT id FROM item WHERE parent_id = ? AND type = 'season' \
                   AND season_number = ?) \
                 AND type = 'episode' AND episode_number = ? \
                 ORDER BY id DESC LIMIT 1",
            )
            .bind(series_item_id)
            .bind(season_number)
            .bind(ep.episode_number)
            .fetch_optional(self.db.pool())
            .await?;

            let res = sqlx::query(
                "UPDATE item SET title = COALESCE(?, title), \
                 description = COALESCE(?, description), date_air = COALESCE(?, date_air), \
                 runtime = COALESCE(?, runtime), \
                 community_rating = COALESCE(?, community_rating), \
                 updated_at = ? \
                 WHERE parent_id IN \
                   (SELECT id FROM item WHERE parent_id = ? AND type = 'season' \
                     AND season_number = ?) \
                 AND type = 'episode' AND episode_number = ?",
            )
            .bind(title)
            .bind(ep.overview.as_deref())
            .bind(ep.air_date.as_deref())
            .bind(ep.runtime.map(|r| r * 60))
            .bind(ep.vote_average)
            .bind(&now)
            .bind(series_item_id)
            .bind(season_number)
            .bind(ep.episode_number)
            .execute(self.db.pool())
            .await?;
            updated += res.rows_affected() as usize;

            if let Some(episode_id) = episode_id {
                // 剧照（still）：为集写入自身的 Primary 图片，避免客户端全部回退到季海报。
                if let Some(still) = ep.still_path.as_deref().filter(|s| !s.is_empty()) {
                    let url = format!("https://image.tmdb.org/t/p/w500{still}");
                    self.attach_image(episode_id, "primary", &url).await;
                }
                // 单集演职员（cast + guest_stars）。失败只记录，不阻断季刮削。
                if let Err(e) = self
                    .attach_episode_credits(
                        scraper,
                        episode_id,
                        tmdb_series_id,
                        season_number,
                        ep.episode_number,
                        series_name,
                    )
                    .await
                {
                    tracing::warn!(
                        series_name,
                        season_number,
                        episode = ep.episode_number,
                        error = %e,
                        "TMDB 单集演职员抓取失败"
                    );
                }
            }
        }
        tracing::info!(
            series_name,
            season_number,
            total = season.episodes.len(),
            updated,
            "TMDB 剧集列表刮削完成"
        );
        Ok(())
    }

    /// 抓取单集演职员（cast + guest_stars）并写入 people / item_people。
    /// 先清空该集旧关联再写（重刮幂等）；`role` 记 `Actor`（与剧集 cast 一致），
    /// 人物 upsert / 头像 / 详情回填复用 `link_credits` 的公共逻辑。
    async fn attach_episode_credits(
        &self,
        scraper: &TmdbScraper,
        episode_id: i64,
        tmdb_series_id: i64,
        season_number: i64,
        episode_number: i64,
        series_name: &str,
    ) -> Result<()> {
        let credits = scraper
            .get_episode_credits(tmdb_series_id, season_number, episode_number)
            .await?;
        if credits.cast.is_empty() && credits.guest_stars.is_empty() {
            return Ok(());
        }

        let now = crate::emby::format_time_now();
        item_store::clear_item_people(&self.db, episode_id).await?;

        let mut sort_order: i64 = 0;
        for c in credits
            .cast
            .iter()
            .chain(credits.guest_stars.iter())
            .filter(|c| !c.name.is_empty())
        {
            let (people_id, needs_enrich) = self
                .upsert_people(c.id, &c.name, c.original_name.as_deref(), c.gender, &now)
                .await;
            if people_id <= 0 {
                continue;
            }
            if needs_enrich {
                self.enrich_new_person(scraper, c.id, people_id).await;
            }
            if let Some(profile) = c.profile_path.as_deref().filter(|s| !s.is_empty()) {
                let url = format!("https://image.tmdb.org/t/p/w500{profile}");
                self.attach_image_for("people", people_id, "primary", &url)
                    .await;
            }
            item_store::link_person(
                &self.db,
                episode_id,
                people_id,
                "Actor",
                c.character.as_deref(),
                sort_order,
            )
            .await;
            sort_order += 1;
        }
        tracing::info!(
            series_name,
            season_number,
            episode_number,
            total = sort_order,
            "TMDB 单集演职员写入完成"
        );
        Ok(())
    }

    /// `item` 是否已有非空 tmdb_id。
    async fn has_tmdb_id(&self, item_id: i64) -> bool {
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT tmdb_id FROM item WHERE id = ? AND tmdb_id IS NOT NULL AND tmdb_id != ''",
        )
        .bind(item_id)
        .fetch_optional(self.db.pool())
        .await
        .unwrap_or(None)
        .is_some()
    }

    /// 生成虚拟条目：对比 TMDB 季集全集 vs 本地 item 树，缺失集建 is_virtual=true。
    /// 仅在 series 已有 tmdb_id 时触发。返回创建的虚拟条目数。
    pub async fn generate_virtual_entries(&self, series_id: i64) -> Result<usize> {
        let tmdb_id_str: Option<String> = sqlx::query_scalar::<_, Option<String>>(
            "SELECT tmdb_id FROM item WHERE id = ? AND type = 'series'",
        )
        .bind(series_id)
        .fetch_optional(self.db.pool())
        .await?
        .flatten();

        let Some(tmdb_id_str) = tmdb_id_str else {
            return Ok(0);
        };
        let tmdb_id: i64 = tmdb_id_str.parse().unwrap_or(0);
        if tmdb_id <= 0 {
            return Ok(0);
        }

        let scraper = TmdbScraper::new(self.tmdb_config());

        let detail = match scraper.get_tv(tmdb_id).await {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(series_id, tmdb_id, error = %e, "虚拟条目生成: 获取 TMDB 详情失败");
                return Ok(0);
            }
        };

        let seasons = detail.seasons.unwrap_or_default();
        let mut virtual_count = 0usize;
        let now = crate::emby::format_time_now();

        for season in &seasons {
            if season.season_number == 0 {
                continue;
            }

            let season_item_id: Option<i64> = sqlx::query_scalar(
                "SELECT id FROM item WHERE parent_id = ? AND type = 'season' \
                 AND season_number = ? LIMIT 1",
            )
            .bind(series_id)
            .bind(season.season_number)
            .fetch_optional(self.db.pool())
            .await?;

            let season_item_id = match season_item_id {
                Some(id) => id,
                None => {
                    let fallback = format!("Season {}", season.season_number);
                    let season_title = season.name.as_deref().unwrap_or(&fallback);
                    sqlx::query(
                        "INSERT INTO item (type, parent_id, library_id, scrape_status, title, \
                         season_number, is_virtual, created_at, updated_at) \
                         VALUES ('season', ?, ?, 'scraped', ?, ?, 1, ?, ?)",
                    )
                    .bind(series_id)
                    .bind(self.series_library_id(series_id).await.unwrap_or(0))
                    .bind(season_title)
                    .bind(season.season_number)
                    .bind(&now)
                    .bind(&now)
                    .execute(self.db.pool())
                    .await?;
                    let new_season_id = sqlx::query_scalar::<_, i64>(
                        "SELECT id FROM item ORDER BY id DESC LIMIT 1",
                    )
                    .fetch_one(self.db.pool())
                    .await?;

                    if let Some(poster) = season.poster_path.as_deref().filter(|s| !s.is_empty()) {
                        let url = format!("https://image.tmdb.org/t/p/w500{poster}");
                        self.attach_image(new_season_id, "primary", &url).await;
                    }

                    match scraper.get_season(tmdb_id, season.season_number).await {
                        Ok(sd) => {
                            for ep in &sd.episodes {
                                self.create_virtual_episode(
                                    new_season_id,
                                    season.season_number,
                                    ep,
                                    &now,
                                )
                                .await;
                                virtual_count += 1;
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                series_id, tmdb_id, season = season.season_number,
                                error = %e, "虚拟条目: 获取 TMDB 季详情失败"
                            );
                        }
                    }
                    continue;
                }
            };

            let existing_eps: Vec<i64> = sqlx::query_scalar(
                "SELECT episode_number FROM item WHERE parent_id = ? AND type = 'episode'",
            )
            .bind(season_item_id)
            .fetch_all(self.db.pool())
            .await?;

            match scraper.get_season(tmdb_id, season.season_number).await {
                Ok(sd) => {
                    for ep in &sd.episodes {
                        if !existing_eps.contains(&ep.episode_number) {
                            self.create_virtual_episode(
                                season_item_id,
                                season.season_number,
                                ep,
                                &now,
                            )
                            .await;
                            virtual_count += 1;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        series_id, tmdb_id, season = season.season_number,
                        error = %e, "虚拟条目: 获取 TMDB 季详情失败"
                    );
                }
            }
        }

        if virtual_count > 0 {
            info!(
                series_id,
                tmdb_id,
                count = virtual_count,
                "虚拟条目生成完成"
            );
        }
        Ok(virtual_count)
    }

    /// 创建单个虚拟集 item。
    async fn create_virtual_episode(
        &self,
        season_item_id: i64,
        season_number: i64,
        ep: &EpisodeBrief,
        now: &str,
    ) {
        let title = ep.name.as_deref().unwrap_or("Unknown Episode");
        let _ = sqlx::query(
            "INSERT INTO item (type, parent_id, scrape_status, title, \
             season_number, episode_number, is_virtual, date_air, description, runtime, \
             community_rating, \
             created_at, updated_at) \
             VALUES ('episode', ?, 'scraped', ?, ?, ?, 1, ?, ?, ?, ?, ?, ?)",
        )
        .bind(season_item_id)
        .bind(title)
        .bind(season_number)
        .bind(ep.episode_number)
        .bind(ep.air_date.as_deref())
        .bind(ep.overview.as_deref())
        .bind(ep.runtime.map(|r| r * 60))
        .bind(ep.vote_average)
        .bind(now)
        .bind(now)
        .execute(self.db.pool())
        .await;

        if let Some(still) = ep.still_path.as_deref().filter(|s| !s.is_empty()) {
            let new_ep_id: Option<i64> = sqlx::query_scalar::<_, i64>(
                "SELECT id FROM item WHERE parent_id = ? AND type = 'episode' \
                 AND episode_number = ? AND is_virtual = 1 ORDER BY id DESC LIMIT 1",
            )
            .bind(season_item_id)
            .bind(ep.episode_number)
            .fetch_optional(self.db.pool())
            .await
            .ok()
            .flatten();
            if let Some(ep_id) = new_ep_id {
                let url = format!("https://image.tmdb.org/t/p/w500{still}");
                self.attach_image(ep_id, "primary", &url).await;
            }
        }
    }

    /// 虚拟条目实体化：当本地文件命中同 (parent_id, episode_number) 的虚拟行时，
    /// 设 is_virtual=false 并挂 media_source。返回 true 表示已实体化。
    ///
    /// 状态置 `scraped`：虚拟条目创建时已带全套 TMDB 元数据（create_virtual_episode），
    /// 实体化只是翻 is_virtual + 挂源；若回到 pending 会被 Scrape 阶段拿集标题
    /// 去搜索导致错配（历史 bug，此处即修复点）。后续 force 重刮经 series 级联刷新。
    pub async fn materialize_virtual_episode(
        &self,
        season_item_id: i64,
        episode_number: i64,
        title: &str,
    ) -> Result<bool> {
        let virtual_id: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM item WHERE parent_id = ? AND type = 'episode' \
             AND episode_number = ? AND is_virtual = 1 LIMIT 1",
        )
        .bind(season_item_id)
        .bind(episode_number)
        .fetch_optional(self.db.pool())
        .await?;

        if let Some(vid) = virtual_id {
            let now = crate::emby::format_time_now();
            sqlx::query(
                "UPDATE item SET is_virtual = 0, title = ?, scrape_status = 'scraped', \
                 updated_at = ? WHERE id = ?",
            )
            .bind(title)
            .bind(&now)
            .bind(vid)
            .execute(self.db.pool())
            .await?;
            info!(episode_number, item_id = vid, "虚拟条目实体化");
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// 查 item 的 library_id（用于虚拟季创建）。
    async fn series_library_id(&self, series_id: i64) -> Result<i64> {
        Ok(
            sqlx::query_scalar::<_, i64>("SELECT library_id FROM item WHERE id = ?")
                .bind(series_id)
                .fetch_optional(self.db.pool())
                .await?
                .unwrap_or(0),
        )
    }

    /// upsert genre 规范行（按 tmdb_id 幂等），返回 genre.id。委托 [`taxonomy_store`]。
    async fn upsert_genre(&self, tmdb_id: &str, name: &str) -> i64 {
        let now = crate::emby::format_time_now();
        taxonomy_store::upsert_named(&self.db, "genre", tmdb_id, name, &now).await
    }

    /// 关联 item → genre（item_genre 幂等，重复关联跳过）。委托 [`item_store`]。
    async fn link_genre(&self, item_id: i64, genre_id: i64) {
        if genre_id <= 0 {
            return;
        }
        item_store::link_genre(&self.db, item_id, genre_id).await;
    }

    /// 把 TMDB production_companies 写入 `studio` 规范表 + `item_studio` 关联
    /// （按 tmdb_id 幂等，重复关联跳过）。
    async fn link_studios(&self, item_id: i64, companies: &[crate::importer::tmdb::TmdbCompany]) {
        let now = crate::emby::format_time_now();
        let mut sort_order: i64 = 0;
        for c in companies {
            let studio_id = self.upsert_studio(c.id, &c.name, &now).await;
            if studio_id <= 0 {
                continue;
            }
            item_store::link_studio(&self.db, item_id, studio_id, sort_order).await;
            sort_order += 1;
        }
    }

    /// upsert studio 规范行（按 tmdb_id 幂等），返回 studio.id。委托 [`taxonomy_store`]。
    async fn upsert_studio(&self, tmdb_id: i64, name: &str, now: &str) -> i64 {
        taxonomy_store::upsert_named(&self.db, "studio", &tmdb_id.to_string(), name, now).await
    }

    /// 把 TMDB keywords 写入 `tag` 规范表 + `item_tag` 关联
    /// （按 tmdb_id 幂等，重复关联跳过）。
    async fn link_tags(&self, item_id: i64, keywords: Option<&crate::importer::tmdb::KeywordResp>) {
        let Some(kr) = keywords else {
            return;
        };
        let now = crate::emby::format_time_now();
        let mut sort_order: i64 = 0;
        for k in kr.iter().filter(|k| !k.name.is_empty()) {
            let tag_id = self.upsert_tag(k.id, &k.name, &now).await;
            if tag_id <= 0 {
                continue;
            }
            item_store::link_tag(&self.db, item_id, tag_id, sort_order).await;
            sort_order += 1;
        }
    }

    /// upsert tag 规范行（按 tmdb_id 幂等），返回 tag.id。
    /// 已存在则同步 name；不存在则插入。失败返回 0（调用方忽略）。
    async fn upsert_tag(&self, tmdb_id: i64, name: &str, now: &str) -> i64 {
        taxonomy_store::upsert_named(&self.db, "tag", &tmdb_id.to_string(), name, now).await
    }

    /// 把 TMDB credits（cast + crew）写入 people / item_people。
    /// cast → role 'Actor'（character_name 记角色名），crew → 按 job 映射 Emby 角色。
    async fn link_credits(&self, scraper: &TmdbScraper, item_id: i64, credits: &Credits) {
        let now = crate::emby::format_time_now();
        let mut sort_order: i64 = 0;

        for c in &credits.cast {
            let (people_id, needs_enrich) = self
                .upsert_people(c.id, &c.name, c.original_name.as_deref(), c.gender, &now)
                .await;
            if people_id <= 0 {
                continue;
            }
            if needs_enrich {
                self.enrich_new_person(scraper, c.id, people_id).await;
            }
            // 人物头像（parent_type='people'，与 video 的 item 图片共用 item_image 表）
            if let Some(profile) = c.profile_path.as_deref().filter(|s| !s.is_empty()) {
                let url = format!("https://image.tmdb.org/t/p/w500{profile}");
                self.attach_image_for("people", people_id, "primary", &url)
                    .await;
            }
            item_store::link_person(
                &self.db,
                item_id,
                people_id,
                "Actor",
                c.character.as_deref(),
                sort_order,
            )
            .await;
            sort_order += 1;
        }

        for c in &credits.crew {
            let role = emby_crew_role(c.job.as_deref().unwrap_or(""));
            if role.is_empty() {
                continue;
            }
            let (people_id, needs_enrich) = self
                .upsert_people(c.id, &c.name, c.original_name.as_deref(), c.gender, &now)
                .await;
            if people_id <= 0 {
                continue;
            }
            if needs_enrich {
                self.enrich_new_person(scraper, c.id, people_id).await;
            }
            if let Some(profile) = c.profile_path.as_deref().filter(|s| !s.is_empty()) {
                let url = format!("https://image.tmdb.org/t/p/w500{profile}");
                self.attach_image_for("people", people_id, "primary", &url)
                    .await;
            }
            item_store::link_person(&self.db, item_id, people_id, &role, None, sort_order).await;
            sort_order += 1;
        }
    }

    /// 新人物抓取 `person/{id}` 详情（传记 / 生日 / 忌日）并回填。
    /// 仅新建时触发，避免对已知人物重复请求。
    async fn enrich_new_person(&self, scraper: &TmdbScraper, tmdb_person_id: i64, people_id: i64) {
        match scraper.get_person(tmdb_person_id).await {
            Ok(pd) => {
                let now = crate::emby::format_time_now();
                let _ = sqlx::query(
                    "UPDATE people SET description = COALESCE(?, description), \
                     birthday = COALESCE(?, birthday), deathday = COALESCE(?, deathday), \
                     updated_at = ? WHERE id = ?",
                )
                .bind(pd.biography.as_deref())
                .bind(pd.birthday.as_deref())
                .bind(pd.deathday.as_deref())
                .bind(&now)
                .bind(people_id)
                .execute(self.db.pool())
                .await;
            }
            Err(e) => {
                tracing::warn!(tmdb_person_id, error = %e, "TMDB 人物详情抓取失败");
            }
        }
    }

    /// upsert people 规范行（按 tmdb_id 幂等），返回 `(people.id, needs_enrich)`。
    /// 已存在则同步 name；`needs_enrich=true` 表示该人物详情从未回填（description/birthday/
    /// deathday 全空），调用方据此抓 `person/{id}` 详情。失败返回 (0, false)。
    async fn upsert_people(
        &self,
        tmdb_id: i64,
        name: &str,
        original_name: Option<&str>,
        gender: Option<i64>,
        now: &str,
    ) -> (i64, bool) {
        let tmdb_id_str = tmdb_id.to_string();
        let existing: Option<ExistingPerson> = sqlx::query_as(
            "SELECT id, description, birthday, deathday FROM people \
                 WHERE tmdb_id = ? LIMIT 1",
        )
        .bind(&tmdb_id_str)
        .fetch_optional(self.db.pool())
        .await
        .ok()
        .flatten();
        if let Some((id, description, birthday, deathday)) = existing {
            let _ = sqlx::query("UPDATE people SET name = ?, updated_at = ? WHERE id = ?")
                .bind(name)
                .bind(now)
                .bind(id)
                .execute(self.db.pool())
                .await;
            // 已有人物：仅当详情从未回填过才需要 enrich（避免每次重刮重复请求）。
            let needs_enrich = description.as_deref().is_none_or(|s| s.trim().is_empty())
                && birthday.is_none()
                && deathday.is_none();
            return (id, needs_enrich);
        }
        let _ = sqlx::query(
            "INSERT INTO people (tmdb_id, name, original_name, gender, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&tmdb_id_str)
        .bind(name)
        .bind(original_name)
        .bind(gender.unwrap_or(0))
        .bind(now)
        .bind(now)
        .execute(self.db.pool())
        .await;
        let new_id = sqlx::query_scalar::<_, i64>("SELECT id FROM people ORDER BY id DESC LIMIT 1")
            .fetch_one(self.db.pool())
            .await
            .unwrap_or(0);
        (new_id, new_id > 0)
    }

    // -----------------------------------------------------------------------
    // NFO 兜底落库（扫描期调用；无 TMDB 刮削时的唯一元数据来源）
    // -----------------------------------------------------------------------

    /// 把 NFO 的关系型元数据写入规范表 + 关联表 + 图片：分类 / 制片 / 标签 /
    /// 演员（含角色与头像）/ 海报 / 背景。标量字段（tagline/status/评分/分级）由
    /// `update_item_meta` 负责，本方法只处理需要建关联行的部分。
    ///
    /// 全部幂等：genre/studio/tag 按 name upsert（NFO 无 tmdb_id），`item_*` 关联走
    /// `INSERT OR IGNORE`；演员按 profile 解析出的 TMDB 人物 id upsert（与后续 TMDB
    /// 刮削命中同一行，天然去重），无 profile（无 tmdb_id）的演员跳过——people.tmdb_id
    /// NOT NULL UNIQUE 无法用姓名可靠落库。
    pub(super) async fn apply_nfo_relations(&self, item_id: i64, nfo: &Nfo) {
        let now = crate::emby::format_time_now();

        for g in &nfo.genres {
            if g.trim().is_empty() {
                continue;
            }
            let gid = self.upsert_genre_by_name(g).await;
            self.link_genre(item_id, gid).await;
        }

        let mut sort_order: i64 = 0;
        for s in &nfo.studios {
            if s.trim().is_empty() {
                continue;
            }
            let sid = self.upsert_studio_by_name(s).await;
            if sid > 0 {
                item_store::link_studio(&self.db, item_id, sid, sort_order).await;
                sort_order += 1;
            }
        }

        let mut sort_order: i64 = 0;
        for t in &nfo.tags {
            if t.trim().is_empty() {
                continue;
            }
            let tid = self.upsert_tag_by_name(t).await;
            if tid > 0 {
                item_store::link_tag(&self.db, item_id, tid, sort_order).await;
                sort_order += 1;
            }
        }

        let mut sort_order: i64 = 0;
        for a in &nfo.actors {
            let Some(id_str) = a.tmdb_id.as_deref() else {
                continue;
            };
            let Ok(tmdb_id) = id_str.parse::<i64>() else {
                continue;
            };
            let (people_id, _) = self.upsert_people(tmdb_id, &a.name, None, None, &now).await;
            if people_id <= 0 {
                continue;
            }
            item_store::link_person(
                &self.db,
                item_id,
                people_id,
                "Actor",
                a.role.as_deref(),
                sort_order,
            )
            .await;
            sort_order += 1;
            if let Some(thumb) = a.thumb.as_deref() {
                self.attach_image_for("people", people_id, "primary", thumb)
                    .await;
            }
        }

        if let Some(poster) = nfo.poster.as_deref() {
            self.attach_image(item_id, "primary", poster).await;
        }
        for backdrop in &nfo.backdrops {
            self.attach_image(item_id, "backdrop", backdrop).await;
        }
    }

    /// 按 name upsert genre（NFO 无 tmdb_id）：命中复用，否则插入 `tmdb_id=NULL` 行。失败返回 0。
    async fn upsert_genre_by_name(&self, name: &str) -> i64 {
        self.upsert_taxonomy_by_name("genre", name).await
    }
    /// 按 name upsert studio（NFO 无 tmdb_id）。失败返回 0。
    async fn upsert_studio_by_name(&self, name: &str) -> i64 {
        self.upsert_taxonomy_by_name("studio", name).await
    }
    /// 按 name upsert tag（NFO 无 tmdb_id）。失败返回 0。
    async fn upsert_tag_by_name(&self, name: &str) -> i64 {
        self.upsert_taxonomy_by_name("tag", name).await
    }

    /// genre / studio / tag 三表结构相同（id, tmdb_id, name），共用按 name 的 upsert。
    /// `table` 仅接受字面量 "genre"|"studio"|"tag"（无注入风险）。委托 [`taxonomy_store`]。
    async fn upsert_taxonomy_by_name(&self, table: &'static str, name: &str) -> i64 {
        taxonomy_store::upsert_by_name(&self.db, table, name).await
    }
}

/// 把 TMDB crew 的 job 映射到 Emby 人员角色；未知 job 返回空串（跳过不入库）。
fn emby_crew_role(job: &str) -> String {
    match job.to_ascii_lowercase().as_str() {
        "director" => "Director".to_string(),
        "screenplay" | "writer" | "story" | "novel" | "teleplay" | "scenario writer" => {
            "Writer".to_string()
        }
        "producer" | "executive producer" | "co-producer" | "associate producer"
        | "line producer" => "Producer".to_string(),
        "original music composer" | "music" | "music producer" | "songs" => "Composer".to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::emby_crew_role;

    #[test]
    fn crew_role_maps_known_jobs() {
        assert_eq!(emby_crew_role("Director"), "Director");
        assert_eq!(emby_crew_role("Screenplay"), "Writer");
        assert_eq!(emby_crew_role("Writer"), "Writer");
        assert_eq!(emby_crew_role("Executive Producer"), "Producer");
        assert_eq!(emby_crew_role("Original Music Composer"), "Composer");
        assert_eq!(emby_crew_role("Music"), "Composer");
    }

    #[test]
    fn crew_role_case_insensitive() {
        assert_eq!(emby_crew_role("director"), "Director");
        assert_eq!(emby_crew_role("SCREENPLAY"), "Writer");
    }

    #[test]
    fn crew_role_unknown_is_empty() {
        assert_eq!(emby_crew_role("Editor"), "");
        assert_eq!(emby_crew_role("Cinematography"), "");
        assert_eq!(emby_crew_role(""), "");
    }
}
