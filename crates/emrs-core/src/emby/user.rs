//! Emby `/Users` 系列响应 DTO：User 对象（登录 / `/Users/Me` / `/Users/{id}` 共用）。
//!
//! `Configuration` / `Policy` 是固定模板（仅少数字段随用户变），常量集中到
//! `*_emby_default` 构造器；`user_to_json` 由 [`crate::auth::UserRow`] 成型。

use serde::Serialize;

use crate::auth::UserRow;

/// 用户播放/字幕偏好（固定模板）。
#[derive(Serialize, Default)]
#[serde(rename_all = "PascalCase")]
pub struct UserConfigurationDto {
    pub audio_language_preference: String,
    pub play_default_audio_track: bool,
    pub display_missing_episodes: bool,
    pub subtitle_mode: String,
    pub ordered_views: Vec<String>,
    pub latest_items_excludes: Vec<String>,
    pub search_excludes: Vec<String>,
    pub my_media_excludes: Vec<String>,
    pub hide_played_in_latest: bool,
    pub hide_played_in_more_like_this: bool,
    pub hide_played_in_suggestions: bool,
    pub remember_audio_selections: bool,
    pub remember_subtitle_selections: bool,
    pub enable_next_episode_auto_play: bool,
    pub resume_rewind_seconds: i64,
    pub intro_skip_mode: String,
    pub enable_local_password: bool,
}

impl UserConfigurationDto {
    /// Emby 默认模板：PlayDefaultAudioTrack=true、SubtitleMode="Smart"、IntroSkipMode="None"。
    fn emby_default() -> Self {
        Self {
            play_default_audio_track: true,
            subtitle_mode: "Smart".into(),
            intro_skip_mode: "None".into(),
            ..Default::default()
        }
    }
}

/// 用户权限策略模板（仅 IsAdministrator/IsDisabled 随用户变，其余固定）。
#[derive(Serialize, Default)]
#[serde(rename_all = "PascalCase")]
pub struct UserPolicyDto {
    pub is_administrator: bool,
    pub is_hidden: bool,
    pub is_hidden_remotely: bool,
    pub is_hidden_from_unused_devices: bool,
    pub is_disabled: bool,
    pub locked_out_date: i64,
    pub allow_tag_or_rating: bool,
    pub blocked_tags: Vec<String>,
    pub is_tag_blocking_mode_inclusive: bool,
    pub include_tags: Vec<String>,
    pub enable_user_preference_access: bool,
    /// Emby `AccessSchedule` 为对象数组，此处恒空 → `[]`。
    pub access_schedules: Vec<serde_json::Value>,
    /// Emby `BlockUnratedItem` 为对象数组，此处恒空 → `[]`。
    pub block_unrated_items: Vec<serde_json::Value>,
    pub enable_remote_control_of_other_users: bool,
    pub enable_shared_device_control: bool,
    pub enable_remote_access: bool,
    pub enable_live_tv_management: bool,
    pub enable_live_tv_access: bool,
    pub enable_media_playback: bool,
    pub enable_audio_playback_transcoding: bool,
    pub enable_video_playback_transcoding: bool,
    pub enable_playback_remuxing: bool,
    pub enable_content_deletion: bool,
    pub restricted_features: Vec<String>,
    pub enable_content_deletion_from_folders: Vec<String>,
    pub enable_content_downloading: bool,
    pub enable_subtitle_downloading: bool,
    pub enable_subtitle_management: bool,
    pub enable_sync_transcoding: bool,
    pub enable_media_conversion: bool,
    pub enabled_channels: Vec<String>,
    pub enable_all_channels: bool,
    pub enabled_folders: Vec<String>,
    pub enable_all_folders: bool,
    pub invalid_login_attempt_count: i64,
    pub enable_public_sharing: bool,
    pub remote_client_bitrate_limit: i64,
    pub authentication_provider_id: String,
    pub excluded_sub_folders: Vec<String>,
    pub simultaneous_stream_limit: i64,
    pub enabled_devices: Vec<String>,
    pub enable_all_devices: bool,
    pub allow_camera_upload: bool,
    pub allow_sharing_personal_items: bool,
    pub blocked_media_folders: Vec<String>,
}

impl UserPolicyDto {
    /// Emby 默认策略模板（开放播放/远程访问/全库，关闭转码/删除/下载）。
    fn emby_default() -> Self {
        Self {
            is_hidden_from_unused_devices: true,
            enable_remote_access: true,
            enable_media_playback: true,
            enable_all_channels: true,
            enable_all_folders: true,
            enable_all_devices: true,
            authentication_provider_id: "emrs".into(),
            ..Default::default()
        }
    }
}

/// Emby User DTO（登录 / `/Users/Me` / `/Users/{id}` 共用）。
///
/// `Configuration` / `Policy` 走模板；时间戳（DateCreated/LastLoginDate/
/// LastActivityDate）取 `format_time_now()`。`HasPassword`/`HasConfiguredPassword`
/// 同值（密码哈希非空）。
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct UserDto {
    pub name: String,
    pub server_id: String,
    pub prefix: String,
    pub date_created: String,
    pub id: String,
    pub has_password: bool,
    pub has_configured_password: bool,
    pub last_login_date: String,
    pub last_activity_date: String,
    pub configuration: UserConfigurationDto,
    pub policy: UserPolicyDto,
    pub has_configured_easy_password: bool,
}

/// `UserRow` → Emby `UserDto`。
pub fn user_to_json(server_id: &str, u: &UserRow) -> UserDto {
    let now = super::format_time_now();
    let has_password = !u.password_hash.is_empty();
    UserDto {
        name: u.username.clone(),
        server_id: server_id.to_string(),
        prefix: "E".into(),
        date_created: now.clone(),
        id: u.id.to_string(),
        has_password,
        has_configured_password: has_password,
        last_login_date: now.clone(),
        last_activity_date: now,
        configuration: UserConfigurationDto::emby_default(),
        policy: UserPolicyDto {
            is_administrator: u.is_admin,
            is_disabled: u.is_disable,
            ..UserPolicyDto::emby_default()
        },
        has_configured_easy_password: false,
    }
}
