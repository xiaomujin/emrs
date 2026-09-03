//! HTTP 通用直链 driver：直接返回 path_url 作为 302 目标。

use async_trait::async_trait;

use emrs_core::cloud::{CloudDriver, CloudError, CloudRef, ResolvedSource};

pub struct HttpDriver;

#[async_trait]
impl CloudDriver for HttpDriver {
    fn kind(&self) -> &'static str {
        "http"
    }

    async fn resolve_direct(&self, r: &CloudRef) -> Result<Option<ResolvedSource>, CloudError> {
        Ok(Some(ResolvedSource::Direct(r.path_url.clone())))
    }
}
