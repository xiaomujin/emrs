//! 统一出网模块：自定义 DNS（hosts）覆盖 + HTTP 代理，供 TMDB 刮削器与图片代理共用。
//!
//! 背景：TMDB 域名（`api.themoviedb.org` / `image.tmdb.org`）在国内常被墙。除 HTTP 代理
//! （`http.proxy_url`）外，另提供一条 hosts 覆盖路线——从可配置 URL 拉一份标准 hosts 文件，
//! 用 reqwest 原生 `ClientBuilder::resolve_to_addrs` 把域名钉到可用 IP。TLS 的 SNI / 证书校验
//! 仍用原始域名，证书照常通过。
//!
//! **hosts 与 proxy 互斥生效**：配了 `proxy_url` 时域名走 CONNECT 隧道交代理解析，
//! 本地 `resolve()` 对这些请求不生效。用 hosts 时应把 `http.proxy_url` 留空直连。

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use reqwest::ClientBuilder;
use reqwest::header::HeaderName;

use crate::config::HttpConfig;

/// 拉取远程 hosts 的超时（秒）。远端可能挂起，必须兜底、绝不阻断启动。
const HOSTS_FETCH_TIMEOUT_SECS: u64 = 10;

/// host（小写）→ 解析地址列表。同域名可多 IP（hosts 文件里多行同 host）。
pub type HostMap = BTreeMap<String, Vec<IpAddr>>;

/// 出网配置：代理 + hosts 覆盖。进程启动时构建一次，`Arc` 共享给各 reqwest client。
#[derive(Debug, Clone, Default)]
pub struct Outbound {
    /// HTTP 代理地址（如 `http://127.0.0.1:7890`）。为空直连。
    pub proxy_url: Option<String>,
    /// hosts 覆盖表。空表示不覆盖。
    pub hosts: Arc<HostMap>,
}

impl Outbound {
    /// 空出网（无代理、无 hosts）——测试 / 默认路径的便捷构造。
    pub fn none() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 从 `HttpConfig` 构建：`load_hosts` 拉取 hosts（异步，调用方 `await` 后传入），
    /// 代理取 `http.proxy_url`。
    pub async fn from_config(http: &HttpConfig) -> Arc<Self> {
        let hosts = load_hosts(http).await;
        Arc::new(Self {
            proxy_url: http.proxy_url.clone(),
            hosts,
        })
    }

    /// 把代理 + hosts 套到 reqwest builder 上。调用方保留自己的 UA / 超时 / 默认头。
    pub fn configure(&self, mut builder: ClientBuilder) -> ClientBuilder {
        let proxy_set = self
            .proxy_url
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty());
        if let Some(proxy) = proxy_set {
            match reqwest::Proxy::all(proxy) {
                Ok(p) => builder = builder.proxy(p),
                Err(e) => tracing::warn!(proxy, error = %e, "HTTP 代理解析失败，忽略代理直连"),
            }
        }
        for (host, addrs) in self.hosts.iter() {
            // port 占位 443：reqwest 实际按 URL scheme 选端口，这些域名均为 https。
            let sockets: Vec<SocketAddr> =
                addrs.iter().map(|ip| SocketAddr::new(*ip, 443)).collect();
            if sockets.is_empty() {
                continue;
            }
            // 先用 HeaderName 校验域名合法（含空格等非法名 reqwest 内部会 panic），再传入。
            match HeaderName::from_bytes(host.as_bytes()) {
                Ok(_) => builder = builder.resolve_to_addrs(host.as_str(), &sockets),
                Err(e) => tracing::warn!(host, error = %e, "hosts 域名非法，跳过"),
            }
        }
        if proxy_set.is_some() && !self.hosts.is_empty() {
            tracing::warn!(
                "同时配置了 hosts 与 http.proxy_url：走代理的请求不会应用本地 hosts 覆盖，二者建议只启用其一"
            );
        }
        builder
    }
}

/// 解析标准 hosts 文本为 [`HostMap`]。
///
/// 逐行：跳过空行与 `#` 注释；按空白切分，token[0]=IP、token[1]=host（其余别名列忽略）；
/// IP 解析失败或字段不足则忽略该行。host 统一小写。
pub fn parse_hosts(text: &str) -> HostMap {
    let mut map: HostMap = BTreeMap::new();
    for line in text.lines() {
        // 去行内注释（hosts 允许 `IP host # comment`，本项目样本也用整行注释）
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut it = line.split_whitespace();
        let (Some(ip_str), Some(host)) = (it.next(), it.next()) else {
            continue;
        };
        let Ok(ip) = ip_str.parse::<IpAddr>() else {
            continue;
        };
        map.entry(host.to_ascii_lowercase()).or_default().push(ip);
    }
    map
}

/// 启动时加载 hosts：远程 URL（可配）→ 本地文件（缓存/离线兜底）→ 内联（覆盖）。
/// 任一来源缺失或失败都静默降级，返回空表等价现状。
pub async fn load_hosts(http: &HttpConfig) -> Arc<HostMap> {
    let mut map: HostMap = BTreeMap::new();

    let url = http
        .hosts_url
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty());
    let file = http
        .hosts_file
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    match url {
        Some(u) => match fetch_hosts(u, http.proxy_url.as_deref()).await {
            Ok(text) => {
                merge(&mut map, parse_hosts(&text));
                tracing::info!(url = u, domains = map.len(), "hosts 已从远程拉取");
                // 成功后写回本地文件作离线兜底（失败仅告警，不影响本次内存结果）。
                if let Some(f) = file {
                    cache_hosts_file(f, &text);
                }
            }
            Err(e) => {
                tracing::warn!(url = u, error = %e, "hosts 拉取失败，尝试本地文件兜底");
                if let Some(f) = file {
                    load_hosts_file(f, &mut map);
                }
            }
        },
        None => {
            // 无 URL：纯离线，仅读本地文件（若配）。
            if let Some(f) = file {
                load_hosts_file(f, &mut map);
            }
        }
    }

    // 内联行最后并入 → 同名 host 覆盖远程/文件。
    if !http.hosts_inline.is_empty() {
        let inline = parse_hosts(&http.hosts_inline.join("\n"));
        merge(&mut map, inline);
    }

    if map.is_empty() {
        tracing::debug!("未配置 hosts（url/文件/内联皆空），TMDB 走系统 DNS 或代理");
    } else {
        tracing::info!(domains = map.len(), "自定义 hosts 覆盖已启用");
    }
    Arc::new(map)
}

/// 读本地 hosts 文件并并入 `map`（不存在/读失败仅告警）。
fn load_hosts_file(path: &str, map: &mut HostMap) {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            merge(map, parse_hosts(&text));
            tracing::info!(file = path, domains = map.len(), "hosts 已从本地文件加载");
        }
        Err(e) => tracing::warn!(file = path, error = %e, "hosts 文件读取失败"),
    }
}

/// 把拉取到的 hosts 文本写回本地缓存文件（建父目录；失败仅告警）。
fn cache_hosts_file(path: &str, text: &str) {
    if let Some(dir) = std::path::Path::new(path).parent()
        && !dir.as_os_str().is_empty()
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        tracing::warn!(file = path, error = %e, "hosts 缓存目录创建失败");
        return;
    }
    match std::fs::write(path, text.as_bytes()) {
        Ok(()) => tracing::info!(file = path, "hosts 已缓存到本地文件"),
        Err(e) => tracing::warn!(file = path, error = %e, "hosts 缓存写入失败"),
    }
}

/// 把 `src` 各 host 覆盖进 `dst`（逐 host 替换，非追加）。
fn merge(dst: &mut HostMap, src: HostMap) {
    for (host, addrs) in src {
        dst.insert(host, addrs);
    }
}

/// 用一个**不套 hosts 的裸 client**（仅可选套代理）拉取远程 hosts。
/// 不套 hosts 是为了自举：拉 hosts 这一步本身不能依赖 hosts。
async fn fetch_hosts(url: &str, proxy_url: Option<&str>) -> Result<String> {
    let mut builder = ClientBuilder::new()
        .user_agent("emrs/0.1")
        .timeout(Duration::from_secs(HOSTS_FETCH_TIMEOUT_SECS))
        .connect_timeout(Duration::from_secs(HOSTS_FETCH_TIMEOUT_SECS));
    if let Some(p) = proxy_url.map(str::trim).filter(|s| !s.is_empty())
        && let Ok(pr) = reqwest::Proxy::all(p)
    {
        builder = builder.proxy(pr);
    }
    let client = builder.build()?;
    let resp = client.get(url).send().await?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("HTTP {status}");
    }
    Ok(resp.text().await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"# Tmdb Hosts Start
18.160.10.119               tmdb.org
3.170.19.19                 api.tmdb.org
3.170.42.44                 files.tmdb.org
3.171.38.80                 themoviedb.org
3.170.19.97                 api.themoviedb.org
23.54.127.10                ia.media-imdb.com
18.67.62.75                 www.imdb.com
23.54.127.13                ia.media-imdb.com
# Update time: 2026-09-01T09:11:39+08:00
# IPv6 Update url: https://raw.githubusercontent.com/...
# Tmdb Hosts End"#;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn parses_ip_host_and_skips_comments() {
        let m = parse_hosts(SAMPLE);
        assert_eq!(m.get("api.tmdb.org"), Some(&vec![ip("3.170.19.19")]));
        assert!(m.contains_key("tmdb.org"));
        assert!(m.contains_key("api.themoviedb.org"));
        // 注释行里的 host（如 URL 路径）不应入表
        assert!(!m.contains_key("raw.githubusercontent.com"));
    }

    #[test]
    fn aggregates_multiple_ips_per_host() {
        let m = parse_hosts(SAMPLE);
        let ia = m.get("ia.media-imdb.com").unwrap();
        assert_eq!(ia.len(), 2);
        assert!(ia.contains(&ip("23.54.127.10")));
        assert!(ia.contains(&ip("23.54.127.13")));
    }

    #[test]
    fn lowercases_host_and_ignores_garbage_lines() {
        let m = parse_hosts("1.2.3.4  Api.Example.COM alias\nnot-an-ip host\n\n# comment");
        assert_eq!(m.get("api.example.com"), Some(&vec![ip("1.2.3.4")]));
        assert!(!m.contains_key("host")); // IP 列非法 → 整行忽略
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn parses_ipv6() {
        let m = parse_hosts("2001:db8::1 api.themoviedb.org");
        assert_eq!(m.get("api.themoviedb.org"), Some(&vec![ip("2001:db8::1")]));
    }

    #[test]
    fn merge_overrides_same_host() {
        let mut m = parse_hosts("1.1.1.1 api.tmdb.org");
        merge(&mut m, parse_hosts("2.2.2.2 api.tmdb.org"));
        assert_eq!(m.get("api.tmdb.org"), Some(&vec![ip("2.2.2.2")]));
    }

    #[test]
    fn inline_overrides_remote() {
        // 模拟：远程给出 api.tmdb.org=3.170.19.19，内联改成手填一个更优 IP。
        let mut map = parse_hosts("3.170.19.19 api.tmdb.org");
        let inline = parse_hosts("9.9.9.9 api.tmdb.org");
        merge(&mut map, inline);
        assert_eq!(map.get("api.tmdb.org"), Some(&vec![ip("9.9.9.9")]));
    }

    #[test]
    fn configure_accepts_hosts_without_panic() {
        // resolve_addrs 对合法 host 不应 panic / 报错；空表也应正常返回 builder。
        let mut map = HostMap::new();
        map.insert("api.tmdb.org".to_string(), vec![ip("3.170.19.19")]);
        let outbound = Outbound {
            proxy_url: None,
            hosts: Arc::new(map),
        };
        let client = outbound.configure(ClientBuilder::new()).build();
        assert!(client.is_ok());
        // 非法 host 名（含空格）应被跳过而非中断。
        let mut bad = HostMap::new();
        bad.insert("bad host".to_string(), vec![ip("1.1.1.1")]);
        let ob2 = Outbound {
            proxy_url: None,
            hosts: Arc::new(bad),
        };
        assert!(ob2.configure(ClientBuilder::new()).build().is_ok());
    }
}
