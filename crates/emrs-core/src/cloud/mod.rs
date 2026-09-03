//! 网盘 driver：CloudDriver trait + DriverRegistry 骨架。
//!
//! 当前仅保留 http 直链（通用 302），其余驱动已移除。
//! 内置驱动实现与默认注册表构造（`build_registry`）在 emrs-infra `cloud` 模块；
//! 本模块只保留 trait / 注册表骨架，供未来接入扩展。

use std::sync::Arc;

use async_trait::async_trait;

/// 网盘引用（media_source 行解析结果）。
#[derive(Debug, Clone)]
pub struct CloudRef {
    pub path_type: String,
    pub path_url: String,
}

/// 直链解析结果。
#[derive(Debug, Clone)]
pub enum ResolvedSource {
    /// 可直接 302 的 URL。
    Direct(String),
}

/// 网盘驱动错误。
#[derive(Debug, thiserror::Error)]
pub enum CloudError {
    #[error("driver 不支持此操作: {0}")]
    Unsupported(&'static str),
    #[error("上游错误: {status}")]
    Upstream { status: u16 },
    #[error("内部错误: {0}")]
    Internal(#[from] anyhow::Error),
}

/// 网盘驱动 trait。
#[async_trait]
pub trait CloudDriver: Send + Sync {
    fn kind(&self) -> &'static str;
    /// 解析直链。
    async fn resolve_direct(&self, r: &CloudRef) -> Result<Option<ResolvedSource>, CloudError>;
}

/// 驱动注册表。
pub struct DriverRegistry {
    drivers: Vec<Arc<dyn CloudDriver>>,
}

impl Default for DriverRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl DriverRegistry {
    /// 空注册表（driver 由调用方经 [`Self::register`] 注入；
    /// 内置驱动的默认注册见 emrs-infra `cloud::build_registry`）。
    pub fn new() -> Self {
        Self {
            drivers: Vec::new(),
        }
    }

    pub fn register(&mut self, driver: Arc<dyn CloudDriver>) {
        self.drivers.push(driver);
    }

    /// 按 kind 查找 driver。
    pub fn find(&self, kind: &str) -> Option<&Arc<dyn CloudDriver>> {
        self.drivers.iter().find(|d| d.kind() == kind)
    }

    pub fn all(&self) -> &[Arc<dyn CloudDriver>] {
        &self.drivers
    }

    /// 解析直链：遍历所有 driver 匹配。
    pub async fn resolve(&self, r: &CloudRef) -> Result<Option<ResolvedSource>, CloudError> {
        let kind = r.path_type.as_str();
        if let Some(driver) = self.find(kind) {
            driver.resolve_direct(r).await
        } else {
            // fallback：未知 type 但 path_url 是 http 的，用 http driver
            if r.path_url.starts_with("http://") || r.path_url.starts_with("https://") {
                self.find("http").unwrap().resolve_direct(r).await
            } else {
                Err(CloudError::Unsupported("unknown driver"))
            }
        }
    }
}
