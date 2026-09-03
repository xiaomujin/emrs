//! 缓存抽象：`Cache` trait + 错误类型。
//!
//! 具体实现（memory(moka) / redis / valkey、双层门面）在 emrs-infra `cache` 模块。
//! Redis 与 Valkey 复用同一 RESP 客户端，仅 `name()` 区分用于日志。

use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;

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
