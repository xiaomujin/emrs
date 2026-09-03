//! 缓存实现：memory(moka) / redis / valkey + 两层门面 + 工厂。
//!
//! `Cache` trait 与 [`CacheError`] 定义在 emrs-core `cache` 模块，本模块只做实现。

mod facade;
mod memory;
mod redis;

use std::sync::Arc;

pub use facade::preheat;
pub use facade::{
    AuthCache, CacheFacade, DefaultAuthCache, DefaultMediaCache, DefaultPlaybackCache,
    DefaultSessionCache, MediaCache, PlaybackCache, SessionCache, TwoTierCache,
};
pub use memory::MemoryCache;
pub use redis::{RedisCache, SharedConnection, ValkeyCache};

use emrs_core::cache::{Cache, CacheError};
use emrs_core::config::{CacheBackend, CacheConfig};

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
            let conn = SharedConnection::connect(url, runtime).map_err(CacheError::Unavailable)?;
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
