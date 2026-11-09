//! B 站适配器（M1 只做「监控 + 取流」所需的最小面）。
//!
//! 关键取舍：
//! - 走 room/v1/Room/get_info，而不是 getInfoByRoom：后者不带 buvid 会返回 -352。
//! - 走 getRoomPlayInfo 拿全部档位；登录态缺失时最高只能到 250（超清），
//!   所以请求里带 qn=10000 也只是「期望值」，实际档位以返回的 current_qn 为准。
//! - 候选按 CDN 主机分组（B 站一条流通常给 2 个 host），组内按 qn 排序。

use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use hivecast_ipc::QualityInfo;
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
            LiveTime::Text(text) => {
                let naive =
                    chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S").ok()?;
                let offset = chrono::FixedOffset::east_opt(8 * 3600)?;
                Some(naive.and_local_timezone(offset).single()?.timestamp())
            }
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

#[derive(Debug, Deserialize)]
struct PlayUrl {
    #[serde(default)]
    stream: Vec<StreamDef>,
    #[serde(default)]
    g_qn_desc: Vec<QnDesc>,
}

#[derive(Debug, Deserialize)]
struct StreamDef {
    #[serde(default)]
    protocol_name: String,
    #[serde(default)]
    format: Vec<FormatDef>,
}

#[derive(Debug, Deserialize)]
struct FormatDef {
    #[serde(default)]
    format_name: String,
    #[serde(default)]
    codec: Vec<CodecDef>,
}

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Deserialize)]
struct MediaInfo {
    #[serde(default)]
    width: u32,
    #[serde(default)]
    height: u32,
    /// 平台给的实时带宽，单位 byte/s
    #[serde(default)]
    realtime_avg_bw: u64,
}

#[derive(Debug, Deserialize)]
struct UrlInfo {
    #[serde(default)]
    host: String,
    #[serde(default)]
    extra: String,
}

#[derive(Debug, Deserialize)]
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
        let envelope: Envelope<RoomInfoData> = http::get_json(&url, REFERER, &[]).await?;
        let data = unwrap_envelope(envelope, &room.room_key)?;

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
        let qn = request_qn(qn);
        let url = format!(
            "{API}/xlive/web-room/v2/index/getRoomPlayInfo?room_id={}&protocol=0,1&format=0,1,2&codec=0,1&qn={qn}&platform=web&ptype=8&dolby=5&panorama=1",
            room.room_key
        );
        let envelope: Envelope<PlayInfoData> = http::get_json(&url, REFERER, &[]).await?;
        let data = unwrap_envelope(envelope, &room.room_key)?;

        if data.live_status != 1 {
            return Err(PlatformError::NotLive);
        }
        let playurl = data
            .playurl_info
            .ok_or_else(|| PlatformError::Parse("缺少 playurl_info".into()))?
            .playurl;

        let qn_desc: BTreeMap<i64, String> = playurl
            .g_qn_desc
            .iter()
            .map(|item| (item.qn, item.desc.clone()))
            .collect();
        let accepted: BTreeSet<i64> = playurl
            .stream
            .iter()
            .flat_map(|stream| stream.format.iter())
            .flat_map(|format| format.codec.iter())
            .flat_map(|codec| codec.accept_qn.iter().copied())
            .collect();
        // accept_qn 是按编码列出的，用并集反推每档画质实际支持哪些视频编码。
        let mut qn_codecs: BTreeMap<i64, BTreeSet<&'static str>> = BTreeMap::new();
        for stream in &playurl.stream {
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
        for stream in &playurl.stream {
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
                let hint = if codec.as_deref() == Some("h265") {
                    Some("HEVC".to_string())
                } else if codec.as_deref() == Some("av1") {
                    Some("AV1".to_string())
                } else if desc.contains("杜比") {
                    Some("杜比".to_string())
                } else {
                    None
                };
                QualityInfo {
                    qn: *qn as u32,
                    label: desc.clone(),
                    hint,
                    available: accepted.contains(qn),
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
                hint: codec.as_deref().map(|value| value.to_uppercase()),
                available: true,
                codec,
            });
        }
        qualities.sort_by(|a, b| b.qn.cmp(&a.qn));

        Ok(StreamPlan { groups, qualities })
    }
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
        .filter(|segment| segment.chars().all(|ch| ch.is_ascii_digit()))
        .next_back()
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

/// 应用层用 40000 表示「最高可用」；B 站没有这个 qn，原画 10000 即常规最高档。
fn request_qn(qn: u32) -> u32 {
    if qn == 40_000 {
        QN_ORIGIN
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
    fn maps_highest_sentinel_to_origin() {
        assert_eq!(request_qn(40_000), QN_ORIGIN);
        assert_eq!(request_qn(QN_ORIGIN), QN_ORIGIN);
        assert_eq!(request_qn(20_000), 20_000);
        assert_eq!(request_qn(0), 0);
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
