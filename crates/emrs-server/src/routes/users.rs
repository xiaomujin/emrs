//! Users 域：`/Users` 命名空间下的发现、登录与用户查询。
//!
//! - [`public`]：`/Users/Public`（空列表，不暴露用户名）、`/Users/AuthenticateByName`（登录签发 token）。
//! - [`authenticated`]：`/Users/Me`（当前用户）、`/Users/{user_id}`、`/Users`（用户枚举不开放，返回空）。
//!
//! 客户端发现与登录入口集中于此；`/Users/{uid}/Items*` 等用户视图端点属 Items 域。

use std::time::Duration;

use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::Value;

use emrs_core::auth::random_token;
use emrs_infra::auth_store::AuthStore;

use crate::middleware::device_from_parts;
use crate::state::AppState;

/// 公开组：Users/Public + AuthenticateByName（登录不走 authGuard）。
pub fn public() -> Router<AppState> {
    Router::new()
        .route("/Users/Public", get(users_public))
        .route("/Users/AuthenticateByName", post(authenticate_by_name))
}

/// 认证组：Users/Me + Users/{id} + Users。
pub fn authenticated() -> Router<AppState> {
    Router::new()
        .route("/Users/Me", get(users_me))
        .route("/Users/{user_id}", get(user_by_id))
        .route("/Users", get(users_list))
}

/// Emby User DTO。
pub fn user_dto(state: &AppState, u: &emrs_core::auth::UserRow) -> crate::emby::UserDto {
    crate::emby::user_to_json(&state.cfg.emby.server_id, u)
}

/// GET /Users/Public：空列表（不暴露用户名）。
async fn users_public() -> impl IntoResponse {
    axum::Json(Vec::<serde_json::Value>::new())
}

/// GET /Users：空列表（Emby 兼容：用户枚举不开放）。
async fn users_list() -> impl IntoResponse {
    axum::Json(Vec::<serde_json::Value>::new())
}

/// GET /Users/{user_id}：按 id 取用户。
async fn user_by_id(Path(user_id): Path<i64>, State(state): State<AppState>) -> impl IntoResponse {
    match AuthStore::find_user_by_id(&state.db, user_id).await {
        Ok(Some(u)) => axum::Json(user_dto(&state, &u)).into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// GET /Users/Me（认证组挂载；从 extensions 取 AuthContext）。
async fn users_me(
    axum::Extension(ctx): axum::Extension<emrs_core::auth::AuthContext>,
    State(state): State<AppState>,
) -> Response {
    match AuthStore::find_user_by_id(&state.db, ctx.user_id).await {
        Ok(Some(u)) => axum::Json(user_dto(&state, &u)).into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
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
    let now = crate::emby::format_time_now();
    let user_id_str = user.id.to_string();
    let session_info = crate::emby::SessionInfoDto::new(
        &user_id_str,
        &state.cfg.emby.server_id,
        &user.username,
        &now,
        &device,
    );
    axum::Json(crate::emby::AuthenticateResponseDto {
        user: user_dto_val,
        session_info,
        access_token: token,
        server_id: state.cfg.emby.server_id.clone(),
    })
    .into_response()
}

fn text(status: StatusCode, body: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        body.to_string(),
    )
        .into_response()
}
