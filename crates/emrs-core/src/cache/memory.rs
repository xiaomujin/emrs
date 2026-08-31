//! 内存缓存实现：moka future cache + 每条目 TTL。

use std::time::{Duration, Instant};

use async_trait::async_trait;
use moka::Expiry;
use moka::future::Cache as MokaCache;

use super::{Cache, CacheError};

/// 值 + 过期时间点；moka Expiry 负责清理，get 时再校验兜底。
#[derive(Clone)]
struct Entry {
    value: String,
    expires_at: Instant,
}

struct TtlExpiry;

impl Expiry<String, Entry> for TtlExpiry {
    fn expire_after_create(&self, _key: &String, value: &Entry, _now: Instant) -> Option<Duration> {
        Some(value.expires_at.saturating_duration_since(Instant::now()))
    }
}

pub struct MemoryCache {
    inner: MokaCache<String, Entry>,
}

impl MemoryCache {
    pub fn new() -> Self {
        Self {
            inner: MokaCache::builder().expire_after(TtlExpiry).build(),
        }
    }
}

impl Default for MemoryCache {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Cache for MemoryCache {
    async fn get(&self, key: &str) -> Option<String> {
        let entry = self.inner.get(key).await?;
        (Instant::now() < entry.expires_at).then(|| entry.value.clone())
    }

    async fn set(&self, key: &str, value: &str, ttl: Duration) -> Result<(), CacheError> {
        self.inner
            .insert(
                key.to_string(),
                Entry {
                    value: value.to_string(),
                    expires_at: Instant::now() + ttl,
                },
            )
            .await;
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), CacheError> {
        self.inner.invalidate(key).await;
        Ok(())
    }

    fn name(&self) -> &'static str {
        "memory"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn set_get_delete_roundtrip() {
        let c = MemoryCache::new();
        c.set("k", "v", Duration::from_secs(60)).await.unwrap();
        assert_eq!(c.get("k").await.as_deref(), Some("v"));
        c.delete("k").await.unwrap();
        assert!(c.get("k").await.is_none());
    }

    #[tokio::test]
    async fn ttl_expires() {
        let c = MemoryCache::new();
        c.set("k", "v", Duration::from_millis(30)).await.unwrap();
        assert_eq!(c.get("k").await.as_deref(), Some("v"));
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(c.get("k").await.is_none(), "过期条目应不可见");
    }

    #[tokio::test]
    async fn overwrite_updates_value() {
        let c = MemoryCache::new();
        c.set("k", "1", Duration::from_secs(60)).await.unwrap();
        c.set("k", "2", Duration::from_secs(60)).await.unwrap();
        assert_eq!(c.get("k").await.as_deref(), Some("2"));
    }
}
