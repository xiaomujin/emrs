//! Emby `/System/Info` + `/System/Info/Public` 响应 DTO。
//!
//! 服务器能力声明：转码一律不支持（防客户端探测崩溃）；路径/系统名固定常量；
//! 仅 ServerName/Id/端口取自配置。`ServerIdentityDto`（ServerName/Version/Id）
//! 被 Public 与完整 Info 共用，`#[serde(flatten)]` 注入父对象。

use serde::Serialize;

/// 服务器身份（`ServerName` / `Version` / `Id`）。
///
/// `/System/Info` 与 `/System/Info/Public` 共用，flatten 注入父对象。
/// `Version` 固定 `4.8.10.0`（Default 与 [`new`] 一致）。
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ServerIdentityDto {
    pub server_name: String,
    pub version: String,
    pub id: String,
}

impl Default for ServerIdentityDto {
    fn default() -> Self {
        Self {
            server_name: String::new(),
            version: "4.8.10.0".into(),
            id: String::new(),
        }
    }
}

impl ServerIdentityDto {
    /// 由配置项构造（`Version` 取固定 `4.8.10.0`）。
    pub fn new(server_name: &str, server_id: &str) -> Self {
        Self {
            server_name: server_name.into(),
            id: server_id.into(),
            ..Default::default()
        }
    }
}

/// `/System/Info/Public` 响应（匿名探测，Infuse/Senplayer 发现第一跳）。
#[derive(Serialize, Default)]
#[serde(rename_all = "PascalCase")]
pub struct SystemInfoPublicDto {
    #[serde(flatten)]
    pub server_identity: ServerIdentityDto,
    pub local_addresses: Vec<String>,
    pub remote_addresses: Vec<String>,
}

impl SystemInfoPublicDto {
    pub fn new(server_name: &str, server_id: &str) -> Self {
        Self {
            server_identity: ServerIdentityDto::new(server_name, server_id),
            ..Default::default()
        }
    }
}

/// WakeOnLan 信息（固定 MAC 广播）。
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct WakeOnLanInfoDto {
    pub mac_address: String,
    pub broadcast_address: String,
    pub port: i64,
}

impl WakeOnLanInfoDto {
    /// Emby 默认值：全 1 MAC、广播地址、端口 9。
    fn emby_default() -> Self {
        Self {
            mac_address: "FFFFFFFFFFFF".into(),
            broadcast_address: "255.255.255.255".into(),
            port: 9,
        }
    }
}

/// `/System/Info` 响应（需认证）：完整服务器能力声明。
///
/// 转码相关一律"不支持"。常量集中在 `Default`；handler 经 [`new`] 仅覆盖
/// 配置项（ServerName/Id/端口）。
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct SystemInfoDto {
    #[serde(flatten)]
    pub server_identity: ServerIdentityDto,
    pub system_update_level: String,
    pub operating_system_display_name: String,
    pub has_pending_restart: bool,
    pub is_shutting_down: bool,
    pub has_image_enhancers: bool,
    pub operating_system: String,
    pub supports_library_monitor: bool,
    pub supports_local_port_configuration: bool,
    pub supports_wake_server: bool,
    pub web_socket_port_number: String,
    pub completed_installations: Vec<serde_json::Value>,
    pub can_self_restart: bool,
    pub can_self_update: bool,
    pub can_launch_web_browser: bool,
    pub program_data_path: String,
    pub items_by_name_path: String,
    pub cache_path: String,
    pub log_path: String,
    pub internal_metadata_path: String,
    pub transcoding_temp_path: String,
    pub http_server_port_number: String,
    pub supports_https: bool,
    pub https_port_number: i64,
    pub has_update_available: bool,
    pub supports_auto_run_at_startup: bool,
    pub hardware_acceleration_requires_premiere: bool,
    pub wake_on_lan_info: WakeOnLanInfoDto,
    pub is_in_maintenance_mode: bool,
}

impl Default for SystemInfoDto {
    fn default() -> Self {
        Self {
            server_identity: ServerIdentityDto::default(),
            system_update_level: "Release".into(),
            operating_system_display_name: "Linux".into(),
            has_pending_restart: false,
            is_shutting_down: false,
            has_image_enhancers: false,
            operating_system: "Linux".into(),
            supports_library_monitor: true,
            supports_local_port_configuration: true,
            supports_wake_server: false,
            web_socket_port_number: String::new(),
            completed_installations: Vec::new(),
            can_self_restart: false,
            can_self_update: false,
            can_launch_web_browser: false,
            program_data_path: "/emrs".into(),
            items_by_name_path: "/emrs/metadata".into(),
            cache_path: "/emrs/cache".into(),
            log_path: "/emrs/logs".into(),
            internal_metadata_path: "/emrs/metadata".into(),
            transcoding_temp_path: "/emrs/transcoding-temp".into(),
            http_server_port_number: String::new(),
            supports_https: false,
            https_port_number: 8920,
            has_update_available: false,
            supports_auto_run_at_startup: false,
            hardware_acceleration_requires_premiere: true,
            wake_on_lan_info: WakeOnLanInfoDto::emby_default(),
            is_in_maintenance_mode: false,
        }
    }
}

impl SystemInfoDto {
    /// 由配置项构造（ServerName/Id/端口；其余走 `Default` 常量）。
    pub fn new(server_name: &str, server_id: &str, port: &str) -> Self {
        Self {
            server_identity: ServerIdentityDto::new(server_name, server_id),
            web_socket_port_number: port.to_string(),
            http_server_port_number: port.to_string(),
            ..Default::default()
        }
    }
}
