//! 播放后端：Redirect / Proxy / Ticket 三策略。
//!
//! - `RedirectBackend`：调 driver 签发直链 → 302 Location
//! - `ProxyBackend`：reqwest Range 转发，零拷贝
//! - `TicketBackend`：短票据（jwt）自校验播放

pub mod block_cache;
pub mod proxy;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::MemoryCache;
    use std::sync::Arc as StdArc;

    fn cloud_ref(path_type: &str, path_url: &str) -> CloudRef {
        CloudRef {
            path_type: path_type.to_string(),
            path_url: path_url.to_string(),
        }
    }

    /// 测试用临时 sqlite 库 + 默认配置（DriverRegistry 构造需要）。
    async fn test_registry() -> StdArc<DriverRegistry> {
        let dir = tempfile::tempdir().unwrap().keep();
        let dsn = format!(
            "sqlite:{}?mode=rwc",
            dir.join("t.db").to_string_lossy().replace('\\', "/")
        );
        let db = crate::db::Db::connect(&crate::config::StorageConfig {
            dsn,
            max_connections: 2,
        })
        .await
        .unwrap();
        db.migrate().await.unwrap();
        StdArc::new(DriverRegistry::new(
            StdArc::new(db),
            StdArc::new(crate::config::Config::default()),
        ))
    }

    #[tokio::test]
    async fn direct_url_cached() {
        let cache: Arc<dyn Cache> = StdArc::new(MemoryCache::new());
        let router = PlaybackRouter::new(test_registry().await, cache.clone());
        let req = PlayRequest {
            cloud_ref: cloud_ref("url", "https://cdn.example.com/a.mp4"),
            user_id: 1,
            device_id: None,
        };

        let first = router.resolve_direct(&req).await.unwrap().unwrap();
        assert_eq!(first, "https://cdn.example.com/a.mp4");

        // 缓存写入验证
        let key = direct_cache_key(&req.cloud_ref);
        assert_eq!(cache.get(&key).await.as_deref(), Some(first.as_str()));

        // 再次解析：key 已在缓存（driver 无感知）
        let second = router.resolve_direct(&req).await.unwrap().unwrap();
        assert_eq!(second, first);
    }
}
