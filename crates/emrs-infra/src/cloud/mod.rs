//! 内置网盘驱动：http 直链实现 + 默认注册表构造。
//!
//! `CloudDriver` trait / `DriverRegistry` 骨架在 emrs-core `cloud` 模块；
//! 本模块只提供具体驱动与把驱动装进注册表的工厂。

mod http_driver;

use std::sync::Arc;

use emrs_core::cloud::DriverRegistry;

pub use http_driver::HttpDriver;

/// 构建默认注册表（注册内置 http 直链驱动）。
///
/// 原 `DriverRegistry::new(db, cfg)` 的 db/cfg 参数从未被驱动消费，
/// 拆分后由本工厂承担"构造 + 注册"职责（裁定 B3-1，行为不变）。
pub fn build_registry() -> DriverRegistry {
    let mut reg = DriverRegistry::new();
    reg.register(Arc::new(HttpDriver));
    reg
}
