//! Emby 播放会话相关响应 DTO。
//!
//! - [`PlaybackInfoResponseDto`]：`/Items/{id}/PlaybackInfo` 响应壳。
//! - [`PlayStateDto`]：`PlayState` 子对象，登录与 `/Sessions` 共用（字段并集 +
//!   `skip`，各自保形状）。两端 `PlayState` 均为嵌套对象，故 **不** flatten，
//!   仅类型复用。
//! - [`SessionInfoDto`]：登录响应的 `SessionInfo`。
//! - [`SessionListEntryDto`]：`/Sessions` 数组元素。
//! - [`AuthenticateResponseDto`]：`/Users/AuthenticateByName` 响应壳。

use serde::Serialize;

use super::dto::{ItemDto, MediaSourceDto};
use super::user::UserDto;

/// `/Items/{id}/PlaybackInfo` 响应壳。
///
/// `MediaSources` 为类型化 [`MediaSourceDto`]；`ErrorCode` 仅出错时设置，
/// 成功（`None`）时**省略**该字段——对齐参考 Emby 成功响应无 `ErrorCode` 的输出。
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct PlaybackInfoResponseDto {
    pub media_sources: Vec<MediaSourceDto>,
    pub play_session_id: String,
    /// 出错时在此填错误码；成功为 `None` → 省略字段（参考 Emby 成功响应不含 `ErrorCode`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<i64>,
}

impl PlaybackInfoResponseDto {
    pub fn new(media_sources: Vec<MediaSourceDto>, play_session_id: String) -> Self {
        Self {
            media_sources,
            play_session_id,
            error_code: None,
        }
    }
}

/// Emby `PlayState` 子对象（登录 + `/Sessions` 共用）。
///
/// 字段并集全 `Option`+skip：登录设 8 个（无 `PositionTicks`），`/Sessions` 设
/// 6 个（含 `PositionTicks`），skip-None 各自保旧 `json!` 形状。
#[derive(Serialize, Default)]
#[serde(rename_all = "PascalCase")]
pub struct PlayStateDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    can_seek: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_paused: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_muted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repeat_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sleep_timer_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subtitle_offset: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shuffle: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    playback_rate: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    position_ticks: Option<i64>,
}

impl PlayStateDto {
    /// 登录响应 `PlayState`（无 `PositionTicks`）。
    fn login() -> Self {
        Self {
            can_seek: Some(false),
            is_paused: Some(false),
            is_muted: Some(false),
            repeat_mode: Some("RepeatNone".into()),
            sleep_timer_mode: Some("None".into()),
            subtitle_offset: Some(0),
            shuffle: Some(false),
            playback_rate: Some(1),
            ..Default::default()
        }
    }

    /// `/Sessions` 条目 `PlayState`（含 `PositionTicks`，`CanSeek=true`）。
    fn session(position_ticks: i64) -> Self {
        Self {
            can_seek: Some(true),
            repeat_mode: Some("RepeatNone".into()),
            playback_rate: Some(1),
            position_ticks: Some(position_ticks),
            ..Default::default()
        }
    }
}

/// 登录响应的 `SessionInfo`（`PlayState` 嵌套子对象 + 会话元字段）。
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct SessionInfoDto {
    /// 嵌套 `PlayState` 子对象（**非** flatten）。
    pub play_state: PlayStateDto,
    pub additional_users: Vec<serde_json::Value>,
    pub remote_end_point: String,
    pub playable_media_types: Vec<String>,
    pub playlist_index: i64,
    pub playlist_length: i64,
    pub id: String,
    pub server_id: String,
    pub user_id: String,
    pub user_name: String,
    pub client: String,
    pub last_activity_date: String,
    pub device_name: String,
    pub internal_device_id: i64,
    pub device_id: String,
    pub application_version: String,
    pub supported_commands: Vec<String>,
    pub supports_remote_control: bool,
}

impl SessionInfoDto {
    /// 由登录上下文构造（`Id`/`UserId` 同取用户 id；`UserName` 取用户名；
    /// 设备四字段从 `device` 取）。
    pub fn new(
        user_id: &str,
        server_id: &str,
        user_name: &str,
        last_activity_date: &str,
        device: &crate::auth::DeviceInfo,
    ) -> Self {
        Self {
            play_state: PlayStateDto::login(),
            additional_users: Vec::new(),
            remote_end_point: "emrs".into(),
            playable_media_types: Vec::new(),
            playlist_index: 0,
            playlist_length: 0,
            id: user_id.into(),
            server_id: server_id.into(),
            user_id: user_id.into(),
            user_name: user_name.into(),
            client: device.client.clone(),
            last_activity_date: last_activity_date.into(),
            device_name: device.device.clone(),
            internal_device_id: 0,
            device_id: device.device_id.clone(),
            application_version: device.version.clone(),
            supported_commands: Vec::new(),
            supports_remote_control: false,
        }
    }
}

/// `/Sessions` 数组元素（`NowPlayingItem` + 嵌套 `PlayState`）。
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct SessionListEntryDto {
    pub id: String,
    pub user_id: String,
    pub user_name: String,
    pub client: String,
    pub device_name: String,
    pub device_id: String,
    pub now_playing_item: ItemDto,
    /// 嵌套 `PlayState` 子对象（**非** flatten）。
    pub play_state: PlayStateDto,
    pub playable_media_types: Vec<String>,
    pub supported_commands: Vec<String>,
    pub supports_remote_control: bool,
}

impl SessionListEntryDto {
    pub fn new(
        now_playing_item: ItemDto,
        session_id: String,
        user_id: &str,
        user_name: &str,
        device: &crate::auth::DeviceInfo,
        position_ticks: i64,
    ) -> Self {
        Self {
            id: session_id,
            user_id: user_id.into(),
            user_name: user_name.into(),
            client: device.client.clone(),
            device_name: device.device.clone(),
            device_id: device.device_id.clone(),
            now_playing_item,
            play_state: PlayStateDto::session(position_ticks),
            playable_media_types: vec!["Video".into()],
            supported_commands: Vec::new(),
            supports_remote_control: false,
        }
    }
}

/// `/Users/AuthenticateByName` 响应壳。
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct AuthenticateResponseDto {
    pub user: UserDto,
    pub session_info: SessionInfoDto,
    pub access_token: String,
    pub server_id: String,
}
