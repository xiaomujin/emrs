//! 本地视频源轻量探测：扩展名判定、容器名、时长（MP4/MKV 头部解析）。
//!
//! - MP4/MOV/M4V（ISO BMFF）：解析 `moov → mvhd` 的 timescale/duration
//! - MKV/WebM（EBML）：定位 `Info` 元素，解析 `Duration`（毫秒浮点）
//! - 其它容器或解析失败 → 返回 `None`（调用方保持 file_second 为 NULL/0）
//!
//! 流信息（分辨率/编码/帧率/像素格式等）通过 ffprobe（ffmpeg-sidecar 自动
//! 定位/下载，见 [`probe_streams`]）解析；不可用时返回空列表。

use std::path::Path;

use serde::{Deserialize, Serialize};

/// 需要直扫入库存入 `media_source` 的视频扩展名白名单。
pub const VIDEO_EXTS: &[&str] = &[
    "mp4", "mkv", "avi", "ts", "m4v", "mov", "wmv", "flv", "webm", "mpg", "mpeg", "m2ts", "3gp",
    "ogv",
];

/// 单个媒体流信息（存入 `media_source.metadata`，输出到 Emby `MediaStreams`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StreamInfo {
    /// "Video" | "Audio" | "Subtitle"
    pub stream_type: String,
    /// 编码名（h264/hevc/ac3/aac/ass…）
    pub codec: String,
    /// ffprobe 全局流索引（视频0/音频1/字幕2…）
    pub index: Option<i64>,
    /// 显示标题（Emby DisplayTitle；如视频 "1080p hevc"、字幕 "JPSC (ass)"）
    pub title: Option<String>,
    /// 语言代码（如 jpn/chi/eng）
    pub language: Option<String>,
    /// 视频宽（像素）
    pub width: Option<i64>,
    /// 视频高（像素）
    pub height: Option<i64>,
    /// 帧率（fps）
    pub frame_rate: Option<f64>,
    /// 码率（bit/s）
    pub bit_rate: Option<i64>,
    /// 像素格式（如 yuv420p10le）
    pub pixel_format: Option<String>,
    /// 音频声道数
    pub channels: Option<i64>,
    /// 音频采样率（Hz）
    pub sample_rate: Option<i64>,
    /// 音频采样位数
    pub bit_depth: Option<i64>,
    /// 原色（如 bt709）
    pub color_primaries: Option<String>,
    /// 色彩空间（如 bt709）
    pub color_space: Option<String>,
    /// 色彩转换（如 bt709）
    pub color_transfer: Option<String>,
    /// 长宽比（如 16:9）
    pub display_aspect_ratio: Option<String>,
    /// 默认流标记
    pub is_default: Option<bool>,
    /// 强制标记（字幕）
    pub is_forced: Option<bool>,
    /// 编码档次（视频 High/Main 10、音频 LC…）
    pub profile: Option<String>,
    /// 编码级别（h264 50 表示 5.0、hevc 120 表示 12.0）
    pub level: Option<i64>,
    /// 参考帧数
    pub refs: Option<i64>,
    /// 是否隔行扫描（由 field_order 推断）
    pub is_interlaced: Option<bool>,
    /// 时间基（如 1/1000、1/90000）
    pub time_base: Option<String>,
    /// 音频声道布局（如 stereo、5.1）
    pub channel_layout: Option<String>,
    /// 像素宽高比（如 1:1；非 1:1 视为变形 IsAnamorphic）
    pub sample_aspect_ratio: Option<String>,
    /// h264 是否 AVC 封装（决定 NalLengthSize）
    pub is_avc: Option<bool>,
}

/// 是否是需要直扫的视频扩展名（入参需已小写）。
pub fn is_video_ext(ext: &str) -> bool {
    VIDEO_EXTS.contains(&ext)
}

/// 扩展名 → Emby 容器名（`file_container` 列）。
pub fn container_for(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "mp4" | "m4v" => "mp4",
        "mkv" | "webm" => "mkv",
        "avi" => "avi",
        "ts" | "m2ts" => "mpegts",
        "mov" => "mov",
        "wmv" => "wmv",
        "flv" => "flv",
        "mpg" | "mpeg" => "mpeg",
        "3gp" => "3gp",
        "ogv" => "ogg",
        _ => return None,
    })
}

/// 探测本地视频时长（秒）。失败/不支持返回 `None`。
pub async fn probe_duration(path: &Path) -> Option<i64> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "mp4" | "m4v" | "mov" | "3gp" => probe_mp4(path).await,
        "mkv" | "webm" => probe_mkv(path).await,
        _ => None,
    }
}

const PROBE_HEAD: u64 = 1024 * 1024;

/// MP4 家族：先读文件头 1MB；找不到 moov 再读文件尾 1MB（moov 可能在文件尾）。
async fn probe_mp4(path: &Path) -> Option<i64> {
    if let Some(secs) = parse_mp4(&read_head(path, PROBE_HEAD).await?) {
        return Some(secs);
    }
    let tail = read_tail(path, PROBE_HEAD).await?;
    // 尾部扫描：定位 moov box 起始（[size(4)] "moov" [payload]）
    let pos = tail.windows(4).position(|w| w == b"moov")?;
    if pos < 4 || pos + 4 > tail.len() {
        return None;
    }
    parse_moov(&tail[pos + 4..])
}

/// MKV 家族：读文件头 1MB，定位 `Info` 元素后解析 `Duration`。
async fn probe_mkv(path: &Path) -> Option<i64> {
    let head = read_head(path, PROBE_HEAD).await?;
    parse_mkv(&head)
}

async fn read_head(path: &Path, max: u64) -> Option<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let f = tokio::fs::File::open(path).await.ok()?;
    let mut buf = Vec::with_capacity(max as usize);
    f.take(max).read_to_end(&mut buf).await.ok()?;
    Some(buf)
}

async fn read_tail(path: &Path, max: u64) -> Option<Vec<u8>> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
    let mut f = tokio::fs::File::open(path).await.ok()?;
    let len = f.metadata().await.ok()?.len();
    let start = len.saturating_sub(max);
    f.seek(SeekFrom::Start(start)).await.ok()?;
    let mut buf = Vec::with_capacity((len - start) as usize);
    f.read_to_end(&mut buf).await.ok()?;
    Some(buf)
}

// ---------------------------------------------------------------------------
// ISO BMFF（MP4）
// ---------------------------------------------------------------------------

fn be_u32(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes(b[off..off + 4].try_into().unwrap_or([0; 4]))
}

fn be_u64(b: &[u8], off: usize) -> u64 {
    u64::from_be_bytes(b[off..off + 8].try_into().unwrap_or([0; 8]))
}

/// 遍历顶层 box 找 `moov`，解析 `mvhd` 得到秒数。
fn parse_mp4(data: &[u8]) -> Option<i64> {
    let mut off = 0usize;
    while off + 8 <= data.len() {
        let size = be_u32(data, off) as usize;
        let typ = &data[off + 4..off + 8];
        let (box_size, header) = if size == 1 {
            if off + 16 > data.len() {
                break;
            }
            (be_u64(data, off + 8) as usize, 16)
        } else if size == 0 {
            (data.len() - off, 8)
        } else {
            (size, 8)
        };
        if typ == b"moov" {
            return parse_moov(&data[off + header..]);
        }
        if box_size < header {
            break;
        }
        off += box_size;
    }
    None
}

/// 在 moov 载荷内找 `mvhd`，解析 timescale/duration。
fn parse_moov(data: &[u8]) -> Option<i64> {
    let mut off = 0usize;
    while off + 8 <= data.len() {
        let size = be_u32(data, off) as usize;
        let typ = &data[off + 4..off + 8];
        let (box_size, header) = if size == 1 {
            if off + 16 > data.len() {
                break;
            }
            (be_u64(data, off + 8) as usize, 16)
        } else if size == 0 {
            (data.len() - off, 8)
        } else {
            (size, 8)
        };
        if typ == b"mvhd" {
            return mvhd_seconds(data, off + header);
        }
        if box_size < header {
            break;
        }
        off += box_size;
    }
    None
}

/// mvhd 载荷：4B version/flags + timescale + duration。
fn mvhd_seconds(data: &[u8], p: usize) -> Option<i64> {
    if p + 8 > data.len() {
        return None;
    }
    let version = data[p];
    let (timescale, duration) = if version == 1 {
        // 4 + 8 + 8 + 4(ts) + 8(dur)
        if p + 32 > data.len() {
            return None;
        }
        (be_u32(data, p + 20) as f64, be_u64(data, p + 24) as f64)
    } else {
        // 4 + 4 + 4 + 4(ts) + 4(dur)
        if p + 20 > data.len() {
            return None;
        }
        (be_u32(data, p + 12) as f64, be_u32(data, p + 16) as f64)
    };
    if timescale > 0.0 {
        Some((duration / timescale).round() as i64)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// EBML（MKV/WebM）
// ---------------------------------------------------------------------------

/// 定位 `Info` 元素（ID 0x1549A966，通常紧跟 Segment 头、位于文件开头），
/// 在其子元素中解析 `Duration`（ID 0x4489，8B 浮点毫秒）。
///
/// 采用「直接搜索 + 边界校验」策略：先在文件头 1MB 内定位 Duration ID，
/// 再读取其后的 size（EBML vint）与浮点值。不依赖逐个子元素的精确边界遍历，
/// 因为 Info 内子元素 ID 的 vint 语义与 size 不同，逐个遍历容易在嵌套/未知元素处跳越。
fn parse_mkv(data: &[u8]) -> Option<i64> {
    let info = data
        .windows(4)
        .position(|w| w == [0x15, 0x49, 0xA9, 0x66])?;
    let info_end = (info + 64 * 1024).min(data.len()); // Info 通常很小，放宽到其后 64KB
    let mut search = info;
    while search + 2 <= info_end {
        if data[search] == 0x44 && data[search + 1] == 0x89 {
            let mut q = search + 2;
            let size = read_vint(data, &mut q)?;
            let ms = match size {
                8 if q + 8 <= data.len() => f64::from_be_bytes(data[q..q + 8].try_into().ok()?),
                4 if q + 4 <= data.len() => {
                    f32::from_be_bytes(data[q..q + 4].try_into().ok()?) as f64
                }
                _ => return None,
            };
            if ms > 0.0 {
                return Some((ms / 1000.0).round() as i64);
            }
            return None;
        }
        search += 1;
    }
    None
}

/// EBML 可变长整数：按首字节前导 0 位数决定长度，去掉标记位后取值。
fn read_vint(data: &[u8], off: &mut usize) -> Option<u64> {
    if *off >= data.len() {
        return None;
    }
    let first = data[*off];
    let mut length = 1usize;
    let mut mask = 0x80u8;
    while mask & first == 0 && length < 8 {
        length += 1;
        mask >>= 1;
    }
    if *off + length > data.len() {
        return None;
    }
    let mut value = (first & (mask - 1)) as u64;
    for i in 1..length {
        value = (value << 8) | data[*off + i] as u64;
    }
    *off += length;
    Some(value)
}

// ---------------------------------------------------------------------------
// ffprobe 流信息（ffmpeg-sidecar）
// ---------------------------------------------------------------------------

/// 确保 ffprobe/ffmpeg 可用：已安装则跳过，否则自动下载到程序同目录。
/// 失败仅告警，不中断（流信息退化为空，时长探测仍走头部自解析）。
/// 同步阻塞调用，建议在后台线程执行（见 emrs-server main）。
pub fn ensure_ffmpeg_binary() {
    use ffmpeg_sidecar::download::auto_download;
    if ffprobe_is_installed() {
        return;
    }
    match auto_download() {
        Ok(_) => tracing::info!("ffmpeg/ffprobe 已自动就绪"),
        Err(e) => tracing::warn!(error = %e, "自动下载 ffmpeg 失败，流信息将不可用"),
    }
}

fn ffprobe_is_installed() -> bool {
    ffmpeg_sidecar::ffprobe::ffprobe_is_installed()
}

/// 定位 ffprobe：优先 ffmpeg-sidecar 下载/系统路径，回退在 PATH 中查找。
fn locate_ffprobe() -> Option<std::path::PathBuf> {
    let p = ffmpeg_sidecar::ffprobe::ffprobe_path();
    if p.exists() {
        return Some(p);
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        for name in ["ffprobe", "ffprobe.exe"] {
            let cand = dir.join(name);
            if cand.exists() {
                return Some(cand);
            }
        }
    }
    None
}

/// 用 ffprobe 解析文件流信息（分辨率/编码/帧率/码率/像素格式等）。
/// ffprobe 不可用或解析失败返回空列表（不中断扫描）。
pub async fn probe_streams(path: &Path) -> Vec<StreamInfo> {
    probe_media(path).await.streams
}

/// 用 ffprobe 解析章节（Emby `ChapterInfo` 形状）。ffprobe 不可用返回空列表。
pub async fn probe_chapters(path: &Path) -> Vec<serde_json::Value> {
    probe_media(path).await.chapters
}

/// ffprobe 探测结果（单次调用同时拿流信息 + 章节）。
#[derive(Default)]
pub struct ProbeMedia {
    pub streams: Vec<StreamInfo>,
    /// Emby `ChapterInfo` 形状（`StartPositionTicks` / `Name`）的 JSON 值列表。
    pub chapters: Vec<serde_json::Value>,
    /// ffprobe `format.duration`（秒，四舍五入）。原生头部解析
    /// （[`probe_duration`]）只认 MP4 moov / MKV Duration，fragmented MP4、
    /// TS/AVI/WMV 等容器拿不到时长时以此为回填源。
    pub format_duration: Option<i64>,
}

/// 用 ffprobe 解析流信息 + 章节（`-show_streams -show_format -show_chapters`）。
/// ffprobe 不可用或解析失败返回空结构（不中断扫描）。
pub async fn probe_media(path: &Path) -> ProbeMedia {
    probe_media_checked(path).await.unwrap_or_default()
}

/// 带失败原因的全量探测。`Err(reason)` 表示 ffprobe 缺失 / 执行失败 / 非零退出 /
/// 输出损坏；`Ok` 含"正常执行但未解析出可识别流"的空结果——两者区分开，
/// 供 Probe 阶段写 `media_source.status='ok'/'failed'`。
pub async fn probe_media_checked(path: &Path) -> Result<ProbeMedia, String> {
    let bin = locate_ffprobe().ok_or_else(|| "ffprobe 不可用".to_string())?;
    let output = tokio::process::Command::new(&bin)
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_streams",
            "-show_format",
            "-show_chapters",
        ])
        .arg(path)
        // 丢弃 future（如外层 timeout 触发）时杀掉 ffprobe 子进程，
        // 避免网络挂死的探测进程随调用方 drop 泄漏累积。
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("ffprobe 执行失败: {e}"))?;
    if !output.status.success() {
        return Err(format!("ffprobe 非零退出: {}", output.status));
    }
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|e| format!("ffprobe 输出解析失败: {e}"))?;
    Ok(ProbeMedia {
        streams: parse_streams_json(&json),
        chapters: parse_chapters_json(&json),
        format_duration: parse_format_duration(&json),
    })
}

/// ffprobe `format.duration`（字符串或数字，秒为单位的浮点）→ 四舍五入秒。
/// 原生头部解析覆盖不到的容器（fragmented MP4 / TS / AVI / WMV 等）的时长回填源。
fn parse_format_duration(json: &serde_json::Value) -> Option<i64> {
    let v = json.get("format")?.get("duration")?;
    let secs = match v {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }?;
    if !secs.is_finite() || secs <= 0.0 {
        return None;
    }
    Some(secs.round() as i64)
}

/// ffprobe `-show_chapters` JSON → Emby `ChapterInfo` 数组。
/// 每章 `StartPositionTicks`（秒×10^7）+ `Name`（tags.title，缺省用 "Chapter N"）+
/// `MarkerType: "Chapter"` + `ChapterIndex`（对齐 Emby 真实响应，见 emby_json/PlaybackInfo.json）。
fn parse_chapters_json(json: &serde_json::Value) -> Vec<serde_json::Value> {
    let Some(chapters) = json.get("chapters").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    chapters
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let start_time = c
                .get("start_time")
                .and_then(|v| v.as_str())
                .and_then(|s| s.trim().parse::<f64>().ok())
                .unwrap_or(0.0);
            let name = c
                .get("tags")
                .and_then(|t| t.get("title"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("Chapter {}", i + 1));
            if start_time.is_nan() || start_time.is_infinite() {
                return None;
            }
            Some(serde_json::json!({
                "StartPositionTicks": (start_time * 10_000_000.0) as i64,
                "Name": name,
                "MarkerType": "Chapter",
                "ChapterIndex": i,
            }))
        })
        .collect()
}

/// ffprobe `-show_streams -show_format` JSON → StreamInfo 列表。
fn parse_streams_json(json: &serde_json::Value) -> Vec<StreamInfo> {
    let mut out = Vec::new();
    let Some(streams) = json.get("streams").and_then(|v| v.as_array()) else {
        return out;
    };
    // 容器总码率（format.bit_rate）：流级码率缺失时兜底
    let format_bitrate = json.get("format").and_then(|f| json_i64(f.get("bit_rate")));
    for s in streams {
        let stream_type = match s.get("codec_type").and_then(|v| v.as_str()) {
            Some("video") => "Video",
            Some("audio") => "Audio",
            Some("subtitle") => "Subtitle",
            _ => continue, // 跳过数据/附件等其它流
        };
        let codec = s
            .get("codec_name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // tags：title / language；disposition：default / forced（ffprobe 输出 0/1）
        let tags = s.get("tags");
        let title = tags
            .and_then(|t| t.get("title"))
            .and_then(|v| v.as_str())
            .map(|x| x.to_string());
        let language = tags
            .and_then(|t| t.get("language"))
            .and_then(|v| v.as_str())
            .map(|x| x.to_string());
        let disposition = s.get("disposition");
        let disposition_flag = |name: &str| {
            disposition
                .and_then(|d| d.get(name))
                .and_then(|v| v.as_i64())
                .map(|v| v > 0)
        };
        // 隔行扫描：field_order 非 progressive/unknown 视为隔行
        let is_interlaced = s
            .get("field_order")
            .and_then(|v| v.as_str())
            .map(|f| !matches!(f, "progressive" | "unknown" | ""));
        out.push(StreamInfo {
            stream_type: stream_type.to_string(),
            codec,
            index: s.get("index").and_then(|v| v.as_i64()),
            title,
            language,
            width: s.get("width").and_then(|v| v.as_i64()),
            height: s.get("height").and_then(|v| v.as_i64()),
            frame_rate: s
                .get("r_frame_rate")
                .and_then(|v| v.as_str())
                .and_then(parse_fraction),
            // 流级码率优先，缺失时用容器总码率兜底
            bit_rate: json_i64(s.get("bit_rate")).or(format_bitrate),
            pixel_format: s
                .get("pix_fmt")
                .and_then(|v| v.as_str())
                .map(|x| x.to_string()),
            channels: s.get("channels").and_then(|v| v.as_i64()),
            sample_rate: json_i64(s.get("sample_rate")),
            bit_depth: json_i64(s.get("bits_per_raw_sample"))
                .or_else(|| json_i64(s.get("bits_per_sample"))),
            color_primaries: s
                .get("color_primaries")
                .and_then(|v| v.as_str())
                .map(|x| x.to_string()),
            color_space: s
                .get("color_space")
                .and_then(|v| v.as_str())
                .map(|x| x.to_string()),
            color_transfer: s
                .get("color_transfer")
                .and_then(|v| v.as_str())
                .map(|x| x.to_string()),
            display_aspect_ratio: s
                .get("display_aspect_ratio")
                .and_then(|v| v.as_str())
                .map(|x| x.to_string()),
            is_default: disposition_flag("default"),
            is_forced: disposition_flag("forced"),
            profile: s
                .get("profile")
                .and_then(|v| v.as_str())
                .map(|x| x.to_string()),
            level: s.get("level").and_then(|v| v.as_i64()),
            refs: s.get("refs").and_then(|v| v.as_i64()),
            is_interlaced,
            time_base: s
                .get("time_base")
                .and_then(|v| v.as_str())
                .map(|x| x.to_string()),
            channel_layout: s
                .get("channel_layout")
                .and_then(|v| v.as_str())
                .map(|x| x.to_string()),
            sample_aspect_ratio: s
                .get("sample_aspect_ratio")
                .and_then(|v| v.as_str())
                .map(|x| x.to_string()),
            is_avc: s.get("is_avc").and_then(|v| v.as_i64()).map(|v| v > 0),
        });
    }
    out
}

/// 解析 "num/den" 分数（如 24000/1001）为浮点帧率。
fn parse_fraction(s: &str) -> Option<f64> {
    let (n, d) = s.split_once('/')?;
    let n: f64 = n.trim().parse().ok()?;
    let d: f64 = d.trim().parse().ok()?;
    if d > 0.0 { Some(n / d) } else { None }
}

/// 字段值可能是数字或数字字符串。
fn json_i64(v: Option<&serde_json::Value>) -> Option<i64> {
    match v? {
        serde_json::Value::Number(n) => n.as_i64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// 构造最小 MP4：ftyp + moov(mvhd v0)，返回字节。
    fn minimal_mp4(timescale: u32, duration: u32) -> Vec<u8> {
        let ftyp = {
            let mut b = Vec::new();
            b.extend_from_slice(&20u32.to_be_bytes());
            b.extend_from_slice(b"ftyp");
            b.extend_from_slice(b"isom");
            b.extend_from_slice(&0u32.to_be_bytes());
            b.extend_from_slice(b"isom");
            b
        };
        let mvhd_payload = {
            let mut b = Vec::new();
            b.extend_from_slice(&[0u8, 0, 0, 0]);
            b.extend_from_slice(&0u32.to_be_bytes());
            b.extend_from_slice(&0u32.to_be_bytes());
            b.extend_from_slice(&timescale.to_be_bytes());
            b.extend_from_slice(&duration.to_be_bytes());
            b
        };
        let mvhd = {
            let mut b = Vec::new();
            b.extend_from_slice(&((8 + mvhd_payload.len()) as u32).to_be_bytes());
            b.extend_from_slice(b"mvhd");
            b.extend_from_slice(&mvhd_payload);
            b
        };
        let moov = {
            let mut b = Vec::new();
            b.extend_from_slice(&((8 + mvhd.len()) as u32).to_be_bytes());
            b.extend_from_slice(b"moov");
            b.extend_from_slice(&mvhd);
            b
        };
        let mut file = ftyp;
        file.extend_from_slice(&moov);
        file
    }

    #[test]
    fn ext_and_container() {
        assert!(is_video_ext("mp4"));
        assert!(is_video_ext("mkv"));
        assert!(!is_video_ext("strm"));
        assert!(!is_video_ext("txt"));
        assert_eq!(container_for("mp4"), Some("mp4"));
        assert_eq!(container_for("ts"), Some("mpegts"));
        assert_eq!(container_for("unknown"), None);
    }

    #[test]
    fn mp4_mvhd_v0() {
        let file = minimal_mp4(1000, 120_000);
        assert_eq!(parse_mp4(&file), Some(120));
    }

    #[test]
    fn mp4_no_moov() {
        let file = b"not an mp4 at all, just some random bytes".to_vec();
        assert_eq!(parse_mp4(&file), None);
    }

    #[test]
    fn mkv_duration() {
        // Segment → Info → Duration(0x4489) = 90000ms → 90s
        let duration_elem = {
            let mut b = Vec::new();
            b.extend_from_slice(&[0x44, 0x89]);
            b.push(0x88);
            b.extend_from_slice(&90_000.0f64.to_be_bytes());
            b
        };
        let info = {
            let mut b = Vec::new();
            b.extend_from_slice(&[0x15, 0x49, 0xA9, 0x66]);
            b.push(0x80 | duration_elem.len() as u8);
            b.extend_from_slice(&duration_elem);
            b
        };
        let segment = {
            let mut b = Vec::new();
            b.extend_from_slice(&[0x18, 0x53, 0x80, 0x67]);
            b.push(0x80 | info.len() as u8);
            b.extend_from_slice(&info);
            b
        };
        assert_eq!(parse_mkv(&segment), Some(90));
    }

    #[test]
    fn mkv_duration_f32() {
        // 真实 MKV 常见：Duration 用 4 字节 f32 编码（0x4489 size=0x84）
        let duration_elem = {
            let mut b = Vec::new();
            b.extend_from_slice(&[0x44, 0x89]);
            b.push(0x84); // 4 字节 f32
            b.extend_from_slice(&90_000.0f32.to_be_bytes());
            b
        };
        let info = {
            let mut b = Vec::new();
            b.extend_from_slice(&[0x15, 0x49, 0xA9, 0x66]);
            b.push(0x80 | duration_elem.len() as u8);
            b.extend_from_slice(&duration_elem);
            b
        };
        let segment = {
            let mut b = Vec::new();
            b.extend_from_slice(&[0x18, 0x53, 0x80, 0x67]);
            b.push(0x80 | info.len() as u8);
            b.extend_from_slice(&info);
            b
        };
        assert_eq!(parse_mkv(&segment), Some(90));
    }

    #[tokio::test]
    async fn probe_mp4_file_head() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.mp4");
        let mut f = std::fs::File::create(&path).unwrap();
        let mut file = minimal_mp4(1000, 5000); // 5s
        // 追加 mdat，让 moov 位于文件中部（走头部读取路径）
        file.extend_from_slice(&100u32.to_be_bytes());
        file.extend_from_slice(b"mdat");
        file.resize(file.len() + 92, 0);
        f.write_all(&file).unwrap();
        drop(f);
        assert_eq!(probe_duration(&path).await, Some(5));
    }

    #[tokio::test]
    async fn probe_mp4_file_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.mp4");
        let mut f = std::fs::File::create(&path).unwrap();
        // moov 放在文件尾（模拟非流式优化文件）
        let ftyp = {
            let mut b = Vec::new();
            b.extend_from_slice(&20u32.to_be_bytes());
            b.extend_from_slice(b"ftyp");
            b.extend_from_slice(b"isom");
            b.extend_from_slice(&0u32.to_be_bytes());
            b.extend_from_slice(b"isom");
            b
        };
        let moov = &minimal_mp4(1000, 5000)[20..]; // 取 moov 部分
        let mut file = ftyp;
        // 大 mdat 前置（500KB，超出头部读取窗口，验证尾部路径）
        let mdat_size = 500 * 1024;
        file.extend_from_slice(&(mdat_size as u32).to_be_bytes());
        file.extend_from_slice(b"mdat");
        file.resize(file.len() + mdat_size as usize - 8, 0);
        file.extend_from_slice(moov);
        f.write_all(&file).unwrap();
        drop(f);
        assert_eq!(probe_duration(&path).await, Some(5));
    }

    #[tokio::test]
    async fn probe_unknown_ext() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.avi");
        std::fs::write(&path, b"fake").unwrap();
        assert_eq!(probe_duration(&path).await, None);
    }

    /// ffprobe `-show_streams -show_format` 样例：视频(hevc/bt709/yuv420p10le) + 音频(flac/jpn) + 两条字幕(ass/chi)。
    fn sample_ffprobe_json() -> serde_json::Value {
        serde_json::json!({
            "format": { "bit_rate": "7000000" },
            "streams": [
                {
                    "index": 0,
                    "codec_type": "video",
                    "codec_name": "hevc",
                    "profile": "Main 10",
                    "level": 120,
                    "refs": 1,
                    "width": 1920,
                    "height": 1080,
                    "pix_fmt": "yuv420p10le",
                    "r_frame_rate": "24000/1001",
                    "bit_rate": "6816000",
                    "time_base": "1/1000",
                    "field_order": "progressive",
                    "sample_aspect_ratio": "1:1",
                    "color_primaries": "bt709",
                    "color_space": "bt709",
                    "color_transfer": "bt709",
                    "display_aspect_ratio": "16:9",
                    "disposition": { "default": 1, "forced": 0 },
                    "tags": { "language": "jpn" }
                },
                {
                    "index": 1,
                    "codec_type": "audio",
                    "codec_name": "flac",
                    "profile": "unknown",
                    "channels": 2,
                    "channel_layout": "stereo",
                    "sample_rate": "48000",
                    "bits_per_sample": 24,
                    "time_base": "1/1000",
                    "disposition": { "default": 1, "forced": 0 },
                    "tags": { "language": "jpn" }
                },
                {
                    "index": 2,
                    "codec_type": "subtitle",
                    "codec_name": "ass",
                    "disposition": { "default": 0, "forced": 0 },
                    "tags": { "title": "JPSC", "language": "chi" }
                },
                {
                    "index": 3,
                    "codec_type": "subtitle",
                    "codec_name": "ass",
                    "disposition": { "default": 0, "forced": 0 },
                    "tags": { "title": "JPTC", "language": "chi" }
                }
            ]
        })
    }

    #[test]
    fn streams_video_audio_subtitle_fields() {
        let list = parse_streams_json(&sample_ffprobe_json());
        assert_eq!(list.len(), 4);

        // 视频流：分辨率/码率/帧率/色彩/像素格式/默认
        let v = &list[0];
        assert_eq!(v.stream_type, "Video");
        assert_eq!(v.index, Some(0));
        assert_eq!(v.codec, "hevc");
        assert_eq!(v.width, Some(1920));
        assert_eq!(v.height, Some(1080));
        assert_eq!(v.pixel_format.as_deref(), Some("yuv420p10le"));
        assert_eq!(v.bit_rate, Some(6_816_000));
        assert!(v.frame_rate.is_some());
        assert_eq!(v.color_primaries.as_deref(), Some("bt709"));
        assert_eq!(v.color_space.as_deref(), Some("bt709"));
        assert_eq!(v.color_transfer.as_deref(), Some("bt709"));
        assert_eq!(v.display_aspect_ratio.as_deref(), Some("16:9"));
        assert_eq!(v.is_default, Some(true));
        assert_eq!(v.is_forced, Some(false));
        // 新增字段：档次/级别/参考帧/时间基/变形/隔行
        assert_eq!(v.profile.as_deref(), Some("Main 10"));
        assert_eq!(v.level, Some(120));
        assert_eq!(v.refs, Some(1));
        assert_eq!(v.time_base.as_deref(), Some("1/1000"));
        assert_eq!(v.is_interlaced, Some(false));
        assert_eq!(v.sample_aspect_ratio.as_deref(), Some("1:1"));
        assert_eq!(v.is_avc, None);

        // 音频流：语言/声道/采样/声道布局/时间基
        let a = &list[1];
        assert_eq!(a.stream_type, "Audio");
        assert_eq!(a.index, Some(1));
        assert_eq!(a.language.as_deref(), Some("jpn"));
        assert_eq!(a.channels, Some(2));
        assert_eq!(a.sample_rate, Some(48_000));
        assert_eq!(a.channel_layout.as_deref(), Some("stereo"));
        assert_eq!(a.time_base.as_deref(), Some("1/1000"));
        // 音频流无流级码率 → 回退容器总码率兜底
        assert_eq!(a.bit_rate, Some(7_000_000));

        // 字幕流：不再被跳过，含标题/语言/编码
        let s1 = &list[2];
        assert_eq!(s1.stream_type, "Subtitle");
        assert_eq!(s1.index, Some(2));
        assert_eq!(s1.codec, "ass");
        assert_eq!(s1.title.as_deref(), Some("JPSC"));
        assert_eq!(s1.language.as_deref(), Some("chi"));
        assert_eq!(s1.is_default, Some(false));
        let s2 = &list[3];
        assert_eq!(s2.stream_type, "Subtitle");
        assert_eq!(s2.title.as_deref(), Some("JPTC"));
    }

    #[test]
    fn format_duration_rounds_and_accepts_string_or_number() {
        // ffprobe JSON 的 format.duration 是字符串浮点；容忍数字形态
        let string_form = serde_json::json!({ "format": { "duration": "7123.456000" } });
        assert_eq!(parse_format_duration(&string_form), Some(7123));
        let number_form = serde_json::json!({ "format": { "duration": 123.6 } });
        assert_eq!(parse_format_duration(&number_form), Some(124));
        // 非法/边界：0 与负数无效、缺字段为 None
        let zero = serde_json::json!({ "format": { "duration": "0" } });
        assert_eq!(parse_format_duration(&zero), None);
        let negative = serde_json::json!({ "format": { "duration": "-5.0" } });
        assert_eq!(parse_format_duration(&negative), None);
        let missing = serde_json::json!({ "format": { "bit_rate": "1000" } });
        assert_eq!(parse_format_duration(&missing), None);
        let garbage = serde_json::json!({ "format": { "duration": "N/A" } });
        assert_eq!(parse_format_duration(&garbage), None);
    }

    #[test]
    fn chapters_parsed_to_emby_shape() {
        let json = serde_json::json!({
            "chapters": [
                { "id": 0, "time_base": "1/1000", "start_time": "0.000000",
                  "tags": { "title": "开场" } },
                { "id": 1, "time_base": "1/1000", "start_time": "3600.500000",
                  "tags": { "title": "中场" } },
                { "id": 2, "time_base": "1/1000", "start_time": "7200.000000",
                  "tags": {} }
            ]
        });
        let chapters = parse_chapters_json(&json);
        assert_eq!(chapters.len(), 3);
        assert_eq!(chapters[0]["StartPositionTicks"], 0);
        assert_eq!(chapters[0]["Name"], "开场");
        assert_eq!(chapters[0]["MarkerType"], "Chapter");
        assert_eq!(chapters[0]["ChapterIndex"], 0);
        assert_eq!(chapters[1]["StartPositionTicks"], 36_005_000_000i64);
        assert_eq!(chapters[1]["Name"], "中场");
        assert_eq!(chapters[1]["ChapterIndex"], 1);
        // 无 title → 兜底 "Chapter N"
        assert_eq!(chapters[2]["Name"], "Chapter 3");
        assert_eq!(chapters[2]["ChapterIndex"], 2);
    }

    #[test]
    fn chapters_missing_is_empty() {
        assert!(parse_chapters_json(&serde_json::json!({ "streams": [] })).is_empty());
    }
}
