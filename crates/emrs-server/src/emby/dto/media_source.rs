//! MediaSource / MediaStream 成型：MediaSourceRow + ffprobe 流信息 → Emby JSON。
//!
//! 需要 DB 的媒体源成型（`attach_media_sources` 在 `item` 模块，此处只提供
//! [`media_sources_json`] 与流成型辅助）；`DirectStreamUrl` 恒为签名短票据 `/s/{ticket}`。

use serde::Serialize;

use emby_proto::{RequiredHttpHeaders, item_id};
use emrs_core::playback::ticket::{TicketClaims, issue_ticket};
use emrs_infra::db::Db;
use emrs_infra::stores::{MediaSourceRow, StreamInfo};

/// Emby `MediaStreams` 元素（扁平化：Video/Audio/Subtitle 变体字段一律 `Option`+skip，
/// 仅本类型设置的字段出现）。
///
/// 与旧 `json!` 版的差异：类型内 `Option` 字段为空时旧版发 `null`、新版省略
/// （更贴近真实 Emby 省略风格；客户端 null==absent）。`NalLengthSize` 仅 h264/AVC
/// 时输出；其余变体字段按流类型设置。
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct MediaStreamDto {
    // base（恒发）
    index: i64,
    #[serde(rename = "Type")]
    stream_type: String,
    codec: String,
    display_title: String,
    time_base: String,
    is_default: bool,
    is_forced: bool,
    is_external: bool,
    is_hearing_impaired: bool,
    is_interlaced: bool,
    is_text_subtitle_stream: bool,
    supports_external_stream: bool,
    protocol: String,
    extended_video_type: String,
    extended_video_sub_type: String,
    extended_video_sub_type_description: String,
    attachment_size: i64,
    // 变体字段（仅本类型设置时 Some，否则 None→省略）
    #[serde(skip_serializing_if = "Option::is_none")]
    color_transfer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    color_primaries: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    color_space: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    video_range: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bit_rate: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bit_depth: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ref_frames: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    height: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    width: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    aspect_ratio: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    codec_tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    nal_length_size: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_anamorphic: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pixel_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    level: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    average_frame_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    real_frame_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    display_language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    channel_layout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    channels: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sample_rate: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delivery_method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subtitle_location_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delivery_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
}

/// `MediaStreamDto` 默认值：协议 `File`、扩展视频三字段 `None`、附件大小 0，
/// 其余 bool=false / String 空串 / Option=None。供 `stream_json` 与外部字幕构造器
/// `..Default::default()` 复用，省逐字段 `None` 枚举。
impl Default for MediaStreamDto {
    fn default() -> Self {
        Self {
            index: 0,
            stream_type: String::new(),
            codec: String::new(),
            display_title: String::new(),
            time_base: String::new(),
            is_default: false,
            is_forced: false,
            is_external: false,
            is_hearing_impaired: false,
            is_interlaced: false,
            is_text_subtitle_stream: false,
            supports_external_stream: false,
            protocol: "File".into(),
            extended_video_type: "None".into(),
            extended_video_sub_type: "None".into(),
            extended_video_sub_type_description: "None".into(),
            attachment_size: 0,
            color_transfer: None,
            color_primaries: None,
            color_space: None,
            video_range: None,
            bit_rate: None,
            bit_depth: None,
            ref_frames: None,
            height: None,
            width: None,
            profile: None,
            aspect_ratio: None,
            codec_tag: None,
            nal_length_size: None,
            is_anamorphic: None,
            pixel_format: None,
            level: None,
            average_frame_rate: None,
            real_frame_rate: None,
            language: None,
            display_language: None,
            channel_layout: None,
            channels: None,
            sample_rate: None,
            title: None,
            delivery_method: None,
            subtitle_location_type: None,
            delivery_url: None,
            path: None,
        }
    }
}

/// Emby `MediaSources` 元素（详情 PlaybackInfo / item 详情）。
///
/// `Chapters` 留 `Vec<Value>`（`file_chapters` JSONB 任意结构，无法静态类型化）；
/// `RequiredHttpHeaders` 恒空对象；`Bitrate` / `DefaultAudioStreamIndex` /
/// `DefaultSubtitleStreamIndex` 为 `Option` 且 **skip**（无值时省略，对齐真实 Emby；
/// 部分客户端对 `null` 数值字段解析会报 SerializationException）。
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct MediaSourceDto {
    protocol: String,
    id: String,
    path: String,
    #[serde(rename = "Type")]
    source_type: String,
    container: String,
    name: String,
    is_remote: bool,
    has_mixed_protocols: bool,
    size: i64,
    run_time_ticks: i64,
    supports_transcoding: bool,
    supports_direct_stream: bool,
    supports_direct_play: bool,
    is_infinite_stream: bool,
    requires_opening: bool,
    requires_closing: bool,
    requires_looping: bool,
    supports_probing: bool,
    media_streams: Vec<MediaStreamDto>,
    formats: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bitrate: Option<i64>,
    required_http_headers: RequiredHttpHeaders,
    direct_stream_url: String,
    add_api_key_to_direct_stream_url: bool,
    read_at_native_framerate: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_audio_stream_index: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_subtitle_stream_index: Option<i64>,
    item_id: String,
    chapters: Vec<serde_json::Value>,
}

/// 签发播放短票据：绑定 (uuid, user_id)，TTL = max(PLAYBACK_TICKET_TTL, 时长+1h)。
/// 已知时长时按「时长+1h 缓冲」伸缩（覆盖拖动/暂停续播），未知或过短兜底 6h。
/// 未配置 `playback.signing_key` 时返回 Err——调用方据此 500，不降级为 `/Videos/` 直链。
fn issue_playback_ticket(
    signing_key: Option<&str>,
    user_id: i64,
    uuid: &str,
    duration_secs: Option<i64>,
) -> anyhow::Result<String> {
    let key = signing_key
        .ok_or_else(|| anyhow::anyhow!("playback.signing_key 未配置，无法签发播放票据"))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let ttl = duration_secs
        .filter(|&s| s > 0)
        .map(|s| (s as u64).saturating_add(3600))
        .unwrap_or(0)
        .max(emrs_core::playback::PLAYBACK_TICKET_TTL.as_secs());
    let claims = TicketClaims {
        uuid: uuid.to_string(),
        user_id,
        exp: now + ttl,
    };
    issue_ticket(&claims, key.as_bytes())
}

/// MediaSourceRow → Emby MediaSource JSON。
/// `DirectStreamUrl` 恒为签名短票据 `/s/{ticket}`，
/// 客户端直连播放无需再带 token；票据过期即失效。
/// 未配置签名密钥时返回错误（不降级为 `/Videos/` 直链）。
/// `is_first`：多版本时第一个为 `Default`，其余为 `Grouping`（对齐官方）。
pub async fn media_sources_json(
    db: &Db,
    signing_key: Option<&str>,
    user_id: i64,
    media: &MediaSourceRow,
    is_first: bool,
) -> anyhow::Result<Vec<MediaSourceDto>> {
    let uuid = media.uuid.as_deref().unwrap_or("");
    let name = media.name.as_deref().unwrap_or("Stream");
    let container = media.file_container.as_deref().unwrap_or("mp4");

    // DirectStreamUrl 用短时效票据 /s/{ticket}：不泄露用户主 token，
    // 票据按时长伸缩（≥6h）且仅限本 uuid 播放。
    let direct_url = format!(
        "/s/{}",
        issue_playback_ticket(signing_key, user_id, uuid, media.file_second)?
    );

    // 流信息（ffprobe 解析，扫描时写入 media_source.metadata；外部字幕从 external_subtitle 表合并）
    let mut streams = media_streams_json(db, media).await;
    // 从流列表提取 Bitrate（首视频流）/默认音频/默认字幕索引
    let (mut bitrate, default_audio, default_subtitle) = defaults_from_streams(&streams);
    // 视频流无码率时，用文件大小/时长估算平均码率，保证 Bitrate 不为空
    if bitrate.is_none() {
        bitrate = estimate_bitrate(media.file_size, media.file_second);
    }
    // 视频流自身的 BitRate 为空时回填，保证 MediaStream 内码率也不为空
    for s in streams.iter_mut() {
        if s.stream_type == "Video" && s.bit_rate.is_none() {
            s.bit_rate = bitrate;
        }
    }
    // 章节：file_chapters（media_source.chapters）由 ffprobe 写入，缺失时兜底空数组
    let chapters = media
        .file_chapters
        .as_deref()
        .and_then(|s| serde_json::from_str::<Vec<serde_json::Value>>(s).ok())
        .unwrap_or_default();

    Ok(vec![MediaSourceDto {
        protocol: "File".into(),
        id: uuid.to_string(),
        path: media.path_url.as_deref().unwrap_or("").to_string(),
        source_type: (if is_first { "Default" } else { "Grouping" }).into(),
        container: container.to_string(),
        name: name.to_string(),
        is_remote: false,
        has_mixed_protocols: false,
        size: media.file_size.unwrap_or(0),
        run_time_ticks: media.file_second.unwrap_or(0) * 10_000_000,
        supports_transcoding: false,
        supports_direct_stream: true,
        supports_direct_play: true,
        is_infinite_stream: false,
        requires_opening: false,
        requires_closing: false,
        requires_looping: false,
        supports_probing: true,
        media_streams: streams,
        formats: Vec::new(),
        bitrate,
        required_http_headers: RequiredHttpHeaders {},
        direct_stream_url: direct_url,
        add_api_key_to_direct_stream_url: false,
        read_at_native_framerate: false,
        default_audio_stream_index: default_audio,
        default_subtitle_stream_index: default_subtitle,
        item_id: item_id(media.item_id),
        chapters,
    }])
}

/// 从 MediaStreams JSON 提取：Bitrate（首视频流）、DefaultAudioStreamIndex（首音频流）、
/// DefaultSubtitleStreamIndex（首个默认字幕流，无则首个字幕流）。
fn defaults_from_streams(streams: &[MediaStreamDto]) -> (Option<i64>, Option<i64>, Option<i64>) {
    let mut bitrate = None;
    let mut audio = None;
    let mut subtitle = None;
    let mut subtitle_default = None;
    for s in streams {
        match s.stream_type.as_str() {
            "Video" if bitrate.is_none() => bitrate = s.bit_rate,
            "Audio" if audio.is_none() => audio = Some(s.index),
            "Subtitle" => {
                if subtitle.is_none() {
                    subtitle = Some(s.index);
                }
                if subtitle_default.is_none() && s.is_default {
                    subtitle_default = Some(s.index);
                }
            }
            _ => {}
        }
    }
    (bitrate, audio, subtitle_default.or(subtitle))
}

/// 平均码率估算（bps）：文件大小 × 8 / 时长（秒）。缺任一项返回 None。
fn estimate_bitrate(size: Option<i64>, seconds: Option<i64>) -> Option<i64> {
    let size = size?;
    let secs = seconds?;
    if secs > 0 {
        Some(size.saturating_mul(8) / secs)
    } else {
        None
    }
}

/// 外部字幕查询返回行（display_title, codec, path, is_forced）。
/// is_forced 以 0/1 整数承载（sqlx Any 下 bool 解码失败，见 stores/mod.rs 约定）。
type ExternalSubtitleRow = (Option<String>, Option<String>, Option<String>, i64);

/// 从 `media_source.metadata` 反序列化流信息并转换为 Emby `MediaStreams`，
/// 再合并 `external_subtitle` 表中的外部字幕（IsExternal=true）。
/// 无数据或解析失败时仅输出外部字幕。
async fn media_streams_json(db: &Db, media: &MediaSourceRow) -> Vec<MediaStreamDto> {
    let mut streams: Vec<MediaStreamDto> = Vec::new();
    if let Some(meta) = media.file_metadata.as_deref().filter(|s| !s.is_empty()) {
        match serde_json::from_str::<Vec<StreamInfo>>(meta) {
            Ok(list) => {
                let container = media.file_container.as_deref();
                for (i, s) in list.iter().enumerate() {
                    streams.push(stream_json(s, i, container));
                }
            }
            Err(e) => tracing::debug!(error = %e, "file_metadata 反序列化失败"),
        }
    }

    // 外部字幕（接在内嵌流之后，Index 顺延）
    // 新表 external_subtitle：只存外部字幕（外挂附件）
    if let Some(media_id) = media.media_id {
        let rows: Vec<ExternalSubtitleRow> = match sqlx::query_as(
            "SELECT display_title, codec, path, is_forced FROM external_subtitle \
             WHERE media_source_id = ? ORDER BY id",
        )
        .bind(media_id)
        .fetch_all(db.pool())
        .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, media_id, "外部字幕查询失败，跳过外部字幕");
                Vec::new()
            }
        };
        let next_index = streams.iter().map(|s| s.index).max().unwrap_or(-1) + 1;
        for (i, (title, codec, path_url, is_forced_raw)) in rows.into_iter().enumerate() {
            let title = title.unwrap_or_default();
            let codec = codec.unwrap_or_default().to_ascii_lowercase();
            let language = external_subtitle_language(&title);
            let index = next_index + i as i64;
            let is_forced = is_forced_raw != 0;
            // DeliveryUrl 用外部字幕内部序号（0 基），与 /Videos/{uuid}/Subtitles/
            // 路由的 OFFSET 一致；Index 字段则顺延内嵌流，供客户端区分流。
            let delivery_url = media
                .uuid
                .as_deref()
                .map(|u| format!("/Videos/{u}/Subtitles/{i}"));
            streams.push(MediaStreamDto {
                index,
                stream_type: "Subtitle".into(),
                codec: subtitle_codec(&codec),
                display_title: external_subtitle_title(&title, &codec, is_forced),
                is_forced,
                is_external: true,
                is_text_subtitle_stream: true,
                supports_external_stream: true,
                delivery_method: Some("External".into()),
                delivery_url,
                display_language: language
                    .as_deref()
                    .and_then(language_name)
                    .map(str::to_string),
                language,
                title: Some(title),
                path: Some(path_url.unwrap_or_default()),
                ..Default::default()
            });
        }
    }

    streams
}

/// 单个流 → 类型化 Emby MediaStream DTO（对齐参考 Emby 输出字段集）。
/// `container`：媒体源容器（mp4/mkv 等），决定是否输出 `CodecTag`
/// （仅 mp4 家族输出 avc1/mp4a，mkv 参考 Emby 不发 CodecTag）。
fn stream_json(s: &StreamInfo, fallback_index: usize, container: Option<&str>) -> MediaStreamDto {
    let index = s.index.unwrap_or(fallback_index as i64);
    let is_default = s.is_default.unwrap_or(false);
    let is_forced = s.is_forced.unwrap_or(false);
    let time_base = s.time_base.as_deref().unwrap_or("").to_string();
    let ty = s.stream_type.as_str();
    // 仅 mp4/m4v/mov 输出 CodecTag（ISO BMFF 带 sample entry tag；mkv 无）。
    let has_codec_tag = matches!(container, Some("mp4" | "m4v" | "mov"));

    let mut dto = MediaStreamDto {
        index,
        stream_type: s.stream_type.clone(),
        codec: subtitle_codec_if_sub(ty, &s.codec),
        display_title: stream_display_title(s),
        time_base,
        is_default,
        is_forced,
        is_interlaced: s.is_interlaced.unwrap_or(false),
        ..Default::default()
    };
    match ty {
        "Video" => {
            dto.color_transfer = s.color_transfer.clone();
            dto.color_primaries = s.color_primaries.clone();
            dto.color_space = s.color_space.clone();
            dto.video_range = Some(video_range(s));
            dto.bit_rate = s.bit_rate;
            dto.bit_depth = s.bit_depth;
            dto.ref_frames = s.refs;
            dto.height = s.height;
            dto.width = s.width;
            dto.profile = s.profile.clone();
            dto.aspect_ratio = s.display_aspect_ratio.clone();
            dto.codec_tag = if has_codec_tag {
                codec_tag(&s.codec).map(str::to_string)
            } else {
                None
            };
            // NalLengthSize 仅 h264/AVC 输出 "4"
            dto.nal_length_size = is_h264_avc(s).then(|| "4".to_string());
            dto.is_anamorphic = Some(is_anamorphic(s));
            dto.pixel_format = s.pixel_format.clone();
            dto.level = s.level;
            // 帧率输出原始值（对齐参考 Emby：23.976025 不四舍五入）
            dto.average_frame_rate = s.frame_rate;
            dto.real_frame_rate = s.frame_rate;
        }
        "Audio" => {
            dto.language = s.language.clone();
            dto.display_language = display_language(s);
            dto.codec_tag = if has_codec_tag {
                codec_tag(&s.codec).map(str::to_string)
            } else {
                None
            };
            dto.channel_layout = s.channel_layout.clone();
            dto.bit_rate = s.bit_rate;
            dto.channels = s.channels;
            dto.sample_rate = s.sample_rate;
            dto.profile = s.profile.clone();
        }
        "Subtitle" => {
            dto.language = s.language.clone();
            dto.display_language = display_language(s);
            dto.title = s.title.clone();
            dto.delivery_method = Some("Embed".into());
            dto.subtitle_location_type = Some("InternalStream".into());
            dto.is_text_subtitle_stream = true;
            dto.supports_external_stream = true;
        }
        _ => {}
    }
    dto
}

/// h264 且 AVC 封装 → 输出 NalLengthSize="4"（对齐参考 Emby）。
fn is_h264_avc(s: &StreamInfo) -> bool {
    s.codec.eq_ignore_ascii_case("h264") && s.is_avc.unwrap_or(false)
}

/// 像素宽高比非 1:1 → 变形画面 IsAnamorphic。
fn is_anamorphic(s: &StreamInfo) -> bool {
    matches!(
        s.sample_aspect_ratio.as_deref(),
        Some(sar) if !sar.is_empty() && sar != "1:1" && sar != "0:1"
    )
}

/// 视频动态范围（由色彩转换推断；SDR/HDR10/HLG）。
fn video_range(s: &StreamInfo) -> String {
    match s.color_transfer.as_deref() {
        Some("smpte2084" | "smpte2086") => "HDR10".to_string(),
        Some("arib-std-b67") => "HLG".to_string(),
        _ => "SDR".to_string(),
    }
}

/// 语言代码 → 英文名（用于 DisplayLanguage）；未知代码原样返回。
fn display_language(s: &StreamInfo) -> Option<String> {
    s.language.as_deref().map(|code| {
        language_name(code)
            .map(|n| n.to_string())
            .unwrap_or_else(|| code.to_string())
    })
}

/// 语言代码 → 英文名。
fn language_name(code: &str) -> Option<&'static str> {
    match code.to_ascii_lowercase().as_str() {
        "eng" | "en" => Some("English"),
        "jpn" | "ja" => Some("Japanese"),
        "chi" | "zho" | "cmn" | "zh" => Some("Chinese"),
        "spa" | "es" => Some("Spanish"),
        "por" | "pt" => Some("Portuguese"),
        "fre" | "fra" | "fr" => Some("French"),
        "ger" | "deu" | "de" => Some("German"),
        "ara" | "ar" => Some("Arabic"),
        "ita" | "it" => Some("Italian"),
        "rus" | "ru" => Some("Russian"),
        "tha" | "th" => Some("Thai"),
        "vie" | "vi" => Some("Vietnamese"),
        "ind" | "id" => Some("Indonesian"),
        "may" | "msa" | "ms" => Some("Malay"),
        "kor" | "ko" => Some("Korean"),
        "nld" | "dut" | "nl" => Some("Dutch"),
        "pol" | "pl" => Some("Polish"),
        "tur" | "tr" => Some("Turkish"),
        "swe" | "sv" => Some("Swedish"),
        "nor" | "no" => Some("Norwegian"),
        "dan" | "da" => Some("Danish"),
        "fin" | "fi" => Some("Finnish"),
        "heb" | "he" => Some("Hebrew"),
        "hin" | "hi" => Some("Hindi"),
        "ben" | "bn" => Some("Bengali"),
        "tam" | "ta" => Some("Tamil"),
        "tel" | "te" => Some("Telugu"),
        "ukr" | "uk" => Some("Ukrainian"),
        "ces" | "cze" | "cs" => Some("Czech"),
        "hun" | "hu" => Some("Hungarian"),
        "ell" | "gre" | "el" => Some("Greek"),
        "cat" | "ca" => Some("Catalan"),
        "yue" => Some("Chinese (Cantonese)"),
        _ => None,
    }
}

/// 仅对字幕流做编码名映射（hdmv_pgs_subtitle→pgs / dvd_subtitle→dvdsub / webvtt→vtt）；
/// subrip 保留原始名（参考 Emby `Codec` 输出 `subrip`）。
fn subtitle_codec_if_sub(stream_type: &str, codec: &str) -> String {
    if stream_type == "Subtitle" {
        subtitle_codec(codec)
    } else {
        codec.to_string()
    }
}

/// 字幕编码名归一化（保留 ffprobe 原始名，对齐参考 Emby 输出的 `Codec` 字段）。
fn subtitle_codec(codec: &str) -> String {
    match codec.to_ascii_lowercase().as_str() {
        "hdmv_pgs_subtitle" => "pgs".to_string(),
        "dvd_subtitle" => "dvdsub".to_string(),
        "webvtt" => "vtt".to_string(),
        other => other.to_string(),
    }
}

/// 媒体流 CodecTag（mp4 box tag 近似，对齐参考 Emby 输出）：
/// h264→avc1、hevc→hvc1、aac→mp4a、ac3→ac-3、其余返回 null。
fn codec_tag(codec: &str) -> Option<&'static str> {
    match codec.to_ascii_lowercase().as_str() {
        "h264" => Some("avc1"),
        "hevc" | "h265" => Some("hvc1"),
        "aac" => Some("mp4a"),
        "ac3" => Some("ac-3"),
        _ => None,
    }
}

/// 流显示标题（Emby DisplayTitle，对齐参考 Emby）。
/// 视频：`{height}p {codec}`（编码大写，如 "1080p H264"）；
/// 音频：`{语言} {codec} {声道}`，默认流追加 "(默认)"（如 "Japanese AAC stereo (默认)"）；
/// 字幕：`{标题|语言} ({默认 }编码)`（编码大写，如 "Chinese (默认 ASS)"）。
fn stream_display_title(s: &StreamInfo) -> String {
    match s.stream_type.as_str() {
        "Video" => match s.height {
            Some(h) if h > 0 => format!("{}p {}", h, s.codec.to_uppercase()),
            _ => s.codec.to_uppercase(),
        },
        "Audio" => {
            let mut parts: Vec<String> = Vec::new();
            if let Some(lang) = display_language(s) {
                parts.push(lang);
            }
            parts.push(s.codec.to_uppercase());
            if let Some(layout) = s.channel_layout.as_deref().filter(|l| !l.is_empty()) {
                parts.push(layout.to_string());
            } else if let Some(label) = s.channels.and_then(channel_label) {
                parts.push(label.to_string());
            }
            let mut title = parts.join(" ");
            if s.is_default.unwrap_or(false) {
                title.push_str(" (默认)");
            }
            title
        }
        _ => {
            // 字幕：显示语言名优先（对齐参考 Emby，"Chinese Simplified (默认 SUBRIP)" 用语言名
            // 而非轨道 Title 作基底）。但语言码 `chi` 分不出简繁（参考能给出
            // Simplified/Traditional，我们拿不到该信息），此时退回轨道 Title（"简体"/"繁体"）保区分。
            let lang = display_language(s);
            let base = match lang.as_deref() {
                Some("Chinese") => s
                    .title
                    .as_deref()
                    .filter(|t| !t.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| "Chinese".to_string()),
                Some(name) => name.to_string(),
                None => s
                    .title
                    .as_deref()
                    .filter(|t| !t.is_empty())
                    .map(str::to_string)
                    .or_else(|| s.language.clone())
                    .unwrap_or_else(|| s.codec.clone()),
            };
            let codec = subtitle_codec(&s.codec).to_uppercase();
            if s.is_default.unwrap_or(false) {
                format!("{} (默认 {})", base, codec)
            } else {
                format!("{} ({})", base, codec)
            }
        }
    }
}

/// 声道数 → 显示标签（2→stereo、1→mono、6→5.1、8→7.1）。
fn channel_label(channels: i64) -> Option<&'static str> {
    match channels {
        1 => Some("mono"),
        2 => Some("stereo"),
        6 => Some("5.1"),
        8 => Some("7.1"),
        _ => None,
    }
}

/// 外部字幕显示标题：`{文件名} ({codec})`，如 "JPSC.ass (ass)" → "JPSC.ass (ass)"；
/// 强制字幕追加 "(强制)" 标注。若文件名仅由语言标签组成则简化标题。
fn external_subtitle_title(title: &str, codec: &str, is_forced: bool) -> String {
    if title.is_empty() {
        return if is_forced {
            format!("{codec} (强制)")
        } else {
            codec.to_string()
        };
    }
    if is_forced {
        format!("{} ({}) (强制)", title, codec)
    } else {
        format!("{} ({})", title, codec)
    }
}

/// 从外部字幕文件名推断语言（尽力而为，未识别返回 null）。
fn external_subtitle_language(title: &str) -> Option<String> {
    let lower = title.to_ascii_lowercase();
    let table = [
        (
            "chi",
            ["zh", "chi", "chs", "cht", "sc", "tc", "cn", "yue"].as_slice(),
        ),
        ("jpn", ["jp", "jpn", "ja", "jpsc", "jptc"].as_slice()),
        ("eng", ["eng", "en", "gb", "us"].as_slice()),
        ("kor", ["kor", "ko", "kr"].as_slice()),
        ("fre", ["fre", "fr", "fra"].as_slice()),
        ("ger", ["ger", "de", "deu"].as_slice()),
        ("spa", ["spa", "es", "esp"].as_slice()),
        ("ita", ["ita", "it"].as_slice()),
        ("rus", ["rus", "ru"].as_slice()),
        ("tha", ["tha", "th"].as_slice()),
        ("vie", ["vie", "vi"].as_slice()),
        ("por", ["por", "pt"].as_slice()),
        ("ara", ["ara", "ar"].as_slice()),
    ];
    for (lang, toks) in table {
        if toks.iter().any(|t| {
            let pat = format!(".{t}.");
            lower.contains(&pat)
        }) {
            return Some(lang.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    #[test]
    fn external_subtitle_title_marks_forced() {
        assert_eq!(
            external_subtitle_title("JPSC.ass", "ass", false),
            "JPSC.ass (ass)"
        );
        assert_eq!(
            external_subtitle_title("Movie.eng.forced.srt", "srt", true),
            "Movie.eng.forced.srt (srt) (强制)"
        );
        // 空文件名退化为仅编码（强制时带标注）
        assert_eq!(external_subtitle_title("", "ass", false), "ass");
        assert_eq!(external_subtitle_title("", "ass", true), "ass (强制)");
    }

    /// MediaStreamDto / MediaSourceDto 序列化形状：
    /// RequiredHttpHeaders 恒 `{}`；流类型字段按 Video/Audio 设置、其余 skip 省略；
    /// NalLengthSize 仅 h264/AVC 出现。
    #[test]
    fn media_stream_dto_shape() {
        // RequiredHttpHeaders 恒空对象（非 null）
        assert_eq!(
            serde_json::to_value(RequiredHttpHeaders {}).unwrap(),
            json!({})
        );

        // Video 流：h264/AVC → NalLengthSize="4"；无 Language/Channels（skip）
        let v = StreamInfo {
            stream_type: "Video".into(),
            codec: "h264".into(),
            index: Some(0),
            width: Some(1920),
            height: Some(1080),
            frame_rate: Some(23.976),
            bit_rate: Some(7541653),
            color_transfer: Some("bt709".into()),
            color_primaries: Some("bt709".into()),
            color_space: Some("bt709".into()),
            is_avc: Some(true),
            is_default: Some(true),
            time_base: Some("1/1000".into()),
            pixel_format: Some("yuv420p".into()),
            profile: Some("High".into()),
            level: Some(40),
            title: None,
            language: None,
            channels: None,
            sample_rate: None,
            bit_depth: None,
            display_aspect_ratio: None,
            is_forced: None,
            refs: None,
            is_interlaced: None,
            channel_layout: None,
            sample_aspect_ratio: None,
        };
        let s = serde_json::to_value(stream_json(&v, 0, Some("mp4"))).unwrap();
        assert_eq!(s["Type"], "Video");
        assert_eq!(s["Codec"], "h264");
        assert_eq!(s["VideoRange"], "SDR");
        assert_eq!(s["BitRate"], 7541653);
        assert_eq!(s["Height"], 1080);
        assert_eq!(s["NalLengthSize"], "4", "h264/AVC 应输出 NalLengthSize=4");
        assert!(
            !s.as_object().unwrap().contains_key("Language"),
            "Video 不应带 Language"
        );
        assert!(
            !s.as_object().unwrap().contains_key("Channels"),
            "Video 不应带 Channels"
        );

        // 非 h264 → 无 NalLengthSize
        let mut v2 = v.clone();
        v2.codec = "hevc".into();
        v2.is_avc = Some(false);
        let s2 = serde_json::to_value(stream_json(&v2, 0, Some("mkv"))).unwrap();
        assert!(
            !s2.as_object().unwrap().contains_key("NalLengthSize"),
            "非 h264/AVC 不应带 NalLengthSize"
        );

        // Audio 流：带 Language/Channels；无 Height
        let a = StreamInfo {
            stream_type: "Audio".into(),
            codec: "flac".into(),
            index: Some(1),
            language: Some("jpn".into()),
            channels: Some(2),
            sample_rate: Some(48000),
            channel_layout: Some("stereo".into()),
            is_default: Some(true),
            time_base: Some("1/1000".into()),
            profile: None,
            title: None,
            width: None,
            height: None,
            frame_rate: None,
            bit_rate: None,
            pixel_format: None,
            bit_depth: None,
            color_primaries: None,
            color_space: None,
            color_transfer: None,
            display_aspect_ratio: None,
            is_forced: None,
            level: None,
            refs: None,
            is_interlaced: None,
            sample_aspect_ratio: None,
            is_avc: None,
        };
        let sa = serde_json::to_value(stream_json(&a, 1, Some("mp4"))).unwrap();
        assert_eq!(sa["Type"], "Audio");
        assert_eq!(sa["Language"], "jpn");
        assert_eq!(sa["Channels"], 2);
        assert!(
            !sa.as_object().unwrap().contains_key("Height"),
            "Audio 不应带 Height"
        );
    }
}