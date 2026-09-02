//! Emby `/Users/{id}/Views` 响应 DTO：媒体库视图（CollectionFolder）成型。
//!
//! 与 `dto.rs`（item → Emby JSON）不同，这里是**类型化**结构体：
//! 库视图不再关联 `ItemRow`，直接由 `LibraryView` 行 + 固定默认值成型。
//! [`ViewsUserData`] 类型定义在 `emby-proto`，其存储层转换是 core 的固有方法
//! [`UserItemData::to_views_user_data`](emrs_core::stores::UserItemData::to_views_user_data)。

use serde::Serialize;

use super::{BaseItemDto, library_id};
use emrs_core::stores::LibraryView;

/// 单个媒体库视图（CollectionFolder）。
///
/// 对齐参考 Emby 输出：库本身不关联 ItemRow，直接由 `library` 行成型；
/// 共有顶层字段（Name/ServerId/Id/Type/IsFolder/DateCreated/UserData/
/// PrimaryImageAspectRatio/ImageTags/BackdropImageTags/ProviderIds）走 [`BaseItemDto`]
/// flatten，固定值（IsFolder/Type/ChildCount/ParentId/宽高比）放在 `Default` 里，
/// 仅覆盖每库变化字段。
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct CollectionFolderView {
    #[serde(flatten)]
    base: BaseItemDto,
    guid: String,
    etag: String,
    date_modified: String,
    can_delete: bool,
    can_download: bool,
    presentation_unique_key: String,
    sort_name: String,
    forced_sort_name: String,
    external_urls: Vec<String>,
    taglines: Vec<String>,
    remote_trailers: Vec<String>,
    parent_id: String,
    child_count: i64,
    display_preferences_id: String,
    collection_type: String,
    locked_fields: Vec<String>,
    lock_data: bool,
}

impl CollectionFolderView {
    /// 由 `LibraryView` 行成型（不再关联 ItemRow）。
    /// 共有字段在 `base`（库专属的 IsFolder/Type/宽高比 由 `Default` 提供），
    /// 仅覆盖每库变化字段。
    pub fn from_library(server_id: &str, lib: &LibraryView) -> Self {
        let mut v = Self::default();
        let id = library_id(lib.id);
        v.base.name = lib.name.clone();
        v.base.server_id = server_id.to_string();
        v.base.id = id.clone();
        v.base.date_created = lib.created_at.clone();
        v.guid = id.clone();
        v.etag = id.clone();
        v.date_modified = lib.updated_at.clone();
        v.presentation_unique_key = id.clone();
        v.sort_name = lib.name.clone();
        v.forced_sort_name = lib.name.clone();
        v.display_preferences_id = id;
        v.collection_type = lib.collection_type.clone();
        v
    }
}

impl Default for CollectionFolderView {
    fn default() -> Self {
        Self {
            base: BaseItemDto {
                is_folder: true,
                item_type: "CollectionFolder".to_string(),
                primary_image_aspect_ratio: 1.777778,
                ..Default::default()
            },
            guid: String::new(),
            etag: String::new(),
            date_modified: String::new(),
            can_delete: false,
            can_download: false,
            presentation_unique_key: String::new(),
            sort_name: String::new(),
            forced_sort_name: String::new(),
            external_urls: Vec::new(),
            taglines: Vec::new(),
            remote_trailers: Vec::new(),
            parent_id: "0".to_string(),
            child_count: 1,
            display_preferences_id: String::new(),
            collection_type: String::new(),
            locked_fields: Vec::new(),
            lock_data: false,
        }
    }
}

/// `GET /Users/{user_id}/Views` 响应壳。
#[derive(Serialize)]
pub struct ViewsResponse {
    #[serde(rename = "Items")]
    pub items: Vec<CollectionFolderView>,
    #[serde(rename = "TotalRecordCount")]
    pub total_record_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_folder_json_matches_reference_shape() {
        let mut v = CollectionFolderView::default();
        v.base.name = "电影".into();
        v.base.server_id = "emya".into();
        v.base.id = "42".into();
        v.base.date_created = "2026-08-21T00:00:00.0000000Z".into();
        v.guid = "42".into();
        v.etag = "42".into();
        v.date_modified = "2026-08-21T00:00:00.0000000Z".into();
        v.presentation_unique_key = "42".into();
        v.sort_name = "电影".into();
        v.forced_sort_name = "电影".into();
        v.display_preferences_id = "42".into();
        v.collection_type = "movies".into();
        let out = serde_json::to_value(&v).unwrap();
        let obj = out.as_object().unwrap();

        // 参考 Views_emos.json + Views_yuchu.json：字段集合取并集，
        // key 大小写（PascalCase）必须与参考完全一致。
        let mut expected: Vec<&str> = vec![
            "Name",
            "ServerId",
            "Id",
            "Guid",
            "Etag",
            "DateCreated",
            "DateModified",
            "CanDelete",
            "CanDownload",
            "PresentationUniqueKey",
            "SortName",
            "ForcedSortName",
            "ExternalUrls",
            "Taglines",
            "RemoteTrailers",
            "ProviderIds",
            "IsFolder",
            "ParentId",
            "Type",
            "UserData",
            "ChildCount",
            "DisplayPreferencesId",
            "PrimaryImageAspectRatio",
            "CollectionType",
            "ImageTags",
            "BackdropImageTags",
            "LockedFields",
            "LockData",
        ];
        expected.sort_unstable();
        let mut actual: Vec<&str> = obj.keys().map(String::as_str).collect();
        actual.sort_unstable();
        assert_eq!(actual, expected, "key 集合/大小写与参考 JSON 不一致");

        assert_eq!(obj["Type"], "CollectionFolder");
        assert_eq!(obj["ChildCount"], 1);
        assert_eq!(obj["IsFolder"], true);
        assert_eq!(obj["ParentId"], "0");
        assert_eq!(obj["PrimaryImageAspectRatio"], 1.777778);
        assert_eq!(obj["UserData"]["PlaybackPositionTicks"], 0);
        assert_eq!(obj["UserData"]["IsFavorite"], false);
        assert_eq!(obj["UserData"]["Played"], false);
        assert_eq!(obj["CollectionType"], "movies");
        assert_eq!(obj["ProviderIds"], serde_json::json!({}));
        assert_eq!(obj["ImageTags"], serde_json::json!({}));
        // UserData 统一 ViewsUserData 成型：key 大小写（PascalCase）齐全，库视图默认全零/空。
        assert_eq!(
            obj["UserData"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec![
                "IsFavorite",
                "LastPlayedDate",
                "PlayCount",
                "PlaybackPositionTicks",
                "Played",
                "PlayedPercentage"
            ],
        );
        assert_eq!(obj["UserData"]["PlayCount"], 0);
        assert!(obj["UserData"]["LastPlayedDate"].is_null());
        assert!(obj["UserData"]["PlayedPercentage"].is_null());
    }

    #[test]
    fn views_response_wraps_items_and_count() {
        let resp = ViewsResponse {
            items: vec![CollectionFolderView::default()],
            total_record_count: 1,
        };
        let out = serde_json::to_value(resp).unwrap();
        assert_eq!(out["TotalRecordCount"], 1);
        assert_eq!(out["Items"].as_array().unwrap().len(), 1);
    }
}
