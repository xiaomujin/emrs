//! 实机验收用 mock 上游：极简 HTTP 文件服务器（支持 Range / Basic Auth）。
//!
//! ```bash
//! MOCK_USER=nasuser MOCK_PASS=naspass cargo run -p emrs-server --example mock_upstream
//! ```

use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let listen = std::env::var("MOCK_LISTEN").unwrap_or_else(|_| "127.0.0.1:9100".into());
    let user = std::env::var("MOCK_USER").ok();
    let pass = std::env::var("MOCK_PASS").ok();

    // 1MB 伪视频数据
    let content: Vec<u8> = (0..=255u8).cycle().take(1024 * 1024).collect();
    let content = Arc::new(content);

    let expect_basic: Option<String> = match (user, pass) {
        (Some(u), Some(p)) => {
            use base64::Engine as _;
            Some(format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("{u}:{p}"))
            ))
        }
        _ => None,
    };

    let listener = tokio::net::TcpListener::bind(&listen).await?;
    println!("mock 上游监听 http://{listen}（视频 1MB，Range 支持）");
    if let Some(b) = &expect_basic {
        println!("要求认证: {b}");
    }

    loop {
        let (mut socket, _) = listener.accept().await?;
        let content = content.clone();
        let expect_basic = expect_basic.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 8192];
            let mut used = 0usize;
            loop {
                match socket.read(&mut buf[used..]).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => {
                        used += n;
                        if buf[..used].windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                        if used == buf.len() {
                            return;
                        }
                    }
                }
            }
            let head = String::from_utf8_lossy(&buf[..used]).to_string();
            println!("<-- {head}");

            let auth_ok = match &expect_basic {
                Some(expected) => head.lines().any(|l| {
                    l.to_ascii_lowercase().starts_with("authorization:") && l.contains(expected)
                }),
                None => true,
            };
            if !auth_ok {
                let _ = socket
                    .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
                    .await;
                return;
            }

            let len = content.len();
            let range_line = head
                .lines()
                .find(|l| l.to_ascii_lowercase().starts_with("range:"));
            let (code, start, end) = match range_line {
                Some(l) => {
                    let spec = l.split(':').nth(1).unwrap_or("").trim();
                    let spec = spec.trim_start_matches("bytes=");
                    let mut parts = spec.split('-');
                    let s: usize = parts.next().unwrap_or("0").parse().unwrap_or(0);
                    let e: usize = match parts.next().and_then(|x| x.parse().ok()) {
                        Some(e) => e,
                        None => len - 1,
                    };
                    (206, s, e.min(len - 1))
                }
                None => (200, 0, len - 1),
            };

            let body = &content[start..=end];
            let content_range = if code == 206 {
                format!("Content-Range: bytes {start}-{end}/{len}\r\n")
            } else {
                String::new()
            };
            let resp = format!(
                "HTTP/1.1 {} {}\r\nContent-Type: video/mp4\r\nContent-Length: {}\r\n\
                 Accept-Ranges: bytes\r\n{content_range}\r\n",
                code,
                if code == 206 { "Partial Content" } else { "OK" },
                body.len(),
            );
            let _ = socket.write_all(resp.as_bytes()).await;
            let _ = socket.write_all(body).await;
        });
    }
}
