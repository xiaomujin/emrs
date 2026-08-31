//! 缓存抽象：`Cache` trait + 三实现（memory / redis / valkey）。
//!
//! Redis 与 Valkey 复用同一 RESP 客户端，仅 `name()` 区分用于日志。

mod facade;
mod memory;
mod redis;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;

pub use facade::preheat;
pub use facade::{
    AuthCache, CacheFacade, DefaultAuthCache, DefaultMediaCache, DefaultPlaybackCache,
    DefaultSessionCache, MediaCache, PlaybackCache, SessionCache, TwoTierCache,
};
pub use memory::MemoryCache;
pub use redis::{RedisCache, ValkeyCache};

use crate::config::{CacheBackend, CacheConfig};

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("缓存后端不可用: {0}")]
    Unavailable(String),
    #[error("缓存操作失败: {0}")]
    Backend(String),
}

/// 统一字符串缓存接口。
#[async_trait]
pub trait Cache: Send + Sync {
    async fn get(&self, key: &str) -> Option<String>;
    async fn set(&self, key: &str, value: &str, ttl: Duration) -> Result<(), CacheError>;
    async fn delete(&self, key: &str) -> Result<(), CacheError>;
    /// 后端名（memory/redis/valkey），日志用。
    fn name(&self) -> &'static str;
}

/// 按配置构建缓存实例。
pub fn new_cache(cfg: &CacheConfig) -> Result<Arc<dyn Cache>, CacheError> {
    match cfg.backend {
        CacheBackend::Memory => Ok(Arc::new(MemoryCache::new())),
        CacheBackend::Redis | CacheBackend::Valkey => {
            let url = cfg.url.as_deref().ok_or_else(|| {
                CacheError::Unavailable(format!("{:?} 后端必须配置 cache.url", cfg.backend))
            })?;
            let runtime = tokio::runtime::Handle::try_current()
                .map_err(|e| CacheError::Unavailable(format!("不在 tokio 运行时内: {e}")))?;
            let conn =
                redis::SharedConnection::connect(url, runtime).map_err(CacheError::Unavailable)?;
            if cfg.backend == CacheBackend::Valkey {
                Ok(Arc::new(ValkeyCache::new(conn)))
            } else {
                Ok(Arc::new(RedisCache::new(conn)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_memory_default() {
        let cfg = CacheConfig::default();
        let c = new_cache(&cfg).unwrap();
        assert_eq!(c.name(), "memory");
    }

    #[test]
    fn factory_redis_requires_url() {
        let cfg = CacheConfig {
            backend: CacheBackend::Redis,
            url: None,
            ..Default::default()
        };
        match new_cache(&cfg) {
            Err(CacheError::Unavailable(_)) => {}
            Err(e) => panic!("错误类型不符: {e}"),
            Ok(_) => panic!("缺 url 应报 Unavailable"),
        }
    }
}
