//! TMDB 刮削。
//!
//! 强类型响应、标题候选清洗、匹配打分、
//! 外键查找（IMDb/TVDB）、详情抓取（电影/剧集/季/集）、海报写入。

use std::error::Error;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::http_client::Outbound;

/// TMDB 配置。
#[derive(Debug, Clone)]
pub struct TmdbConfig {
    /// v3 API key，或 v4 Bearer token（以 `eyJ` 开头自动识别为 Bearer 认证）。
    pub api_key: String,
    pub language: String,
    pub base_url: String,
    /// 请求限速（次/秒），0 表示不限速，默认 20 req/s。
    pub requests_per_second: u32,
    /// 出网配置（代理 + hosts 覆盖），进程启动时构建共享。
    pub outbound: Arc<Outbound>,
}

impl Default for TmdbConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            language: "zh-CN".to_string(),
            base_url: "https://api.themoviedb.org/3".to_string(),
            requests_per_second: 20,
            outbound: Arc::new(Outbound::default()),
        }
    }
}

/// 进程级 TMDB 请求限速基准：跨 scraper 实例共享，避免批量刮削触发 429。
static LAST_TMDB_REQUEST: tokio::sync::Mutex<Option<Instant>> = tokio::sync::Mutex::const_new(None);

/// TMDB 刮削器。
pub struct TmdbScraper {
    client: reqwest::Client,
    config: TmdbConfig,
    /// 两次请求的最小间隔（None = 不限速）。
    interval: Option<Duration>,
    /// 是否用 v4 Bearer 认证（`eyJ` 开头的 token）。
    use_bearer: bool,
}

// ---------- 响应类型 ----------

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SearchMovieResp {
    pub results: Vec<SearchMovieItem>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SearchMovieItem {
    pub id: i64,
    pub title: String,
    pub original_title: Option<String>,
    pub overview: Option<String>,
    pub release_date: Option<String>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SearchTvResp {
    pub results: Vec<SearchTvItem>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SearchTvItem {
    pub id: i64,
    pub name: String,
    pub original_name: Option<String>,
    pub overview: Option<String>,
    pub first_air_date: Option<String>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct MovieDetail {
    pub id: i64,
    pub title: String,
    pub original_title: Option<String>,
    pub overview: Option<String>,
    pub tagline: Option<String>,
    pub release_date: Option<String>,
    pub runtime: Option<i64>,
    pub imdb_id: Option<String>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    pub genres: Vec<TmdbGenre>,
    pub production_companies: Vec<TmdbCompany>,
    pub vote_average: Option<f64>,
    pub status: Option<String>,
    pub credits: Credits,
    /// `append_to_response=release_dates`（分级 certification，Emby OfficialRating）。
    pub release_dates: Option<ReleaseDates>,
    /// `append_to_response=keywords`（电影关键词）。
    pub keywords: Option<KeywordResp>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TvDetail {
    pub id: i64,
    pub name: String,
    pub original_name: Option<String>,
    pub overview: Option<String>,
    pub tagline: Option<String>,
    pub first_air_date: Option<String>,
    pub last_air_date: Option<String>,
    pub episode_run_time: Option<Vec<i64>>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    pub external_ids: ExternalIds,
    pub seasons: Option<Vec<SeasonBrief>>,
    pub genres: Vec<TmdbGenre>,
    pub production_companies: Vec<TmdbCompany>,
    pub vote_average: Option<f64>,
    pub status: Option<String>,
    pub credits: Credits,
    /// `append_to_response=content_ratings`（TV 分级）。
    pub content_ratings: Option<ContentRatings>,
    /// `append_to_response=keywords`（剧集关键词，`results` 键）。
    pub keywords: Option<KeywordResp>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ExternalIds {
    pub imdb_id: Option<String>,
    pub tvdb_id: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SeasonBrief {
    pub season_number: i64,
    pub name: Option<String>,
    pub overview: Option<String>,
    pub air_date: Option<String>,
    pub vote_average: Option<f64>,
    pub poster_path: Option<String>,
    pub id: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SeasonDetail {
    pub episodes: Vec<EpisodeBrief>,
}

/// 单集演职员（`tv/{id}/season/{s}/episode/{e}/credits`）。
/// `guest_stars` 形状与 cast 相同（id/name/character/order/gender/profile_path），
/// 单独字段以便与常规 cast 区分。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EpisodeCredits {
    #[serde(default)]
    pub cast: Vec<CastPerson>,
    #[serde(default)]
    pub guest_stars: Vec<CastPerson>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct EpisodeBrief {
    pub id: i64,
    pub episode_number: i64,
    pub name: Option<String>,
    pub overview: Option<String>,
    pub air_date: Option<String>,
    pub runtime: Option<i64>,
    pub still_path: Option<String>,
    pub vote_average: Option<f64>,
}

// ---------- 附加响应子结构（release_dates / content_ratings / keywords / person） ----------

/// 电影 `release_dates`：按地区分组的 certification。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ReleaseDates {
    pub results: Vec<ReleaseDateRegion>,
}

impl ReleaseDates {
    /// 取分级（Emby OfficialRating）：优先 US，否则第一个非空 certification。
    pub fn certification(&self) -> Option<String> {
        let first_non_empty = |dates: &[CertificationDate]| {
            dates
                .iter()
                .find_map(|d| d.certification.as_deref().filter(|c| !c.trim().is_empty()))
                .map(str::to_string)
        };
        // 优先 US 地区，否则任意地区第一个非空 certification。
        self.results
            .iter()
            .find(|r| r.iso_3166_1.eq_ignore_ascii_case("US"))
            .and_then(|r| first_non_empty(&r.release_dates))
            .or_else(|| {
                self.results
                    .iter()
                    .find_map(|r| first_non_empty(&r.release_dates))
            })
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ReleaseDateRegion {
    pub iso_3166_1: String,
    pub release_dates: Vec<CertificationDate>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CertificationDate {
    pub certification: Option<String>,
}

/// TV `content_ratings`：按国家/地区的 rating。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ContentRatings {
    pub results: Vec<ContentRatingRegion>,
}

impl ContentRatings {
    /// 取 TV 分级：优先 US，否则第一个非空 rating。
    pub fn rating(&self) -> Option<String> {
        self.results
            .iter()
            .find(|r| r.iso_3166_1.eq_ignore_ascii_case("US"))
            .and_then(|r| r.rating.as_deref().filter(|c| !c.trim().is_empty()))
            .map(str::to_string)
            .or_else(|| {
                self.results
                    .iter()
                    .find_map(|r| r.rating.as_deref().filter(|c| !c.trim().is_empty()))
                    .map(str::to_string)
            })
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ContentRatingRegion {
    pub iso_3166_1: String,
    pub rating: Option<String>,
}

/// 关键词响应：电影用 `keywords.keywords`，剧集用 `keywords.results`，两个键都收。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct KeywordResp {
    #[serde(default)]
    pub results: Vec<Keyword>,
    #[serde(default)]
    pub keywords: Vec<Keyword>,
}

impl KeywordResp {
    /// 所有 keyword（电影 `keywords` + 剧集 `results` 两键合并）。
    pub fn iter(&self) -> impl Iterator<Item = &Keyword> {
        self.results.iter().chain(self.keywords.iter())
    }

    pub fn names(&self) -> Vec<String> {
        self.iter()
            .map(|k| k.name.clone())
            .filter(|s| !s.is_empty())
            .collect()
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Keyword {
    pub id: i64,
    pub name: String,
}

/// `person/{id}` 详情（people 传记 / 出生 / 去世）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PersonDetail {
    pub id: i64,
    pub name: String,
    pub biography: Option<String>,
    pub birthday: Option<String>,
    pub deathday: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FindResp<M, T> {
    pub movie_results: Vec<M>,
    pub tv_results: Vec<T>,
}

// ---------- 详情子结构（genre / credits / company） ----------

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TmdbGenre {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TmdbCompany {
    pub id: i64,
    pub name: String,
}

/// 演职员（`movie/{id}` / `tv/{id}` 的 credits）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Credits {
    pub cast: Vec<CastPerson>,
    pub crew: Vec<CrewPerson>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CastPerson {
    pub id: i64,
    pub name: String,
    pub original_name: Option<String>,
    /// 角色名（Emby character_name）。
    pub character: Option<String>,
    /// 卡司排序（TMDB order，0 为主演）。
    pub order: Option<i64>,
    /// 0=未设置, 1=女, 2=男, 3=非二元。
    pub gender: Option<i64>,
    pub profile_path: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CrewPerson {
    pub id: i64,
    pub name: String,
    pub original_name: Option<String>,
    pub job: Option<String>,
    pub gender: Option<i64>,
    pub profile_path: Option<String>,
}

/// `/{movie|tv}/{id}/images` 响应：全语言图片集（backdrops / posters / logos）。
///
/// **必须不带 `language` 参数请求**——实测带 `language=zh-CN` 会把 logos 从 66 条
/// 过滤到 6 条（仅该语言）。要按语言挑最佳 logo（zh → en → 任意），必须拿全集。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Images {
    #[serde(default)]
    pub backdrops: Vec<TmdbImage>,
    #[serde(default)]
    pub posters: Vec<TmdbImage>,
    #[serde(default)]
    pub logos: Vec<TmdbImage>,
}

/// TMDB 图片条目（海报 / 背景 / logo 共用此形状）。
///
/// `file_path` 是 image.tmdb.org 上的相对路径（含扩展名）；logo 可能是 `.svg`（矢量）
/// 或 `.png`（光栅透明），靠扩展名区分——TMDB 不单独给 `file_type` 字段。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TmdbImage {
    pub file_path: String,
    #[serde(default)]
    pub iso_639_1: Option<String>,
    #[serde(default)]
    pub iso_3166_1: Option<String>,
    #[serde(default)]
    pub width: Option<i64>,
    #[serde(default)]
    pub height: Option<i64>,
    #[serde(default)]
    pub aspect_ratio: Option<f64>,
    #[serde(default)]
    pub vote_average: Option<f64>,
}

// ---------- 对外返回值（兼容 scanner） ----------

#[derive(Debug, Clone)]
pub struct TmdbMovie {
    pub id: i64,
    pub title: String,
    pub original_title: Option<String>,
    pub overview: String,
    pub release_date: String,
    pub runtime: Option<i64>,
    pub imdb_id: Option<String>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TmdbTv {
    pub id: i64,
    pub name: String,
    pub original_name: Option<String>,
    pub overview: String,
    pub first_air_date: String,
    pub runtime: Option<i64>,
    pub imdb_id: Option<String>,
    pub tvdb_id: Option<i64>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
}

impl TmdbScraper {
    pub fn new(config: TmdbConfig) -> Self {
        let interval = (config.requests_per_second > 0)
            .then(|| Duration::from_secs_f64(1.0 / config.requests_per_second as f64));
        // v4 token 以 `eyJ` 开头且足够长
        let use_bearer = config.api_key.starts_with("eyJ") && config.api_key.len() > 40;
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::ACCEPT,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        let builder = reqwest::Client::builder()
            .user_agent("emrs/0.1")
            .default_headers(headers)
            .timeout(Duration::from_secs(15));
        // 代理 + hosts 覆盖统一由 Outbound 套用（见 http_client.rs）。
        let builder = config.outbound.configure(builder);
        let client = builder.build().unwrap_or_default();
        Self {
            client,
            config,
            interval,
            use_bearer,
        }
    }

    fn enabled(&self) -> bool {
        !self.config.api_key.is_empty()
    }

    /// 查询电影，返回最佳匹配（含打分）。
    pub async fn search_movie(&self, title: &str, year: Option<i32>) -> Result<Option<TmdbMovie>> {
        if !self.enabled() {
            return Ok(None);
        }
        // 清洗标题作为 query（去年份/分辨率/季标记等噪音，避免 TMDB 匹配不到）；
        // year 仍单独作为 TMDB 的 `year` 参数精确过滤。
        let query = clean_search_title(title)
            .first()
            .cloned()
            .unwrap_or_else(|| title.trim().to_string());
        let resp: SearchMovieResp = self
            .get_json(
                "search/movie",
                &[
                    ("query", query.as_str()),
                    ("year", year.map(|y| y.to_string()).as_deref().unwrap_or("")),
                ],
            )
            .await?;
        let best = best_result(&resp.results, &query, year, ResultField::MovieTitle);
        Ok(best.map(|it| TmdbMovie {
            id: it.id,
            title: it.title.clone(),
            original_title: it.original_title.clone(),
            overview: it.overview.clone().unwrap_or_default(),
            release_date: it.release_date.clone().unwrap_or_default(),
            runtime: None,
            imdb_id: None,
            poster_path: it.poster_path.clone(),
            backdrop_path: it.backdrop_path.clone(),
        }))
    }

    /// 查询剧集，返回最佳匹配。
    pub async fn search_tv(&self, title: &str, year: Option<i32>) -> Result<Option<TmdbTv>> {
        if !self.enabled() {
            return Ok(None);
        }
        // 清洗标题作为 query（去年份/分辨率/季标记等噪音），year 单独作为
        // `first_air_date_year` 参数精确过滤。
        let query = clean_search_title(title)
            .first()
            .cloned()
            .unwrap_or_else(|| title.trim().to_string());
        let mut params: Vec<(&str, &str)> = vec![("query", query.as_str())];
        let year_owned;
        if let Some(y) = year {
            year_owned = y.to_string();
            params.push(("first_air_date_year", &year_owned));
        }
        let resp: SearchTvResp = self.get_json("search/tv", &params).await?;
        let best = best_result(&resp.results, &query, year, ResultField::TvName);
        Ok(best.map(|it| TmdbTv {
            id: it.id,
            name: it.name.clone(),
            original_name: it.original_name.clone(),
            overview: it.overview.clone().unwrap_or_default(),
            first_air_date: it.first_air_date.clone().unwrap_or_default(),
            runtime: None,
            imdb_id: None,
            tvdb_id: None,
            poster_path: it.poster_path.clone(),
            backdrop_path: it.backdrop_path.clone(),
        }))
    }

    /// 按 IMDb 外键查找（电影或剧集）。
    pub async fn find_by_imdb(&self, imdb_id: &str) -> Result<Option<TmdbScrapeDetail>> {
        if !self.enabled() {
            return Ok(None);
        }
        self.find_external("imdb", imdb_id).await
    }

    /// 按 TVDB 外键查找。
    pub async fn find_by_tvdb(&self, tvdb_id: &str) -> Result<Option<TmdbScrapeDetail>> {
        if !self.enabled() {
            return Ok(None);
        }
        self.find_external("tvdb_id", tvdb_id).await
    }

    pub async fn get_movie(&self, id: i64) -> Result<MovieDetail> {
        self.get_json(
            &format!("movie/{id}"),
            &[("append_to_response", "credits,release_dates,keywords")],
        )
        .await
    }

    /// 抓取剧集完整详情（含外键、演职员）。
    pub async fn get_tv(&self, id: i64) -> Result<TvDetail> {
        self.get_json(
            &format!("tv/{id}"),
            &[(
                "append_to_response",
                "external_ids,credits,content_ratings,keywords",
            )],
        )
        .await
    }

    /// 抓取某季剧集列表。
    pub async fn get_season(&self, tv_id: i64, season: i64) -> Result<SeasonDetail> {
        self.get_json(&format!("tv/{tv_id}/season/{season}"), &[])
            .await
    }

    /// 抓取单集演职员（cast + guest_stars）。
    pub async fn get_episode_credits(
        &self,
        tv_id: i64,
        season: i64,
        episode: i64,
    ) -> Result<EpisodeCredits> {
        self.get_json(
            &format!("tv/{tv_id}/season/{season}/episode/{episode}/credits"),
            &[],
        )
        .await
    }

    /// 抓取人物详情（传记 / 出生 / 去世）。
    pub async fn get_person(&self, id: i64) -> Result<PersonDetail> {
        self.get_json(&format!("person/{id}"), &[]).await
    }

    /// 抓取全语言图片集（`/{kind}/{id}/images`，`kind` = `"movie"` / `"tv"`）。
    ///
    /// 不带 `language` 参数：logos 跨语言全返回，供 [`best_logo`] 按语言挑最佳。
    pub async fn get_images(&self, kind: &str, id: i64) -> Result<Images> {
        self.get_json_with(&format!("{kind}/{id}/images"), &[], false)
            .await
    }

    /// 外键查找统一实现。
    async fn find_external(&self, source: &str, value: &str) -> Result<Option<TmdbScrapeDetail>> {
        let resp: FindResp<SearchMovieItem, SearchTvItem> = self
            .get_json(&format!("find/{value}"), &[("external_source", source)])
            .await?;
        if let Some(m) = resp.movie_results.first() {
            return Ok(Some(TmdbScrapeDetail::Movie(m.clone())));
        }
        if let Some(t) = resp.tv_results.first() {
            return Ok(Some(TmdbScrapeDetail::Tv(t.clone())));
        }
        Ok(None)
    }

    /// 发起 GET 请求并解析 JSON，自动附加 api_key / language / 限速。
    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        extra: &[(&str, &str)],
    ) -> Result<T> {
        self.get_json_with(path, extra, true).await
    }

    /// `get_json` 的可变体：`with_language=false` 时不附 `language` 参数
    ///（供 [`get_images`](Self::get_images) 拿跨语言 logo 全集）。
    ///
    /// 传输层错误（超时/连接）与 HTTP 错误（含响应体片段）均透传底层细节，
    /// 便于区分超时、429 限流、401 鉴权失败等真实原因。
    async fn get_json_with<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        extra: &[(&str, &str)],
        with_language: bool,
    ) -> Result<T> {
        self.throttle().await;

        let mut req = self.client.get(format!("{}/{path}", self.config.base_url));
        if self.use_bearer {
            req = req.bearer_auth(&self.config.api_key);
        } else {
            req = req.query(&[("api_key", self.config.api_key.as_str())]);
        }
        if with_language && !self.config.language.is_empty() {
            req = req.query(&[("language", self.config.language.as_str())]);
        }
        for (k, v) in extra {
            if !v.is_empty() {
                req = req.query(&[(*k, *v)]);
            }
        }

        let resp = req.send().await.map_err(|e| {
            // reqwest 的关键原因在 source 链（timeout / connection refused / TLS），
            // 用 `a <- b <- c` 形式完整透传，便于一眼定位。
            let mut msg = format!("TMDB 请求失败: {e}");
            let mut src = e.source();
            while let Some(s) = src {
                msg.push_str(&format!(" <- {s}"));
                src = s.source();
            }
            anyhow::anyhow!("{msg}")
        })?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            let snippet: String = body.chars().take(256).collect();
            return Err(anyhow::anyhow!("TMDB 返回 {status}: {snippet}"));
        }
        resp.json().await.context("TMDB 响应解析失败")
    }

    /// 请求限速：按 `requests_per_second` 控制与上次请求的间隔（进程级共享）。
    async fn throttle(&self) {
        let Some(interval) = self.interval else {
            return;
        };
        let mut last = LAST_TMDB_REQUEST.lock().await;
        let now = Instant::now();
        if let Some(prev) = *last {
            let elapsed = now.saturating_duration_since(prev);
            if elapsed < interval {
                tokio::time::sleep(interval - elapsed).await;
            }
        }
        *last = Some(Instant::now());
    }
}

/// 从标题提取年份（19xx/20xx），用于剧集搜索的 `first_air_date_year`。
pub fn extract_year(title: &str) -> Option<i32> {
    title.split(|c: char| !c.is_ascii_digit()).find_map(|s| {
        if s.len() != 4 {
            return None;
        }
        let y: i32 = s.parse().ok()?;
        (1900..=2099).contains(&y).then_some(y)
    })
}

/// 从 logos 中按语言挑最佳 logo：优先 `prefer_lang`（如 `"zh"`），其次 `"en"`，
/// 最后任意语言；同优先级内取 `vote_average` 最高。返回供 [`TmdbScraper::get_images`] 结果用。
///
/// `prefer_lang` 取 `language` 的主语言子tag（`zh-CN` → `"zh"`）。null 语言的 logo
/// （语言无关字标）不参与前两档，仅落入"任意"兜底档。
pub fn best_logo<'a>(logos: &'a [TmdbImage], prefer_lang: &str) -> Option<&'a TmdbImage> {
    if logos.is_empty() {
        return None;
    }
    let vote = |l: &TmdbImage| l.vote_average.unwrap_or(0.0);
    let best_in = |lang: &str| {
        logos
            .iter()
            .filter(|l| {
                l.iso_639_1
                    .as_deref()
                    .is_some_and(|x| x.eq_ignore_ascii_case(lang))
            })
            .max_by(|a, b| {
                vote(a)
                    .partial_cmp(&vote(b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    };
    best_in(prefer_lang).or_else(|| best_in("en")).or_else(|| {
        logos.iter().max_by(|a, b| {
            vote(a)
                .partial_cmp(&vote(b))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    })
}

/// 查找结果归一化（电影或剧集）。
#[derive(Debug, Clone)]
pub enum TmdbScrapeDetail {
    Movie(SearchMovieItem),
    Tv(SearchTvItem),
}

pub enum ResultField {
    MovieTitle,
    TvName,
}

/// 标题候选清洗：剥离分辨率/编码/字幕等噪音、去季后缀、去年份。
pub fn clean_search_title(title: &str) -> Vec<String> {
    let mut t = title.trim().to_string();
    // 去括号内噪音，如 (1080p) [hdr] {CHS}
    for open in ['(', '[', '{', '（', '【'] {
        let close = match open {
            '(' => ')',
            '[' => ']',
            '{' => '}',
            '（' => '）',
            '【' => '】',
            _ => unreachable!(),
        };
        if let (Some(i), Some(j)) = (t.find(open), t.rfind(close)) {
            let inner = &t[i + open.len_utf8()..j];
            if is_noise(inner) || is_year_str(inner) {
                t.replace_range(i..=j, " ");
            }
        }
    }
    // 去季后缀
    for pat in [
        "第2季", "第3季", "第1季", "Season 2", "Season 3", "Season 1", "S02", "S03", "S01",
    ] {
        if let Some(i) = t.find(pat) {
            t.truncate(i);
            break;
        }
    }
    // 去年份 + 去散落噪音词
    let cleaned = t
        .split_whitespace()
        .filter(|w| {
            let is_year = w.len() == 4
                && w.chars().all(|c| c.is_ascii_digit())
                && (1900..=2099).contains(&w.parse::<i32>().unwrap_or(0));
            !is_year && !is_noise(w)
        })
        .collect::<Vec<_>>()
        .join(" ");
    let mut out = vec![];
    if !cleaned.is_empty() {
        out.push(cleaned);
    }
    if out.is_empty() {
        out.push(title.trim().to_string());
    }
    out
}

fn is_noise(s: &str) -> bool {
    let s = s.to_ascii_lowercase();
    s.contains("1080p")
        || s.contains("720p")
        || s.contains("2160p")
        || s.contains("4k")
        || s.contains("hdr")
        || s.contains("h265")
        || s.contains("hevc")
        || s.contains("x264")
        || s.contains("x265")
        || s.contains("aac")
        || s.contains("dts")
        || s.contains("chs")
        || s.contains("cht")
        || s.contains("字幕")
        || s.contains("合集")
        || s.contains("web")
        || s.contains("bluray")
        || s.contains("remux")
}

fn is_year_str(s: &str) -> bool {
    s.len() == 4
        && s.chars().all(|c| c.is_ascii_digit())
        && (1900..=2099).contains(&s.parse::<i32>().unwrap_or(0))
}

/// 匹配打分：完全匹配最高，其次包含，再 token 重叠，年份加分。
pub fn search_score(
    item_title: &str,
    query: &str,
    year: Option<i32>,
    item_year: Option<i32>,
) -> i32 {
    let query_l = query.to_lowercase();
    let title_l = item_title.to_lowercase();
    if title_l == query_l {
        let mut s = 100;
        if let (Some(qy), Some(iy)) = (year, item_year)
            && (qy - iy).abs() <= 1
        {
            s += 20;
        }
        return s;
    }
    if title_l.contains(&query_l) || query_l.contains(&title_l) {
        let mut s = 80;
        if let (Some(qy), Some(iy)) = (year, item_year)
            && (qy - iy).abs() <= 1
        {
            s += 10;
        }
        return s;
    }
    let q_tokens: Vec<&str> = query_l
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    let t_tokens: Vec<&str> = title_l
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    if q_tokens.is_empty() {
        return 0;
    }
    let overlap = q_tokens
        .iter()
        .filter(|q| t_tokens.iter().any(|t| *q == t))
        .count();
    if overlap == 0 {
        return 0;
    }
    let mut s = 60 * (overlap as i32) / (q_tokens.len().max(1) as i32);
    if let (Some(qy), Some(iy)) = (year, item_year)
        && (qy - iy).abs() <= 1
    {
        s += 10;
    }
    s
}

/// 从搜索结果中选最佳（`search_score` 最高者）。
///
/// **平局取首个**：TMDB 搜索结果按相关性排序，靠前者更优。`Iterator::max_by_key`
/// 平局时返回最后一个——曾导致"租借女友"在同名空壳条目（320068，0 背景/0 logo）
/// 与真正条目间误选空壳。故用 `Reverse(idx)` 让平局时 idx 小者（靠前）胜出。
fn best_result<'a, I>(
    results: &'a [I],
    query: &str,
    year: Option<i32>,
    field: ResultField,
) -> Option<&'a I>
where
    I: SearchCandidate,
{
    results
        .iter()
        .enumerate()
        .map(|(idx, it)| {
            let (title, item_year) = match field {
                ResultField::MovieTitle => (it.cand_title(), it.cand_release_year()),
                ResultField::TvName => (it.cand_title(), None),
            };
            let score = search_score(title, query, year, item_year);
            (score, std::cmp::Reverse(idx), it)
        })
        .filter(|(score, _, _)| *score > 0)
        .max_by_key(|(score, rev_idx, _)| (*score, *rev_idx))
        .map(|(_, _, it)| it)
}

trait SearchCandidate {
    fn cand_title(&self) -> &str;
    fn cand_release_year(&self) -> Option<i32>;
}

impl SearchCandidate for SearchMovieItem {
    fn cand_title(&self) -> &str {
        &self.title
    }
    fn cand_release_year(&self) -> Option<i32> {
        self.release_date
            .as_deref()
            .and_then(|s| s.get(..4))
            .and_then(|y| y.parse().ok())
    }
}

impl SearchCandidate for SearchTvItem {
    fn cand_title(&self) -> &str {
        &self.name
    }
    fn cand_release_year(&self) -> Option<i32> {
        self.first_air_date
            .as_deref()
            .and_then(|s| s.get(..4))
            .and_then(|y| y.parse().ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoring_exact() {
        assert!(
            search_score("Movie", "Movie", None, None)
                > search_score("Movie 2", "Movie", None, None)
        );
    }

    #[test]
    fn scoring_year_bonus() {
        assert!(
            search_score("Movie", "Movie", Some(2020), Some(2020))
                > search_score("Movie", "Movie", Some(2020), Some(1990))
        );
    }

    #[test]
    fn scoring_token_overlap() {
        assert!(search_score("My Movie Title", "Movie", None, None) > 0);
        assert_eq!(search_score("Completely Different", "Movie", None, None), 0);
    }

    #[test]
    fn best_movie_picks_highest() {
        let results = vec![
            SearchMovieItem {
                id: 2,
                title: "Movie 2".into(),
                release_date: Some("2020-01-01".into()),
                ..Default::default()
            },
            SearchMovieItem {
                id: 1,
                title: "Movie".into(),
                release_date: Some("2020-01-01".into()),
                ..Default::default()
            },
        ];
        let best = best_result(&results, "Movie", Some(2020), ResultField::MovieTitle);
        assert_eq!(best.map(|b| b.id), Some(1));
    }

    #[test]
    fn best_result_tie_picks_first() {
        // 两个同名条目都精确匹配（score 100），应取搜索结果靠前者（idx 小），
        // 而非 `max_by_key` 默认的"平局取最后"——避免误选靠后的空壳/重复条目。
        let results = vec![
            SearchMovieItem {
                id: 88396,
                title: "租借女友".into(),
                release_date: Some("2020-07-10".into()),
                ..Default::default()
            },
            SearchMovieItem {
                id: 320068,
                title: "租借女友".into(),
                release_date: Some("2022-01-01".into()),
                ..Default::default()
            },
        ];
        let best = best_result(&results, "租借女友", None, ResultField::MovieTitle);
        assert_eq!(best.map(|b| b.id), Some(88396));
    }

    #[test]
    fn clean_title_removes_noise() {
        assert_eq!(
            clean_search_title("Movie (2020) 1080p hdr [CHS]")[0],
            "Movie"
        );
    }

    #[test]
    fn clean_title_removes_season_suffix() {
        assert_eq!(clean_search_title("爱书的下克上 第2季")[0], "爱书的下克上");
    }

    #[test]
    fn clean_title_removes_bare_year() {
        // 目录名带裸年份（如「租借女友 2020」）应清洗掉，否则 TMDB query 匹配不到
        assert_eq!(clean_search_title("租借女友 2020")[0], "租借女友");
        assert_eq!(clean_search_title("My Movie 2021")[0], "My Movie");
    }

    #[test]
    fn extract_year_finds_4_digit_year() {
        assert_eq!(extract_year("租借女友 2020"), Some(2020));
        assert_eq!(extract_year("1899"), None); // 越界
        assert_eq!(extract_year("无年份"), None);
        assert_eq!(extract_year("2022 剧集"), Some(2022));
    }

    #[test]
    fn keyword_resp_accepts_movie_or_tv_shape() {
        // 电影：只有 `keywords` 键
        let movie: KeywordResp =
            serde_json::from_str(r#"{"keywords":[{"id":1,"name":"romance"}]}"#).unwrap();
        assert_eq!(movie.names(), vec!["romance".to_string()]);
        // 剧集：只有 `results` 键
        let tv: KeywordResp =
            serde_json::from_str(r#"{"results":[{"id":2,"name":"magic"}]}"#).unwrap();
        assert_eq!(tv.names(), vec!["magic".to_string()]);
    }

    #[test]
    fn release_dates_prefers_us_certification() {
        let rd: ReleaseDates = serde_json::from_str(
            r#"{"results":[
                {"iso_3166_1":"JP","release_dates":[{"certification":""}]},
                {"iso_3166_1":"US","release_dates":[{"certification":"PG-13"}]}
            ]}"#,
        )
        .unwrap();
        assert_eq!(rd.certification().as_deref(), Some("PG-13"));
        // 无 US 时回退任意非空
        let rd2: ReleaseDates = serde_json::from_str(
            r#"{"results":[{"iso_3166_1":"DE","release_dates":[{"certification":"6"}]}]}"#,
        )
        .unwrap();
        assert_eq!(rd2.certification().as_deref(), Some("6"));
    }

    #[test]
    fn content_ratings_prefers_us() {
        let cr: ContentRatings = serde_json::from_str(
            r#"{"results":[{"iso_3166_1":"GB","rating":"12"},{"iso_3166_1":"US","rating":"TV-14"}]}"#,
        )
        .unwrap();
        assert_eq!(cr.rating().as_deref(), Some("TV-14"));
    }

    #[test]
    fn best_logo_prefers_language_then_en_then_any() {
        use super::{TmdbImage, best_logo};
        let mk = |lang: Option<&str>, vote: f64| TmdbImage {
            file_path: format!("/{}_{}.png", lang.unwrap_or("null"), vote),
            iso_639_1: lang.map(str::to_string),
            vote_average: Some(vote),
            ..Default::default()
        };
        // prefer zh 命中 → 取 zh 内 vote 最高（7.0）
        let logos = vec![
            mk(Some("en"), 5.0),
            mk(Some("zh"), 7.0),
            mk(Some("zh"), 6.0),
            mk(Some("he"), 9.0),
            mk(None, 8.0),
        ];
        assert_eq!(best_logo(&logos, "zh").unwrap().file_path, "/zh_7.png");

        // 无 zh → 回退 en
        let no_zh = vec![mk(Some("en"), 5.0), mk(Some("he"), 9.0)];
        assert_eq!(best_logo(&no_zh, "zh").unwrap().file_path, "/en_5.png");

        // 无 zh 无 en → 任意，取全局 vote 最高
        let any = vec![mk(Some("he"), 3.0), mk(Some("he"), 9.0)];
        assert_eq!(best_logo(&any, "zh").unwrap().file_path, "/he_9.png");

        // 空 → None
        assert!(best_logo(&[], "zh").is_none());
    }
}
