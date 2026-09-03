//! `PlaybackRouter::resolve_direct` 直链解析 + 缓存行为测试。
//!
//! 原 emrs-core `playback/mod.rs` 内联测试随实现依赖（MemoryCache / Db /
//! DriverRegistry 构造 + HttpDriver 注册）迁至本文件。

use std::sync::Arc;

use emrs_core::cache::Cache;
use emrs_core::cloud::CloudRef;
use emrs_core::playback::{PlayRequest, PlaybackRouter};
use emrs_infra::cache::MemoryCache;
use emrs_infra::cloud::build_registry;

fn cloud_ref(path_type: &str, path_url: &str) -> CloudRef {
    CloudRef {
        path_type: path_type.to_string(),
        path_url: path_url.to_string(),
    }
}

#[tokio::test]
async fn direct_url_cached() {
    let cache: Arc<dyn Cache> = Arc::new(MemoryCache::new());
    let router = PlaybackRouter::new(Arc::new(build_registry()), cache.clone());
    let req = PlayRequest {
        cloud_ref: cloud_ref("url", "https://cdn.example.com/a.mp4"),
        user_id: 1,
        device_id: None,
    };

    let first = router.resolve_direct(&req).await.unwrap().unwrap();
    assert_eq!(first, "https://cdn.example.com/a.mp4");

    // 缓存写入验证
    let key = format!(
        "direct:{}:{}",
        req.cloud_ref.path_type, req.cloud_ref.path_url
    );
    assert_eq!(cache.get(&key).await.as_deref(), Some(first.as_str()));

    // 再次解析：key 已在缓存（driver 无感知）
    let second = router.resolve_direct(&req).await.unwrap().unwrap();
    assert_eq!(second, first);
}
