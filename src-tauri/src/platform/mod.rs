//! 平台适配层：只做「用户输入 → 房间引用 → 房间信息 / 流候选」，
//! 不打开连接、不写文件、不碰数据库与 UI（见 docs/04）。

pub mod abogus;
pub mod bilibili;
pub mod douyin;
pub mod douyin_sign;
pub mod douyu;
pub mod http;
pub mod huya;

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use serde::Serialize;
use url::Url;

pub type PlatformId = &'static str;

/// 房间引用：由用户输入解析出来的稳定标识，纯本地解析、不联网。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomRef {
    pub platform: String,
    /// 平台内稳定标识（B 站为真实 room_id 或短号）
    pub room_key: String,
    /// 规范化 URL（去掉追踪参数）
    pub canonical_url: String,
    /// 用户原始输入，用于展示与排障
    pub input: String,
}

/// 房间信息：随开播状态变化，可缓存。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomInfo {
    pub host_name: String,
    pub room_name: String,
    pub avatar_url: Option<String>,
    pub cover_url: Option<String>,
    pub area_name: Option<String>,
    pub is_live: bool,
    /// 开播时间（Unix 秒）
    pub started_at: Option<i64>,
    pub online: Option<u64>,
    /// 平台原始附加字段，原样透传
    pub extra: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)] // Dash / AudioOnly 为 M2 起的封装预留
pub enum StreamFormat {
    Flv,
    Hls,
    Dash,
    HttpTs,
    AudioOnly,
}

impl StreamFormat {
    #[allow(dead_code)]
    pub fn as_str(self) -> &'static str {
        match self {
            StreamFormat::Flv => "flv",
            StreamFormat::Hls => "hls",
            StreamFormat::Dash => "dash",
            StreamFormat::HttpTs => "ts",
            StreamFormat::AudioOnly => "audio",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
pub enum Codec {
    H264,
    H265,
    Av1,
    Aac,
    Unknown,
}

impl Codec {
    #[allow(dead_code)]
    pub fn as_str(self) -> &'static str {
        match self {
            Codec::H264 => "h264",
            Codec::H265 => "h265",
            Codec::Av1 => "av1",
            Codec::Aac => "aac",
            Codec::Unknown => "unknown",
        }
    }
}

/// 流候选：一个可直接交给下载器/播放器的地址。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamCandidate {
    /// 稳定 id，用于记住「用户上次选的档位」
    pub id: String,
    /// 展示名，平台原名优先
    pub name: String,
    /// 权重，越大越好；只在同一线路组内比较
    pub weight: f64,
    pub format: StreamFormat,
    pub codec: Option<Codec>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f32>,
    pub bitrate_kbps: Option<u32>,
    pub url: Option<String>,
    /// 下载时必须带的请求头
    pub headers: BTreeMap<String, String>,
    /// 平台附加属性（qn、协议、编码原名等）
    pub attrs: BTreeMap<String, String>,
}

/// 按线路（CDN）分组的候选集合。组间是不同线路，组内按清晰度排序。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamGroup {
    pub name: String,
    pub priority: u32,
    pub candidates: Vec<StreamCandidate>,
}

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("不支持的平台或输入：{0}")]
    Unsupported(String),
    #[error("房间不存在：{0}")]
    RoomNotFound(String),
    #[error("房间未开播")]
    NotLive,
    #[error("平台返回错误 {code}：{message}")]
    Upstream { code: i64, message: String },
    #[error("网络请求失败：{0}")]
    Network(String),
    #[error("响应解析失败：{0}")]
    Parse(String),
    /// broker 本地冷却：这次请求根本没出门，按 retry_after 秒后重试即可。
    /// 它不是平台返回的错误，调用方不该把它计入失败次数。
    #[error("平台请求过快，请 {retry_after} 秒后重试")]
    Throttled { retry_after: u64 },
}

impl PlatformError {
    /// 应用级错误码，供前端与日志使用。
    pub fn code(&self) -> &'static str {
        match self {
            PlatformError::Unsupported(_) => "platform_unsupported",
            PlatformError::RoomNotFound(_) => "room_not_found",
            PlatformError::NotLive => "room_not_live",
            PlatformError::Upstream { .. } => "platform_upstream",
            PlatformError::Network(_) => "network",
            PlatformError::Parse(_) => "parse",
            PlatformError::Throttled { .. } => "platform_throttled",
        }
    }

    /// broker 冷却剩余秒数；只有本地合成的冷却错误才有值。
    pub fn retry_after(&self) -> Option<u64> {
        match self {
            PlatformError::Throttled { retry_after } => Some(*retry_after),
            _ => None,
        }
    }

    /// 限流或网络抖动：值得退避后再试，而不是当成房间本身出错。
    pub fn is_throttle(&self) -> bool {
        match self {
            PlatformError::Network(_) => true,
            PlatformError::Upstream { code, .. } => {
                matches!(*code, -352 | -412 | -429 | 412 | 429)
            }
            PlatformError::Throttled { .. } => true,
            _ => false,
        }
    }
}

#[async_trait]
pub trait PlatformAdapter: Send + Sync {
    fn id(&self) -> PlatformId;
    #[allow(dead_code)]
    fn display_name(&self) -> &'static str;
    /// 域名/URL 是否属于本平台
    fn matches(&self, url: &Url) -> bool;
    /// 纯本地解析：观众可零网络把房间加进列表
    fn resolve(&self, input: &str) -> Option<RoomRef>;
    async fn room_info(&self, room: &RoomRef) -> Result<RoomInfo, PlatformError>;
    /// qn 为期望清晰度，平台可能返回更低的档位（未登录/无权限）
    async fn stream_groups(
        &self,
        room: &RoomRef,
        qn: u32,
    ) -> Result<Vec<StreamGroup>, PlatformError>;
}

fn registry() -> &'static Vec<Arc<dyn PlatformAdapter>> {
    static REGISTRY: OnceLock<Vec<Arc<dyn PlatformAdapter>>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        vec![
            Arc::new(bilibili::BilibiliAdapter::new()) as Arc<dyn PlatformAdapter>,
            Arc::new(douyu::DouyuAdapter::new()) as Arc<dyn PlatformAdapter>,
            Arc::new(huya::HuyaAdapter::new()),
            Arc::new(douyin::DouyinAdapter::new()),
        ]
    })
}

/// 把用户输入（房间号 / 短号 / 直播链接）解析成房间引用。
pub fn resolve_input(input: &str) -> Result<RoomRef, PlatformError> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err(PlatformError::Unsupported("输入为空".into()));
    }

    let parsed = Url::parse(raw).ok();
    if let Some(url) = &parsed {
        for adapter in registry() {
            if adapter.matches(url) {
                return adapter
                    .resolve(raw)
                    .ok_or_else(|| PlatformError::Unsupported(raw.to_string()));
            }
        }
        return Err(PlatformError::Unsupported(format!(
            "暂不支持的直播地址：{raw}"
        )));
    }

    for adapter in registry() {
        if let Some(room) = adapter.resolve(raw) {
            return Ok(room);
        }
    }
    Err(PlatformError::Unsupported(format!("无法识别的输入：{raw}")))
}

/// 按平台取适配器；未知平台返回错误而不是 panic。
pub fn adapter_for(platform: &str) -> Result<Arc<dyn PlatformAdapter>, PlatformError> {
    registry()
        .iter()
        .find(|adapter| adapter.id() == platform)
        .cloned()
        .ok_or_else(|| PlatformError::Unsupported(platform.to_string()))
}
