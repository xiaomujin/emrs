//! 扫描唤醒抽象：目录监听（emrs-infra）反向通知导入流水线（emrs-service）的依赖注入点。
//!
//! watcher（infra）入队 `scan_job(pending)` 后经本 trait 立即唤醒 scan 消费循环，
//! service 的 `Pipeline` 实现之，server 装配时注入，避免 infra → service 反向依赖。

/// 扫描唤醒器：立即唤醒 scan 消费循环（缺省依赖轮询间隔自然拾取）。
pub trait ScanWaker: Send + Sync {
    /// 唤醒 scan 消费循环。
    fn wake_scan(&self);
}
