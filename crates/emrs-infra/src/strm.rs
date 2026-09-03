//! STRM 文件解析。
//!
//! 解析规则：
//! - 多行（仅第一行有效行）
//! - `#` 开头注释行
//! - BOM（`\u{FEFF}`）跳过
//! - UTF-16 LE/BE 自动检测
//! - 相对路径（相对 .strm 所在目录解析）
//! - scheme 白名单校验

use std::path::Path;

use anyhow::{Context, Result, bail};

/// 支持的 scheme 白名单（网盘驱动已移除，仅保留 http/https 直链与本地文件）。
const SCHEME_WHITELIST: &[&str] = &["file", "http", "https"];

/// 解析后的媒体路径。
#[derive(Debug, Clone)]
pub struct StrmPath {
    /// 路径类型（即 scheme，如 `url` / `webdav` / `115` 等）。
    pub path_type: String,
    /// 完整 URL 或路径。
    pub path_url: String,
}

/// 解析 STRM 文件内容，返回第一条有效的媒体路径。
pub fn parse_strm(content: &[u8], strm_dir: &Path) -> Result<StrmPath> {
    // 1. 检测 BOM 并解码
    let decoded = decode_strm_content(content)?;

    // 2. 逐行扫描，取第一条非空、非注释行
    let line = decoded
        .lines()
        .find(|l| {
            let trimmed = l.trim();
            !trimmed.is_empty() && !trimmed.starts_with('#')
        })
        .map(|l| l.trim().to_string())
        .context("STRM 文件中没有有效的 URL 行")?;

    // 3. 解析 scheme
    let (scheme, path_url) = if let Some(pos) = line.find("://") {
        let scheme = line[..pos].to_ascii_lowercase();
        // URL 型：保持原样
        (scheme, line)
    } else {
        // 无 scheme：视为相对路径，相对于 strm 文件所在目录
        let abs = strm_dir.join(&line);
        let abs_str = abs.to_string_lossy().replace('\\', "/");
        ("file".to_string(), format!("file://{abs_str}"))
    };

    // 4. scheme 白名单校验
    if !SCHEME_WHITELIST.contains(&scheme.as_str()) {
        bail!("不支持的 scheme: {scheme}");
    }

    // 5. 映射 scheme 到 path_type
    let path_type = match scheme.as_str() {
        "file" => "local",
        "http" | "https" => "url",
        other => other,
    };

    Ok(StrmPath {
        path_type: path_type.to_string(),
        path_url,
    })
}

/// 检测并解码 STRM 内容（BOM / UTF-16 / UTF-8）。
fn decode_strm_content(content: &[u8]) -> Result<String> {
    // UTF-16 LE with BOM
    if content.len() >= 2 && content[0] == 0xFF && content[1] == 0xFE {
        let u16_data: Vec<u16> = content[2..]
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c.get(1).copied().unwrap_or(0)]))
            .take_while(|&c| c != 0)
            .collect();
        return String::from_utf16(&u16_data).context("UTF-16 LE 解码失败");
    }
    // UTF-16 BE with BOM
    if content.len() >= 2 && content[0] == 0xFE && content[1] == 0xFF {
        let u16_data: Vec<u16> = content[2..]
            .chunks(2)
            .map(|c| u16::from_be_bytes([c[0], c.get(1).copied().unwrap_or(0)]))
            .take_while(|&c| c != 0)
            .collect();
        return String::from_utf16(&u16_data).context("UTF-16 BE 解码失败");
    }
    // UTF-8（跳过 BOM）
    let s = if content.starts_with(b"\xEF\xBB\xBF") {
        String::from_utf8(content[3..].to_vec())
    } else {
        String::from_utf8(content.to_vec())
    };
    s.context("UTF-8 解码失败")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_url() {
        let result = parse_strm(b"http://example.com/movie.mp4\n", Path::new("/x")).unwrap();
        assert_eq!(result.path_type, "url");
        assert_eq!(result.path_url, "http://example.com/movie.mp4");
    }

    #[test]
    fn webdav_scheme_rejected() {
        // 网盘驱动已移除，webdav 不再受支持
        let result = parse_strm(b"webdav://nas/video.mkv\n", Path::new("/x"));
        assert!(result.is_err());
    }

    #[test]
    fn with_comment_and_newlines() {
        let content = b"# comment line\n\nhttp://cdn.example.com/file.mp4\n";
        let result = parse_strm(content, Path::new("/x")).unwrap();
        assert_eq!(result.path_type, "url");
        assert_eq!(result.path_url, "http://cdn.example.com/file.mp4");
    }

    #[test]
    fn relative_path() {
        let result = parse_strm(b"relative/path.mkv\n", Path::new("/strm_dir")).unwrap();
        assert_eq!(result.path_type, "local");
        assert!(result.path_url.contains("relative/path.mkv"));
    }

    #[test]
    fn bom_utf8() {
        let mut content = vec![0xEF, 0xBB, 0xBF];
        content.extend_from_slice(b"http://example.com/movie.mp4\n");
        let result = parse_strm(&content, Path::new("/x")).unwrap();
        assert_eq!(result.path_type, "url");
        assert_eq!(result.path_url, "http://example.com/movie.mp4");
    }

    #[test]
    fn unsupported_scheme() {
        let result = parse_strm(b"ftp://example.com/file.mp4\n", Path::new("/x"));
        assert!(result.is_err());
    }

    #[test]
    fn empty_file() {
        let result = parse_strm(b"", Path::new("/x"));
        assert!(result.is_err());
    }
}
