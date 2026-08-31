//! Redis / Valkey 缓存实现（共用 RESP 协议，ConnectionManager 自动重连）。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use redis::AsyncCommands;
use redis::aio::ConnectionManager;

use super::{Cache, CacheError};

/// 共享连接：工厂在 tokio 运行时内同步建连，之后 clone 使用。
pub struct SharedConnection {
    conn: Arc<tokio::sync::Mutex<ConnectionManager>>,
}

impl SharedConnection {
    /// 在指定 tokio 运行时内建立 ConnectionManager（阻塞等待握手完成）。
    pub fn connect(url: &str, runtime: tokio::runtime::Handle) -> Result<Self, String> {
        let url = url.to_string();
        let conn = runtime
            .block_on({
                let url = url.clone();
                async move {
                    let client = redis::Client::open(url.as_str()).map_err(|e| e.to_string())?;
                    ConnectionManager::new(client)
                        .await
                        .map_err(|e| e.to_string())
                }
            })
            .map_err(|e| format!("连接 {url} 失败: {e}"))?;
        Ok(Self {
            conn: Arc::new(tokio::sync::Mutex::new(conn)),
        })
    }

    async fn cmd(&self) -> tokio::sync::MutexGuard<'_, ConnectionManager> {
        self.conn.lock().await
    }
}

pub struct RedisCache {
    conn: SharedConnection,
}

pub struct ValkeyCache {
    conn: SharedConnection,
}

impl RedisCache {
    pub fn new(conn: SharedConnection) -> Self {
        Self { conn }
    }
}

impl ValkeyCache {
    pub fn new(conn: SharedConnection) -> Self {
        Self { conn }
    }
}

macro_rules! impl_cache {
    ($ty:ty, $name:literal) => {
        #[async_trait]
        impl Cache for $ty {
            async fn get(&self, key: &str) -> Option<String> {
                let mut conn = self.conn.cmd().await;
                conn.get(key).await.ok()
            }

            async fn set(&self, key: &str, value: &str, ttl: Duration) -> Result<(), CacheError> {
                let secs = ttl.as_secs().max(1);
                let mut conn = self.conn.cmd().await;
                conn.set_ex(key, value, secs)
                    .await
                    .map_err(|e| CacheError::Backend(e.to_string()))
            }

            async fn delete(&self, key: &str) -> Result<(), CacheError> {
                let mut conn = self.conn.cmd().await;
                conn.del(key)
                    .await
                    .map_err(|e| CacheError::Backend(e.to_string()))
            }

            fn name(&self) -> &'static str {
                $name
            }
        }
    };
}

impl_cache!(RedisCache, "redis");
impl_cache!(ValkeyCache, "valkey");

#[cfg(test)]
mod tests {
    use super::*;

    /// 环境变量存在时才跑（EMRS_TEST_REDIS_URL=redis://127.0.0.1:6379）。
    fn live() -> Option<(SharedConnection, String)> {
        let url = std::env::var("EMRS_TEST_REDIS_URL").ok()?;
        let rt = tokio::runtime::Runtime::new().ok()?;
        let conn = SharedConnection::connect(&url, rt.handle().clone()).ok()?;
        Some((conn, url))
    }

    #[tokio::test]
    async fn redis_roundtrip() {
        let Some((conn, url)) = live() else {
            eprintln!("跳过：未设置 EMRS_TEST_REDIS_URL");
            return;
        };
        let c = RedisCache::new(conn);
        let key = format!("emrs:cache-test:{}", std::process::id());
        c.set(&key, "v1", Duration::from_secs(30)).await.unwrap();
        assert_eq!(c.get(&key).await.as_deref(), Some("v1"));
        c.delete(&key).await.unwrap();
        assert!(c.get(&key).await.is_none());
        eprintln!("redis 冒烟通过: {url}");
    }
}
