//! NFO 解析：从 Kodi 风格的 XML 元数据文件提取标题、描述、年份、id、海报等。
//!
//! 支持 `tvshow.nfo` / `episode.nfo` /
//! `season.nfo` / 电影 `.nfo`。解析失败返回 `None`，不阻塞入库。

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
    /// 海报/背景图 URL（`<art><poster>/<thumb>`）。
    pub images: Vec<String>,
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
    Some(build(&collect_map(&mut reader)))
}

/// 从缓冲区解析 NFO（供测试/内存字符串）。
pub fn parse_nfo_str(content: &str) -> Option<Nfo> {
    let mut reader = Reader::from_str(content);
    reader.config_mut().trim_text(true);
    Some(build(&collect_map(&mut reader)))
}

/// 展平 XML 为 (路径, 文本) 映射；`<uniqueid type="X">` 记录为键 `uniqueid/X`。
fn collect_map<R: std::io::BufRead>(reader: &mut Reader<R>) -> HashMap<String, String> {
    let mut map: HashMap<String, String> = HashMap::new();
    let mut stack: Vec<String> = Vec::new();
    let mut uniqueid_type: Option<String> = None;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if name == "uniqueid" {
                    uniqueid_type = e
                        .attributes()
                        .with_checks(false)
                        .filter_map(|a| a.ok())
                        .find(|a| a.key.as_ref() == b"type")
                        .map(|a| String::from_utf8_lossy(&a.value).into_owned());
                }
                stack.push(name);
            }
            Ok(Event::Empty(_)) => {}
            Ok(Event::Text(t)) => {
                let text = t.unescape().map(|s| s.into_owned()).unwrap_or_default();
                let text = text.trim();
                if !text.is_empty() && !stack.is_empty() {
                    let key = if let Some(ut) = &uniqueid_type {
                        format!("uniqueid/{ut}")
                    } else {
                        stack.join("/")
                    };
                    map.entry(key).or_insert(text.to_string());
                }
            }
            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if name == "uniqueid" {
                    uniqueid_type = None;
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
    map
}

/// 从键值映射构建 Nfo。
fn build(map: &HashMap<String, String>) -> Nfo {
    let mut nfo = Nfo::default();

    let get = |prefix: &str, key: &str| map.get(&format!("{prefix}/{key}")).cloned();
    let root = detect_root(map);

    nfo.kind = root_kind(root);
    let prefix = root;

    nfo.title = get(prefix, "title");
    nfo.description = get(prefix, "overview").or_else(|| get(prefix, "plot"));
    nfo.air_date = get(prefix, "airdate").or_else(|| get(prefix, "premiered"));
    nfo.year = get(prefix, "year")
        .as_deref()
        .and_then(|s| s.trim().parse::<i32>().ok());
    nfo.runtime = get(prefix, "runtime")
        .as_deref()
        .and_then(|s| s.trim().parse::<i64>().ok());

    // <uniqueid type="tmdb|imdb|tvdb"> / <imdbid> / <tvdbid> / <tmdbid>
    for (k, v) in map {
        if let Some(typ) = k.strip_prefix("uniqueid/") {
            match typ.to_ascii_lowercase().as_str() {
                "imdb" | "imdbid" => {
                    if nfo.imdb_id.is_none() {
                        nfo.imdb_id = Some(v.clone());
                    }
                }
                "tvdb" | "tvdbid" => {
                    if nfo.tvdb_id.is_none() {
                        nfo.tvdb_id = Some(v.clone());
                    }
                }
                "tmdb" | "tmdbid" => {
                    if nfo.tmdb_id.is_none() {
                        nfo.tmdb_id = Some(v.clone());
                    }
                }
                _ => {}
            }
            continue;
        }
        if k.ends_with("/imdbid") || k.ends_with("/imdb_id") {
            if nfo.imdb_id.is_none() {
                nfo.imdb_id = Some(v.clone());
            }
        } else if k.ends_with("/tvdbid") || k.ends_with("/tvdb_id") {
            if nfo.tvdb_id.is_none() {
                nfo.tvdb_id = Some(v.clone());
            }
        } else if (k.ends_with("/tmdbid") || k.ends_with("/tmdb_id") || k.ends_with("/tmdb_id"))
            && nfo.tmdb_id.is_none()
        {
            nfo.tmdb_id = Some(v.clone());
        }
    }

    // 季/集
    nfo.season = get(prefix, "season")
        .as_deref()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(0);
    nfo.episode = get(prefix, "episode")
        .as_deref()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(0);

    // 图片：<art><poster>/<thumb>、<thumb>
    for (k, v) in map {
        if (k.contains("/art/") || k.ends_with("/thumb"))
            && (k.contains("poster") || k.contains("thumb"))
        {
            nfo.images.push(v.clone());
        }
    }

    nfo
}

/// 识别文档根元素（movie / tvshow / episode / season）。
fn detect_root(map: &HashMap<String, String>) -> &'static str {
    for k in map.keys() {
        let first = k.split('/').next().unwrap_or("");
        match first {
            "movie" => return "movie",
            "episodedetails" | "episode" => return "episodedetails",
            "tvshow" => return "tvshow",
            "season" => return "season",
            _ => {}
        }
    }
    "movie"
}

fn root_kind(root: &str) -> Option<NfoKind> {
    match root {
        "movie" => Some(NfoKind::Movie),
        "episodedetails" => Some(NfoKind::Episode),
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
                        .chunks_exact(2)
                        .map(|c| u16::from_le_bytes([c[0], c[1]]))
                        .collect::<Vec<_>>(),
                )
                .unwrap_or_default(),
                "UTF-16BE" => String::from_utf16(
                    &body
                        .chunks_exact(2)
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
  <runtime>24</runtime>
  <uniqueid type="tmdb">108978</uniqueid>
  <uniqueid type="imdb">tt1234567</uniqueid>
  <art>
    <poster>https://image.tmdb.org/p/example.jpg</poster>
  </art>
</tvshow>"#;
        let nfo = parse_nfo_str(xml).unwrap();
        assert_eq!(nfo.kind, Some(NfoKind::TvShow));
        assert_eq!(nfo.title.as_deref(), Some("爱书的下克上"));
        assert_eq!(nfo.year, Some(2019));
        assert_eq!(nfo.runtime, Some(24));
        assert_eq!(nfo.tmdb_id.as_deref(), Some("108978"));
        assert_eq!(nfo.imdb_id.as_deref(), Some("tt1234567"));
        assert!(nfo.has_any_id());
        assert_eq!(nfo.images.len(), 1);
    }

    #[test]
    fn parse_episode() {
        let xml = r#"<episodedetails>
  <title>第二话</title>
  <season>2</season>
  <episode>1</episode>
  <uniqueid type="tmdb">1234</uniqueid>
</episodedetails>"#;
        let nfo = parse_nfo_str(xml).unwrap();
        assert_eq!(nfo.kind, Some(NfoKind::Episode));
        assert_eq!(nfo.season, 2);
        assert_eq!(nfo.episode, 1);
        assert_eq!(nfo.tmdb_id.as_deref(), Some("1234"));
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
