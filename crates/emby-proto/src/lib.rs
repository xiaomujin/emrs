//! Emby 协议原语 crate：时间格式化（7 位小数秒）、命名空间 ItemId、共享 DTO
//! base 与响应壳。全部为纯 serde 类型与无副作用函数，不依赖本项目任何其它 crate。
//!
//! - [`format_time`] / [`format_time_now`] / [`DEFAULT_TIME`]：Emby 时间格式（本文件）
//! - [`parse_item_id`] / [`parse_id`] / [`IdKind`] 等：ID 编解码，见 [`id`]
//! - [`BaseItemDto`] / [`ImageTagsDto`] 等：多接口复用的 flatten 字段组，见 [`base`]
//! - [`ViewsUserData`]：统一 UserData 响应类型（`From<DB 行>` 转换在业务侧实现）
//! - [`ItemsResponse`] / [`ItemsCounts`]：列表端点响应壳
//! - [`SystemInfoDto`] 等：`/System/Info` 系列响应，见 [`system`]

mod base;
mod id;
mod person;
mod system;
mod userdata;
mod wrappers;

pub use base::*;
pub use id::*;
pub use person::*;
pub use system::*;
pub use userdata::*;
pub use wrappers::*;

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
