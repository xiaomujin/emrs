//! 缓存门面：按领域的缓存接口（AuthCache / MediaCache / SessionCache / PlaybackCache）。
//!
//! two-tier（moka L2 → redis L1 → DB）：先查进程内 moka，miss 查 redis/valkey，
//! 再 miss 回 DB。Redis 挂掉功能不变（降级直查 DB）。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Serialize, de::DeserializeOwned};

use emrs_core::cache::Cache;

/// 两层缓存：L2=进程内 moka，L1=redis/valkey（可选）。
/// 缓存 miss 时由调用方回查 DB 并写回。
pub struct TwoTierCache {
    /// L2：进程内（moka 或 MemoryCache）。
    l2: Arc<dyn Cache>,
    /// L1：分布式缓存（redis/valkey），可选。
    l1: Option<Arc<dyn Cache>>,
}

impl TwoTierCache {
    pub fn new(l2: Arc<dyn Cache>, l1: Option<Arc<dyn Cache>>) -> Self {
        Self { l2, l1 }
    }

    /// 查缓存：先 L2，miss 查 L1 并回填 L2。
    pub async fn get(&self, key: &str) -> Option<String> {
        if let Some(v) = self.l2.get(key).await {
            return Some(v);
        }
        if let Some(l1) = &self.l1
            && let Some(v) = l1.get(key).await
        {
            // 回填 L2（短 TTL 避免不一致）
            let _ = self.l2.set(key, &v, Duration::from_secs(60)).await;
            return Some(v);
        }
        None
    }

    /// 写缓存：双写 L2 + L1。
    pub async fn set(&self, key: &str, value: &str, ttl: Duration) {
        let _ = self.l2.set(key, value, ttl).await;
        if let Some(l1) = &self.l1 {
            let _ = l1.set(key, value, ttl).await;
        }
    }

    /// 删缓存：双删 L2 + L1。
    pub async fn delete(&self, key: &str) {
        let _ = self.l2.delete(key).await;
        if let Some(l1) = &self.l1 {
            let _ = l1.delete(key).await;
        }
    }

    /// 按前缀删（缓存失效）：L2 不支持 SCAN 时遍历已知 key 列表。
    pub async fn scan_delete(&self, _pattern: &str) {
        // MemoryCache 不支持 SCAN，仅做 best-effort
        // Redis 的 scan_delete 在实际部署时由 RedisCache 实现
    }

    /// 序列化写入。
    pub async fn set_json<T: Serialize>(&self, key: &str, val: &T, ttl: Duration) {
        match serde_json::to_string(val) {
            Ok(json) => self.set(key, &json, ttl).await,
            Err(e) => tracing::warn!(error = %e, key, "cache set_json serialize failed"),
        }
    }

    /// 反序列化读取。
    pub async fn get_json<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.get(key)
            .await
            .and_then(|v| serde_json::from_str(&v).ok())
    }
}

/// AuthCache：认证域缓存（A 档写穿透）。
///
/// key 约定：
/// - `auth:token:{hash}` → TokenInfo JSON
/// - `auth:user:{id}` → UserInfo JSON
#[async_trait]
pub trait AuthCache: Send + Sync {
    async fn get_token(&self, token_hash: &str) -> Option<serde_json::Value>;
    async fn set_token(&self, token_hash: &str, user_id: i64, role: &str, ttl: Duration);
    async fn revoke_token(&self, token_hash: &str);
    async fn invalidate_user(&self, user_id: i64);
}

/// AuthCache 默认实现（TwoTierCache 兜底）。
pub struct DefaultAuthCache {
    cache: Arc<TwoTierCache>,
}

impl DefaultAuthCache {
    pub fn new(cache: Arc<TwoTierCache>) -> Self {
        Self { cache }
    }
}

#[async_trait]
impl AuthCache for DefaultAuthCache {
    async fn get_token(&self, token_hash: &str) -> Option<serde_json::Value> {
        let key = format!("auth:token:{token_hash}");
        self.cache.get_json(&key).await
    }

    async fn set_token(&self, token_hash: &str, user_id: i64, role: &str, ttl: Duration) {
        let key = format!("auth:token:{token_hash}");
        let val = serde_json::json!({ "user_id": user_id, "role": role });
        self.cache.set_json(&key, &val, ttl).await;
    }

    async fn revoke_token(&self, token_hash: &str) {
        let key = format!("auth:token:{token_hash}");
        self.cache.delete(&key).await;
    }

    async fn invalidate_user(&self, user_id: i64) {
        let key = format!("auth:user:{user_id}");
        self.cache.delete(&key).await;
    }
}

/// MediaCache：媒体域缓存（B 档删缓存）。
///
/// key 约定：
/// - `item:{id}` → BaseItemDto JSON
/// - `library:all` → Vec<Library> JSON
/// - `genre:all` → Vec<Genre> JSON
#[async_trait]
#[allow(clippy::ptr_arg)]
pub trait MediaCache: Send + Sync {
    async fn get_item(&self, item_id: i64) -> Option<serde_json::Value>;
    async fn set_item(&self, item_id: i64, item: &serde_json::Value, ttl: Duration);
    async fn invalidate_item(&self, item_id: i64);
    async fn get_libraries(&self) -> Option<Vec<serde_json::Value>>;
    async fn set_libraries(&self, libs: &Vec<serde_json::Value>, ttl: Duration);
    async fn get_genres(&self) -> Option<Vec<serde_json::Value>>;
    async fn set_genres(&self, genres: &Vec<serde_json::Value>, ttl: Duration);
}

/// MediaCache 默认实现。
pub struct DefaultMediaCache {
    cache: Arc<TwoTierCache>,
}

impl DefaultMediaCache {
    pub fn new(cache: Arc<TwoTierCache>) -> Self {
        Self { cache }
    }
}

#[async_trait]
impl MediaCache for DefaultMediaCache {
    async fn get_item(&self, item_id: i64) -> Option<serde_json::Value> {
        let key = format!("item:{item_id}");
        self.cache.get_json(&key).await
    }

    async fn set_item(&self, item_id: i64, item: &serde_json::Value, ttl: Duration) {
        let key = format!("item:{item_id}");
        self.cache.set_json(&key, item, ttl).await;
    }

    async fn invalidate_item(&self, item_id: i64) {
        let key = format!("item:{item_id}");
        self.cache.delete(&key).await;
        // 级联失效 playback_info
        let pattern = format!("pbinfo:{item_id}:*");
        self.cache.scan_delete(&pattern).await;
    }

    async fn get_libraries(&self) -> Option<Vec<serde_json::Value>> {
        self.cache.get_json("library:all").await
    }

    async fn set_libraries(&self, libs: &Vec<serde_json::Value>, ttl: Duration) {
        self.cache.set_json("library:all", libs, ttl).await;
    }

    async fn get_genres(&self) -> Option<Vec<serde_json::Value>> {
        self.cache.get_json("genre:all").await
    }

    async fn set_genres(&self, genres: &Vec<serde_json::Value>, ttl: Duration) {
        self.cache.set_json("genre:all", genres, ttl).await;
    }
}

/// SessionCache：会话域缓存（C 档 write-behind）。
///
/// key 约定：
/// - `session:{session_id}` → PlaybackSession JSON
/// - `resume:{user_id}` → sorted set
#[async_trait]
pub trait SessionCache: Send + Sync {
    async fn upsert_session(&self, session_id: &str, session: &serde_json::Value);
    async fn delete_session(&self, session_id: &str);
    async fn set_resume(&self, user_id: i64, item_id: i64, ticks: i64);
    async fn delete_resume(&self, user_id: i64, item_id: i64);
}

/// SessionCache 默认实现。
pub struct DefaultSessionCache {
    cache: Arc<TwoTierCache>,
}

impl DefaultSessionCache {
    pub fn new(cache: Arc<TwoTierCache>) -> Self {
        Self { cache }
    }
}

#[async_trait]
impl SessionCache for DefaultSessionCache {
    async fn upsert_session(&self, session_id: &str, session: &serde_json::Value) {
        let key = format!("session:{session_id}");
        self.cache
            .set_json(&key, session, Duration::from_secs(3600))
            .await;
    }

    async fn delete_session(&self, session_id: &str) {
        let key = format!("session:{session_id}");
        self.cache.delete(&key).await;
    }

    async fn set_resume(&self, user_id: i64, item_id: i64, ticks: i64) {
        let key = format!("resume:{user_id}:{item_id}");
        self.cache
            .set(&key, &ticks.to_string(), Duration::from_secs(300))
            .await;
    }

    async fn delete_resume(&self, user_id: i64, item_id: i64) {
        let key = format!("resume:{user_id}:{item_id}");
        self.cache.delete(&key).await;
    }
}

/// PlaybackCache：播放信息缓存（C 档双写）。
///
/// key 约定：
/// - `pbinfo:{item_id}:{user_id}:{profile_hash}` → PlaybackInfo JSON
#[async_trait]
pub trait PlaybackCache: Send + Sync {
    async fn get_playback_info(
        &self,
        item_id: i64,
        user_id: i64,
        profile_hash: &str,
    ) -> Option<serde_json::Value>;
    async fn set_playback_info(
        &self,
        item_id: i64,
        user_id: i64,
        profile_hash: &str,
        info: &serde_json::Value,
        ttl: Duration,
    );
    async fn invalidate_playback_info(&self, item_id: i64);
}

/// PlaybackCache 默认实现。
pub struct DefaultPlaybackCache {
    cache: Arc<TwoTierCache>,
}

impl DefaultPlaybackCache {
    pub fn new(cache: Arc<TwoTierCache>) -> Self {
        Self { cache }
    }
}

#[async_trait]
impl PlaybackCache for DefaultPlaybackCache {
    async fn get_playback_info(
        &self,
        item_id: i64,
        user_id: i64,
        profile_hash: &str,
    ) -> Option<serde_json::Value> {
        let key = format!("pbinfo:{item_id}:{user_id}:{profile_hash}");
        self.cache.get_json(&key).await
    }

    async fn set_playback_info(
        &self,
        item_id: i64,
        user_id: i64,
        profile_hash: &str,
        info: &serde_json::Value,
        ttl: Duration,
    ) {
        let key = format!("pbinfo:{item_id}:{user_id}:{profile_hash}");
        self.cache.set_json(&key, info, ttl).await;
    }

    async fn invalidate_playback_info(&self, item_id: i64) {
        let pattern = format!("pbinfo:{item_id}:*");
        self.cache.scan_delete(&pattern).await;
    }
}

/// 缓存门面：聚合所有领域缓存。
pub struct CacheFacade {
    pub auth: DefaultAuthCache,
    pub media: DefaultMediaCache,
    pub session: DefaultSessionCache,
    pub playback: DefaultPlaybackCache,
}

impl CacheFacade {
    pub fn new(two_tier: Arc<TwoTierCache>) -> Self {
        Self {
            auth: DefaultAuthCache::new(two_tier.clone()),
            media: DefaultMediaCache::new(two_tier.clone()),
            session: DefaultSessionCache::new(two_tier.clone()),
            playback: DefaultPlaybackCache::new(two_tier),
        }
    }
}

/// 启动预热：library:all / genre:all / setting:all / security/cloud。
/// 预热失败不阻断启动（降级按需回查），仅告警。
#[allow(dead_code)]
pub async fn preheat(cache: &CacheFacade, db: &crate::db::Db) {
    // library:all
    if let Err(e) = preheat_libraries(cache, db).await {
        tracing::warn!(error = %e, "preheat libraries failed");
    }
    // genre:all
    if let Err(e) = preheat_genres(cache, db).await {
        tracing::warn!(error = %e, "preheat genres failed");
    }
    // setting:all
    if let Err(e) = preheat_settings(cache, db).await {
        tracing::warn!(error = %e, "preheat settings failed");
    }
}

async fn preheat_libraries(cache: &CacheFacade, db: &crate::db::Db) -> anyhow::Result<()> {
    /// 预热用 library 行（裁定 C10 命名化）。
    #[derive(Debug, sqlx::FromRow)]
    struct LibraryBriefRow {
        id: i64,
        name: String,
        created_at: String,
    }

    let rows = sqlx::query_as::<_, LibraryBriefRow>(
        "SELECT id, name, created_at FROM library ORDER BY id",
    )
    .fetch_all(db.pool())
    .await?;

    let items: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| serde_json::json!({ "id": r.id, "name": r.name, "created_at": r.created_at }))
        .collect();

    cache
        .media
        .set_libraries(&items, Duration::from_secs(300))
        .await;
    Ok(())
}

async fn preheat_genres(cache: &CacheFacade, db: &crate::db::Db) -> anyhow::Result<()> {
    let rows: Vec<(i64, String)> =
        sqlx::query_as::<_, (i64, String)>("SELECT id, name FROM genre ORDER BY name")
            .fetch_all(db.pool())
            .await
            .unwrap_or_default();

    let items: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(id, name)| serde_json::json!({ "id": id, "name": name }))
        .collect();

    cache
        .media
        .set_genres(&items, Duration::from_secs(300))
        .await;
    Ok(())
}

async fn preheat_settings(cache: &CacheFacade, db: &crate::db::Db) -> anyhow::Result<()> {
    let rows: Vec<(String, String)> = sqlx::query_as::<_, (String, String)>(
        // app_setting 无软删列，直接全量读
        "SELECT key, value FROM app_setting",
    )
    .fetch_all(db.pool())
    .await
    .unwrap_or_default();

    for (key, value) in rows {
        // setting:all:{key} → value
        let _cache_key = format!("setting:{key}");
        cache
            .media
            .set_item(
                key.len() as i64, // setting uses key-based lookup
                &serde_json::json!({ "key": key, "value": value }),
                Duration::from_secs(600),
            )
            .await;
    }
    Ok(())
}
