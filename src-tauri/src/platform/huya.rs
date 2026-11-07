//! 虎牙适配器（docs/07 第一部分）。
//!
//! 关键点：
//! - 房间信息与取流参数都在房间页里：`"nick":"…"`、`"isOn":true`、`stream: {...}`。
//! - 画质不是改 streamName，而是靠 antiCode 里的 ratio 参数（vMultiStreamInfo 的 iBitRate）。
//! - 取流地址 = sFlvUrl + "/" + sStreamName + "." + 后缀 + "?" + 重算后的 antiCode。
//! - 签名要点（对齐 streamlink/plugins/huya.py）：fm 先 URL 解码再 base64 解码取 `_` 前缀，
//!   wsSecretHash = md5("{seqid}|{ctype}|100")，wsSecret = md5("{前缀}_{convert_uid}_{streamName}_{hash}_{wsTime}")。
//!   seqid 与时间戳相关，每次请求都要重算，否则 CDN 只给一小段就断流。

use std::collections::BTreeMap;

use async_trait::async_trait;
use base64::Engine;
use serde_json::Value;
use url::Url;

use super::http;
use super::{
    Codec, PlatformAdapter, PlatformError, PlatformId, RoomInfo, RoomRef, StreamCandidate,
    StreamFormat, StreamGroup,
};

const ROOM_HOST: &str = "https://www.huya.com";
const REFERER_HOME: &str = "https://www.huya.com/";
/// 客户端版本号；过旧可能被拒（docs/07 §3），平台校验失败时优先怀疑这里。
const CLIENT_VERSION: &str = "2401090219";
/// 签名常量 t（streamlink 里固定 100），参与 wsSecretHash 计算。
const SIGN_T: u32 = 100;

pub struct HuyaAdapter;

impl HuyaAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for HuyaAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PlatformAdapter for HuyaAdapter {
    fn id(&self) -> PlatformId {
        "huya"
    }

    fn display_name(&self) -> &'static str {
        "虎牙"
    }

    fn matches(&self, url: &Url) -> bool {
        url.host_str()
            .map(|host| host == "huya.com" || host.ends_with(".huya.com"))
            .unwrap_or(false)
    }

    fn resolve(&self, input: &str) -> Option<RoomRef> {
        let key = extract_room_key(input)?;
        Some(RoomRef {
            platform: self.id().to_string(),
            canonical_url: format!("{ROOM_HOST}/{key}"),
            room_key: key,
            input: input.to_string(),
        })
    }

    async fn room_info(&self, room: &RoomRef) -> Result<RoomInfo, PlatformError> {
        let referer = format!("{ROOM_HOST}/{}", room.room_key);
        let (body, _) = http::get_text(&referer, REFERER_HOME, &[]).await?;

        if body.contains("虎牙君找不到这个主播") || body.contains("该主播不存在") {
            return Err(PlatformError::RoomNotFound(format!(
                "虎牙房间 {}",
                room.room_key
            )));
        }

        let banned = body.contains("该主播涉嫌违规");
        let nick = field(&body, "nick")
            .map(unescape)
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| format!("主播 {}", room.room_key));
        let introduction = field(&body, "introduction")
            .map(unescape)
            .filter(|text| !text.is_empty());
        let room_name = field(&body, "roomName")
            .map(unescape)
            .filter(|text| !text.is_empty())
            .or_else(|| introduction.clone())
            .unwrap_or_default();

        let is_on = parse_is_on(&body);
        let started_at = field(&body, "startTime")
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|value| *value > 0);
        let online = field(&body, "totalCount").and_then(|value| value.parse::<u64>().ok());

        Ok(RoomInfo {
            host_name: nick,
            room_name: if banned {
                "该主播涉嫌违规，正在整改中".to_string()
            } else {
                room_name
            },
            avatar_url: field(&body, "avatar180").map(unescape),
            cover_url: field(&body, "screenshot").map(unescape),
            area_name: field(&body, "gameFullName").map(unescape),
            // 封禁房间返回「未开播」而不是错误，避免监控每轮都重试（docs/07 §5.5）。
            is_live: is_on && !banned,
            started_at,
            online,
            extra: serde_json::json!({
                "roomKey": room.room_key,
                "banned": banned,
            }),
        })
    }

    async fn stream_groups(
        &self,
        room: &RoomRef,
        qn: u32,
    ) -> Result<Vec<StreamGroup>, PlatformError> {
        let referer = format!("{ROOM_HOST}/{}", room.room_key);
        let (body, _) = http::get_text(&referer, REFERER_HOME, &[]).await?;
        if body.contains("虎牙君找不到这个主播") {
            return Err(PlatformError::RoomNotFound(format!(
                "虎牙房间 {}",
                room.room_key
            )));
        }
        if !parse_is_on(&body) {
            return Err(PlatformError::NotLive);
        }

        let stream = extract_stream_json(&body)
            .ok_or_else(|| PlatformError::Parse("虎牙页面里没有 stream 数据".into()))?;
        let list = stream
            .pointer("/data/0/gameStreamInfoList")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let quality = pick_quality(&stream, qn);

        let mut groups: Vec<StreamGroup> = Vec::new();
        for entry in &list {
            let Some(stream_name) = text(entry, "sStreamName") else {
                continue;
            };
            let cdn = text(entry, "sCdnType").unwrap_or_else(|| "默认".to_string());
            let priority = entry
                .get("iWebPriorityRate")
                .and_then(Value::as_i64)
                .unwrap_or(-1);

            let mut candidates = Vec::new();
            if let (Some(base), Some(anti)) = (text(entry, "sFlvUrl"), text(entry, "sFlvAntiCode"))
            {
                let suffix = text(entry, "sFlvUrlSuffix").unwrap_or_else(|| "flv".to_string());
                if let Some(url) =
                    build_stream_url(&base, &stream_name, &suffix, &anti, quality.bitrate)
                {
                    candidates.push(candidate(
                        url,
                        StreamFormat::Flv,
                        1000.0 + priority as f64,
                        &cdn,
                        &stream_name,
                        &referer,
                        &quality,
                    ));
                }
            }

            if candidates.is_empty() {
                continue;
            }

            let name = cdn_label(&cdn);
            match groups.iter_mut().find(|group| group.name == name) {
                Some(group) => group.candidates.extend(candidates),
                None => groups.push(StreamGroup {
                    name,
                    priority: 0,
                    candidates,
                }),
            }
        }

        if groups.is_empty() {
            return Err(PlatformError::NotLive);
        }

        // 组内权重高优先；组间按最佳候选权重降序（iWebPriorityRate 越高线路越好）。
        for group in groups.iter_mut() {
            group.candidates.sort_by(|a, b| {
                b.weight
                    .partial_cmp(&a.weight)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        groups.sort_by(|a, b| {
            let rank = |group: &StreamGroup| {
                group
                    .candidates
                    .first()
                    .map(|item| item.weight)
                    .unwrap_or(0.0)
            };
            rank(b)
                .partial_cmp(&rank(a))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for (index, group) in groups.iter_mut().enumerate() {
            group.priority = index as u32;
        }
        Ok(groups)
    }
}

/// 选中的画质档：bitrate 直接进 ratio 参数，qn 用于前端显示实际画质。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Quality {
    bitrate: u32,
    name: String,
    qn: u32,
}

/// 按期望 qn 从 vMultiStreamInfo 里挑最接近的一档（只考虑 H.264 兼容档）。
fn pick_quality(stream: &Value, qn: u32) -> Quality {
    let mut options: Vec<(u32, String)> = stream
        .get("vMultiStreamInfo")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter(|item| item.get("iCodecType").and_then(Value::as_i64).unwrap_or(0) == 0)
                .filter_map(|item| {
                    let bitrate = item.get("iBitRate").and_then(Value::as_u64).unwrap_or(0) as u32;
                    let name = item
                        .get("sDisplayName")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    (!name.is_empty()).then_some((bitrate, name))
                })
                .collect()
        })
        .unwrap_or_default();

    if options.is_empty() {
        return Quality {
            bitrate: 0,
            name: "原画".to_string(),
            qn: 10000,
        };
    }

    // 0 表示源码率（最高档），排序时当作最大值。
    options.sort_by_key(|(bitrate, _)| if *bitrate == 0 { u32::MAX } else { *bitrate });

    let target = target_bitrate(qn);
    let chosen = if target == 0 {
        options
            .iter()
            .find(|(bitrate, _)| *bitrate == 0)
            .or_else(|| options.last())
    } else {
        options
            .iter()
            .filter(|(bitrate, _)| *bitrate != 0 && *bitrate <= target)
            .max_by_key(|(bitrate, _)| *bitrate)
            .or_else(|| options.first())
    };

    match chosen {
        Some((bitrate, name)) => {
            let bitrate = *bitrate;
            Quality {
                bitrate,
                name: name.clone(),
                qn: qn_for_bitrate(bitrate),
            }
        }
        None => Quality {
            bitrate: 0,
            name: "原画".to_string(),
            qn: 10000,
        },
    }
}

/// qn → 期望码率（kbps）：10000 原画、400 蓝光 8M、250 蓝光 4M、150 超清、80 流畅。
fn target_bitrate(qn: u32) -> u32 {
    match qn {
        0 | 10000 | 20000 | 30000 => 0,
        400 => 8000,
        250 => 4000,
        150 => 2000,
        80 => 500,
        _ => 0,
    }
}

/// 实际码率 → 反推 qn，供 UI 显示「请求 4M，实际 2M」这类降级提示。
fn qn_for_bitrate(bitrate: u32) -> u32 {
    if bitrate == 0 || bitrate >= 8000 {
        10000
    } else if bitrate >= 4000 {
        250
    } else if bitrate >= 2000 {
        150
    } else {
        80
    }
}

fn candidate(
    url: String,
    format: StreamFormat,
    weight: f64,
    cdn: &str,
    stream_name: &str,
    referer: &str,
    quality: &Quality,
) -> StreamCandidate {
    let mut headers = BTreeMap::new();
    headers.insert("Referer".to_string(), referer.to_string());
    headers.insert("Origin".to_string(), ROOM_HOST.to_string());
    headers.insert("User-Agent".to_string(), http::USER_AGENT.to_string());

    let mut attrs = BTreeMap::new();
    attrs.insert("cdn".to_string(), cdn.to_string());
    attrs.insert("streamName".to_string(), stream_name.to_string());
    attrs.insert("bitrateKbps".to_string(), quality.bitrate.to_string());
    attrs.insert("qn".to_string(), quality.qn.to_string());

    StreamCandidate {
        id: format!("{cdn}-{stream_name}-{}", quality.bitrate),
        name: format!("{} · FLV", quality.name),
        weight,
        format,
        codec: Some(Codec::H264),
        width: None,
        height: None,
        fps: None,
        bitrate_kbps: (quality.bitrate > 0).then_some(quality.bitrate),
        url: Some(url),
        headers,
        attrs,
    }
}

/// 重算虎牙防盗链参数并拼出可下载地址。
fn build_stream_url(
    base: &str,
    stream_name: &str,
    suffix: &str,
    anti_code: &str,
    ratio: u32,
) -> Option<String> {
    let params: BTreeMap<String, String> = url::form_urlencoded::parse(anti_code.as_bytes())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    let fm = params.get("fm")?;
    let ctype = params
        .get("ctype")
        .map(String::as_str)
        .unwrap_or("huya_live");
    let ws_time = params.get("wsTime")?;
    let fs = params.get("fs").map(String::as_str).unwrap_or("bgct");

    let template = base64::engine::general_purpose::STANDARD
        .decode(fm.as_bytes())
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())?;
    // fm 形如 "DWq8BcJ3h6DJt6TY_$0_$1_$2_$3"，只有前缀参与签名。
    let prefix = template.split('_').next().unwrap_or(template.as_str());

    let timestamp = chrono::Utc::now().timestamp_millis();
    // 前端 uid 是随机的，这里用时间戳派生，保证每次请求不同即可。
    let uid = 12_340_000u64 + (timestamp % 10_000) as u64;
    let convert_uid = (((uid << 8) | (uid >> 24)) & 0xFFFF_FFFF) as u32;
    let seqid = uid as i64 + timestamp;

    let secret_hash = md5_hex(&format!("{seqid}|{ctype}|{SIGN_T}"));
    let ws_secret = md5_hex(&format!(
        "{prefix}_{convert_uid}_{stream_name}_{secret_hash}_{ws_time}"
    ));

    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("wsSecret", &ws_secret)
        .append_pair("wsTime", ws_time)
        .append_pair("ctype", ctype)
        .append_pair("fs", fs)
        .append_pair("seqid", &seqid.to_string())
        .append_pair("u", &convert_uid.to_string())
        .append_pair("sdk_sid", &timestamp.to_string())
        .append_pair("ratio", &ratio.to_string())
        .append_pair("t", &SIGN_T.to_string())
        .append_pair("ver", "1")
        .append_pair("sv", CLIENT_VERSION)
        .append_pair("codec", "264")
        .finish();

    // 统一走 https：http 会 302 到 IP，部分节点在 https 下更稳定。
    let base = base.trim_end_matches('/');
    let base = base
        .strip_prefix("http://")
        .map(|rest| format!("https://{rest}"))
        .unwrap_or_else(|| base.to_string());

    Some(format!("{base}/{stream_name}.{suffix}?{query}"))
}

fn md5_hex(value: &str) -> String {
    md5::compute(value.as_bytes())
        .0
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 从页面里截取 `stream: {...}`，用花括号配对而不是正则，避免嵌套 JSON 截断。
fn extract_stream_json(body: &str) -> Option<Value> {
    let index = body.find("stream: ")? + "stream: ".len();
    let text = balanced_object(&body[index..])?;
    serde_json::from_str(text).ok()
}

fn balanced_object(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, byte) in bytes.iter().enumerate() {
        let ch = *byte;
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == b'\\' {
                escaped = true;
            } else if ch == b'"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[..=index]);
                }
            }
            _ => {}
        }
    }
    None
}

/// 取 `"key":"value"` 里的 value（页面里是 JSON 片段，值里可能有转义）。
fn field<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\":\"");
    let start = body.find(&needle)? + needle.len();
    let rest = &body[start..];
    let mut escaped = false;
    for (index, ch) in rest.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => return Some(&rest[..index]),
            _ => {}
        }
    }
    None
}

fn parse_is_on(body: &str) -> bool {
    let needle = "\"isOn\":";
    let Some(start) = body.find(needle) else {
        return false;
    };
    let rest = body[start + needle.len()..].trim_start();
    rest.starts_with("true") || rest.starts_with("\"true\"")
}

/// 页面里的字符串是 JSON 转义过的，中文会写成 \uXXXX。
fn unescape(value: &str) -> String {
    if !value.contains('\\') {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    Some(decoded) => out.push(decoded),
                    None => out.push('\\'),
                }
            }
            Some('/') => out.push('/'),
            Some('"') => out.push('"'),
            Some('n') => out.push('\n'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// 虎牙的 sCdnType 形如 AL / TX / HS / TX15 / AL13：前缀是厂商，数字是线路编号。
fn cdn_label(cdn: &str) -> String {
    let split = cdn
        .char_indices()
        .find(|(_, ch)| !ch.is_ascii_alphabetic())
        .map(|(index, _)| index)
        .unwrap_or(cdn.len());
    let (prefix, suffix) = cdn.split_at(split);
    let vendor = match prefix {
        "AL" => "阿里云",
        "TX" => "腾讯云",
        "HS" => "华为云",
        "WS" => "网宿",
        _ => return cdn.to_string(),
    };
    if suffix.is_empty() {
        vendor.to_string()
    } else {
        format!("{vendor} {suffix}")
    }
}

fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(|text| text.to_string())
}

fn extract_room_key(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix("huya:") {
        let rest = rest.trim();
        return is_room_token(rest).then(|| rest.to_string());
    }
    if trimmed.chars().all(|ch| ch.is_ascii_digit()) {
        return Some(trimmed.to_string());
    }

    let url = Url::parse(trimmed).ok()?;
    let host = url.host_str()?;
    if !(host == "huya.com" || host.ends_with(".huya.com")) {
        return None;
    }
    let segment = url
        .path_segments()?
        .filter(|part| !part.is_empty())
        .next_back()?;
    is_room_token(segment).then(|| segment.to_string())
}

fn is_room_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANTI: &str = "wsSecret=a7ba9f94876cac87266e27faf1052b1b&wsTime=6aec2a46&fm=RFdxOEJjSjNoNkRKdDZUWV8kMF8kMV8kMl8kMw%3D%3D&ctype=huya_live&txyp=o%3An4%3B&fs=bgct";

    #[test]
    fn resolves_rooms_and_urls() {
        let adapter = HuyaAdapter::new();
        assert_eq!(adapter.resolve("huya:660000").unwrap().room_key, "660000");
        assert_eq!(
            adapter
                .resolve("https://www.huya.com/11342412?x=1")
                .unwrap()
                .room_key,
            "11342412"
        );
        assert_eq!(
            adapter
                .resolve("https://www.huya.com/lpl")
                .unwrap()
                .room_key,
            "lpl"
        );
        assert!(adapter.resolve("https://www.douyu.com/9999").is_none());
    }

    #[test]
    fn builds_signed_stream_url() {
        let url = build_stream_url(
            "http://tx.flv.huya.com/src",
            "78941969-2559461593-10057-A-0-1",
            "flv",
            ANTI,
            4000,
        )
        .expect("签名失败");
        assert!(url.starts_with("https://tx.flv.huya.com/src/78941969-"));
        assert!(url.contains("wsSecret="));
        assert!(url.contains("wsTime=6aec2a46"));
        assert!(url.contains("seqid="));
        assert!(url.contains("ratio=4000"));
        assert!(url.contains("codec=264"));
        assert!(url.contains("sv=2401090219"));
        // 同一输入两次签名必须不同：seqid 与 uid 每次重算。
        let again = build_stream_url("http://tx.flv.huya.com/src", "s", "flv", ANTI, 4000).unwrap();
        assert_ne!(url, again);
    }

    #[test]
    fn picks_quality_close_to_request() {
        let stream = serde_json::json!({
            "vMultiStreamInfo": [
                { "sDisplayName": "4K HDR", "iBitRate": 20100, "iCodecType": 1 },
                { "sDisplayName": "蓝光10M", "iBitRate": 0, "iCodecType": 0 },
                { "sDisplayName": "蓝光4M", "iBitRate": 4000, "iCodecType": 0 },
                { "sDisplayName": "超清", "iBitRate": 2000, "iCodecType": 0 },
                { "sDisplayName": "流畅", "iBitRate": 500, "iCodecType": 0 }
            ]
        });
        assert_eq!(pick_quality(&stream, 10000).bitrate, 0);
        assert_eq!(pick_quality(&stream, 10000).qn, 10000);
        assert_eq!(pick_quality(&stream, 400).bitrate, 4000);
        assert_eq!(pick_quality(&stream, 400).qn, 250);
        assert_eq!(pick_quality(&stream, 250).bitrate, 4000);
        assert_eq!(pick_quality(&stream, 150).bitrate, 2000);
        assert_eq!(pick_quality(&stream, 80).bitrate, 500);
        assert_eq!(pick_quality(&stream, 80).name, "流畅");
        // 没有 vMultiStreamInfo 时退回原画。
        assert_eq!(pick_quality(&serde_json::json!({}), 250).bitrate, 0);
    }

    #[test]
    fn parses_is_on_and_unescapes() {
        assert!(parse_is_on(r#"{"isOn":true,"x":1}"#));
        assert!(parse_is_on(r#"{"isOn":"true"}"#));
        assert!(!parse_is_on(r#"{"isOn":false}"#));
        assert_eq!(unescape(r#"\u4e2d\u6587"#), "中文");
        assert_eq!(
            field(r#"{"nick":"\u5c0f\u660e","b":2}"#, "nick"),
            Some(r#"\u5c0f\u660e"#)
        );
    }

    #[test]
    fn maps_cdn_labels() {
        assert_eq!(cdn_label("AL"), "阿里云");
        assert_eq!(cdn_label("TX15"), "腾讯云 15");
        assert_eq!(cdn_label("AL13"), "阿里云 13");
        assert_eq!(cdn_label("default"), "default");
    }

    #[test]
    fn extracts_balanced_stream_object() {
        let body = r#"var x = 1; stream: {"data":[{"a":{"b":1}}]} ; var y = 2;"#;
        let value = extract_stream_json(body).expect("解析失败");
        assert_eq!(
            value.pointer("/data/0/a/b").and_then(Value::as_i64),
            Some(1)
        );
    }

    /// 联网冒烟：cargo test --lib -- --ignored --nocapture huya
    #[tokio::test]
    #[ignore = "需要访问虎牙接口"]
    async fn live_room_probe() {
        let adapter = HuyaAdapter::new();
        let room = adapter.resolve("huya:660000").expect("解析房间号失败");
        let info = adapter.room_info(&room).await.expect("取房间信息失败");
        println!(
            "room: {} / {} live={}",
            info.host_name, info.room_name, info.is_live
        );
        match adapter.stream_groups(&room, 10000).await {
            Ok(groups) => {
                println!("groups={}", groups.len());
                for group in &groups {
                    println!("  line {} priority {}", group.name, group.priority);
                    for candidate in group.candidates.iter().take(2) {
                        println!(
                            "    {} weight={} qn={} ratio={} url={}",
                            candidate.name,
                            candidate.weight,
                            candidate.attrs.get("qn").cloned().unwrap_or_default(),
                            candidate
                                .attrs
                                .get("bitrateKbps")
                                .cloned()
                                .unwrap_or_default(),
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
