//! Person 相关成型：`PersonRow` / `PersonBrief` → Emby JSON。
//!
//! - [`person_to_json`]：Person 详情（`/Users/{uid}/Items/p-{id}`）
//! - [`person_item_dto`]：`PersonBrief` → `People` 数组元素，供 item/latest/list 复用

use serde_json::json;

use emby_proto::{
    ImageTagsDto, NameIdDto, NameIdTypeDto, PersonDetailDto, PersonItemDto, image_tag, person_id,
};
use emrs_infra::stores::taxonomy_store::{PersonBrief, PersonRow};

/// Emby Person 详情 DTO（`/Users/{uid}/Items/p-{id}`）。
///
/// flatten [`NameIdTypeDto`]（Name/Id/Type）。`primary_image_id`（头像图片行 id）
/// 存在时 `ImageTags.Primary` = `img-{图片行 id}`；`PremiereDate`/`ProductionYear`
/// 仅 birthday 存在时发（`ProductionYear` 取 birthday 前 4 位 parse，失败→0）；
/// `Overview` 仅 description 存在时发。
/// PersonRow → Emby Person 详情 DTO（DTO 定义在 emby-proto，裁定 C11）。
///
/// Id 用 `p-{id}` 前缀，与 `attach_taxonomy` 的 People 数组、`/Persons` 列表、
/// `/Items/{id}/Images/p-{id}` 图片路由保持一致。`primary_image_id` 为人员头像
/// 图片行 id（`item_image.parent_type='people'`，无则 None）；tag 标识图片本身。
pub fn person_to_json(
    server_id: &str,
    person: &PersonRow,
    primary_image_id: Option<i64>,
) -> PersonDetailDto {
    let id = person_id(person.id);

    let image_tags = match primary_image_id {
        Some(img_id) => ImageTagsDto {
            primary: Some(image_tag(img_id)),
            ..Default::default()
        },
        None => ImageTagsDto::default(),
    };

    let mut provider_ids = serde_json::Map::new();
    if let Some(t) = person.tmdb_id.as_deref().filter(|s| !s.is_empty()) {
        provider_ids.insert("Tmdb".into(), json!(t));
    }

    let (premiere_date, production_year) = match &person.birthday {
        Some(birthday) => {
            let year = birthday.get(0..4).map(|y| y.parse::<i64>().unwrap_or(0));
            (Some(birthday.clone()), year)
        }
        None => (None, None),
    };

    PersonDetailDto {
        name_id_type: NameIdTypeDto {
            name_id: NameIdDto {
                name: person.name.clone(),
                id,
            },
            item_type: "Person".into(),
        },
        server_id: server_id.to_string(),
        production_locations: Vec::new(),
        provider_ids,
        image_tags,
        backdrop_image_tags: Vec::new(),
        primary_image_aspect_ratio: 0.666667,
        date_created: person.created_at.clone(),
        date_modified: person.updated_at.clone(),
        external_urls: Vec::new(),
        premiere_date,
        production_year,
        overview: person.description.clone(),
    }
}

/// `PersonBrief` → `People` 数组元素（Name/Id/Role/Type + 可选 Character/PrimaryImageTag）。
/// 供 `item_to_json` / `LatestItemJson` 复用，两处 taxonomy 折入同构。
/// `PersonItemDto` 定义在 emby-proto（裁定 C11）；构造依赖 infra 的 `PersonBrief`，留 server。
pub(crate) fn person_item_dto(p: &PersonBrief) -> PersonItemDto {
    PersonItemDto {
        name_id: NameIdDto {
            name: p.name.clone(),
            id: person_id(p.id),
        },
        role: p.role.clone(),
        item_type: "Person".into(),
        character: p.character_name.clone(),
        primary_image_tag: p.primary_image_id.map(image_tag),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emrs_infra::stores::taxonomy_store::PersonRow;

    /// PersonDetailDto 形状：primary_image_id → `ImageTags.Primary`（`img-{图片行 id}`）；
    /// birthday → `PremiereDate` + `ProductionYear`（前 4 位 parse）；description → `Overview`；
    /// tmdb → `ProviderIds.Tmdb`；无则省略（保旧 `json!` 形状）。
    #[test]
    fn person_detail_dto_shape() {
        let person = PersonRow {
            id: 7,
            tmdb_id: Some("12345".into()),
            name: "演员甲".into(),
            original_name: None,
            gender: 0,
            description: Some("简介".into()),
            birthday: Some("1990-05-01".into()),
            deathday: None,
            created_at: "2026-01-01T00:00:00.0000000Z".into(),
            updated_at: "2026-01-02T00:00:00.0000000Z".into(),
        };
        // 有头像图片行 id=77 → tag img-77
        let v = serde_json::to_value(person_to_json("srv", &person, Some(77))).unwrap();
        assert_eq!(v["Type"], "Person");
        assert_eq!(v["Name"], "演员甲");
        assert_eq!(v["Id"], "p-7");
        assert_eq!(v["ServerId"], "srv");
        assert_eq!(v["ProviderIds"]["Tmdb"], "12345");
        assert_eq!(v["ImageTags"], json!({ "Primary": "img-77" }));
        assert_eq!(v["BackdropImageTags"], json!([]));
        assert_eq!(v["ProductionLocations"], json!([]));
        assert_eq!(v["ExternalUrls"], json!([]));
        assert_eq!(v["PrimaryImageAspectRatio"], 0.666667);
        assert_eq!(v["DateCreated"], "2026-01-01T00:00:00.0000000Z");
        assert_eq!(v["DateModified"], "2026-01-02T00:00:00.0000000Z");
        assert_eq!(v["PremiereDate"], "1990-05-01");
        assert_eq!(v["ProductionYear"], 1990);
        assert_eq!(v["Overview"], "简介");

        // 无头像 → ImageTags={}
        let v2 = serde_json::to_value(person_to_json("srv", &person, None)).unwrap();
        assert_eq!(v2["ImageTags"], json!({}));

        // 无 birthday/description → PremiereDate/ProductionYear/Overview 省略
        let mut p2 = person.clone();
        p2.birthday = None;
        p2.description = None;
        let v3 = serde_json::to_value(person_to_json("srv", &p2, None)).unwrap();
        assert!(!v3.as_object().unwrap().contains_key("PremiereDate"));
        assert!(!v3.as_object().unwrap().contains_key("ProductionYear"));
        assert!(!v3.as_object().unwrap().contains_key("Overview"));

        // 无 tmdb_id → ProviderIds={}
        let mut p3 = person.clone();
        p3.tmdb_id = None;
        let v4 = serde_json::to_value(person_to_json("srv", &p3, None)).unwrap();
        assert_eq!(v4["ProviderIds"], json!({}));
    }
}