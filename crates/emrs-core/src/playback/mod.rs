//! 播放后端：直链解析（PlaybackRouter）+ Ticket 短票据。
//!
//! - `PlaybackRouter::resolve_direct`：调 driver 签发直链（Cache 加速）→ 302 Location
//! - `ticket`：短票据（jwt）自校验播放
//!
//! `PlaybackRouter`（直链解析 + 缓存）在 core（仅依赖 Cache/Driver trait）；
//! 磁盘块缓存 [`block_cache`](emrs_infra::block_cache) 是文件 IO，属 emrs-infra。

pub mod redirect;
pub mod ticket;

use std::sync::Arc;
use std::time::Duration;

use crate::cache::Cache;
use crate::cloud::{CloudRef, DriverRegistry, ResolvedSource};

/// 直链缓存 TTL（10 分钟）。
pub const DIRECT_URL_TTL: Duration = Duration::from_secs(600);

/// 播放票据 TTL 下限（6 小时）：覆盖任意单片整场播放（含拖动/暂停续播）。
/// 与 [`DIRECT_URL_TTL`]（直链解析结果缓存）解耦——票据在每个 `/s/{ticket}`
/// 请求上重新校验，本地/Proxy 源的播放器会发多次 Range，TTL 过短会在播放中途 403。
/// 已知媒体时长时还会取「时长+1h」与此下限的较大值。
pub const PLAYBACK_TICKET_TTL: Duration = Duration::from_secs(6 * 3600);

/// 播放请求。
#[derive(Debug, Clone)]
pub struct PlayRequest {
    pub cloud_ref: CloudRef,
    pub user_id: i64,
    pub device_id: Option<String>,
}

/// 播放后端错误。
#[derive(Debug, thiserror::Error)]
pub enum PlayError {
    #[error("未找到媒体")]
    NotFound,
    #[error("禁止访问")]
    Forbidden,
    #[error("上游错误: {status}")]
    Upstream { status: u16 },
    #[error("内部错误: {0}")]
    Internal(#[from] anyhow::Error),
}

/// 播放后端路由。
pub struct PlaybackRouter {
    pub drivers: Arc<DriverRegistry>,
    pub cache: Arc<dyn Cache>,
}

impl PlaybackRouter {
    pub fn new(drivers: Arc<DriverRegistry>, cache: Arc<dyn Cache>) -> Self {
        Self { drivers, cache }
    }

    /// 解析直链（用于 302 重定向）。命中 Cache 直接返回，miss 时调 driver 并写缓存。
    pub async fn resolve_direct(&self, req: &PlayRequest) -> Result<Option<String>, PlayError> {
        let cache_key = direct_cache_key(&req.cloud_ref);
        if let Some(hit) = self.cache.get(&cache_key).await {
            return Ok(Some(hit));
        }

        let resolved = self
            .drivers
            .resolve(&req.cloud_ref)
            .await
            .map_err(|e| PlayError::Internal(anyhow::anyhow!("driver resolve failed: {e}")))?;

        match resolved {
            Some(ResolvedSource::Direct(url)) => {
                if let Err(e) = self.cache.set(&cache_key, &url, DIRECT_URL_TTL).await {
                    tracing::warn!(error = %e, "direct url cache set failed");
                }
                Ok(Some(url))
            }
            None => Ok(None),
        }
    }
}

/// 直链缓存 key：`direct:{path_type}:{path_url}`。
fn direct_cache_key(r: &CloudRef) -> String {
    format!("direct:{}:{}", r.path_type, r.path_url)
}

// 注：`resolve_direct` 的缓存命中行为测试随实现依赖（MemoryCache / Db /
// DriverRegistry 构造）迁至 emrs-infra `tests/playback_direct.rs`。
