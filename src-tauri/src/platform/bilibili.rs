//! B 站适配器（M1 只做「监控 + 取流」所需的最小面）。
//!
//! 关键取舍：
//! - 走 room/v1/Room/get_info，而不是 getInfoByRoom：后者不带 buvid 会返回 -352。
//! - 走 getRoomPlayInfo 拿全部档位；登录态缺失时最高只能到 250（超清），
//!   所以请求里带 qn=10000 也只是「期望值」，实际档位以返回的 current_qn 为准。
//! - 候选按 CDN 主机分组（B 站一条流通常给 2 个 host），组内按 qn 排序。

use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use hivecast_ipc::{FollowingPage, FollowingUser, LiveStatus, QualityInfo};
use serde::Deserialize;
use serde_json::json;
use url::Url;

use super::http;
use super::{
    Codec, PlatformAdapter, PlatformError, PlatformId, RoomInfo, RoomRef, StreamCandidate,
    StreamFormat, StreamGroup, StreamPlan,
};

const API: &str = "https://api.live.bilibili.com";
const REFERER: &str = "https://live.bilibili.com/";

/// 清晰度权重（数字与 B 站 qn 一致，越大越好）。
#[allow(dead_code)] // 供 M1.5 的画质协商使用
pub const QN_ORIGIN: u32 = 10000;
/// B 站接口能接受的最大 qn（杜比 30000）；应用层的 40000 只是“最高可用”哨兵。
const QN_MAX: u32 = 30_000;
#[allow(dead_code)]
pub const QN_BLURAY: u32 = 400;
#[allow(dead_code)]
pub const QN_SUPER: u32 = 250;

pub struct BilibiliAdapter;

impl BilibiliAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BilibiliAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct Envelope<T> {
    code: i64,
    message: Option<String>,
    data: Option<T>,
}

fn unwrap_envelope<T>(response: Envelope<T>, what: &str) -> Result<T, PlatformError> {
    match (response.code, response.data) {
        (0, Some(data)) => Ok(data),
        (0, None) => Err(PlatformError::Parse(format!("{what} 返回空数据"))),
        (-400, _) | (-404, _) => Err(PlatformError::RoomNotFound(what.to_string())),
        (code, _) => Err(PlatformError::Upstream {
            code,
            message: response.message.unwrap_or_default(),
        }),
    }
}

/// get_info 在房间不存在时返回 HTTP 200 + { code: 1, data: [] }，
/// data 不是对象。用宽松的 Value 接住再判断，否则会在反序列化阶段
/// 变成「响应解析失败」，既掩盖了真实原因，也把内部 URL 暴露给用户。
#[derive(Debug, Deserialize)]
struct RoomInfoEnvelope {
    code: i64,
    message: Option<String>,
    data: Option<serde_json::Value>,
}

fn parse_room_info(
    envelope: RoomInfoEnvelope,
    room_key: &str,
) -> Result<RoomInfoData, PlatformError> {
    let message = envelope.message.unwrap_or_default();
    if envelope.code != 0 {
        let missing = matches!(envelope.code, -400 | -404)
            || message.contains("未找到")
            || message.contains("不存在");
        return if missing {
            Err(PlatformError::RoomNotFound(room_key.to_string()))
        } else {
            Err(PlatformError::Upstream {
                code: envelope.code,
                message,
            })
        };
    }
    let Some(value) = envelope.data else {
        return Err(PlatformError::Parse(format!("房间 {room_key} 返回空数据")));
    };
    if value.is_null() || value.as_array().is_some_and(|items| items.is_empty()) {
        return Err(PlatformError::RoomNotFound(room_key.to_string()));
    }
    serde_json::from_value(value)
        .map_err(|err| PlatformError::Parse(format!("房间信息格式异常：{err}")))
}

#[derive(Debug, Deserialize)]
struct RoomInfoData {
    room_id: i64,
    #[serde(default)]
    short_id: i64,
    #[serde(default)]
    uid: i64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    live_status: i64,
    #[serde(default)]
    online: i64,
    #[serde(default)]
    area_name: Option<String>,
    #[serde(default)]
    parent_area_name: Option<String>,
    #[serde(default)]
    user_cover: Option<String>,
    #[serde(default)]
    keyframe: Option<String>,
    #[serde(default)]
    live_time: Option<LiveTime>,
    #[serde(default)]
    description: String,
}

/// B 站的 live_time 有时是「2026-11-05 11:59:14」（北京时间），有时是 0/数字。
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum LiveTime {
    Number(i64),
    Text(String),
}

impl LiveTime {
    fn unix(&self) -> Option<i64> {
        match self {
            LiveTime::Number(value) => (*value > 0).then_some(*value),
            LiveTime::Text(text) => crate::clock::parse_cn_naive(text),
        }
    }
}

#[derive(Debug, Deserialize)]
struct AnchorData {
    #[serde(default)]
    info: Option<AnchorInfo>,
}

#[derive(Debug, Deserialize)]
struct AnchorInfo {
    #[serde(default)]
    uname: String,
    #[serde(default)]
    face: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PlayInfoData {
    #[serde(default)]
    live_status: i64,
    #[serde(default)]
    playurl_info: Option<PlayUrlInfo>,
}

#[derive(Debug, Deserialize)]
struct PlayUrlInfo {
    playurl: PlayUrl,
}

#[derive(Debug, Deserialize, Clone)]
struct PlayUrl {
    #[serde(default)]
    stream: Vec<StreamDef>,
    #[serde(default)]
    g_qn_desc: Vec<QnDesc>,
}

#[derive(Debug, Deserialize, Clone)]
struct StreamDef {
    #[serde(default)]
    protocol_name: String,
    #[serde(default)]
    format: Vec<FormatDef>,
}

#[derive(Debug, Deserialize, Clone)]
struct FormatDef {
    #[serde(default)]
    format_name: String,
    #[serde(default)]
    codec: Vec<CodecDef>,
}

#[derive(Debug, Deserialize, Clone)]
struct CodecDef {
    #[serde(default)]
    codec_name: String,
    #[serde(default)]
    current_qn: i64,
    #[serde(default)]
    accept_qn: Vec<i64>,
    #[serde(default)]
    base_url: String,
    #[serde(default)]
    url_info: Vec<UrlInfo>,
    #[serde(default)]
    media_info: Option<MediaInfo>,
}

#[derive(Debug, Deserialize, Clone)]
struct MediaInfo {
    #[serde(default)]
    width: u32,
    #[serde(default)]
    height: u32,
    /// 平台给的实时带宽，单位 byte/s
    #[serde(default)]
    realtime_avg_bw: u64,
}

#[derive(Debug, Deserialize, Clone)]
struct UrlInfo {
    #[serde(default)]
    host: String,
    #[serde(default)]
    extra: String,
}

#[derive(Debug, Deserialize, Clone)]
struct QnDesc {
    qn: i64,
    #[serde(default)]
    desc: String,
}

#[async_trait]
impl PlatformAdapter for BilibiliAdapter {
    fn id(&self) -> PlatformId {
        "bilibili"
    }

    fn display_name(&self) -> &'static str {
        "哔哩哔哩"
    }

    fn matches(&self, url: &Url) -> bool {
        matches!(
            url.host_str(),
            Some("live.bilibili.com") | Some("www.bilibili.com") | Some("b23.tv")
        )
    }

    fn resolve(&self, input: &str) -> Option<RoomRef> {
        let digits = extract_room_number(input)?;
        Some(RoomRef {
            platform: self.id().to_string(),
            canonical_url: format!("https://live.bilibili.com/{digits}"),
            room_key: digits,
            input: input.to_string(),
        })
    }

    async fn room_info(&self, room: &RoomRef) -> Result<RoomInfo, PlatformError> {
        let url = format!("{API}/room/v1/Room/get_info?room_id={}", room.room_key);
        let envelope: RoomInfoEnvelope = http::get_json(&url, REFERER, &[]).await?;
        let data = parse_room_info(envelope, &room.room_key)?;

        // 主播名单独取；失败不影响房间信息，降级成 UID。
        let anchor_url = format!(
            "{API}/live_user/v1/UserInfo/get_anchor_in_room?roomid={}",
            data.room_id
        );
        let anchor = match http::get_json::<Envelope<AnchorData>>(&anchor_url, REFERER, &[]).await {
            Ok(envelope) => unwrap_envelope(envelope, "anchor")
                .ok()
                .and_then(|d| d.info),
            Err(_) => None,
        };

        let host_name = anchor
            .as_ref()
            .map(|info| info.uname.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| format!("主播 {}", data.uid));

        Ok(RoomInfo {
            host_name,
            room_name: data.title,
            avatar_url: anchor.and_then(|info| info.face),
            cover_url: data.user_cover.or(data.keyframe),
            area_name: data.area_name.or(data.parent_area_name),
            is_live: data.live_status == 1,
            started_at: data.live_time.as_ref().and_then(LiveTime::unix),
            online: Some(data.online.max(0) as u64),
            stable_id: Some(data.room_id.to_string()),
            extra: json!({
                "roomId": data.room_id,
                "shortId": data.short_id,
                "uid": data.uid,
                "liveStatus": data.live_status,
                "description": data.description,
            }),
        })
    }

    async fn stream_plan(&self, room: &RoomRef, qn: u32) -> Result<StreamPlan, PlatformError> {
        let requested_qn = request_qn(qn);
        let data = fetch_playinfo(&room.room_key, requested_qn).await?;
        if data.live_status != 1 {
            return Err(PlatformError::NotLive);
        }
        let base = data
            .playurl_info
            .ok_or_else(|| PlatformError::Parse("缺少 playurl_info".into()))?
            .playurl;

        // 线路按用户请求的档位取；能力清单另用 qn=30000 探测一次。
        // 不能拿“当前目标 10000”的响应去判断 2K/4K/杜比是否可用：
        // 登录账号请求 10000 时，平台会把 granted 封在 10000，菜单就会永远把高档标灰。
        let (quality_playurl, probed_high) = if requested_qn < QN_MAX {
            match fetch_playinfo(&room.room_key, QN_MAX).await {
                Ok(high) if high.live_status == 1 => match high.playurl_info {
                    Some(info) => (info.playurl, true),
                    None => (base.clone(), false),
                },
                Ok(_) => (base.clone(), false),
                Err(err) => {
                    tracing::debug!(
                        room = %room.room_key,
                        %err,
                        "B 站高档画质探测失败，回退到请求档位的能力清单"
                    );
                    (base.clone(), false)
                }
            }
        } else {
            // 请求已经是最高档，一次响应同时是线路与能力清单。
            (base.clone(), true)
        };

        let qn_desc: BTreeMap<i64, String> = quality_playurl
            .g_qn_desc
            .iter()
            .map(|item| (item.qn, item.desc.clone()))
            .collect();
        let accepted: BTreeSet<i64> = quality_playurl
            .stream
            .iter()
            .flat_map(|stream| stream.format.iter())
            .flat_map(|format| format.codec.iter())
            .flat_map(|codec| codec.accept_qn.iter().copied())
            .collect();
        // 高档探测响应的 current_qn 才是账号真实上限；未登录会回 250。
        let granted_qn = quality_playurl
            .stream
            .iter()
            .flat_map(|stream| stream.format.iter())
            .flat_map(|format| format.codec.iter())
            .map(|codec| codec.current_qn)
            .max()
            .unwrap_or(0);

        // accept_qn 是按编码列出的，用并集反推每档画质实际支持哪些视频编码。
        let mut qn_codecs: BTreeMap<i64, BTreeSet<&'static str>> = BTreeMap::new();
        for stream in &quality_playurl.stream {
            for format in &stream.format {
                for codec in &format.codec {
                    let codec_kind = map_codec(&codec.codec_name).as_str();
                    for qn in &codec.accept_qn {
                        qn_codecs.entry(*qn).or_default().insert(codec_kind);
                    }
                }
            }
        }

        // key = CDN 主机；保持首次出现顺序作为线路优先级。
        let mut grouped: Vec<(String, Vec<StreamCandidate>)> = Vec::new();
        for stream in &base.stream {
            for format in &stream.format {
                for codec in &format.codec {
                    let format_kind = map_format(&format.format_name);
                    let codec_kind = map_codec(&codec.codec_name);
                    let qn_name = qn_desc
                        .get(&codec.current_qn)
                        .cloned()
                        .unwrap_or_else(|| format!("qn{}", codec.current_qn));

                    for (host_index, info) in codec.url_info.iter().enumerate() {
                        let Some(url) = build_url(&info.host, &codec.base_url, &info.extra) else {
                            continue;
                        };

                        let mut attrs = BTreeMap::new();
                        attrs.insert("qn".to_string(), codec.current_qn.to_string());
                        attrs.insert("qnDesc".to_string(), qn_name.clone());
                        attrs.insert("protocol".to_string(), stream.protocol_name.clone());
                        attrs.insert("format".to_string(), format.format_name.clone());
                        attrs.insert("codec".to_string(), codec.codec_name.clone());
                        attrs.insert(
                            "acceptQn".to_string(),
                            codec
                                .accept_qn
                                .iter()
                                .map(|value| value.to_string())
                                .collect::<Vec<_>>()
                                .join(","),
                        );

                        let mut headers = BTreeMap::new();
                        headers.insert("Referer".to_string(), REFERER.to_string());
                        headers.insert(
                            "Origin".to_string(),
                            "https://live.bilibili.com".to_string(),
                        );
                        headers.insert("User-Agent".to_string(), http::USER_AGENT.to_string());

                        let candidate = StreamCandidate {
                            id: format!(
                                "{}-{}-{}-{}",
                                codec.current_qn, format.format_name, codec.codec_name, host_index
                            ),
                            name: format!(
                                "{} · {} · {}",
                                qn_name,
                                display_format(format_kind, &format.format_name),
                                codec_label(codec_kind)
                            ),
                            weight: codec.current_qn as f64,
                            format: format_kind,
                            codec: Some(codec_kind),
                            width: codec
                                .media_info
                                .as_ref()
                                .map(|info| info.width)
                                .filter(|w| *w > 0),
                            height: codec
                                .media_info
                                .as_ref()
                                .map(|info| info.height)
                                .filter(|h| *h > 0),
                            fps: None,
                            bitrate_kbps: codec
                                .media_info
                                .as_ref()
                                .map(|info| (info.realtime_avg_bw * 8 / 1000) as u32)
                                .filter(|kbps| *kbps > 0),
                            url: Some(url),
                            headers,
                            attrs,
                        };

                        let host_key = host_label(&info.host);
                        match grouped.iter_mut().find(|(name, _)| name == &host_key) {
                            Some((_, list)) => list.push(candidate),
                            None => grouped.push((host_key, vec![candidate])),
                        }
                    }
                }
            }
        }

        let mut groups: Vec<StreamGroup> = grouped
            .into_iter()
            .enumerate()
            .map(|(index, (name, mut candidates))| {
                // 组内：先按清晰度降序，同清晰度优先 H.264（兼容性最好，录制可直接 copy）。
                candidates.sort_by(|a, b| {
                    b.weight
                        .partial_cmp(&a.weight)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| codec_rank(a.codec).cmp(&codec_rank(b.codec)))
                });
                StreamGroup {
                    name,
                    priority: index as u32,
                    candidates,
                }
            })
            .collect();

        if groups.is_empty() {
            return Err(PlatformError::Parse("没有可用线路".into()));
        }
        // 组间：FLV 线路优先（录制最稳），其次按平台给的顺序。
        groups.sort_by(|a, b| {
            group_rank(a)
                .cmp(&group_rank(b))
                .then_with(|| a.priority.cmp(&b.priority))
        });
        for (index, group) in groups.iter_mut().enumerate() {
            group.priority = index as u32;
        }

        let qualities = build_qualities(&qn_desc, &accepted, &qn_codecs, granted_qn, probed_high);

        Ok(StreamPlan { groups, qualities })
    }
}

async fn fetch_playinfo(room_key: &str, qn: u32) -> Result<PlayInfoData, PlatformError> {
    let url = format!(
        "{API}/xlive/web-room/v2/index/getRoomPlayInfo?room_id={room_key}&protocol=0,1&format=0,1,2&codec=0,1,2&qn={qn}&platform=web&ptype=8&dolby=5&panorama=1"
    );
    let envelope: Envelope<PlayInfoData> = http::get_json(&url, REFERER, &[]).await?;
    unwrap_envelope(envelope, room_key)
}

/// 从能力清单构造房间级画质列表。
///
/// `probed_high` 为 true 时，`granted_qn` 来自 qn=30000 的探测响应，
/// 可以直接用 `qn <= granted` 判断；探测失败时退回旧规则，避免把未知档位误标成不可用。
fn build_qualities(
    qn_desc: &BTreeMap<i64, String>,
    accepted: &BTreeSet<i64>,
    qn_codecs: &BTreeMap<i64, BTreeSet<&'static str>>,
    granted_qn: i64,
    probed_high: bool,
) -> Vec<QualityInfo> {
    let codec_for = |qn: &i64| -> Option<String> {
        qn_codecs
            .get(qn)
            .and_then(|set| {
                if set.contains("h264") {
                    Some("h264")
                } else if set.contains("h265") {
                    Some("h265")
                } else if set.contains("av1") {
                    Some("av1")
                } else {
                    None
                }
            })
            .map(str::to_string)
    };
    let mut qualities: Vec<QualityInfo> = qn_desc
        .iter()
        .filter(|(qn, _)| **qn > 0)
        .map(|(qn, desc)| {
            let codec = codec_for(qn);
            QualityInfo {
                qn: *qn as u32,
                label: desc.clone(),
                hint: quality_hint(desc, codec.as_deref()),
                available: quality_available(*qn, accepted, granted_qn, probed_high),
                codec,
            }
        })
        .collect();
    // 平台有时只把新档位放进 accept_qn、不补 g_qn_desc；合并两份数据避免漏档。
    let known: BTreeSet<u32> = qualities.iter().map(|quality| quality.qn).collect();
    for qn in accepted.iter().filter(|qn| **qn > 0) {
        let value = *qn as u32;
        if known.contains(&value) {
            continue;
        }
        let codec = codec_for(qn);
        qualities.push(QualityInfo {
            qn: value,
            label: format!("qn{qn}"),
            hint: quality_hint("", codec.as_deref()),
            available: quality_available(*qn, accepted, granted_qn, probed_high),
            codec,
        });
    }
    qualities.sort_by_key(|quality| std::cmp::Reverse(quality.qn));
    qualities
}

fn quality_hint(desc: &str, codec: Option<&str>) -> Option<String> {
    // 杜比是画质语义，优先于编码；否则 30000 会被标成 HEVC，用户看不出是杜比。
    if desc.contains("杜比") {
        return Some("杜比".to_string());
    }
    match codec {
        Some("h265") => Some("HEVC".to_string()),
        Some("av1") => Some("AV1".to_string()),
        _ => None,
    }
}

/// 画质是否真的可选。
///
/// `accept_qn` 是平台给的编码能力清单，未登录时也会列出 4K/原画；
/// 高档探测的 `current_qn` 才是账号当前的实际上限。
fn quality_available(qn: i64, accepted: &BTreeSet<i64>, granted: i64, probed_high: bool) -> bool {
    if !accepted.contains(&qn) {
        return false;
    }
    if probed_high {
        qn <= granted
    } else {
        granted >= QN_ORIGIN as i64 || qn <= granted
    }
}

/// b23.tv 分享短链需要先跟随 302 才能拿到真实房间号。
/// 只接受 b23 入口，且最终必须仍落在 B 站域名，避免把任意 URL 当直播链接。
pub async fn expand_short_link(input: &str) -> Option<String> {
    let url = Url::parse(input.trim()).ok()?;
    if !is_short_link(&url) {
        return None;
    }
    let response = crate::platform::http::client().get(url).send().await.ok()?;
    let final_url = response.url().clone();
    let host = final_url.host_str()?;
    if !matches!(
        host,
        "live.bilibili.com" | "www.bilibili.com" | "bilibili.com"
    ) {
        return None;
    }
    Some(final_url.to_string())
}

fn is_short_link(url: &Url) -> bool {
    url.host_str() == Some("b23.tv")
}

const FOLLOW_API: &str = "https://api.bilibili.com";
const CENTER_REFERER: &str = "https://space.bilibili.com/";
const FOLLOW_PAGE_SIZE: u32 = 50;
const STATUS_CHUNK: usize = 200;

#[derive(Debug, Deserialize)]
struct FollowingData {
    #[serde(default)]
    total: u64,
    #[serde(default)]
    list: Vec<FollowingItem>,
}

#[derive(Debug, Deserialize)]
struct FollowingItem {
    #[serde(default)]
    mid: i64,
    #[serde(default)]
    uname: String,
    #[serde(default)]
    face: String,
}

#[derive(Debug, Deserialize)]
struct StatusItem {
    #[serde(default)]
    uid: i64,
    #[serde(default)]
    room_id: i64,
    #[serde(default)]
    short_id: i64,
    #[serde(default)]
    uname: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    face: String,
    #[serde(default)]
    cover_from_user: String,
    #[serde(default)]
    keyframe: String,
    #[serde(default)]
    area_name: String,
    #[serde(default)]
    live_status: i64,
    #[serde(default)]
    live_time: Option<LiveTime>,
    #[serde(default)]
    online: i64,
}

fn non_empty(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

fn positive_id(value: i64) -> Option<String> {
    (value > 0).then(|| value.to_string())
}

/// 批量接口返回的是以 UID 为键的对象；个别条目缺 uid 时才用键兜底。
///
/// live_time 在状态接口里是 Unix 秒（实测与 get_info 一致），0 表示没有开播时间；
/// 这里保留为 started_at，快速下播重开时监控才能识别出新一场。
fn status_from_item(key: String, item: StatusItem) -> LiveStatus {
    let uid = if item.uid > 0 {
        item.uid.to_string()
    } else {
        key
    };
    LiveStatus {
        uid,
        room_id: positive_id(item.room_id),
        short_id: positive_id(item.short_id),
        uname: item.uname,
        title: item.title,
        avatar_url: non_empty(item.face),
        cover_url: non_empty(item.cover_from_user).or_else(|| non_empty(item.keyframe)),
        area_name: non_empty(item.area_name),
        is_live: item.live_status == 1,
        video_loop: false,
        // 只有本场正在直播时，live_time 才是本场开播时间；离线房间即使
        // 返回了上一次的开播时间，也不能把它当成当前场次。
        started_at: if item.live_status == 1 {
            item.live_time.as_ref().and_then(LiveTime::unix)
        } else {
            None
        },
        online: (item.online >= 0).then_some(item.online as u64),
    }
}

/// 拉取一页关注列表。B 站每页最多 50 条，调用方负责翻页。
pub async fn fetch_followings(uid: &str, page: u32) -> Result<FollowingPage, PlatformError> {
    let page = page.max(1);
    let url = format!(
        "{FOLLOW_API}/x/relation/followings?vmid={uid}&pn={page}&ps={FOLLOW_PAGE_SIZE}&order=desc&order_type=attention"
    );
    let envelope: Envelope<FollowingData> = http::get_json(&url, CENTER_REFERER, &[]).await?;
    let data = unwrap_envelope(envelope, "关注列表")?;
    Ok(FollowingPage {
        total: data.total,
        page,
        page_size: FOLLOW_PAGE_SIZE,
        items: data
            .list
            .into_iter()
            .filter(|item| item.mid > 0)
            .map(|item| FollowingUser {
                uid: item.mid.to_string(),
                uname: item.uname,
                avatar_url: non_empty(item.face),
            })
            .collect(),
    })
}

async fn fetch_status_chunk(uids: &[String]) -> Result<Vec<LiveStatus>, PlatformError> {
    let url = format!("{API}/room/v1/Room/get_status_info_by_uids");
    let body = json!({ "uids": uids });
    let envelope: Envelope<BTreeMap<String, StatusItem>> =
        http::post_json(&url, REFERER, "https://live.bilibili.com", &body, &[])
            .await?
            .data;
    let data = unwrap_envelope(envelope, "直播状态")?;
    Ok(data
        .into_iter()
        .map(|(key, item)| status_from_item(key, item))
        .collect())
}

/// 按 UID 批量查询直播状态；单次最多 200 个，失败重试一次。
///
/// 返回里缺失某个 UID 只表示「没有可用的直播间信息」，不能当成离线。
pub async fn fetch_live_statuses(uids: &[String]) -> Result<Vec<LiveStatus>, PlatformError> {
    let mut unique: Vec<String> = uids
        .iter()
        .map(|uid| uid.trim().to_string())
        .filter(|uid| !uid.is_empty())
        .collect();
    unique.sort();
    unique.dedup();

    // 每一片都独立并发：5000 个 UID 是 25 片，串行时单片一次 400ms 重试
    // 就会把整批拖成十几秒。片内仍然按接口要求最多 200 个。
    let results =
        futures_util::future::join_all(unique.chunks(STATUS_CHUNK).map(|chunk| async move {
            match fetch_status_chunk(chunk).await {
                Ok(statuses) => Ok(statuses),
                Err(first) => {
                    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                    match fetch_status_chunk(chunk).await {
                        Ok(statuses) => Ok(statuses),
                        Err(_) => Err(first),
                    }
                }
            }
        }))
        .await;
    let mut out = Vec::with_capacity(unique.len());
    for result in results {
        out.append(&mut result?);
    }
    Ok(out)
}

fn extract_room_number(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.chars().all(|ch| ch.is_ascii_digit()) {
        return Some(trimmed.to_string());
    }

    // 只处理 B 站域名；URL 里取最后一个纯数字路径段（兼容 /blanc/123、/123?x=1）
    let url = Url::parse(trimmed).ok()?;
    if !matches!(
        url.host_str(),
        Some("live.bilibili.com") | Some("www.bilibili.com") | Some("b23.tv")
    ) {
        return None;
    }
    url.path_segments()?
        .filter(|segment| !segment.is_empty())
        .rfind(|segment| segment.chars().all(|ch| ch.is_ascii_digit()))
        .map(|segment| segment.to_string())
}

fn build_url(host: &str, base_url: &str, extra: &str) -> Option<String> {
    if host.is_empty() || base_url.is_empty() {
        return None;
    }
    Some(format!("{host}{base_url}{extra}"))
}

/// 把 "https://d1--cn-gotcha09.bilivideo.com" 收敛成可读线路名。
fn host_label(host: &str) -> String {
    Url::parse(host)
        .ok()
        .and_then(|url| url.host_str().map(|value| value.to_string()))
        .unwrap_or_else(|| host.to_string())
}

/// 应用层用 40000 表示「最高可用」；B 站接口最高接受 30000（杜比）。
/// 实测 qn=30000 会按账号/房间权限回退到 15000 或 25000，是安全的最高探测值。
fn request_qn(qn: u32) -> u32 {
    if qn == 40_000 {
        QN_MAX
    } else {
        qn
    }
}

fn map_format(format: &str) -> StreamFormat {
    match format {
        "flv" => StreamFormat::Flv,
        "fmp4" => StreamFormat::Hls,
        // B 站的 HLS/TS 也是 MPEG-TS 源流：既算 HLS，也要能命中「TS」偏好。
        // 之前把它归成 Hls，用户选 TS 时永远匹配不上，最后会回退到 FLV。
        "ts" => StreamFormat::HttpTs,
        _ => StreamFormat::Hls,
    }
}

fn display_format(kind: StreamFormat, raw: &str) -> String {
    match kind {
        StreamFormat::Flv => "FLV".to_string(),
        StreamFormat::Hls if raw == "fmp4" => "HLS/fMP4".to_string(),
        StreamFormat::Hls => "HLS/TS".to_string(),
        _ => raw.to_uppercase(),
    }
}

fn codec_label(codec: Codec) -> &'static str {
    match codec {
        Codec::H264 => "H.264",
        Codec::H265 => "HEVC",
        Codec::Av1 => "AV1",
        Codec::Aac => "AAC",
        Codec::Unknown => "未知编码",
    }
}

fn codec_rank(codec: Option<Codec>) -> u8 {
    match codec {
        Some(Codec::H264) => 0,
        Some(Codec::H265) => 1,
        Some(Codec::Av1) => 2,
        _ => 3,
    }
}

fn group_rank(group: &StreamGroup) -> u8 {
    if group
        .candidates
        .iter()
        .any(|candidate| candidate.format == StreamFormat::Flv)
    {
        0
    } else {
        1
    }
}

fn map_codec(name: &str) -> Codec {
    match name {
        "avc" => Codec::H264,
        "hevc" => Codec::H265,
        "av1" => Codec::Av1,
        _ => Codec::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hls_ts_is_a_ts_source_not_a_plain_hls_source() {
        assert_eq!(map_format("flv"), StreamFormat::Flv);
        assert_eq!(map_format("fmp4"), StreamFormat::Hls);
        assert_eq!(map_format("ts"), StreamFormat::HttpTs);
    }

    #[test]
    fn missing_room_maps_to_room_not_found() {
        // 真实接口：房间不存在时 HTTP 200，code=1，data=[]。
        let envelope: RoomInfoEnvelope =
            serde_json::from_str(r#"{"code":1,"message":"未找到该房间","data":[]}"#)
                .expect("应能反序列化");
        let err = parse_room_info(envelope, "999999999").expect_err("应判定房间不存在");
        assert!(matches!(err, PlatformError::RoomNotFound(_)));
        assert_eq!(err.code(), "room_not_found");
        assert!(
            !err.to_string().contains("http"),
            "用户错误文案不应带内部接口 URL"
        );
    }

    #[test]
    fn room_info_accepts_object_payload() {
        let envelope: RoomInfoEnvelope = serde_json::from_str(
            r#"{"code":0,"message":"0","data":{"room_id":7734200,"uid":1794315001,"title":"测试"}}"#,
        )
        .expect("应能反序列化");
        let info = parse_room_info(envelope, "6").expect("正常房间应解析成功");
        assert_eq!(info.room_id, 7734200);
        assert_eq!(info.uid, 1794315001);
    }

    #[test]
    fn maps_highest_sentinel_to_dolby_probe() {
        assert_eq!(request_qn(40_000), QN_MAX);
        assert_eq!(request_qn(QN_MAX), QN_MAX);
        assert_eq!(request_qn(20_000), 20_000);
        assert_eq!(request_qn(QN_ORIGIN), QN_ORIGIN);
        assert_eq!(request_qn(0), 0);
    }

    #[test]
    fn caps_quality_availability_to_high_probe_grant() {
        let accepted = BTreeSet::from([25_000, 15_000, 10_000, 400, 250, 150]);
        assert!(quality_available(25_000, &accepted, 25_000, true));
        assert!(quality_available(15_000, &accepted, 25_000, true));
        assert!(quality_available(10_000, &accepted, 10_000, true));
        assert!(!quality_available(15_000, &accepted, 10_000, true));
        assert!(!quality_available(15_000, &accepted, 250, true));
        assert!(!quality_available(400, &accepted, 250, true));
        assert!(quality_available(250, &accepted, 250, true));
        // 高档探测失败时退回旧规则，不能把未知的高档直接标灰。
        assert!(quality_available(15_000, &accepted, 10_000, false));
        assert!(!quality_available(15_000, &accepted, 250, false));
    }

    #[test]
    fn builds_room_catalog_from_high_probe() {
        let qn_desc = BTreeMap::from([
            (30_000, "杜比".to_string()),
            (25_000, "原画真彩".to_string()),
            (15_000, "2K".to_string()),
            (10_000, "原画".to_string()),
            (250, "超清".to_string()),
        ]);
        let accepted = BTreeSet::from([30_000, 25_000, 15_000, 10_000, 250]);
        let codecs = BTreeMap::from([
            (30_000, BTreeSet::from(["h265"])),
            (25_000, BTreeSet::from(["h265"])),
            (15_000, BTreeSet::from(["h264"])),
            (10_000, BTreeSet::from(["h264", "h265", "av1"])),
            (250, BTreeSet::from(["h264"])),
        ]);
        let qualities = build_qualities(&qn_desc, &accepted, &codecs, 25_000, true);
        assert_eq!(qualities.first().map(|q| q.qn), Some(30_000));
        let by_qn = |qn: u32| qualities.iter().find(|q| q.qn == qn).expect("档位应存在");
        assert_eq!(by_qn(30_000).hint.as_deref(), Some("杜比"));
        assert!(!by_qn(30_000).available, "超过探测到的 25000 上限");
        assert!(by_qn(25_000).available);
        assert!(by_qn(15_000).available);
        assert_eq!(by_qn(10_000).codec.as_deref(), Some("h264"));
    }

    #[test]
    fn maps_batch_status_and_falls_back_to_the_uid_key() {
        let status = status_from_item(
            "352130563".into(),
            StatusItem {
                uid: 0,
                room_id: 0,
                short_id: 42,
                uname: "执事只食芝士".into(),
                title: "测试直播".into(),
                face: String::new(),
                cover_from_user: String::new(),
                keyframe: "https://i0.hdslb.com/keyframe.jpg".into(),
                area_name: "虚拟主播".into(),
                live_status: 1,
                online: -1,
                live_time: Some(LiveTime::Number(1_792_050_615)),
            },
        );
        assert_eq!(status.uid, "352130563");
        assert_eq!(status.room_id, None);
        assert_eq!(status.short_id.as_deref(), Some("42"));
        assert_eq!(
            status.cover_url.as_deref(),
            Some("https://i0.hdslb.com/keyframe.jpg")
        );
        assert_eq!(status.avatar_url, None);
        assert_eq!(status.online, None);
        assert_eq!(status.started_at, Some(1_792_050_615));
        assert!(status.is_live);

        let explicit = status_from_item(
            "fallback".into(),
            StatusItem {
                uid: 1794315001,
                room_id: 7734200,
                short_id: 6,
                uname: "LPL".into(),
                title: String::new(),
                face: "https://i0.hdslb.com/face.jpg".into(),
                cover_from_user: "https://i0.hdslb.com/cover.jpg".into(),
                keyframe: "https://i0.hdslb.com/keyframe.jpg".into(),
                area_name: String::new(),
                live_status: 0,
                online: 12345,
                live_time: Some(LiveTime::Number(1_700_000_000)),
            },
        );
        assert_eq!(explicit.uid, "1794315001");
        assert_eq!(explicit.room_id.as_deref(), Some("7734200"));
        assert_eq!(
            explicit.cover_url.as_deref(),
            Some("https://i0.hdslb.com/cover.jpg")
        );
        assert_eq!(explicit.online, Some(12345));
        assert_eq!(
            explicit.started_at, None,
            "离线房间的 live_time 可能是上一场，不能当成本场开播时间"
        );
        assert!(!explicit.is_live);
    }

    #[test]
    fn resolves_plain_room_id_and_url() {
        let adapter = BilibiliAdapter::new();
        assert_eq!(adapter.resolve("545068").unwrap().room_key, "545068");
        assert_eq!(adapter.resolve("7777").unwrap().room_key, "7777");
        assert_eq!(
            adapter
                .resolve("https://live.bilibili.com/545068?spm_id_from=333")
                .unwrap()
                .room_key,
            "545068"
        );
        assert_eq!(
            adapter
                .resolve("https://live.bilibili.com/blanc/545068")
                .unwrap()
                .room_key,
            "545068"
        );
        assert!(adapter.resolve("https://www.douyu.com/9999").is_none());
    }

    #[test]
    fn only_b23_is_treated_as_short_link() {
        assert!(is_short_link(&Url::parse("https://b23.tv/abc123").unwrap()));
        assert!(!is_short_link(
            &Url::parse("https://live.bilibili.com/6").unwrap()
        ));
    }

    /// 联网冒烟测试：cargo test --lib -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "需要访问 B 站接口"]
    async fn live_room_probe() {
        let adapter = BilibiliAdapter::new();
        let room = adapter.resolve("545068").expect("解析房间号失败");
        let info = adapter.room_info(&room).await.expect("取房间信息失败");
        println!(
            "room: {} / {} live={}",
            info.host_name, info.room_name, info.is_live
        );

        match adapter.stream_groups(&room, QN_ORIGIN).await {
            Ok(groups) => {
                println!("groups={}", groups.len());
                for group in groups.iter().take(3) {
                    println!(
                        "  line {} priority {} candidates {}",
                        group.name,
                        group.priority,
                        group.candidates.len()
                    );
                    for candidate in group.candidates.iter().take(3) {
                        println!(
                            "    {} weight={} url={}",
                            candidate.name,
                            candidate.weight,
                            candidate.url.as_deref().unwrap_or("")
                        );
                    }
                }
                assert!(!groups.is_empty());
            }
            Err(PlatformError::NotLive) => println!("房间当前未开播，跳过流候选断言"),
            Err(err) => panic!("取流失败: {err}"),
        }
    }
}
