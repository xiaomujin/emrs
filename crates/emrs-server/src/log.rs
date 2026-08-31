//! 日志初始化：stdout（DEBUG 级别，带颜色）+ 文件（INFO 级别，每日滚动）。
//!
//! 参考 sp_web 项目的日志配置方式。

use time::macros::format_description;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::time::LocalTime;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// 初始化日志系统。
///
/// 返回的 `WorkerGuard` **必须**保持在 main() 函数中存活，
/// 否则日志文件输出会静默丢失。
pub fn init_log() -> WorkerGuard {
    let file_appender = tracing_appender::rolling::daily("./logs", "tracing.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    let timer = LocalTime::new(format_description!(
        "[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:3] +[offset_hour]:[offset_minute]"
    ));
    let format = tracing_subscriber::fmt::format()
        .with_level(true)
        .with_target(true)
        .with_thread_names(true)
        .with_line_number(true)
        .with_timer(timer);

    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stdout)
        .with_ansi(true)
        .event_format(format.clone())
        .with_filter(LevelFilter::INFO);

    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false)
        .event_format(format)
        .with_filter(LevelFilter::INFO);

    tracing_subscriber::registry()
        .with(stdout_layer)
        .with(file_layer)
        .init();

    tracing::debug!("日志系统初始化完成");
    _guard
}
