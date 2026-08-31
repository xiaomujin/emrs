//! Emby 协议工具：时间格式化（7 位小数秒）、ItemId 解析、DTO 成型。
//!
//! - 时间 / ItemId：本文件
//! - [`dto`]：`ItemRow` / `MediaSourceRow` → Emby JSON（从 HTTP 层下沉到 core）
//! - [`base`]：响应 DTO 共享 base（`BaseItemDto` / `ImageTagsDto`，`#[serde(flatten)]` 注入）
//! - [`system`]：`/System/Info` 系列响应
//! - [`session`]：PlaybackInfo / 会话 / 登录响应
//! - [`user`]：`/Users` 系列响应（UserDto）

mod base;
mod dto;
mod id;
mod latest;
mod list_dto;
mod session;
mod system;
mod user;
mod views;
pub use base::*;
pub use dto::*;
pub use id::*;
pub use latest::*;
pub use list_dto::*;
pub use session::*;
pub use system::*;
pub use user::*;
pub use views::*;

use chrono::{SecondsFormat, Utc};

/// Emby 零值时间。
pub const DEFAULT_TIME: &str = "0001-01-01T00:00:00.0000000Z";

/// Emby 期望的 ISO8601：UTC + 7 位小数秒 + Z。
pub fn format_time(t: chrono::DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Nanos, true)
        .replacen(".000000000", ".0000000", 1)
}

pub fn format_time_now() -> String {
    format_time(Utc::now())
}

/// 解析纯数字 ItemId：返回正整数 id。
/// ItemId 不再带类型前缀，类型由路由层按 `item.type` 列查回。
pub fn parse_item_id(value: &str) -> Option<i64> {
    value.trim().parse::<i64>().ok().filter(|&id| id > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_format_7_frac_digits() {
        use chrono::TimeZone;
        let t = Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap();
        assert_eq!(format_time(t), "2026-01-02T03:04:05.0000000Z");
    }

    #[test]
    fn parse_item_id_numeric() {
        assert_eq!(parse_item_id("42"), Some(42));
        assert_eq!(parse_item_id("  7 "), Some(7));
        assert_eq!(parse_item_id("0"), None);
        assert_eq!(parse_item_id("-1"), None);
        assert_eq!(parse_item_id("x-1"), None);
        assert_eq!(parse_item_id(""), None);
    }
}
