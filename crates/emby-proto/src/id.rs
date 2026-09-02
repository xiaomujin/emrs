//! ID 命名空间前缀：library / item / people / genre / studio 等数字 ID 加类型前缀。
//!
//! Emby 客户端把 `library.id` 与 `item.id` 都当裸数字字符串携带，二者数值撞车时
//! `ParentId` 无法判型（库 2 vs item 2）。给返回给客户端的 ID 加类型前缀
//! （`l-2` / `i-42` / `p-24` / `g-5` / `s-7`），服务端凭前缀直接分派。
//! `img-{id}`（item_image 行）作 egress 图片标记（ImageTags 值），也用于 ingress 按 tag 直查图片。
//!
//! 前缀格式带连字符，与既有 `p-24` / `session-42` 一致。DB 不变（id 仍存裸 i64，
//! 前缀纯传输层）。裸数字不再兼容——egress 始终发带前缀，ingress 裸数字一律视为非法。

use crate::parse_item_id;

/// ID 类型（命名空间）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdKind {
    /// item（movie/series/season/episode），裸数字兼容归此项。
    Item,
    /// library（媒体库）。
    Library,
    /// people（演职员）。
    People,
    /// genre（类型）。
    Genre,
    /// studio（工作室）。
    Studio,
    /// item_image 行（图片标记 `img-{id}`）。
    Image,
}

// ---------------------------------------------------------------------------
// 格式化（egress：数字 id → 带前缀字符串）
// ---------------------------------------------------------------------------

/// item id → `i-{id}`。
pub fn item_id(id: i64) -> String {
    format!("i-{id}")
}

/// library id → `l-{id}`。
pub fn library_id(id: i64) -> String {
    format!("l-{id}")
}

/// people id → `p-{id}`（与既有格式一致）。
pub fn person_id(id: i64) -> String {
    format!("p-{id}")
}

/// genre id → `g-{id}`。
pub fn genre_id(id: i64) -> String {
    format!("g-{id}")
}

/// studio id → `s-{id}`。
pub fn studio_id(id: i64) -> String {
    format!("s-{id}")
}

/// item_image 行 id → `img-{id}`（图片唯一标记，ImageTags / BackdropImageTags 值）。
pub fn image_tag(id: i64) -> String {
    format!("img-{id}")
}

// ---------------------------------------------------------------------------
// 解析（ingress：客户端字符串 → (kind, 数字 id)）
// ---------------------------------------------------------------------------

/// 统一解析：`{prefix}-{id}`，严格按前缀判型，**裸数字不再兼容**。
///
/// - `i-42` / `l-2` / `p-24` / `g-5` / `s-7` → 对应 kind
/// - `42`（裸数字）→ `None`：egress 始终发带前缀，客户端拿到的都是前缀 id，
///   裸数字一律视为非法，由调用方回 404/空
/// - `0` / `p-0` / `-24` / `p-` / `24-3` / `x-1` / 空 → `None`
///
/// 数值必须为正整数（复用 [`parse_item_id`] 的 `> 0` 校验）。
pub fn parse_id(raw: &str) -> Option<(IdKind, i64)> {
    let t = raw.trim();
    let (prefix, num) = t.split_once('-')?;
    let kind = prefix_to_kind(prefix)?;
    let id = parse_item_id(num)?;
    Some((kind, id))
}

/// 前缀字面量 → kind；未知前缀 → None。
fn prefix_to_kind(prefix: &str) -> Option<IdKind> {
    match prefix {
        "i" => Some(IdKind::Item),
        "l" => Some(IdKind::Library),
        "p" => Some(IdKind::People),
        "g" => Some(IdKind::Genre),
        "s" => Some(IdKind::Studio),
        "img" => Some(IdKind::Image),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_helpers() {
        assert_eq!(item_id(42), "i-42");
        assert_eq!(library_id(2), "l-2");
        assert_eq!(person_id(24), "p-24");
        assert_eq!(genre_id(5), "g-5");
        assert_eq!(studio_id(7), "s-7");
        assert_eq!(image_tag(11), "img-11");
    }

    #[test]
    fn parse_prefixed() {
        assert_eq!(parse_id("i-42"), Some((IdKind::Item, 42)));
        assert_eq!(parse_id("l-2"), Some((IdKind::Library, 2)));
        assert_eq!(parse_id("p-24"), Some((IdKind::People, 24)));
        assert_eq!(parse_id("g-5"), Some((IdKind::Genre, 5)));
        assert_eq!(parse_id("s-7"), Some((IdKind::Studio, 7)));
    }

    #[test]
    fn parse_bare_rejected() {
        // 裸数字不再兼容：egress 始终发带前缀，裸数字一律非法
        assert_eq!(parse_id("42"), None);
        assert_eq!(parse_id("  7 "), None);
    }

    #[test]
    fn parse_trims_whitespace() {
        assert_eq!(parse_id("  i-42  "), Some((IdKind::Item, 42)));
        assert_eq!(parse_id("  p-24\n"), Some((IdKind::People, 24)));
    }

    #[test]
    fn parse_invalid() {
        assert_eq!(parse_id(""), None);
        assert_eq!(parse_id("0"), None, "裸 0 非正整数");
        assert_eq!(parse_id("i-0"), None, "id 必须 > 0");
        assert_eq!(parse_id("-24"), None, "前缀为空");
        assert_eq!(parse_id("p-"), None, "缺数字");
        assert_eq!(parse_id("24-3"), None, "数字前缀非法");
        assert_eq!(parse_id("x-1"), None, "未知前缀");
        assert_eq!(parse_id("y-2023"), None, "year 不在本方案");
        assert_eq!(parse_id("i--1"), None, "负数");
    }
}
