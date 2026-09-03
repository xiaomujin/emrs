//! 文件名/目录名解析：从媒体文件名提取标题、年份、季号、集号、provider ID。
//!
//! 解析规则：
//! - provider 标签 `[tmdb=502419]` / `[imdb=tt...]` / `[tvdb=N]` 提取并剥离
//! - `SxxExx` / `NxN`(1x02) 为最强剧集信号
//! - `E01` / `EP01` / `第01集` / `第X话`（含中文数字）
//! - `Season 1` / `第X季`
//! - `(2023)` 年份
//!
//! 纯函数、无 IO，便于单元测试。

use regex::Regex;
use std::sync::LazyLock;

/// provider id 标签：`[tmdb=502419]`、`{imdb-tt1234567}`、`(tvdb:123)`。
static RE_PROVIDER_TAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)[\[\(\{【（]\s*(tmdb|tmdbid|imdb|imdbid|tvdb|tvdbid)\s*[=\-:]\s*([a-z0-9]+)\s*[\]\)\}】）]"#)
        .unwrap()
});

/// `Movie (2023)` / `Movie [2023]`。
static RE_TITLE_YEAR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)^(?P<title>.+?)[\s._]*[\(\[](?P<year>(19|20)\d{2})[\)\]]").unwrap()
});

/// `Show.Name.S01E02` / `Show Name - S01E02` / `Show Season 1 Episode 2` / `Show.Name.1x02`。
static RE_SXXEXX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|[\s._\-])(?:s|season[\s._\-]*)(\d{1,3})[\s._\-]*(?:episode|ep|e)[\s._\-]*(\d{1,4})(?:[\s._\-]|$)")
        .unwrap()
});

/// `Show.Name.1x02`。
static RE_NXN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:^|[\s._\-])(\d{1,2})x(\d{1,4})(?:[\s._\-]|$)").unwrap());

/// 中文/通用集号标记：`第01集`、`第 1 话`、`EP01`、`01集`。
static RE_CHINESE_EPISODE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:第|ep|episode|e)?[\s._\-]*(\d{1,4})[\s._\-]*(?:集|话|話)").unwrap()
});

/// 中文数字集号：`第X话`、`第X集`。
static RE_CHINESE_EPISODE_NUM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"第\s*([零〇一二两三四五六七八九十百]+)\s*(?:集|话|話)").unwrap());

/// 裸季号目录：`S01` / `S2`。
static RE_BARE_SEASON: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^s0?(\d{1,3})(?:e0)?$").unwrap());

/// `Season 1` / `season01`。
static RE_SEASON_WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)season[\s._\-]*(\d{1,3})").unwrap());

/// `第1季` / `第 1 季`。
static RE_SEASON_CHINESE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"第\s*(\d{1,3})\s*[季部]").unwrap());

/// 中文数字季：`第X季`。
static RE_SEASON_CHINESE_NUM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"第\s*([零〇一二两三四五六七八九十百]+)\s*[季部]").unwrap());

/// 显式集标记：`EP02`、`Episode 12`、`E03`。
static RE_EPISODE_MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|[\s._\-])(?:episode|ep|e)[\s._\-]*(\d{1,4})(?:[\s._\-]|$)").unwrap()
});

/// 年份提取：`19xx`/`20xx`。
static RE_YEAR_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:19|20)\d{2}\b").unwrap());
/// 合并集：`S01E03-E04` / `S01E03E04` / `E03-E04` / `EP03-EP04`。
static RE_MERGED_EPISODE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|[\s._\-])(?:s|season[\s._\-]*)?(\d{1,3})?[\s._\-]*(?:e|ep|episode)[\s._\-]*(\d{1,4})[\s._\-]*[-~][\s._\-]*(?:e|ep|episode)?[\s._\-]*(\d{1,4})(?:[\s._\-]|$)")
        .unwrap()
});

/// 文件名解析结果。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedName {
    pub title: String,
    pub year: Option<i32>,
    /// 季号，0 = 未知。
    pub season: i64,
    /// 集号，0 = 未知。
    pub episode: i64,
    /// 合并集尾号（如 E03-E04 → episode=3, episode_end=4），0 = 非合并集。
    pub episode_end: i64,
    /// 仅靠裸数字（`01.mkv`）判定集号时为 true，需额外 TV 上下文才视为剧集。
    pub weak_episode: bool,
    pub tmdb_id: Option<String>,
    pub imdb_id: Option<String>,
    pub tvdb_id: Option<String>,
}

impl ParsedName {
    /// 是否含任何季/集提示。
    pub fn is_episode(&self) -> bool {
        self.episode > 0
    }
}

/// 解析单个路径段（自动去扩展名）。
pub fn parse_filename(name: &str) -> ParsedName {
    let base = match name.rsplit_once('.') {
        Some((stem, ext))
            if !ext.is_empty()
                && ext.len() <= 5
                && ext.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            stem
        }
        _ => name,
    };
    parse_base(base)
}

/// 解析季目录名，返回季号。非季目录返回 -1，Specials 返回 0。
pub fn parse_season_folder(folder: &str) -> i64 {
    if let Some(c) = RE_SEASON_WORD.captures(folder)
        && let Ok(n) = c.get(1).unwrap().as_str().parse::<i64>()
    {
        return n;
    }
    if let Some(c) = RE_SEASON_CHINESE_NUM.captures(folder)
        && let Some(n) = chinese_number(c.get(1).unwrap().as_str())
    {
        return n;
    }
    if let Some(c) = RE_SEASON_CHINESE.captures(folder)
        && let Ok(n) = c.get(1).unwrap().as_str().parse::<i64>()
    {
        return n;
    }
    let f = folder.trim();
    // 裸季号目录：`S01` / `S2` / `S1E0` 前缀
    if let Some(c) = RE_BARE_SEASON.captures(f)
        && let Ok(n) = c.get(1).unwrap().as_str().parse::<i64>()
    {
        return n;
    }
    if f.eq_ignore_ascii_case("specials") || f.contains("特别篇") || f.contains("花絮") {
        return 0;
    }
    -1
}

/// 无扩展名文件名解析（核心逻辑）。
fn parse_base(s: &str) -> ParsedName {
    let mut p = ParsedName::default();

    // 0. 提取 provider 标签并剥离
    for cap in RE_PROVIDER_TAG.captures_iter(s) {
        let kind = cap.get(1).unwrap().as_str().to_ascii_lowercase();
        let val = cap.get(2).unwrap().as_str().to_string();
        match kind.as_str() {
            "tmdb" | "tmdbid" => p.tmdb_id = Some(val),
            "imdb" | "imdbid" => p.imdb_id = Some(val),
            "tvdb" | "tvdbid" => p.tvdb_id = Some(val),
            _ => {}
        }
    }
    let s = RE_PROVIDER_TAG.replace_all(s, "").into_owned();

    // 0. 合并集：S01E03-E04 / E03-E04（优先于单集匹配）
    if let Some(c) = RE_MERGED_EPISODE.captures(&s) {
        if let Some(season_cap) = c.get(1) {
            p.season = season_cap.as_str().parse().unwrap_or(0);
        }
        p.episode = c.get(2).unwrap().as_str().parse().unwrap_or(0);
        p.episode_end = c.get(3).unwrap().as_str().parse().unwrap_or(0);
        p.title = clean_title(RE_MERGED_EPISODE.split(&s).next().unwrap_or(""));
        p.year = extract_year(&s);
        return p;
    }

    // 1. SxxExx / NxN —— 最强剧集信号
    if let Some(c) = RE_SXXEXX.captures(&s) {
        p.season = c.get(1).unwrap().as_str().parse().unwrap_or(0);
        p.episode = c.get(2).unwrap().as_str().parse().unwrap_or(0);
        p.title = clean_title(RE_SXXEXX.split(&s).next().unwrap_or(""));
        p.year = extract_year(&s);
        return p;
    }
    if let Some(c) = RE_NXN.captures(&s) {
        p.season = c.get(1).unwrap().as_str().parse().unwrap_or(0);
        p.episode = c.get(2).unwrap().as_str().parse().unwrap_or(0);
        p.title = clean_title(RE_NXN.split(&s).next().unwrap_or(""));
        p.year = extract_year(&s);
        return p;
    }

    // 2. 显式集标记：E01 / EP01 / Episode 12
    if let Some(c) = RE_EPISODE_MARKER.captures(&s) {
        p.episode = c.get(1).unwrap().as_str().parse().unwrap_or(0);
        p.title = clean_title(RE_EPISODE_MARKER.split(&s).next().unwrap_or(""));
        p.year = extract_year(&s);
        return p;
    }
    if let Some(c) = RE_CHINESE_EPISODE.captures(&s) {
        p.episode = c.get(1).unwrap().as_str().parse().unwrap_or(0);
        p.title = clean_title(RE_CHINESE_EPISODE.split(&s).next().unwrap_or(""));
        p.year = extract_year(&s);
        return p;
    }
    if let Some(c) = RE_CHINESE_EPISODE_NUM.captures(&s)
        && let Some(n) = chinese_number(c.get(1).unwrap().as_str())
    {
        p.episode = n;
        p.title = clean_title(RE_CHINESE_EPISODE_NUM.split(&s).next().unwrap_or(""));
        p.year = extract_year(&s);
        return p;
    }

    // 3. 年份 + 剩余标题
    p.year = extract_year(&s);
    let title = RE_YEAR_TOKEN.replace_all(&s, "").into_owned();
    p.title = clean_title(&title);

    // 3. 裸数字兜底：`01.mkv`（弱剧集信号）
    let title_digits: String = title.chars().filter(|c| c.is_ascii_digit()).collect();
    if !title_digits.is_empty()
        && title_digits.len() == title.chars().filter(|c| !c.is_whitespace()).count()
        && let Ok(n) = title_digits.parse::<i64>()
    {
        p.episode = n;
        p.weak_episode = true;
        p.title = String::new();
        return p;
    }

    p
}

/// 清洗标题：去括号噪音、去年份、压缩空白。
fn clean_title(raw: &str) -> String {
    let mut t = raw.trim().to_string();
    // 关键：剥离 `(2023)` 这类年份括号
    if let Some(c) = RE_TITLE_YEAR.captures(&t) {
        t = c
            .name("title")
            .map(|m| m.as_str().to_string())
            .unwrap_or(t.clone());
    }
    // 去前后括号残留
    t = t
        .trim_matches(|c| {
            matches!(
                c,
                ' ' | '\t' | '\r' | '\n' | '.' | '_' | '-' | '+' | '~' | '[' | ']' | '(' | ')'
            )
        })
        .to_string();
    // 压缩空白
    let mut out = String::with_capacity(t.len());
    let mut space = false;
    for ch in t.chars() {
        if ch.is_whitespace() {
            if !space && !out.is_empty() {
                out.push(' ');
            }
            space = true;
        } else {
            out.push(ch);
            space = false;
        }
    }
    out.trim().to_string()
}

/// 提取年份：`Movie (2019)` / `Movie.2019`。
pub fn extract_year(name: &str) -> Option<i32> {
    RE_YEAR_TOKEN
        .find(name)
        .and_then(|m| m.as_str().parse::<i32>().ok())
        .filter(|y| (1900..=2099).contains(y))
}

/// 中文数字 → 阿拉伯数字：`一二三` → 123，`十` → 10。
fn chinese_number(s: &str) -> Option<i64> {
    let mut tens = 0i64;
    let mut units = 0i64;
    let mut seen_shi = false;
    for c in s.chars() {
        let d = match c {
            '零' | '〇' => 0,
            '一' => 1,
            '二' | '两' => 2,
            '三' => 3,
            '四' => 4,
            '五' => 5,
            '六' => 6,
            '七' => 7,
            '八' => 8,
            '九' => 9,
            '十' => 10,
            _ => return None,
        };
        if d == 10 {
            if !seen_shi {
                if tens == 0 && units == 0 {
                    tens = 1; // 裸 "十" 或 "十 X"
                }
                seen_shi = true;
            }
        } else if seen_shi {
            units = d;
        } else {
            tens = d;
        }
    }
    Some(tens * 10 + units)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sxxexx_basic() {
        let p = parse_filename("爱书的下克上 S2E01.mkv");
        assert_eq!(p.title, "爱书的下克上");
        assert_eq!(p.season, 2);
        assert_eq!(p.episode, 1);
        assert!(p.is_episode());
    }

    #[test]
    fn sxxexx_leading_series_dir() {
        let p = parse_filename("租借女友 S1E49.mkv");
        assert_eq!(p.title, "租借女友");
        assert_eq!(p.season, 1);
        assert_eq!(p.episode, 49);
    }

    #[test]
    fn nxn_pattern() {
        let p = parse_filename("Show.Name.1x02.mkv");
        assert_eq!(p.title, "Show.Name");
        assert_eq!(p.season, 1);
        assert_eq!(p.episode, 2);
    }

    #[test]
    fn episode_marker() {
        let p = parse_filename("EP03.mkv");
        assert_eq!(p.episode, 3);
    }

    #[test]
    fn chinese_episode() {
        let p = parse_filename("第01集.mkv");
        assert_eq!(p.episode, 1);
    }

    #[test]
    fn chinese_number_episode() {
        let p = parse_filename("第十二话.mkv");
        assert_eq!(p.episode, 12);
    }

    #[test]
    fn provider_tag() {
        let p = parse_filename("Movie 2020 [tmdb=502419].mkv");
        assert_eq!(p.tmdb_id.as_deref(), Some("502419"));
        assert_eq!(p.year, Some(2020));
        assert_eq!(p.title, "Movie");
    }

    #[test]
    fn year_only_movie() {
        let p = parse_filename("Movie (2019).mkv");
        assert_eq!(p.year, Some(2019));
        assert_eq!(p.title, "Movie");
    }

    #[test]
    fn season_folder() {
        assert_eq!(parse_season_folder("Season 2"), 2);
        assert_eq!(parse_season_folder("S01"), 1);
        assert_eq!(parse_season_folder("第 3 季"), 3);
        assert_eq!(parse_season_folder("specials"), 0);
        assert_eq!(parse_season_folder("爱书的下克上"), -1);
    }

    #[test]
    fn bare_number_weak_episode() {
        let p = parse_filename("01.mkv");
        assert_eq!(p.episode, 1);
        assert!(p.weak_episode);
    }

    #[test]
    fn chinese_number_values() {
        assert_eq!(chinese_number("十"), Some(10));
        assert_eq!(chinese_number("十二"), Some(12));
        assert_eq!(chinese_number("二十"), Some(20));
        assert_eq!(chinese_number("三十七"), Some(37));
    }
}
