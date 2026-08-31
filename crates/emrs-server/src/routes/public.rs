//! 公开发现层（无需认证）：System/Info/Public、Ping、Users/Public、/web stub、AuthenticateByName。
//!
//! 响应字段逐条对齐 Emby 协议。

use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::Value;
use std::time::Duration;

use emrs_core::auth::{AuthStore, random_token};
use emrs_core::cloud::CloudRef;
use emrs_core::playback::{PlayRequest, PlaybackRouter, ticket};

use crate::middleware::device_from_parts;
use crate::routes::user_dto;
use crate::state::AppState;

/// 公开组路由（不走 authGuard）。
pub fn public_routes() -> Router<AppState> {
    Router::new()
        .route("/System/Info/Public", get(info_public))
        .route("/System/Ping", get(ping).head(ping))
        .route("/Users/Public", get(users_public))
        .route("/Users/AuthenticateByName", post(authenticate_by_name))
        .route("/s/{ticket}", get(ticket_play))
        // 图片端点匿名可访问（客户端 <img> 请求不带 token）
        // `{*image_path}` 通配：兼容 Primary / Primary/0 / primary.jpg / Primary/0.jpg
        .route(
            "/Items/{id}/Images/{*image_path}",
            get(super::items::item_image),
        )
        // Admin 登录（公开）
        .route("/admin/login", post(super::admin::admin_login))
}

/// 根级路由（不参与三重前缀）：/ 重定向 /web，/web stub，/admin 管理页。
pub fn root_routes() -> Router<AppState> {
    Router::new()
        .route("/", get(|| async { axum::response::Redirect::to("/web") }))
        .route("/web", get(web_stub))
        .route("/web/", get(web_stub))
        .route("/web/index.html", get(web_stub))
        // 管理后台（单文件 HTML，登录后调用 /admin/* API）
        .route("/admin", get(admin_page))
        .route("/admin/", get(admin_page))
        .route("/admin/index.html", get(admin_page))
}

/// GET /admin：管理后台单文件页面（编译期内联自 assets/admin.html）。
async fn admin_page() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        include_str!("../../assets/admin.html"),
    )
}

/// GET /System/Info/Public：匿名探测（Infuse/Senplayer 发现第一跳）。
async fn info_public(State(state): State<AppState>) -> impl IntoResponse {
    axum::Json(emrs_core::emby::SystemInfoPublicDto::new(
        &state.cfg.emby.server_name,
        &state.cfg.emby.server_id,
    ))
}

/// GET|HEAD /System/Ping：文本应答。
async fn ping() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "emrs Server",
    )
}

/// GET /Users/Public：空列表（不暴露用户名）。
async fn users_public() -> impl IntoResponse {
    axum::Json(Vec::<serde_json::Value>::new())
}

/// GET /web：HTML stub（客户端判定"这是 Emby 服务器"的特征路径）。
async fn web_stub() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        r#"<!doctype html>
<html lang="zh-CN">
<head><meta charset="utf-8"><title>emrs</title>
<meta name="viewport" content="width=device-width,initial-scale=1"></head>
<body style="margin:0;font-family:Segoe UI,Roboto,sans-serif;background:#101828;color:#e6e9ef;min-height:100vh;display:flex;align-items:center;justify-content:center">
<div style="max-width:520px;padding:32px;background:#1d2435;border-radius:12px">
<h1 style="margin:0 0 12px;font-size:20px">emrs</h1>
<p style="color:#aab2c5;line-height:1.5">Emby 兼容媒体服务器（Rust）。请使用 Emby / Infuse 等客户端连接本地址。</p>
</div></body></html>"#,
    )
}

/// 登录失败限流（内存缓存；窗口 10 分钟 10 次）。
const LOGIN_FAIL_LIMIT: u32 = 10;
const LOGIN_FAIL_WINDOW: Duration = Duration::from_secs(600);

fn client_ip_from(req: &Request, peer_addr: Option<std::net::SocketAddr>) -> String {
    // 有直接 socket 地址时优先使用（比 X-Forwarded-For 可靠）
    if let Some(addr) = peer_addr {
        return addr.ip().to_string();
    }
    // 回退 X-Forwarded-For（反向代理场景）
    req.headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

async fn login_fail_allowed(state: &AppState, ip: &str) -> bool {
    let key = format!("login:fail:{ip}");
    let count: u32 = state
        .cache
        .get(&key)
        .await
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    count < LOGIN_FAIL_LIMIT
}

async fn login_fail_record(state: &AppState, ip: &str) {
    let key = format!("login:fail:{ip}");
    let count: u32 = state
        .cache
        .get(&key)
        .await
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let _ = state
        .cache
        .set(&key, &(count + 1).to_string(), LOGIN_FAIL_WINDOW)
        .await;
}

async fn login_fail_clear(state: &AppState, ip: &str) {
    let _ = state.cache.delete(&format!("login:fail:{ip}")).await;
}

/// POST /Users/AuthenticateByName：登录（大小写不敏感 Username/Pw + device 解析 + token 签发）。
async fn authenticate_by_name(State(state): State<AppState>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, 1 << 20).await {
        Ok(b) => b,
        Err(_) => return text(StatusCode::BAD_REQUEST, "bad body"),
    };
    let raw: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return text(StatusCode::BAD_REQUEST, "bad body"),
    };
    // 键小写归一（Pw/pw/PW 均可）
    let mut p: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    if let Value::Object(map) = raw {
        for (k, v) in map {
            if let Value::String(s) = v {
                p.insert(k.to_ascii_lowercase(), s);
            }
        }
    }
    let mut username = p
        .get("username")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if username.is_empty() {
        username = p
            .get("name")
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
    }
    let password = p
        .get("pw")
        .filter(|s| !s.is_empty())
        .or_else(|| p.get("password"))
        .cloned()
        .unwrap_or_default();

    if username.is_empty() {
        return text(StatusCode::UNPROCESSABLE_ENTITY, "用户名不能为空");
    }
    // 保留名（暂未启用）
    // for r in [
    //     "emos", "root", "admin", "system", "test", "null", "true", "false", "emby",
    // ] {
    //     if r.eq_ignore_ascii_case(&username) {
    //         return text(StatusCode::UNPROCESSABLE_ENTITY, "不能使用这个昵称耶");
    //     }
    // }

    // ConnectInfo 在真实 serve 下由 into_make_service_with_connect_info 注入；
    // oneshot 测试无该扩展，回退 X-Forwarded-For。
    let peer_addr = parts
        .extensions
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0);
    let ip = client_ip_from(
        &Request::from_parts(parts.clone(), axum::body::Body::empty()),
        peer_addr,
    );
    if !login_fail_allowed(&state, &ip).await {
        return text(
            StatusCode::TOO_MANY_REQUESTS,
            "登录失败次数过多，请稍后再试",
        );
    }

    // device：X-Emby-Authorization（缺失则拒绝）
    let device = device_from_parts(&parts);
    if device.device_id.is_empty() {
        tracing::warn!(category = "auth", "missing X-Emby-Authorization");
        return text(
            StatusCode::UNAUTHORIZED,
            "暂不兼容此设备 无 x-emby-authorization",
        );
    }

    // 本地库认证
    let user = match AuthStore::find_user(&state.db, &username).await {
        Ok(Some(u)) => u,
        Ok(None) => {
            login_fail_record(&state, &ip).await;
            let _ = AuthStore::log_login_event(
                &state.db,
                &emrs_core::auth::LoginEvent {
                    username: username.clone(),
                    login_type: "user".to_string(),
                    success: false,
                    ip: ip.clone(),
                    device_id: device.device_id.clone(),
                    device_name: device.device.clone(),
                    device_client: device.client.clone(),
                    reason: "user not found".to_string(),
                    ..Default::default()
                },
            )
            .await;
            return text(StatusCode::UNAUTHORIZED, "用户名或密码错误");
        }
        Err(e) => {
            tracing::error!(category = "auth", error = %e, "user lookup failed");
            return text(StatusCode::INTERNAL_SERVER_ERROR, "服务器错误");
        }
    };
    if !user.password_hash.is_empty()
        && !emrs_core::auth::verify_password(&user.password_hash, &password)
    {
        login_fail_record(&state, &ip).await;
        let _ = AuthStore::log_login_event(
            &state.db,
            &emrs_core::auth::LoginEvent {
                user_id: Some(user.id),
                username: username.clone(),
                login_type: "user".to_string(),
                success: false,
                ip: ip.clone(),
                device_id: device.device_id.clone(),
                device_name: device.device.clone(),
                device_client: device.client.clone(),
                reason: "password mismatch".to_string(),
                ..Default::default()
            },
        )
        .await;
        return text(StatusCode::UNAUTHORIZED, "用户名或密码错误");
    }
    if user.is_disable {
        return text(StatusCode::UNAUTHORIZED, "账号已被封禁");
    }

    // 签发 token
    let token = random_token(16);
    if let Err(e) = AuthStore::insert_token(&state.db, &token, user.id, "user", &device).await {
        tracing::error!(category = "auth", error = %e, "store token failed");
        return text(StatusCode::INTERNAL_SERVER_ERROR, "保存会话失败");
    }
    login_fail_clear(&state, &ip).await;
    let _ = AuthStore::touch_last_login(&state.db, user.id).await;
    let _ = AuthStore::log_login_event(
        &state.db,
        &emrs_core::auth::LoginEvent {
            user_id: Some(user.id),
            username: username.clone(),
            login_type: "user".to_string(),
            success: true,
            ip: ip.clone(),
            device_id: device.device_id.clone(),
            device_name: device.device.clone(),
            device_client: device.client.clone(),
            ..Default::default()
        },
    )
    .await;

    let user_dto_val = user_dto(&state, &user);
    let now = emrs_core::emby::format_time_now();
    let user_id_str = user.id.to_string();
    let session_info = emrs_core::emby::SessionInfoDto::new(
        &user_id_str,
        &state.cfg.emby.server_id,
        &user.username,
        &now,
        &device,
    );
    axum::Json(emrs_core::emby::AuthenticateResponseDto {
        user: user_dto_val,
        session_info,
        access_token: token,
        server_id: state.cfg.emby.server_id.clone(),
    })
    .into_response()
}

/// GET /Users/Me（认证组挂载；从 extensions 取 AuthContext）。
pub async fn users_me(
    axum::Extension(ctx): axum::Extension<emrs_core::auth::AuthContext>,
    State(state): State<AppState>,
) -> Response {
    match AuthStore::find_user_by_id(&state.db, ctx.user_id).await {
        Ok(Some(u)) => axum::Json(user_dto(&state, &u)).into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// GET /s/{ticket}：短票据播放（自校验 JWT，无需认证头）。
async fn ticket_play(
    State(st): State<AppState>,
    Path(ticket): Path<String>,
    req: Request,
) -> Response {
    // 1. 验证票据
    let key = match &st.cfg.playback.signing_key {
        Some(k) => k.as_bytes().to_vec(),
        None => {
            tracing::warn!("ticket_play: signing_key not configured");
            return StatusCode::FORBIDDEN.into_response();
        }
    };
    let claims = match ticket::verify_ticket(&ticket, &key) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "ticket_play: invalid ticket");
            return StatusCode::FORBIDDEN.into_response();
        }
    };
    let range = req
        .headers()
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    // 2. 查媒体
    let media = sqlx::query_as::<_, (i64, Option<String>, Option<String>)>(
        "SELECT id, COALESCE(path, remote_path) AS path_url, \
                CASE protocol WHEN 'file' THEN 'local' WHEN 'strm' THEN 'strm' ELSE protocol END AS path_type \
         FROM media_source \
         WHERE uuid = ? LIMIT 1",
    )
    .bind(&claims.uuid)
    .fetch_optional(st.db.pool())
    .await;

    match media {
        Ok(Some((_id, Some(url), Some(typ)))) if typ == "local" => {
            // 本地视频源：Range 流式服务（206/200）
            super::items::serve_local_file(&url, range.as_deref()).await
        }
        Ok(Some((_media_id, Some(url), Some(typ)))) => {
            let cloud_ref = CloudRef {
                path_type: typ,
                path_url: url,
            };
            let req = PlayRequest {
                cloud_ref,
                user_id: claims.user_id,
                device_id: None,
            };
            let router = PlaybackRouter::new(st.drivers.clone(), st.cache.clone());
            match router.resolve_direct(&req).await {
                Ok(Some(direct_url)) => {
                    axum::response::Redirect::temporary(&direct_url).into_response()
                }
                Ok(None) => {
                    tracing::warn!(
                        uuid = claims.uuid,
                        "ticket_play: driver returned no direct url"
                    );
                    StatusCode::NOT_FOUND.into_response()
                }
                Err(e) => {
                    tracing::error!(uuid = claims.uuid, error = %e, "ticket_play: resolve failed");
                    StatusCode::INTERNAL_SERVER_ERROR.into_response()
                }
            }
        }
        Ok(Some((_, Some(url), None))) => {
            // path_type 为 NULL：按 http 直链 302
            axum::response::Redirect::temporary(&url).into_response()
        }
        Ok(Some((_, None, _))) | Ok(None) => {
            tracing::warn!(uuid = claims.uuid, "ticket_play: media not found");
            StatusCode::NOT_FOUND.into_response()
        }
        Err(e) => {
            tracing::error!(uuid = claims.uuid, error = %e, "ticket_play: db error");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

fn text(status: StatusCode, body: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        body.to_string(),
    )
        .into_response()
}
