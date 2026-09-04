//! 中间件：lowercaseQuery / securityHeaders / requestLogger / authGuard。
//!
//! 协议语义逐条对齐：
//! - query key 统一小写（Emby 客户端大小写混乱的 quirk）
//! - 安全响应头
//! - token 提取 9 处位置；命中顺序 master key → auth_token(admin) → auth_token(user)
//! - GET/HEAD 的 `/Sessions`、`/Items/Counts`、`/System/Info` 允许匿名兼容读

use axum::extract::Request;
use axum::http::{HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use emrs_core::auth::{AuthContext, DeviceInfo, token_hash};
use emrs_infra::auth_store::AuthStore;
use std::time::Instant;

use crate::state::AppState;

/// Emby 客户端 token 提取矩阵。
pub fn extract_token(parts: &http::request::Parts) -> String {
    const QUERY_KEYS: [&str; 4] = ["x-emby-token", "api_key", "apikey", "x-mediabrowser-token"];
    const HEADER_KEYS: [&str; 2] = ["x-emby-token", "x-mediabrowser-token"];

    // 1. Authorization: Bearer
    if let Some(auth) = parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        let raw = auth.trim();
        if raw.len() > 7 && raw[..7].eq_ignore_ascii_case("Bearer ") {
            return raw[7..].trim().to_string();
        }
    }
    // 2-5. query: x-emby-token / api_key / apikey / x-mediabrowser-token
    // （惰性解析：命中 key 才解码其值，不整串构造键值对）
    if let Some(v) = query_value_for(parts.uri.query(), &QUERY_KEYS) {
        return v;
    }
    // 6-7. 头：X-Emby-Token / X-MediaBrowser-Token
    for h in HEADER_KEYS {
        if let Some(v) = parts.headers.get(h).and_then(|v| v.to_str().ok())
            && !v.is_empty()
        {
            return v.to_string();
        }
    }
    // 8-9. X-Emby-Authorization 内嵌 Token="..."
    if let Some(raw) = parts
        .headers
        .get("x-emby-authorization")
        .and_then(|v| v.to_str().ok())
        && let Some(token) = extract_token_from_emby_auth(raw)
    {
        return token;
    }
    String::new()
}

/// 惰性 query 取键：仅当某对键名匹配 `keys` 且值非空时才 URL 解码一次返回，
/// 未命中目标键时零分配。
fn query_value_for(query: Option<&str>, keys: &[&str]) -> Option<String> {
    for pair in query?.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if !v.is_empty() && keys.iter().any(|key| k.eq_ignore_ascii_case(key)) {
            return Some(urldecode(v));
        }
    }
    None
}

/// `X-Emby-Authorization` 里解析 `Token="..."` 与 device 信息。
/// 正则 `Token="([^"]+)"`；device 键 client/device/deviceid/version。
fn parse_emby_auth(raw: &str) -> (Option<String>, DeviceInfo) {
    let mut token = None;
    let mut d = DeviceInfo::default();
    let s = raw
        .strip_prefix("MediaBrowser ")
        .or_else(|| raw.strip_prefix("Emby "))
        .unwrap_or(raw)
        .replace(", ", ",");
    for part in s.split(',') {
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        let key = k.trim().to_ascii_lowercase();
        let mut val = v.trim().trim_matches('"').to_string();
        if val.len() > 200 {
            val.truncate(200);
        }
        match key.as_str() {
            "client" => d.client = val,
            "device" => d.device = val,
            "deviceid" => d.device_id = val,
            "version" => d.version = val,
            "token" if !val.is_empty() => token = Some(val),
            _ => {}
        }
    }
    (token, d)
}

fn extract_token_from_emby_auth(raw: &str) -> Option<String> {
    parse_emby_auth(raw).0
}

/// 登录请求的设备解析（公开路由用）。
pub fn device_from_parts(parts: &http::request::Parts) -> DeviceInfo {
    parts
        .headers
        .get("x-emby-authorization")
        .and_then(|v| v.to_str().ok())
        .map(|raw| parse_emby_auth(raw).1)
        .unwrap_or_default()
}

/// 轻量 percent-decode（query 值足够；`%XX` hex 与 `+` → 空格）。
fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            // `%` 后两个 hex 位齐全才解码；否则按字面 `%` 输出
            b'%' if hex_val(bytes.get(i + 1)).and(hex_val(bytes.get(i + 2))).is_some() => {
                let hi = hex_val(bytes.get(i + 1)).unwrap();
                let lo = hex_val(bytes.get(i + 2)).unwrap();
                out.push(hi * 16 + lo);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 单个十六进制位（`b` 为 None 或非 hex 时返回 None）。
fn hex_val(b: Option<&u8>) -> Option<u8> {
    (*b? as char).to_digit(16).map(|d| d as u8)
}

/// 匿名兼容读。
fn is_compat_anonymous_read(method: &Method, path: &str) -> bool {
    if method != Method::GET && method != Method::HEAD {
        return false;
    }
    path.ends_with("/Sessions") || path.ends_with("/Items/Counts") || path.ends_with("/System/Info")
}

/// lowercaseQuery：query key 全部转小写后重写 URI。
/// （进 handler 前统一；body 不动）
pub async fn lowercase_query(mut req: Request, next: Next) -> Response {
    if let Some(q) = req.uri().query().map(|s| s.to_string()) {
        let lowered: Vec<String> = q
            .split('&')
            .filter(|s| !s.is_empty())
            .map(|pair| match pair.split_once('=') {
                Some((k, v)) => format!("{}={}", k.to_ascii_lowercase(), v),
                None => pair.to_ascii_lowercase(),
            })
            .collect();
        if !lowered.is_empty() {
            let new_uri = build_uri(lowered.join("&"), req.uri());
            *req.uri_mut() = new_uri;
        }
    }
    next.run(req).await
}

fn build_uri(new_query: String, uri: &Uri) -> Uri {
    let path = uri.path().to_string();
    let fallback = uri.clone();
    Uri::builder()
        .path_and_query(format!("{path}?{new_query}"))
        .build()
        .unwrap_or(fallback)
}

/// securityHeaders：响应安全头。
pub async fn security_headers(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    h.insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    h.insert(
        "content-security-policy",
        HeaderValue::from_static(
            "default-src 'self'; img-src 'self' data: https:; style-src 'self' 'unsafe-inline'; script-src 'self' 'unsafe-inline'; connect-src 'self'",
        ),
    );
    res
}

/// requestLogger：一行访问日志。
pub async fn request_logger(req: Request, next: Next) -> Response {
    let start = Instant::now();
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or_default().to_string();
    let res = next.run(req).await;
    tracing::info!(
        category = "http",
        method = %method,
        status = res.status().as_u16(),
        path = %path,
        query = %query,
        duration_ms = start.elapsed().as_millis() as u64,
    );
    res
}

/// authGuard：认证矩阵。无效 token 且非兼容读路径 → 401。
/// 通过则把 [`AuthContext`] 插入 request extensions。
pub async fn auth_guard(
    axum::extract::State(state): axum::extract::State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let (parts, body) = req.into_parts();

    // 匿名兼容读
    let token = extract_token(&parts);
    if token.is_empty() {
        if is_compat_anonymous_read(&parts.method, parts.uri.path()) {
            let req = Request::from_parts(parts, body);
            return next.run(req).await;
        }
        return text_response(StatusCode::UNAUTHORIZED, "登录失效 请重新登录");
    }

    // 1. master API key（config）
    if !state.cfg.emby.api_key.is_empty() && token == state.cfg.emby.api_key {
        let ctx = AuthContext {
            user_id: 0,
            username: String::new(),
            is_admin: true,
            token,
            device: DeviceInfo::default(),
        };
        return finish_guard(ctx, parts, body, next).await;
    }

    // 2-3. 带 A 档缓存的 token 校验（写穿透：命中缓存直返，miss 回 DB 后写回缓存）
    let hash = token_hash(&token);
    let cache_key = format!("auth:{hash}");

    // 先查缓存
    if let Some(cached) = state.cache.get(&cache_key).await {
        if cached == "revoked" {
            return text_response(StatusCode::UNAUTHORIZED, "登录失效 请重新登录");
        }
        // 缓存命中：按 kind 分派
        if cached.starts_with("admin:")
            && let Ok(Some(ctx)) = AuthStore::verify_admin_token(&state.db, &token).await
        {
            return finish_guard(ctx, parts, body, next).await;
        } else if cached.starts_with("user:")
            && let Ok(Some(ctx)) = AuthStore::verify_user_token(&state.db, &token).await
        {
            return finish_guard(ctx, parts, body, next).await;
        }
    }

    // 缓存 miss：顺序查 DB，命中后写回缓存
    match AuthStore::verify_admin_token(&state.db, &token).await {
        Ok(Some(ctx)) => {
            let _ = state
                .cache
                .set(&cache_key, "admin:ok", std::time::Duration::from_secs(300))
                .await;
            return finish_guard(ctx, parts, body, next).await;
        }
        Err(e) => tracing::error!(category = "auth", error = %e, "admin token lookup failed"),
        Ok(None) => {}
    }

    // 3. auth_token(user)（用户会话）
    match AuthStore::verify_user_token(&state.db, &token).await {
        Ok(Some(ctx)) => {
            let _ = state
                .cache
                .set(&cache_key, "user:ok", std::time::Duration::from_secs(300))
                .await;
            return finish_guard(ctx, parts, body, next).await;
        }
        Err(e) => tracing::error!(category = "auth", error = %e, "user token lookup failed"),
        Ok(None) => {}
    }

    // token 提供但全部未命中：兼容读路径仍放行
    if is_compat_anonymous_read(&parts.method, parts.uri.path()) {
        let req = Request::from_parts(parts, body);
        return next.run(req).await;
    }
    let _ = token_hash(&token); // 审计钩子位
    text_response(StatusCode::UNAUTHORIZED, "登录失效 请重新登录")
}

async fn finish_guard(
    ctx: AuthContext,
    mut parts: http::request::Parts,
    body: axum::body::Body,
    next: Next,
) -> Response {
    parts.extensions.insert(ctx);
    let req = Request::from_parts(parts, body);
    next.run(req).await
}

fn text_response(status: StatusCode, body: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        body.to_string(),
    )
        .into_response()
}

/// JSON API 超时（from_fn 实现，永不 fallible）。
/// 只用于 JSON 组；流式播放路由绝不能加（防长播掐断）。
pub async fn api_timeout(req: Request, next: Next) -> Response {
    const API_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
    tokio::time::timeout(API_TIMEOUT, next.run(req)).await.unwrap_or_else(|_| text_response(StatusCode::REQUEST_TIMEOUT, "请求超时"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;

    fn parts_with(uri: &str, headers: &[(&str, &str)]) -> http::request::Parts {
        let mut builder = HttpRequest::builder().uri(uri);
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        builder.body(()).unwrap().into_parts().0
    }

    #[test]
    fn token_matrix() {
        // Bearer
        let p = parts_with("/x", &[("Authorization", "Bearer abc")]);
        assert_eq!(extract_token(&p), "abc");
        // query
        let p = parts_with("/x?X-Emby-Token=t1", &[]);
        assert_eq!(extract_token(&p), "t1");
        let p = parts_with("/x?api_key=t2", &[]);
        assert_eq!(extract_token(&p), "t2");
        // header
        let p = parts_with("/x", &[("X-Emby-Token", "t3")]);
        assert_eq!(extract_token(&p), "t3");
        // X-Emby-Authorization 内嵌
        let p = parts_with(
            "/x",
            &[(
                "X-Emby-Authorization",
                r#"MediaBrowser Client="Infuse", Device="iPhone", DeviceId="d1", Version="7", Token="t4""#,
            )],
        );
        assert_eq!(extract_token(&p), "t4");
        // 优先级：Bearer > query
        let p = parts_with("/x?x-emby-token=q", &[("Authorization", "Bearer h")]);
        assert_eq!(extract_token(&p), "h");
    }

    #[test]
    fn device_parse() {
        let p = parts_with(
            "/x",
            &[(
                "X-Emby-Authorization",
                r#"MediaBrowser Client="Infuse", Device="iPhone 15", DeviceId="abc", Version="7.8""#,
            )],
        );
        let d = device_from_parts(&p);
        assert_eq!(d.client, "Infuse");
        assert_eq!(d.device, "iPhone 15");
        assert_eq!(d.device_id, "abc");
        assert_eq!(d.version, "7.8");
    }

    #[tokio::test]
    async fn lowercase_query_rewrites() {
        let app = axum::Router::new()
            .route(
                "/probe",
                axum::routing::get(|uri: Uri| async move {
                    uri.query().unwrap_or_default().to_string()
                }),
            )
            .layer(axum::middleware::from_fn(lowercase_query));
        let res = app
            .oneshot(
                HttpRequest::get("/probe?MediaSourceId=42&UserID=7")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], b"mediasourceid=42&userid=7");
    }

    #[test]
    fn anonymous_read_paths() {
        assert!(is_compat_anonymous_read(&Method::GET, "/Sessions"));
        assert!(is_compat_anonymous_read(&Method::GET, "/emby/Items/Counts"));
        assert!(is_compat_anonymous_read(
            &Method::HEAD,
            "/emby/emby/System/Info"
        ));
        // 播放走 /s/{ticket} 票据通道，/Videos/ 需认证不放行
        assert!(!is_compat_anonymous_read(&Method::GET, "/Videos/1/a.mp4"));
        assert!(!is_compat_anonymous_read(&Method::GET, "/Videos/1"));
        assert!(!is_compat_anonymous_read(&Method::POST, "/Sessions"));
    }
}
