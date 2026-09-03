//! NFO 解析：从 Kodi 风格的 XML 元数据文件提取标题、描述、年份、id、海报、
//! 分类、制片、标签、演员、评分等。
//!
//! 支持 `tvshow.nfo` / `episode.nfo` / `season.nfo` / 电影 `.nfo`。解析失败返回
//! `None`，不阻塞入库。TMDB 刮削是异步、可未配置（无 key 即整体跳过），此时 NFO
//! 是唯一元数据来源，故尽可能多地提取字段作兜底。
//!
//! 一次事件遍历：重复元素（`<genre>` / `<studio>` / `<tag>` / `<actor>`）全部保留，
//! `<actor>` 块按出现顺序分组并各自收集 name/role/thumb/profile；`<thumb>` 的
//! `aspect` 属性用于区分根级海报与 `<actor>` / `<fanart>` 下的普通缩略图。

use quick_xml::Reader;
use quick_xml::events::Event;
use std::collections::HashMap;
use std::path::Path;

/// NFO 类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NfoKind {
    Movie,
    Episode,
    Season,
    TvShow,
}

/// 单个演员（`<actor>` 块）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NfoActor {
    pub name: String,
    /// 角色名（`<role>`）。
    pub role: Option<String>,
    /// TMDB 人物 id（从 `<profile>` URL 末尾 `person/<id>` 解析）。无则 `None`，调用方据此跳过。
    pub tmdb_id: Option<String>,
    /// 头像 URL（`<thumb>`）。
    pub thumb: Option<String>,
}

/// 解析后的 NFO 元数据。
#[derive(Debug, Clone, Default)]
pub struct Nfo {
    pub kind: Option<NfoKind>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub air_date: Option<String>,
    pub year: Option<i32>,
    pub runtime: Option<i64>,
    pub tmdb_id: Option<String>,
    pub imdb_id: Option<String>,
    pub tvdb_id: Option<String>,
    pub season: i64,
    pub episode: i64,
    /// 标语 `<tagline>`。
    pub tagline: Option<String>,
    /// 连载状态 `<status>`（如 Ended / Continuing）。
    pub status: Option<String>,
    /// 内容分级：`<certification>` 优先，回退 `<mpaa>`。
    pub official_rating: Option<String>,
    /// 社区评分：根级扁平 `<rating>` 或嵌套 `<ratings><rating><value>`。
    pub community_rating: Option<f64>,
    /// 分类（重复 `<genre>`）。
    pub genres: Vec<String>,
    /// 制片公司（重复 `<studio>`）。
    pub studios: Vec<String>,
    /// 标签/关键词（重复 `<tag>`）。
    pub tags: Vec<String>,
    /// 演员（重复 `<actor>`）。
    pub actors: Vec<NfoActor>,
    /// 海报：根级 `<thumb aspect="poster">` / 剧集裸根级 `<thumb>` / `<art><poster>`。
    pub poster: Option<String>,
    /// 背景图：`<fanart><thumb>` 与 `<art><fanart>`。
    pub backdrops: Vec<String>,
}

impl Nfo {
    /// 是否有可用的去重 id。
    pub fn has_any_id(&self) -> bool {
        self.tmdb_id.is_some() || self.imdb_id.is_some() || self.tvdb_id.is_some()
    }
}

/// 读取并解析 NFO 文件。
pub fn parse_nfo_file(path: &Path) -> Option<Nfo> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = Reader::from_reader(std::io::BufReader::new(file));
    reader.config_mut().trim_text(true);
    Some(build(collect(&mut reader)))
}

/// 从字符串解析 NFO（供测试/内存）。
pub fn parse_nfo_str(content: &str) -> Option<Nfo> {
    let mut reader = Reader::from_str(content);
    reader.config_mut().trim_text(true);
    Some(build(collect(&mut reader)))
}

/// 遍历收集到的原始结构。
struct Collected {
    /// 文档根元素名（movie / tvshow / episodedetails / season）。
    root: String,
    /// 相对根的「路径 → 全部文本值（文档序）」。叶子为 `<thumb>` 且带 `aspect` 时路径以
    /// `@{aspect}` 结尾，用于区分海报；`<uniqueid>` 不入此表（见 uniqueids）。
    fields: HashMap<String, Vec<String>>,
    /// `<uniqueid type="X">value` 收集。
    uniqueids: HashMap<String, String>,
    /// `<actor>` 块。
    actors: Vec<ActorRaw>,
}

/// 单个 `<actor>` 块的原始字段。
#[derive(Default)]
struct ActorRaw {
    name: Option<String>,
    role: Option<String>,
    thumb: Option<String>,
    profile: Option<String>,
}

/// 一次事件遍历，把 XML 收集进 [`Collected`]。
fn collect<R: std::io::BufRead>(reader: &mut Reader<R>) -> Collected {
    let mut collected = Collected {
        root: String::new(),
        fields: HashMap::new(),
        uniqueids: HashMap::new(),
        actors: Vec::new(),
    };
    // 元素名栈，root 恒为栈底（首个 Start 元素）。
    let mut stack: Vec<String> = Vec::new();
    // 当前 <thumb> 的 aspect 属性（Start 时置位、End 时清），并入路径 @aspect。
    let mut thumb_aspect: Option<String> = None;
    // 当前 <uniqueid> 的 type 属性。
    let mut uniqueid_type: Option<String> = None;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if stack.is_empty() {
                    collected.root = name.clone();
                }
                match name.as_str() {
                    "uniqueid" => {
                        uniqueid_type = attr(&e, b"type");
                    }
                    "thumb" => {
                        thumb_aspect = attr(&e, b"aspect");
                    }
                    "actor" => collected.actors.push(ActorRaw::default()),
                    _ => {}
                }
                stack.push(name);
            }
            Ok(Event::Text(t)) => {
                let text = t.unescape().map(|s| s.into_owned()).unwrap_or_default();
                let text = text.trim();
                if text.is_empty() || stack.is_empty() {
                    // empty
                } else if stack.last().map(|s| s.as_str()) == Some("uniqueid")
                    && let Some(ut) = &uniqueid_type
                {
                    collected
                        .uniqueids
                        .entry(ut.to_ascii_lowercase())
                        .or_insert_with(|| text.to_string());
                } else {
                    let rel = rel_path(&stack, thumb_aspect.as_deref());
                    // <actor> 子元素归入当前 actor 块，不入 fields。
                    if let Some(rest) = rel.strip_prefix("actor/") {
                        if let Some(actor) = collected.actors.last_mut() {
                            match rest {
                                "name" if actor.name.is_none() => {
                                    actor.name = Some(text.to_string())
                                }
                                "role" if actor.role.is_none() => {
                                    actor.role = Some(text.to_string())
                                }
                                "thumb" if actor.thumb.is_none() => {
                                    actor.thumb = Some(text.to_string())
                                }
                                "profile" if actor.profile.is_none() => {
                                    actor.profile = Some(text.to_string())
                                }
                                _ => {}
                            }
                        }
                    } else {
                        collected
                            .fields
                            .entry(rel)
                            .or_default()
                            .push(text.to_string());
                    }
                }
            }
            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                match name.as_str() {
                    "uniqueid" => uniqueid_type = None,
                    "thumb" => thumb_aspect = None,
                    _ => {}
                }
                if let Some(pos) = stack.iter().rposition(|s| s == &name) {
                    stack.truncate(pos);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    collected
}

/// 读取元素属性值（不校验、跳过非法）。
fn attr(e: &quick_xml::events::BytesStart, key: &[u8]) -> Option<String> {
    e.attributes()
        .with_checks(false)
        .filter_map(|a| a.ok())
        .find(|a| a.key.as_ref() == key)
        .map(|a| String::from_utf8_lossy(&a.value).into_owned())
}

/// 栈 → 相对根的路径（去掉栈底 root）。叶子为 `thumb` 且带非空 aspect 时追加 `@aspect`。
fn rel_path(stack: &[String], thumb_aspect: Option<&str>) -> String {
    let mut segs: Vec<String> = stack[1..].to_vec();
    if let Some(last) = segs.last_mut()
        && last == "thumb"
        && let Some(asp) = thumb_aspect
        && !asp.is_empty()
    {
        *last = format!("thumb@{asp}");
    }
    segs.join("/")
}

/// 从键值映射构建 Nfo。
#[allow(clippy::field_reassign_with_default)]
fn build(c: Collected) -> Nfo {
    let mut nfo = Nfo::default();
    nfo.kind = root_kind(&c.root);

    let first = |path: &str| c.fields.get(path).and_then(|v| v.first()).cloned();
    let all = |path: &str| c.fields.get(path).cloned().unwrap_or_default();
    let parse_i64 = |s: Option<String>| s.and_then(|v| v.trim().parse::<i64>().ok());
    let parse_year = |s: Option<String>| s.and_then(|v| v.trim().parse::<i32>().ok());

    nfo.title = first("title");
    nfo.description = first("overview")
        .or_else(|| first("plot"))
        .or_else(|| first("outline"));
    nfo.air_date = first("premiered")
        .or_else(|| first("airdate"))
        .or_else(|| first("aired"));
    nfo.year = parse_year(first("year"));
    nfo.runtime = parse_i64(first("runtime"));
    nfo.tagline = first("tagline").filter(|s| !s.trim().is_empty());
    nfo.status = first("status").filter(|s| !s.trim().is_empty());
    nfo.official_rating = first("certification")
        .or_else(|| first("mpaa"))
        .filter(|s| !s.trim().is_empty());
    nfo.season = parse_i64(first("season")).unwrap_or(0);
    nfo.episode = parse_i64(first("episode")).unwrap_or(0);

    // 社区评分：优先根级扁平 <rating>（tvshow/episode 形），否则嵌套 <ratings><rating><value>。
    nfo.community_rating = first("rating")
        .or_else(|| first("ratings/rating/value"))
        .and_then(|s| s.trim().parse::<f64>().ok());

    nfo.genres = all("genre");
    nfo.studios = all("studio");
    nfo.tags = all("tag");

    // id：uniqueid[type] 优先，回退裸 <tmdbid>/<imdbid>/<tvdbid>。
    nfo.tmdb_id = c
        .uniqueids
        .get("tmdb")
        .or_else(|| c.uniqueids.get("tmdbid"))
        .cloned()
        .or_else(|| first("tmdbid"));
    nfo.imdb_id = c
        .uniqueids
        .get("imdb")
        .or_else(|| c.uniqueids.get("imdbid"))
        .cloned()
        .or_else(|| first("imdbid"));
    nfo.tvdb_id = c
        .uniqueids
        .get("tvdb")
        .or_else(|| c.uniqueids.get("tvdbid"))
        .cloned()
        .or_else(|| first("tvdbid"));

    // 海报：tinyMediaManager 根级 <thumb aspect="poster"> / 剧集裸根级 <thumb> / 经典 <art><poster>。
    nfo.poster = first("thumb@poster")
        .or_else(|| first("art/poster"))
        .or_else(|| first("thumb"));
    // 背景：<fanart><thumb> 与 <art><fanart>。
    nfo.backdrops = {
        let mut v = all("fanart/thumb");
        v.extend(all("art/fanart"));
        v
    };

    nfo.actors = c
        .actors
        .into_iter()
        .filter_map(|a| {
            let name = a.name?.trim().to_string();
            if name.is_empty() {
                return None;
            }
            Some(NfoActor {
                name,
                role: a.role.filter(|s| !s.trim().is_empty()),
                tmdb_id: a.profile.as_deref().and_then(extract_person_id),
                thumb: a.thumb.filter(|s| !s.trim().is_empty()),
            })
        })
        .collect();

    nfo
}

/// 从 `<profile>` URL 提取 TMDB 人物 id：`https://www.themoviedb.org/person/84205` → `84205`。
fn extract_person_id(url: &str) -> Option<String> {
    let after = url.rsplit("person/").next()?;
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        Some(digits)
    }
}

/// 识别文档根元素（movie / tvshow / episode / season）。
fn root_kind(root: &str) -> Option<NfoKind> {
    match root {
        "movie" => Some(NfoKind::Movie),
        "episodedetails" | "episode" => Some(NfoKind::Episode),
        "tvshow" => Some(NfoKind::TvShow),
        "season" => Some(NfoKind::Season),
        _ => None,
    }
}

/// 便捷读取（带 BOM/编码兜底）。
pub fn read_nfo(path: &Path) -> Option<Nfo> {
    let bytes = std::fs::read(path).ok()?;
    let content = decode(bytes);
    parse_nfo_str(&content)
}

fn decode(bytes: Vec<u8>) -> String {
    for (bom, enc) in [
        (&[0xEF, 0xBB, 0xBF][..], "UTF8"),
        (&[0xFF, 0xFE][..], "UTF-16LE"),
        (&[0xFE, 0xFF][..], "UTF-16BE"),
    ] {
        if bytes.starts_with(bom) {
            let body = &bytes[bom.len()..];
            return match enc {
                "UTF-16LE" => String::from_utf16(
                    &body
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|c| u16::from_le_bytes([c[0], c[1]]))
                        .collect::<Vec<_>>(),
                )
                .unwrap_or_default(),
                "UTF-16BE" => String::from_utf16(
                    &body
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|c| u16::from_be_bytes([c[0], c[1]]))
                        .collect::<Vec<_>>(),
                )
                .unwrap_or_default(),
                _ => String::from_utf8_lossy(body).into_owned(),
            };
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tvshow() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<tvshow>
  <title>爱书的下克上</title>
  <originaltitle>本好きの下剋上</originaltitle>
  <overview>女大学生转生为书痴少女的故事。</overview>
  <year>2019</year>
  <status>Ended</status>
  <rating>8.1</rating>
  <runtime>24</runtime>
  <tagline>书痴的异世界</tagline>
  <mpaa>TV-14</mpaa>
  <uniqueid type="tmdb">108978</uniqueid>
  <uniqueid type="imdb">tt1234567</uniqueid>
  <art>
    <poster>https://image.tmdb.org/p/example.jpg</poster>
    <fanart>https://image.tmdb.org/p/bg.jpg</fanart>
  </art>
</tvshow>"#;
        let nfo = parse_nfo_str(xml).unwrap();
        assert_eq!(nfo.kind, Some(NfoKind::TvShow));
        assert_eq!(nfo.title.as_deref(), Some("爱书的下克上"));
        assert_eq!(nfo.year, Some(2019));
        assert_eq!(nfo.runtime, Some(24));
        assert_eq!(nfo.status.as_deref(), Some("Ended"));
        assert_eq!(nfo.tagline.as_deref(), Some("书痴的异世界"));
        assert_eq!(nfo.official_rating.as_deref(), Some("TV-14"));
        assert_eq!(nfo.community_rating, Some(8.1));
        assert_eq!(nfo.tmdb_id.as_deref(), Some("108978"));
        assert_eq!(nfo.imdb_id.as_deref(), Some("tt1234567"));
        assert!(nfo.has_any_id());
        assert_eq!(
            nfo.poster.as_deref(),
            Some("https://image.tmdb.org/p/example.jpg")
        );
        assert_eq!(
            nfo.backdrops,
            vec!["https://image.tmdb.org/p/bg.jpg".to_string()]
        );
    }

    #[test]
    fn parse_episode() {
        let xml = r#"<episodedetails>
  <title>第二话</title>
  <season>2</season>
  <episode>1</episode>
  <uniqueid type="tmdb">1234</uniqueid>
  <thumb>https://img/still.jpg</thumb>
</episodedetails>"#;
        let nfo = parse_nfo_str(xml).unwrap();
        assert_eq!(nfo.kind, Some(NfoKind::Episode));
        assert_eq!(nfo.season, 2);
        assert_eq!(nfo.episode, 1);
        assert_eq!(nfo.tmdb_id.as_deref(), Some("1234"));
        // 剧集裸根级 <thumb>（无 aspect）应归为海报。
        assert_eq!(nfo.poster.as_deref(), Some("https://img/still.jpg"));
    }

    #[test]
    fn parse_movie() {
        let xml = r#"<movie>
  <title>Movie Example</title>
  <year>2020</year>
  <imdbid>tt9999</imdbid>
</movie>"#;
        let nfo = parse_nfo_str(xml).unwrap();
        assert_eq!(nfo.kind, Some(NfoKind::Movie));
        assert_eq!(nfo.imdb_id.as_deref(), Some("tt9999"));
    }

    /// 多值元素与演员分组：genre/studio/tag 全收；actor 按块分组，profile→tmdb_id，
    /// 无 profile 的演员 tmdb_id 为 None。
    #[test]
    fn multi_values_and_actors() {
        let xml = r#"<movie>
  <genre>惊悚</genre>
  <genre>剧情</genre>
  <studio>Atom</studio>
  <studio>Fox</studio>
  <tag>revenge</tag>
  <tag>politics</tag>
  <actor>
    <name>惠英红</name>
    <role>Madame Tang</role>
    <thumb>https://img/hkr.jpg</thumb>
    <profile>https://www.themoviedb.org/person/84205</profile>
  </actor>
  <actor>
    <name>巫书维</name>
    <role>Marco</role>
    <thumb/>
  </actor>
</movie>"#;
        let nfo = parse_nfo_str(xml).unwrap();
        assert_eq!(nfo.genres, vec!["惊悚".to_string(), "剧情".to_string()]);
        assert_eq!(nfo.studios, vec!["Atom".to_string(), "Fox".to_string()]);
        assert_eq!(
            nfo.tags,
            vec!["revenge".to_string(), "politics".to_string()]
        );
        assert_eq!(nfo.actors.len(), 2);
        assert_eq!(nfo.actors[0].name, "惠英红");
        assert_eq!(nfo.actors[0].role.as_deref(), Some("Madame Tang"));
        assert_eq!(nfo.actors[0].tmdb_id.as_deref(), Some("84205"));
        assert_eq!(nfo.actors[0].thumb.as_deref(), Some("https://img/hkr.jpg"));
        // 第二演员无 profile → tmdb_id None；空 <thumb/> 不收集。
        assert_eq!(nfo.actors[1].tmdb_id, None);
        assert_eq!(nfo.actors[1].thumb, None);
    }

    /// 真实 tinyMediaManager 结构：根级 poster / fanart 背景 / 演员头像三者归类正确。
    #[test]
    fn images_classify_poster_backdrop_and_actor_thumb() {
        let xml = r#"<tvshow>
  <title>Show</title>
  <thumb aspect="poster">https://img/poster.jpg</thumb>
  <thumb aspect="poster" season="1" type="season">https://img/season1.jpg</thumb>
  <fanart><thumb>https://img/fanart.jpg</thumb></fanart>
  <actor><name>A</name><thumb>https://img/actor-a.jpg</thumb></actor>
</tvshow>"#;
        let nfo = parse_nfo_str(xml).unwrap();
        // 首现根级 poster 胜出（季海报同路径被 first 取首）。
        assert_eq!(nfo.poster.as_deref(), Some("https://img/poster.jpg"));
        assert_eq!(nfo.backdrops, vec!["https://img/fanart.jpg".to_string()]);
        // 演员头像不进 poster/backdrop。
        assert!(!nfo.backdrops.iter().any(|u| u.contains("actor")));
        assert_eq!(
            nfo.actors[0].thumb.as_deref(),
            Some("https://img/actor-a.jpg")
        );
    }

    /// 无海报标签则 poster 为空（交刮削补，不拿人物图/背景兜底）。
    #[test]
    fn poster_empty_without_any_poster_tag() {
        let xml = r#"<tvshow>
  <title>Show</title>
  <actor><name>A</name><thumb>https://img/actor-a.jpg</thumb></actor>
</tvshow>"#;
        let nfo = parse_nfo_str(xml).unwrap();
        assert!(nfo.poster.is_none(), "仅有人物图时海报应留空");
    }

    /// 嵌套评分（<ratings><rating><value>）解析。
    #[test]
    fn nested_rating() {
        let xml = r#"<movie>
  <title>M</title>
  <ratings>
    <rating default="true" max="10" name="themoviedb"><value>7.006</value><votes>85</votes></rating>
  </ratings>
</movie>"#;
        let nfo = parse_nfo_str(xml).unwrap();
        assert_eq!(nfo.community_rating, Some(7.006));
    }

    #[test]
    fn extract_person_id_variants() {
        assert_eq!(
            extract_person_id("https://www.themoviedb.org/person/84205").as_deref(),
            Some("84205")
        );
        assert_eq!(
            extract_person_id("https://www.themoviedb.org/person/1294367/").as_deref(),
            Some("1294367")
        );
        assert_eq!(extract_person_id("https://example.com/no-id"), None);
    }

    #[test]
    fn decode_utf16_bom() {
        let mut bytes = vec![0xFF, 0xFE];
        for u in "你好".encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        let s = decode(bytes);
        assert_eq!(s, "你好");
    }
}
