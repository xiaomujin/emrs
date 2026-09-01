//! HTTP 客户端封装：图片代理下载（`fetch_image`）。
//!
//! 供 `/Items/{id}/Images` 代理返回图片字节使用；视频播放的直链 302 不走本模块。

use std::sync::Arc;

use anyhow::Result;

use crate::http_client::Outbound;

/// 代理配置。
pub struct ProxyConfig {
    pub connect_timeout_secs: u64,
    pub max_retries: u32,
    /// 出网配置（代理 + hosts 覆盖），图片下载走 image.tmdb.org 时同样受益。
    pub outbound: Arc<Outbound>,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            connect_timeout_secs: 10,
            max_retries: 2,
            outbound: Arc::new(Outbound::default()),
        }
    }
}

/// 代理客户端（reqwest Client 连接池复用，应在 AppState 持有单例）。
pub struct ProxyClient {
    client: reqwest::Client,
    _config: ProxyConfig,
}

impl ProxyClient {
    pub fn new(config: ProxyConfig) -> Self {
        let builder = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(config.connect_timeout_secs))
            .user_agent("emrs/0.1");
        // 代理 + hosts 覆盖统一由 Outbound 套用（见 http_client.rs）。
        let builder = config.outbound.configure(builder);
        // 启动期单例构建：TLS 后端初始化失败属致命配置错误，直接 panic 而非
        // 静默回退到默认 client（会丢失 connect_timeout / proxy / UA 配置）。
        let client = builder
            .build()
            .expect("构建 reqwest client 失败（TLS 后端初始化异常）");
        Self {
            client,
            _config: config,
        }
    }

    /// 下载整张图片（用于 `/Items/{id}/Images` 代理返回）。
    /// 返回 (字节, Content-Type)；非 2xx 视为失败。
    pub async fn fetch_image(&self, url: &str) -> Result<(Vec<u8>, String)> {
        // 图片为有界下载：请求级 30s 总超时，防止上游接受连接后挂起拖死 worker。
        let resp = self
            .client
            .get(url)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("图片下载失败: {status} for {url}");
        }
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_owned();
        let bytes = resp.bytes().await?.to_vec();
        Ok((bytes, content_type))
    }
}
