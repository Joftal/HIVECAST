//! 抖音（live.douyin.com）适配器。
//!
//! 抖音的取流入口是 webcast/room/web/enter，必须带 a_bogus 签名与 ttwid Cookie：
//!   1. GET https://live.douyin.com/{web_rid} 拿 ttwid（响应头 set-cookie）
//!   2. 用 abogus 模块算出 a_bogus，拼在 enter 查询串后面
//!   3. 从 data.data[0].stream_url 取线路与清晰度
//!
//! 实测与坑见 docs/25 §2。这里只做「房间信息 + 流候选」，不开连接、不写文件。

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use url::Url;

use crate::platform::{http, PlatformAdapter, PlatformError, PlatformId, RoomInfo, RoomRef};
use crate::platform::{Codec, StreamCandidate, StreamFormat, StreamGroup};

use super::abogus;

const HOME: &str = "https://live.douyin.com";
const ENTER: &str = "https://live.douyin.com/webcast/room/web/enter/";

/// 抖音直播接口的 aid，写死在签名里，不能改。
const AID: &str = "6383";

pub struct DouyinAdapter;

impl DouyinAdapter {
    pub fn new() -> Self {
        Self
    }

    fn referer(rid: &str) -> String {
        format!("{HOME}/{rid}")
    }

    /// v.douyin.com 短链要先跟随跳转拿到 web_rid；纯数字直接返回。
    async fn real_room_key(&self, room: &RoomRef) -> Result<String, PlatformError> {
        if is_web_rid(&room.room_key) {
            return Ok(room.room_key.clone());
        }

        let response = http::client()
            .get(&room.canonical_url)
            .header("User-Agent", http::USER_AGENT)
            .send()
            .await
            .map_err(|err| PlatformError::Network(format!("{} → {err}", room.canonical_url)))?;
        let final_url = response.url().clone();
        let rid = final_url
            .path_segments()
            .and_then(|mut segments| segments.next_back().map(str::to_string))
            .filter(|segment| is_web_rid(segment));
        rid.ok_or_else(|| PlatformError::RoomNotFound(format!("抖音房间 {}", room.input)))
    }

    /// 拿 ttwid 并请求 enter 接口。
    async fn enter(&self, rid: &str) -> Result<EnterResponse, PlatformError> {
        let page_url = Self::referer(rid);
        let page = http::client()
            .get(&page_url)
            .header("User-Agent", abogus::SIGN_UA)
            .header("Accept-Language", "zh-CN,zh;q=0.9")
            .send()
            .await
            .map_err(|err| PlatformError::Network(format!("{page_url} → {err}")))?;

        let mut cookie_parts: Vec<String> = Vec::new();
        for value in page.headers().get_all(reqwest::header::SET_COOKIE) {
            if let Ok(text) = value.to_str() {
                if let Some(pair) = text.split(';').next() {
                    let pair = pair.trim();
                    if !pair.is_empty() {
                        cookie_parts.push(pair.to_string());
                    }
                }
            }
        }
        // ttwid 是必须的，缺了会拿到空响应或风控页，所以顺带把整页读掉预热。
        let _ = page.text().await;

        let signed = abogus::sign(&enter_params(rid), "", &abogus::SignOptions::now());
        let url = format!("{ENTER}?{signed}");
        let mut request = http::client()
            .get(&url)
            .header("User-Agent", abogus::SIGN_UA)
            .header("Referer", &page_url)
            .header("Accept-Language", "zh-CN,zh;q=0.9");
        if !cookie_parts.is_empty() {
            request = request.header("Cookie", cookie_parts.join("; "));
        }

        let response = request
            .send()
            .await
            .map_err(|err| PlatformError::Network(format!("{ENTER} → {err}")))?;
        if !response.status().is_success() {
            return Err(PlatformError::Network(format!(
                "{ENTER} → HTTP {}",
                response.status()
            )));
        }
        let body = response
            .text()
            .await
            .map_err(|err| PlatformError::Parse(format!("{ENTER} → {err}")))?;
        // 签名不对时抖音不回 4xx，而是 200 + 空 body，只能自己兜底。
        if body.trim().is_empty() {
            return Err(PlatformError::Upstream {
                code: 0,
                message: format!("抖音房间 {rid} 返回空响应"),
            });
        }
        serde_json::from_str(&body)
            .map_err(|err| PlatformError::Parse(format!("{ENTER} → {err}")))
    }
}

impl Default for DouyinAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PlatformAdapter for DouyinAdapter {
    fn id(&self) -> PlatformId {
        "douyin"
    }

    fn display_name(&self) -> &'static str {
        "抖音"
    }

    fn matches(&self, url: &Url) -> bool {
        url.host_str()
            .map(|host| host == "douyin.com" || host.ends_with(".douyin.com"))
            .unwrap_or(false)
    }

    fn resolve(&self, input: &str) -> Option<RoomRef> {
        let key = extract_room_key(input)?;
        let canonical = if is_web_rid(&key) {
            format!("{HOME}/{key}")
        } else {
            // 短链原样记下来，等 room_info / stream_groups 时再跟随跳转
            input.trim().to_string()
        };
        Some(RoomRef {
            platform: self.id().to_string(),
            canonical_url: canonical,
            room_key: key,
            input: input.to_string(),
        })
    }

    async fn room_info(&self, room: &RoomRef) -> Result<RoomInfo, PlatformError> {
        let rid = self.real_room_key(room).await?;
        let entered = self.enter(&rid).await?;
        let (payload, anchor, _) = unwrap_enter(entered, &rid)?;

        let host_name = anchor
            .as_ref()
            .map(|user| user.nickname.clone())
            .filter(|name| !name.is_empty())
            .or_else(|| {
                payload
                    .owner
                    .as_ref()
                    .map(|user| user.nickname.clone())
                    .filter(|name| !name.is_empty())
            })
            .unwrap_or_else(|| format!("主播 {rid}"));

        Ok(RoomInfo {
            host_name,
            room_name: payload.title.clone(),
            avatar_url: anchor
                .as_ref()
                .and_then(|user| user.avatar_thumb.as_ref())
                .and_then(Image::first)
                .or_else(|| {
                    payload
                        .owner
                        .as_ref()
                        .and_then(|user| user.avatar_thumb.as_ref())
                        .and_then(Image::first)
                }),
            cover_url: payload.cover.as_ref().and_then(Image::first),
            // enter 接口不给分区，room_view_stats 是「x 在线观众」，不能当分区用。
            area_name: None,
            // status: 2 直播中，4 已结束；room_status 同理（0 在播，2 结束）。
            is_live: payload.status == 2,
            started_at: payload.start_time.or(payload.create_time),
            online: payload
                .user_count_str
                .as_deref()
                .and_then(parse_count)
                .or(payload.user_count.map(|count| count.max(0) as u64)),
            extra: json!({
                "roomId": payload.id_str,
                "status": payload.status,
                "roomStatus": payload.room_status,
                "secUid": anchor.as_ref().map(|user| user.sec_uid.clone()).unwrap_or_default(),
            }),
        })
    }

    async fn stream_groups(
        &self,
        room: &RoomRef,
        qn: u32,
    ) -> Result<Vec<StreamGroup>, PlatformError> {
        let rid = self.real_room_key(room).await?;
        let entered = self.enter(&rid).await?;
        let (payload, _, _) = unwrap_enter(entered, &rid)?;

        let stream = payload
            .stream_url
            .as_ref()
            .ok_or(PlatformError::NotLive)?;
        if payload.status != 2 {
            return Err(PlatformError::NotLive);
        }

        let groups = build_groups(stream, qn);
        if groups.is_empty() {
            return Err(PlatformError::NotLive);
        }
        Ok(groups)
    }
}

/// enter 接口的响应骨架。字段全部给默认值，抖音会随版本增删字段。
#[derive(Debug, Deserialize, Default)]
struct EnterResponse {
    #[serde(default)]
    status_code: i64,
    #[serde(default)]
    message: String,
    #[serde(default)]
    data: Option<EnterData>,
}

#[derive(Debug, Deserialize, Default)]
struct EnterData {
    #[serde(default)]
    room_status: i64,
    #[serde(default)]
    data: Vec<RoomPayload>,
    #[serde(default)]
    user: Option<Anchor>,
}

#[derive(Debug, Deserialize, Default)]
struct RoomPayload {
    #[serde(default)]
    id_str: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    status: i64,
    #[serde(default)]
    room_status: i64,
    #[serde(default)]
    user_count: Option<i64>,
    #[serde(default)]
    user_count_str: Option<String>,
    #[serde(default)]
    cover: Option<Image>,
    #[serde(default)]
    owner: Option<Anchor>,
    #[serde(default)]
    start_time: Option<i64>,
    #[serde(default)]
    create_time: Option<i64>,
    #[serde(default)]
    stream_url: Option<StreamUrl>,
}

#[derive(Debug, Deserialize, Default)]
struct Anchor {
    #[serde(default)]
    nickname: String,
    #[serde(default)]
    sec_uid: String,
    #[serde(default)]
    avatar_thumb: Option<Image>,
}

#[derive(Debug, Deserialize, Default)]
struct Image {
    #[serde(default)]
    url_list: Vec<String>,
}

impl Image {
    fn first(&self) -> Option<String> {
        self.url_list.first().cloned()
    }
}

#[derive(Debug, Deserialize, Default)]
struct StreamUrl {
    #[serde(default)]
    flv_pull_url: BTreeMap<String, String>,
    #[serde(default)]
    hls_pull_url_map: BTreeMap<String, String>,
    #[serde(default)]
    resolution_name: BTreeMap<String, String>,
    #[serde(default)]
    live_core_sdk_data: Option<CoreSdk>,
}

#[derive(Debug, Deserialize, Default)]
struct CoreSdk {
    #[serde(default)]
    pull_data: Option<PullData>,
}

#[derive(Debug, Deserialize, Default)]
struct PullData {
    /// 这里是「字符串化的 JSON」，要再解析一次
    #[serde(default)]
    stream_data: Option<String>,
    #[serde(default)]
    options: Option<QualityOptions>,
}

#[derive(Debug, Deserialize, Default)]
struct QualityOptions {
    #[serde(default)]
    qualities: Vec<QualityOption>,
}

#[derive(Debug, Deserialize, Default)]
struct QualityOption {
    #[serde(default)]
    sdk_key: String,
    #[serde(default)]
    name: String,
}

#[derive(Debug, Deserialize, Default)]
struct StreamData {
    #[serde(default)]
    data: BTreeMap<String, StreamWrap>,
}

#[derive(Debug, Deserialize, Default)]
struct StreamWrap {
    #[serde(default)]
    main: Option<MainStream>,
}

#[derive(Debug, Deserialize, Default)]
struct MainStream {
    #[serde(default)]
    flv: Option<String>,
    #[serde(default)]
    hls: Option<String>,
    #[serde(default)]
    sdk_params: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct SdkParams {
    #[serde(default, rename = "VCodec")]
    v_codec: String,
    #[serde(default)]
    vbitrate: i64,
    #[serde(default)]
    resolution: String,
    #[serde(default)]
    fps: f64,
}

/// 一个可用的清晰度档位（同档不同协议会拆成多条候选）。
#[derive(Debug, Clone)]
struct Quality {
    key: String,
    name: String,
    qn: u32,
    bitrate_kbps: u32,
    width: Option<u32>,
    height: Option<u32>,
    fps: Option<f32>,
    codec: Codec,
    flv: Option<String>,
    hls: Option<String>,
}

fn unwrap_enter(
    entered: EnterResponse,
    rid: &str,
) -> Result<(RoomPayload, Option<Anchor>, i64), PlatformError> {
    if entered.status_code != 0 {
        return Err(match entered.status_code {
            // 直播已结束
            30003 => PlatformError::NotLive,
            // 房间不存在 / web_rid 非法
            4001038 => PlatformError::RoomNotFound(format!("抖音房间 {rid}")),
            code => PlatformError::Upstream {
                code,
                message: if entered.message.is_empty() {
                    "抖音接口返回错误".to_string()
                } else {
                    entered.message
                },
            },
        });
    }

    let data = entered.data.ok_or(PlatformError::NotLive)?;
    let room_status = data.room_status;
    let user = data.user;
    let payload = data
        .data
        .into_iter()
        .next()
        .ok_or_else(|| PlatformError::RoomNotFound(format!("抖音房间 {rid}")))?;
    let payload = RoomPayload {
        room_status: if payload.room_status == 0 {
            room_status
        } else {
            payload.room_status
        },
        ..payload
    };
    Ok((payload, user, room_status))
}

/// enter 的查询参数，顺序固定（签名覆盖整个参数串，顺序不能换）。
fn enter_params(rid: &str) -> String {
    let pairs = [
        ("aid", AID),
        ("live_id", "1"),
        ("device_platform", "web"),
        ("language", "zh-CN"),
        ("enter_from", "web_live"),
        ("cookie_enabled", "true"),
        ("screen_width", "1920"),
        ("screen_height", "1080"),
        ("browser_language", "zh-CN"),
        ("browser_platform", "MacIntel"),
        ("browser_name", "Chrome"),
        ("browser_version", "131.0.0.0"),
        ("web_rid", rid),
        ("Room-Enter-User-Login-Ab", "0"),
        ("is_need_double_stream", "false"),
    ];
    pairs
        .iter()
        .map(|(key, value)| format!("{}={}", key, encode_query(value)))
        .collect::<Vec<_>>()
        .join("&")
}

/// URLSearchParams 风格的编码：只保留 unreserved 字符，空格转 +。
fn encode_query(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn is_web_rid(value: &str) -> bool {
    value.len() >= 3 && value.chars().all(|ch| ch.is_ascii_digit())
}

fn extract_room_key(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix("douyin:") {
        let rest = rest.trim();
        return allowed_rid(rest).then(|| rest.to_string());
    }
    if is_web_rid(trimmed) {
        return Some(trimmed.to_string());
    }

    let url = Url::parse(trimmed).ok()?;
    let host = url.host_str()?;
    if !(host == "douyin.com" || host.ends_with(".douyin.com")) {
        return None;
    }
    let segment = url
        .path_segments()?
        .filter(|part| !part.is_empty())
        .next_back()?;
    allowed_rid(segment).then(|| segment.to_string())
}

fn allowed_rid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

/// 抖音档位：sdk_key 与 qn 的对应关系（见 docs/25 §2.4）。
pub fn qn_for_key(key: &str) -> u32 {
    match key {
        "origin" => 10_000,
        "uhd" => 400,
        "hd" => 250,
        "sd" => 150,
        "ld" | "md" => 80,
        _ => 0,
    }
}

fn name_for_key(key: &str) -> &'static str {
    match key {
        "origin" => "原画",
        "uhd" => "蓝光",
        "hd" => "超清",
        "sd" => "高清",
        "ld" => "标清",
        "md" => "极速",
        _ => "未知",
    }
}

/// flv_pull_url 的老式键名到 qn 的映射，SDK 数据缺失时用。
fn qn_for_legacy(key: &str) -> u32 {
    match key {
        "FULL_HD1" => 400,
        "HD1" => 250,
        "SD2" => 150,
        "SD1" => 80,
        "ORIGION" | "ORIGIN" => 10_000,
        _ => 0,
    }
}

fn parse_count(text: &str) -> Option<u64> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (number, unit) = if let Some(rest) = trimmed.strip_suffix('万') {
        (rest, 10_000f64)
    } else if let Some(rest) = trimmed.strip_suffix('亿') {
        (rest, 100_000_000f64)
    } else {
        (trimmed, 1f64)
    };
    number
        .parse::<f64>()
        .ok()
        .map(|value| (value * unit).round() as u64)
}

/// 把 stream_url 摊平成档位表：优先 live_core_sdk_data（有 origin 与码率），
/// 缺失时退回 flv_pull_url / hls_pull_url_map。
fn collect_qualities(stream: &StreamUrl) -> Vec<Quality> {
    let mut names: BTreeMap<String, String> = BTreeMap::new();
    let mut qualities: Vec<Quality> = Vec::new();

    if let Some(pull) = stream
        .live_core_sdk_data
        .as_ref()
        .and_then(|sdk| sdk.pull_data.as_ref())
    {
        if let Some(options) = &pull.options {
            for option in &options.qualities {
                if !option.sdk_key.is_empty() && !option.name.is_empty() {
                    names.insert(option.sdk_key.clone(), option.name.clone());
                }
            }
        }

        if let Some(raw) = &pull.stream_data {
            if let Ok(parsed) = serde_json::from_str::<StreamData>(raw) {
                for (key, wrap) in parsed.data {
                    // ao 是纯音频，不参与清晰度选择
                    if key == "ao" || key == "hls" || key == "flv" {
                        continue;
                    }
                    let Some(main) = wrap.main else { continue };
                    if main.flv.is_none() && main.hls.is_none() {
                        continue;
                    }
                    let params = main
                        .sdk_params
                        .as_deref()
                        .and_then(|text| serde_json::from_str::<SdkParams>(text).ok())
                        .unwrap_or_default();
                    let (width, height) = parse_resolution(&params.resolution);
                    qualities.push(Quality {
                        key: key.clone(),
                        name: names
                            .get(&key)
                            .cloned()
                            .unwrap_or_else(|| name_for_key(&key).to_string()),
                        qn: qn_for_key(&key),
                        bitrate_kbps: (params.vbitrate.max(0) / 1000) as u32,
                        width,
                        height,
                        fps: (params.fps > 0.0).then(|| params.fps as f32),
                        codec: parse_codec(&params.v_codec),
                        flv: main.flv.filter(|url| !url.is_empty()),
                        hls: main.hls.filter(|url| !url.is_empty()),
                    });
                }
            }
        }
    }

    if qualities.is_empty() {
        // 老接口形态：只有按 FULL_HD1 / HD1 / SD1 分组的地址
        let keys: Vec<String> = stream.flv_pull_url.keys().cloned().collect();
        for key in keys {
            let flv = stream.flv_pull_url.get(&key).cloned();
            let hls = stream.hls_pull_url_map.get(&key).cloned();
            let qn = qn_for_legacy(&key);
            qualities.push(Quality {
                name: stream
                    .resolution_name
                    .get(&key)
                    .cloned()
                    .unwrap_or_else(|| name_for_key(&key).to_string()),
                key,
                qn,
                bitrate_kbps: 0,
                width: None,
                height: None,
                fps: None,
                codec: Codec::Unknown,
                flv,
                hls,
            });
        }
    }

    qualities.sort_by(|left, right| {
        right
            .qn
            .cmp(&left.qn)
            .then_with(|| right.bitrate_kbps.cmp(&left.bitrate_kbps))
    });
    qualities
}

fn parse_resolution(text: &str) -> (Option<u32>, Option<u32>) {
    let mut parts = text.split('x');
    let width = parts.next().and_then(|part| part.parse::<u32>().ok());
    let height = parts.next().and_then(|part| part.parse::<u32>().ok());
    match (width, height) {
        (Some(width), Some(height)) if width > 0 && height > 0 => (Some(width), Some(height)),
        _ => (None, None),
    }
}

fn parse_codec(text: &str) -> Codec {
    match text.to_ascii_lowercase().as_str() {
        "264" | "h264" => Codec::H264,
        "265" | "h265" | "hevc" => Codec::H265,
        "av1" => Codec::Av1,
        _ => Codec::Unknown,
    }
}

/// 选档：qn=0 视为自动（取码率最高），否则取 qn 最接近的档位。
fn pick_quality(qualities: &[Quality], qn: u32) -> Option<&Quality> {
    if qualities.is_empty() {
        return None;
    }
    if qn == 0 {
        return qualities.iter().max_by_key(|quality| quality.bitrate_kbps);
    }
    let mut best: Option<&Quality> = None;
    for quality in qualities {
        best = match best {
            None => Some(quality),
            Some(current) => {
                let next_gap = quality.qn.abs_diff(qn);
                let current_gap = current.qn.abs_diff(qn);
                if next_gap < current_gap
                    || (next_gap == current_gap && quality.bitrate_kbps > current.bitrate_kbps)
                {
                    Some(quality)
                } else {
                    Some(current)
                }
            }
        };
    }
    best
}

fn build_groups(stream: &StreamUrl, qn: u32) -> Vec<StreamGroup> {
    let qualities = collect_qualities(stream);
    let Some(quality) = pick_quality(&qualities, qn) else {
        return Vec::new();
    };

    let mut groups = Vec::new();
    if let Some(url) = &quality.flv {
        groups.push(StreamGroup {
            name: format!("{} FLV", cdn_tag(url)),
            priority: 0,
            candidates: vec![candidate(
                quality,
                url,
                StreamFormat::Flv,
                quality.bitrate_kbps.max(1) as f64,
            )],
        });
    }
    if let Some(url) = &quality.hls {
        groups.push(StreamGroup {
            name: format!("{} HLS", cdn_tag(url)),
            priority: 1,
            candidates: vec![candidate(
                quality,
                url,
                StreamFormat::Hls,
                (quality.bitrate_kbps.max(1) as f64) - 0.5,
            )],
        });
    }
    groups
}

fn candidate(quality: &Quality, url: &str, format: StreamFormat, weight: f64) -> StreamCandidate {
    let mut attrs = BTreeMap::new();
    attrs.insert("qn".to_string(), quality.qn.to_string());
    attrs.insert("sdkKey".to_string(), quality.key.clone());
    attrs.insert("bitrateKbps".to_string(), quality.bitrate_kbps.to_string());

    StreamCandidate {
        id: format!("{}-{}", quality.key, format.as_str()),
        name: format!("{} · {}", quality.name, format.as_str().to_uppercase()),
        weight,
        format,
        codec: Some(quality.codec),
        width: quality.width,
        height: quality.height,
        fps: quality.fps,
        bitrate_kbps: Some(quality.bitrate_kbps),
        url: Some(url.to_string()),
        headers: BTreeMap::new(),
        attrs,
    }
}

/// 从 CDN 域名里取线路标记：pull-flv-c13.douyincdn.com → c13
fn cdn_tag(url: &str) -> String {
    let host = Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_string))
        .unwrap_or_default();
    let first = host.split('.').next().unwrap_or_default();
    for prefix in ["pull-flv-", "pull-hls-", "pull-"] {
        if let Some(rest) = first.strip_prefix(prefix) {
            return rest.to_string();
        }
    }
    if first.is_empty() {
        "CDN".to_string()
    } else {
        first.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_room_ids_and_urls() {
        let adapter = DouyinAdapter::new();
        let cases = [
            "https://live.douyin.com/734977595361",
            "https://live.douyin.com/734977595361?enter_from=web_live",
            "734977595361",
            "douyin:734977595361",
        ];
        for input in cases {
            let room = adapter.resolve(input).expect(input);
            assert_eq!(room.room_key, "734977595361", "input={input}");
            assert_eq!(room.platform, "douyin");
        }

        let short = adapter
            .resolve("https://v.douyin.com/iAbCdEf/")
            .expect("短链也能收下");
        assert_eq!(short.canonical_url, "https://v.douyin.com/iAbCdEf/");
        assert!(!is_web_rid(&short.room_key));

        assert!(adapter.resolve("https://www.bilibili.com/1").is_none());
        assert!(adapter.resolve("").is_none());
    }

    #[test]
    fn enter_params_keep_order() {
        let params = enter_params("123456");
        assert!(params.starts_with("aid=6383&live_id=1&device_platform=web"));
        assert!(params.contains("&web_rid=123456&Room-Enter-User-Login-Ab=0&is_need_double_stream=false"));
        assert!(!params.contains("a_bogus"));
    }

    #[test]
    fn maps_qn_and_keys() {
        assert_eq!(qn_for_key("origin"), 10_000);
        assert_eq!(qn_for_key("uhd"), 400);
        assert_eq!(qn_for_key("hd"), 250);
        assert_eq!(qn_for_key("sd"), 150);
        assert_eq!(qn_for_key("ld"), 80);
        assert_eq!(qn_for_key("ao"), 0);
        assert_eq!(qn_for_legacy("FULL_HD1"), 400);
        assert_eq!(qn_for_legacy("SD1"), 80);
        assert_eq!(name_for_key("uhd"), "蓝光");
    }

    #[test]
    fn parses_counts() {
        assert_eq!(parse_count("0"), Some(0));
        assert_eq!(parse_count("1.2万"), Some(12_000));
        assert_eq!(parse_count("3.5亿"), Some(350_000_000));
        assert_eq!(parse_count(""), None);
    }

    const FIXTURE: &str = r#"{
      "status_code": 0,
      "data": {
        "room_status": 0,
        "user": {
          "nickname": "测试主播",
          "sec_uid": "MS4wLjABAAAA",
          "avatar_thumb": { "url_list": ["https://p3.douyinpic.com/avatar.jpeg"] }
        },
        "data": [
          {
            "id_str": "7704580089446419235",
            "title": "测试直播间",
            "status": 2,
            "user_count_str": "1.2万",
            "cover": { "url_list": ["https://p3.douyinpic.com/cover.jpeg"] },
            "room_view_stats": { "display_long": "游戏" },
            "stream_url": {
              "flv_pull_url": {
                "FULL_HD1": "http://pull-flv-c13.douyincdn.com/stage/stream-1_uhd.flv",
                "SD1": "http://pull-flv-c13.douyincdn.com/stage/stream-1_ld.flv"
              },
              "hls_pull_url_map": {
                "FULL_HD1": "http://pull-hls-c13.douyincdn.com/stage/stream-1_uhd.m3u8"
              },
              "live_core_sdk_data": {
                "pull_data": {
                  "options": {
                    "qualities": [
                      { "name": "原画", "sdk_key": "origin" },
                      { "name": "蓝光", "sdk_key": "uhd" },
                      { "name": "高清", "sdk_key": "sd" }
                    ]
                  },
                  "stream_data": "{\"data\":{\"origin\":{\"main\":{\"flv\":\"http://pull-flv-c13.douyincdn.com/stage/stream-1_or4.flv\",\"hls\":\"http://pull-hls-c13.douyincdn.com/stage/stream-1_or4.m3u8\",\"sdk_params\":\"{\\\"VCodec\\\":\\\"h264\\\",\\\"vbitrate\\\":9191424,\\\"resolution\\\":\\\"1920x1080\\\",\\\"fps\\\":59}\"}},\"sd\":{\"main\":{\"flv\":\"http://pull-flv-c13.douyincdn.com/stage/stream-1_sd.flv\",\"sdk_params\":\"{\\\"VCodec\\\":\\\"h264\\\",\\\"vbitrate\\\":2000000,\\\"resolution\\\":\\\"1280x720\\\",\\\"fps\\\":30}\"}},\"ao\":{\"main\":{\"flv\":\"http://pull-flv-c13.douyincdn.com/stage/stream-1_ao.flv\"}}}}"
                }
              }
            }
          }
        ]
      }
    }"#;

    fn fixture_room() -> (RoomPayload, Option<Anchor>) {
        let entered: EnterResponse = serde_json::from_str(FIXTURE).expect("fixture");
        let (payload, anchor, _) = unwrap_enter(entered, "7704580089446419235").expect("unwrap");
        (payload, anchor)
    }

    #[test]
    fn parses_enter_payload() {
        let (payload, anchor) = fixture_room();
        assert_eq!(payload.title, "测试直播间");
        assert_eq!(payload.status, 2);
        assert_eq!(payload.user_count_str.as_deref(), Some("1.2万"));
        assert_eq!(anchor.map(|user| user.nickname), Some("测试主播".to_string()));

        let stream = payload.stream_url.expect("stream_url");
        let qualities = collect_qualities(&stream);
        // origin / sd 两条，ao 被丢弃
        assert_eq!(qualities.len(), 2);
        assert_eq!(qualities[0].key, "origin");
        assert_eq!(qualities[0].qn, 10_000);
        assert_eq!(qualities[0].bitrate_kbps, 9191);
        assert_eq!(qualities[0].width, Some(1920));
        assert_eq!(qualities[0].name, "原画");
        assert_eq!(qualities[1].key, "sd");
        assert_eq!(qualities[1].qn, 150);
    }

    #[test]
    fn picks_quality_closest_to_qn() {
        let (payload, _) = fixture_room();
        let stream = payload.stream_url.expect("stream_url");

        let groups = build_groups(&stream, 10_000);
        assert_eq!(groups.len(), 2, "FLV 与 HLS 各一组");
        let candidate = &groups[0].candidates[0];
        assert_eq!(candidate.attrs.get("qn").map(String::as_str), Some("10000"));
        assert_eq!(candidate.name, "原画 · FLV");
        assert_eq!(candidate.format, StreamFormat::Flv);
        assert_eq!(groups[0].name, "c13 FLV");
        assert_eq!(candidate.width, Some(1920));

        // 请求 80：没有 ld 档，最接近的是 sd（150）
        let groups = build_groups(&stream, 80);
        assert_eq!(
            groups[0].candidates[0].attrs.get("qn").map(String::as_str),
            Some("150")
        );

        // 自动：码率最高
        let groups = build_groups(&stream, 0);
        assert_eq!(
            groups[0].candidates[0].attrs.get("qn").map(String::as_str),
            Some("10000")
        );
    }

    #[test]
    fn falls_back_to_legacy_flv_map() {
        let stream: StreamUrl = serde_json::from_str(
            r#"{
              "flv_pull_url": { "FULL_HD1": "http://pull-flv-t5.douyincdn.com/a.flv", "SD1": "http://pull-flv-t5.douyincdn.com/b.flv" },
              "resolution_name": { "FULL_HD1": "蓝光", "SD1": "标清" }
            }"#,
        )
        .expect("legacy");
        let qualities = collect_qualities(&stream);
        assert_eq!(qualities.len(), 2);
        assert_eq!(qualities[0].qn, 400);

        let groups = build_groups(&stream, 400);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].candidates[0].name, "蓝光 · FLV");
        assert_eq!(groups[0].name, "t5 FLV");
    }

    #[test]
    fn maps_cdn_tags() {
        assert_eq!(cdn_tag("http://pull-flv-c13.douyincdn.com/x.flv"), "c13");
        assert_eq!(cdn_tag("https://pull-hls-t5.douyincdn.com/x.m3u8"), "t5");
        assert_eq!(cdn_tag("http://pull-t5.douyincdn.com/x.flv"), "t5");
        assert_eq!(cdn_tag("not a url"), "CDN");
    }

    /// 联网冒烟：HIVE_DOUYIN_ROOM=<web_rid> cargo test --lib -- --ignored --nocapture douyin
    #[tokio::test]
    #[ignore = "需要访问抖音接口"]
    async fn live_room_probe() {
        let rid = std::env::var("HIVE_DOUYIN_ROOM").unwrap_or_else(|_| "369324308707".to_string());
        let adapter = DouyinAdapter::new();
        let room = adapter
            .resolve(&format!("douyin:{rid}"))
            .expect("解析房间号失败");
        let info = adapter.room_info(&room).await.expect("取房间信息失败");
        println!(
            "room: {} / {} live={} area={:?} online={:?}",
            info.host_name, info.room_name, info.is_live, info.area_name, info.online
        );
        match adapter.stream_groups(&room, 10_000).await {
            Ok(groups) => {
                println!("groups={}", groups.len());
                for group in &groups {
                    for candidate in &group.candidates {
                        println!(
                            "  line {} / {} weight={} qn={} {}x{} bitrate={} url={}",
                            group.name,
                            candidate.name,
                            candidate.weight,
                            candidate.attrs.get("qn").cloned().unwrap_or_default(),
                            candidate.width.unwrap_or(0),
                            candidate.height.unwrap_or(0),
                            candidate.bitrate_kbps.unwrap_or(0),
                            candidate.url.as_deref().unwrap_or("")
                        );
                    }
                }
                assert!(!groups.is_empty());
            }
            Err(PlatformError::NotLive) => println!("房间未开播，跳过流候选断言"),
            Err(err) => panic!("取流失败: {err}"),
        }
    }
}
