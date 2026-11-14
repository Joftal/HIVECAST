//! 斗鱼适配器（docs/06）。
//!
//! 关键点：
//! - 房间信息走 betard；show_status=1 且 videoLoop=0 才算真正在播（轮播录像不算）。
//! - 取流必须带动态签名：getEncryption 下发 key/rand_str/enc_time/enc_data，
//!   `auth = md5(f + key + rid + ts)`，其中 f 是 rand_str 迭代 md5(f + key) enc_time 次，
//!   ts 用服务器 Date 头而不是本地时钟（本地偏差几分钟就会签名失败）。
//! - 每次 getH5PlayV1 都要重新取加密参数：enc_data 与时间绑定，缓存会失效。
//! - 一个 CDN 一条线路组，组内是当前清晰度的一个候选；切清晰度由上层带 qn 重新解析。

use std::collections::BTreeMap;

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

const ROOM_HOST: &str = "https://www.douyu.com";
const REFERER_HOME: &str = "https://www.douyu.com/";
const ENC_API: &str = "https://www.douyu.com/wgapi/livenc/liveweb/websec/getEncryption";
const PLAY_API: &str = "https://www.douyu.com/lapi/live/getH5PlayV1";

pub struct DouyuAdapter {
    /// 设备号：斗鱼按 did 认设备，必须固定（见 docs/06 §5）。
    did: String,
}

impl DouyuAdapter {
    pub fn new() -> Self {
        Self { did: device_id() }
    }

    #[cfg(test)]
    pub fn device_id(&self) -> &str {
        &self.did
    }

    fn referer(room_key: &str) -> String {
        format!("{ROOM_HOST}/{room_key}")
    }
}

impl Default for DouyuAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// 用机器名派生一个稳定的 32 位十六进制 did。
///
/// 斗鱼把 did 当设备指纹：每次重启都换等于换设备，会让登录态与匿名流频繁失效。
/// 这里不落库也能做到跨重启稳定，代价是同机多实例共用（可接受）。
fn device_id() -> String {
    let seed = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_default();
    let material = format!("hivecast/douyu/{seed}");
    md5::compute(material.as_bytes())
        .0
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Debug, Deserialize)]
struct Betard {
    #[serde(default)]
    room: Option<BetardRoom>,
}

#[derive(Debug, Deserialize)]
struct BetardRoom {
    #[serde(default)]
    room_id: i64,
    #[serde(default)]
    room_name: String,
    #[serde(default)]
    owner_name: String,
    #[serde(default)]
    nickname: String,
    #[serde(default)]
    show_status: i64,
    #[serde(default, rename = "videoLoop")]
    video_loop: i64,
    #[serde(default)]
    second_lvl_name: Option<String>,
    #[serde(default)]
    room_pic: Option<String>,
    #[serde(default)]
    owner_avatar: Option<String>,
    #[serde(default)]
    avatar: Option<Avatar>,
}

#[derive(Debug, Deserialize)]
struct Avatar {
    #[serde(default)]
    big: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EncResponse {
    #[serde(default)]
    error: i64,
    #[serde(default)]
    msg: String,
    #[serde(default)]
    data: Option<EncData>,
}

#[derive(Debug, Deserialize)]
struct EncData {
    #[serde(default)]
    key: String,
    #[serde(default)]
    rand_str: String,
    #[serde(default)]
    enc_time: u32,
    #[serde(default)]
    enc_data: String,
    #[serde(default)]
    is_special: i64,
}

#[derive(Debug, Deserialize)]
struct PlayResponse {
    #[serde(default)]
    error: i64,
    #[serde(default)]
    msg: String,
    #[serde(default)]
    data: Option<PlayData>,
}

#[derive(Debug, Deserialize, Clone)]
struct PlayData {
    #[serde(default)]
    rtmp_url: String,
    #[serde(default)]
    rtmp_live: String,
    /// 平台实际给的档位（请求原画也可能被降级）
    #[serde(default)]
    rate: i64,
    #[serde(default)]
    rtmp_cdn: String,
    #[serde(default)]
    multirates: Vec<MultiRate>,
    #[serde(default, rename = "cdnsWithName")]
    cdns_with_name: Vec<CdnEntry>,
}

#[derive(Debug, Deserialize, Clone)]
struct MultiRate {
    #[serde(default)]
    name: String,
    #[serde(default)]
    rate: i64,
    #[serde(default)]
    bit: i64,
}

#[derive(Debug, Deserialize, Clone)]
struct CdnEntry {
    #[serde(default)]
    name: String,
    #[serde(default)]
    cdn: String,
}

#[async_trait]
impl PlatformAdapter for DouyuAdapter {
    fn id(&self) -> PlatformId {
        "douyu"
    }

    fn display_name(&self) -> &'static str {
        "斗鱼"
    }

    fn matches(&self, url: &Url) -> bool {
        url.host_str()
            .map(|host| host == "douyu.com" || host.ends_with(".douyu.com"))
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
        let room_key = self.real_room_key(&room.room_key).await?;
        let referer = Self::referer(&room_key);
        let url = format!("{ROOM_HOST}/betard/{room_key}");
        // 不存在的房间返回的是 HTML 错误页，所以先取文本再解析。
        let (body, _) = http::get_text(&url, &referer, &[]).await?;
        let betard: Betard = serde_json::from_str(&body)
            .map_err(|_| PlatformError::RoomNotFound(format!("斗鱼房间 {room_key}")))?;
        let data = betard
            .room
            .ok_or_else(|| PlatformError::RoomNotFound(format!("斗鱼房间 {room_key}")))?;

        if data.show_status == 0 && data.room_name.is_empty() {
            return Err(PlatformError::RoomNotFound(format!("斗鱼房间 {room_key}")));
        }

        let host_name = if !data.owner_name.is_empty() {
            data.owner_name.clone()
        } else if !data.nickname.is_empty() {
            data.nickname.clone()
        } else {
            format!("主播 {}", data.room_id)
        };

        Ok(RoomInfo {
            host_name,
            room_name: data.room_name,
            avatar_url: data
                .owner_avatar
                .or_else(|| data.avatar.and_then(|avatar| avatar.big)),
            cover_url: data.room_pic,
            area_name: data.second_lvl_name,
            // videoLoop=1 是轮播录像，不能当直播，否则会 7×24 一直录。
            is_live: data.show_status == 1 && data.video_loop == 0,
            started_at: None,
            online: None,
            // 别名房间（/xxkk）也要归一到真实数字房间号：监控用别名查状态，
            // 导入关注拿到的却是数字 room_id，没有稳定 ID 会留下两条重复记录。
            stable_id: Some(data.room_id.to_string()),
            extra: json!({
                "roomId": data.room_id,
                "showStatus": data.show_status,
                "videoLoop": data.video_loop,
            }),
        })
    }

    async fn stream_plan(&self, room: &RoomRef, qn: u32) -> Result<StreamPlan, PlatformError> {
        let room_key = self.real_room_key(&room.room_key).await?;
        let referer = Self::referer(&room_key);
        let rate = rate_for_qn(qn);

        let enc = fetch_encryption(&self.did, &referer).await?;
        let ts = enc.date.unwrap_or_else(|| chrono::Utc::now().timestamp());
        let enc_data = enc.data.ok_or_else(|| PlatformError::Upstream {
            code: enc.error,
            message: non_empty(&enc.msg, "拿不到斗鱼加密参数"),
        })?;
        if enc.error != 0 {
            return Err(PlatformError::Upstream {
                code: enc.error,
                message: non_empty(&enc.msg, "斗鱼加密接口报错"),
            });
        }
        let auth = sign_auth(
            &enc_data.rand_str,
            &enc_data.key,
            enc_data.enc_time,
            enc_data.is_special,
            &room_key,
            ts,
        );

        let first = request_play(
            &room_key,
            &referer,
            &self.did,
            &enc_data.enc_data,
            ts,
            &auth,
            "",
            rate,
        )
        .await?;

        if first.error != 0 {
            return Err(map_play_error(first.error, &first.msg));
        }
        let data = first.data.ok_or_else(|| PlatformError::Upstream {
            code: first.error,
            message: non_empty(&first.msg, "斗鱼取流返回空数据"),
        })?;
        if data.rtmp_url.is_empty() || data.rtmp_live.is_empty() {
            return Err(PlatformError::NotLive);
        }

        // 线路：响应里的 cdnsWithName 就是可用 CDN 列表，首条通常就是刚才拿到的。
        let mut lines: Vec<(String, String)> = data
            .cdns_with_name
            .iter()
            .filter(|entry| !entry.cdn.is_empty())
            .map(|entry| (entry.cdn.clone(), label(&entry.name, &entry.cdn)))
            .collect();
        if lines.is_empty() {
            lines.push((data.rtmp_cdn.clone(), label("", &data.rtmp_cdn)));
        }

        // 平台返回的 rate 才是当前账号真正拿到的档位；未登录请求原画时
        // 斗鱼会把 rate 降成 4（蓝光 4M），但 multirates 仍列出原画。
        // 用实际档位限制可选画质，避免 UI 让用户选一个会静默降级的档位。
        //
        // 登录态不能按本次响应的 rate 限制：用户从原画切到蓝光后再拉流，
        // 平台返回的 rate 就是 4，若据此封顶会把原画永久标成不可用，
        // 反而把用户锁在低画质。登录态只信任账号能力表。
        let authed = crate::credential::is_ok("douyu");
        let mut best_qn = if authed { 0 } else { qn_for_rate(data.rate) };
        let mut groups: Vec<StreamGroup> = Vec::new();
        for (cdn, line_name) in lines {
            let payload = if cdn == data.rtmp_cdn {
                data.clone()
            } else {
                let fetched = request_play(
                    &room_key,
                    &referer,
                    &self.did,
                    &enc_data.enc_data,
                    ts,
                    &auth,
                    &cdn,
                    rate,
                )
                .await?;
                if fetched.error != 0 {
                    // 单条线路失败不影响其它线路。
                    continue;
                }
                match fetched.data {
                    Some(play) => play,
                    None => continue,
                }
            };

            let Some(url) = join_url(&payload.rtmp_url, &payload.rtmp_live) else {
                continue;
            };
            if !authed {
                best_qn = best_qn.max(qn_for_rate(payload.rate));
            }
            let rate_name = payload
                .multirates
                .iter()
                .find(|item| item.rate == payload.rate)
                .map(|item| item.name.clone())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| format!("rate{}", payload.rate));
            let bit = payload
                .multirates
                .iter()
                .find(|item| item.rate == payload.rate)
                .map(|item| item.bit)
                .unwrap_or(0);

            let mut headers = BTreeMap::new();
            headers.insert("Referer".to_string(), referer.clone());
            headers.insert("Origin".to_string(), ROOM_HOST.to_string());
            headers.insert("User-Agent".to_string(), http::USER_AGENT.to_string());
            headers.insert("Cookie".to_string(), format!("dy_did={}", self.did));

            let mut attrs = BTreeMap::new();
            attrs.insert("rate".to_string(), payload.rate.to_string());
            attrs.insert("requestedRate".to_string(), rate.to_string());
            attrs.insert("qn".to_string(), qn_for_rate(payload.rate).to_string());
            attrs.insert("cdn".to_string(), cdn.clone());
            attrs.insert("bit".to_string(), bit.to_string());

            let candidate = StreamCandidate {
                id: format!("{}-{}-{}", payload.rate, cdn, "flv"),
                name: format!("{rate_name} · FLV"),
                weight: bit.max(payload.rate as i64 * 1000) as f64,
                format: StreamFormat::Flv,
                codec: Some(Codec::H264),
                width: None,
                height: None,
                fps: None,
                bitrate_kbps: (bit > 0).then_some(bit as u32),
                url: Some(url),
                headers,
                attrs,
            };

            groups.push(StreamGroup {
                name: line_name,
                priority: groups.len() as u32,
                candidates: vec![candidate],
            });
        }

        if groups.is_empty() {
            return Err(PlatformError::Parse("斗鱼没有可用线路".into()));
        }

        // multirates 就是平台当前房间的真实档位表；同一 qn 只保留码率更高的一条。
        let mut quality_map: BTreeMap<u32, (i64, QualityInfo)> = BTreeMap::new();
        for item in &data.multirates {
            let qn = qn_for_rate(item.rate);
            if qn == 0 {
                continue;
            }
            let label = if item.name.is_empty() {
                format!("rate{}", item.rate)
            } else {
                item.name.clone()
            };
            let available = quality_available(qn, best_qn);
            // 不可用时不再展示码率，交给前端统一显示「当前不可用」；
            // 否则用户只会看到「20176 kbps」而不知道这一档拿不到。
            let hint = if available {
                (item.bit > 0).then(|| format!("{} kbps", item.bit))
            } else {
                None
            };
            let info = QualityInfo {
                qn,
                label,
                hint,
                available,
                codec: Some("h264".into()),
            };
            match quality_map.get(&qn) {
                Some((bit, _)) if *bit >= item.bit => {}
                _ => {
                    quality_map.insert(qn, (item.bit, info));
                }
            }
        }
        let mut qualities: Vec<QualityInfo> = quality_map
            .into_values()
            .map(|(_, quality)| quality)
            .collect();
        qualities.sort_by(|a, b| b.qn.cmp(&a.qn));

        Ok(StreamPlan { groups, qualities })
    }
}

struct EncFetched {
    error: i64,
    msg: String,
    data: Option<EncData>,
    date: Option<i64>,
}

async fn fetch_encryption(did: &str, referer: &str) -> Result<EncFetched, PlatformError> {
    let url = format!("{ENC_API}?did={did}");
    let fetched: http::Fetched<EncResponse> = http::get_json_with_date(&url, referer, &[]).await?;
    Ok(EncFetched {
        error: fetched.data.error,
        msg: fetched.data.msg,
        data: fetched.data.data,
        date: fetched.date,
    })
}

struct PlayFetched {
    error: i64,
    msg: String,
    data: Option<PlayData>,
}

#[allow(clippy::too_many_arguments)]
async fn request_play(
    room_key: &str,
    referer: &str,
    did: &str,
    enc_data: &str,
    ts: i64,
    auth: &str,
    cdn: &str,
    rate: i64,
) -> Result<PlayFetched, PlatformError> {
    let url = format!("{PLAY_API}/{room_key}");
    let form = vec![
        ("enc_data", enc_data.to_string()),
        ("tt", ts.to_string()),
        ("did", did.to_string()),
        ("auth", auth.to_string()),
        ("cdn", cdn.to_string()),
        ("rate", rate.to_string()),
        ("hevc", "0".to_string()),
        ("fa", "0".to_string()),
        ("ive", "0".to_string()),
    ];
    let fetched: http::Fetched<PlayResponse> =
        http::post_form_json(&url, referer, &form, &[]).await?;
    Ok(PlayFetched {
        error: fetched.data.error,
        msg: fetched.data.msg,
        data: fetched.data.data,
    })
}

/// 斗鱼签名：f 迭代 md5(f + key) enc_time 次，再拼 key 与（rid + ts）后缀求 md5。
fn sign_auth(
    rand_str: &str,
    key: &str,
    enc_time: u32,
    is_special: i64,
    room_key: &str,
    ts: i64,
) -> String {
    let mut f = rand_str.to_string();
    for _ in 0..enc_time {
        f = md5_hex(&format!("{f}{key}"));
    }
    let suffix = if is_special == 1 {
        String::new()
    } else {
        format!("{room_key}{ts}")
    };
    md5_hex(&format!("{f}{key}{suffix}"))
}

fn md5_hex(value: &str) -> String {
    md5::compute(value.as_bytes())
        .0
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// qn 是跨平台统一的「期望画质」；斗鱼这里映射到 rate。
/// rate2=高清、rate3=超清、rate4=蓝光4M、rate8=蓝光8M。
/// 平台新增的其它 rate 用 1000+rate 作为稳定 qn，保证每个档位都能单独选择。
pub fn rate_for_qn(qn: u32) -> i64 {
    match qn {
        0 | 10000 | 40000 => 0,
        400 => 4,
        401 => 8,
        250 => 3,
        150 => 2,
        80 => 1,
        1000..=9999 => (qn - 1000) as i64,
        _ => 0,
    }
}

/// 平台实际返回的 rate 反推成统一 qn，供 UI 显示「已降级」。
/// rate4/8 分别是蓝光4M/蓝光8M，不能合并。
pub fn qn_for_rate(rate: i64) -> u32 {
    match rate {
        0 => 10000,
        1 => 80,
        2 => 150,
        3 => 250,
        4 => 400,
        8 => 401,
        value if (1..=8999).contains(&value) => 1000 + value as u32,
        _ => 0,
    }
}

/// 画质是否真的可选：平台实际返回的档位才是上限。
/// `best_qn == 0` 表示平台没有给出可判定档位，此时不要误伤全部画质。
fn quality_available(qn: u32, best_qn: u32) -> bool {
    best_qn == 0 || qn <= best_qn
}

fn map_play_error(code: i64, message: &str) -> PlatformError {
    if message.contains("未开播") || message.contains("下播") || message.contains("no live") {
        PlatformError::NotLive
    } else {
        PlatformError::Upstream {
            code,
            message: non_empty(message, "斗鱼取流失败"),
        }
    }
}

fn non_empty(value: &str, fallback: &str) -> String {
    if value.trim().is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

fn label(name: &str, cdn: &str) -> String {
    if name.trim().is_empty() {
        if cdn.is_empty() {
            "默认线路".to_string()
        } else {
            cdn.to_string()
        }
    } else {
        name.to_string()
    }
}

fn join_url(base: &str, live: &str) -> Option<String> {
    if base.is_empty() || live.is_empty() {
        return None;
    }
    Some(format!("{}/{live}", base.trim_end_matches('/')))
}

fn extract_room_key(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    // 显式前缀：douyu:9999
    if let Some(rest) = trimmed.strip_prefix("douyu:") {
        let rest = rest.trim();
        return is_room_token(rest).then(|| rest.to_string());
    }
    if trimmed.chars().all(|ch| ch.is_ascii_digit()) {
        return Some(trimmed.to_string());
    }

    let url = Url::parse(trimmed).ok()?;
    if !(url.host_str()? == "douyu.com" || url.host_str()?.ends_with(".douyu.com")) {
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

impl DouyuAdapter {
    /// 别名房间（/xxkk）要先从房间页抠出真实数字 id。
    async fn real_room_key(&self, key: &str) -> Result<String, PlatformError> {
        if key.chars().all(|ch| ch.is_ascii_digit()) {
            return Ok(key.to_string());
        }
        let url = format!("{ROOM_HOST}/{key}");
        let (body, _) = http::get_text(&url, REFERER_HOME, &[]).await?;
        extract_room_id(&body).ok_or_else(|| PlatformError::RoomNotFound(format!("斗鱼房间 {key}")))
    }
}

/// 按 docs/06 §2.1 的五条正则顺序从页面里找 room_id。
fn extract_room_id(body: &str) -> Option<String> {
    const PATTERNS: [&str; 5] = [
        "$ROOM.room_id",
        "room_id =",
        "\"room_id\":",
        // 房间页把配置塞进 JS 字符串时，引号会转义成 \"room_id\":。
        "room_id\\\":",
        "data-onlineid=",
    ];
    for pattern in PATTERNS {
        let Some(index) = body.find(pattern) else {
            continue;
        };
        let rest = &body[index + pattern.len()..];
        let digits: String = rest
            .trim_start_matches([' ', '=', ':', '"', '\''])
            .chars()
            .take_while(|ch| ch.is_ascii_digit())
            .collect();
        if digits.len() >= 3 {
            return Some(digits);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_room_ids_and_urls() {
        let adapter = DouyuAdapter::new();
        assert_eq!(adapter.resolve("9999").unwrap().room_key, "9999");
        assert_eq!(adapter.resolve("douyu:288016").unwrap().room_key, "288016");
        assert_eq!(
            adapter
                .resolve("https://www.douyu.com/9999?dyshid=abc")
                .unwrap()
                .room_key,
            "9999"
        );
        assert_eq!(
            adapter
                .resolve("https://www.douyu.com/xxkk")
                .unwrap()
                .room_key,
            "xxkk"
        );
        assert!(adapter.resolve("https://www.huya.com/660000").is_none());
        assert!(adapter
            .resolve("https://live.bilibili.com/545068")
            .is_none());
    }

    #[test]
    fn signature_matches_reference_flow() {
        // 固定输入下的期望值由算法本身推导：先确认 f 的迭代与后缀拼接没有抄错。
        let rand_str = "AXBY1OzrrABZDsPK";
        let key = "8sQ3dR1NfvdZfB5ds4eT465672";
        let auth = sign_auth(rand_str, key, 1, 0, "9999", 1_800_000_000);
        let once = md5_hex(&format!("{rand_str}{key}"));
        assert_eq!(auth, md5_hex(&format!("{once}{key}99991800000000")));

        // is_special=1 时没有 rid+ts 后缀
        let special = sign_auth(rand_str, key, 1, 1, "9999", 1_800_000_000);
        assert_eq!(special, md5_hex(&format!("{once}{key}")));
    }

    #[test]
    fn maps_qn_and_rate_both_ways() {
        assert_eq!(rate_for_qn(10000), 0);
        assert_eq!(rate_for_qn(400), 4);
        assert_eq!(rate_for_qn(401), 8);
        assert_eq!(rate_for_qn(250), 3);
        assert_eq!(rate_for_qn(150), 2);
        assert_eq!(rate_for_qn(80), 1);
        assert_eq!(rate_for_qn(0), 0);
        assert_eq!(qn_for_rate(8), 401);
        assert_eq!(qn_for_rate(4), 400);
        assert_eq!(qn_for_rate(3), 250);
        assert_eq!(qn_for_rate(2), 150);
        assert_eq!(qn_for_rate(1), 80);
        assert_eq!(qn_for_rate(0), 10000);
        // 平台新增档位：rate5 等仍可用 1000+rate 唯一选择。
        assert_eq!(qn_for_rate(5), 1005);
        assert_eq!(rate_for_qn(1005), 5);
        // 关键不变量：任何平台 rate 都能 qn→rate 回到自己，不能两个档位共用一个 qn。
        for rate in 0..=20 {
            let qn = qn_for_rate(rate);
            assert_eq!(rate_for_qn(qn), rate, "rate {rate} 的 qn {qn} 无法回选");
        }
    }

    #[test]
    fn quality_available_follows_actual_rate() {
        // 登录态拿到原画时，全档可选。
        assert!(quality_available(10000, qn_for_rate(0)));
        assert!(quality_available(401, qn_for_rate(0)));
        // 未登录请求原画只拿到蓝光 4M 时，原画/蓝光 8M 必须不可选。
        let anon = qn_for_rate(4);
        assert!(!quality_available(10000, anon));
        assert!(!quality_available(401, anon));
        assert!(quality_available(400, anon));
        assert!(quality_available(250, anon));
        assert!(quality_available(80, anon));
        // 平台没有给出可判定档位时，不能把所有画质都判死。
        assert!(quality_available(10000, 0));
    }

    #[test]
    fn extracts_room_id_from_page() {
        let body = "var $ROOM = {}; $ROOM.room_id = 123456; ";
        assert_eq!(extract_room_id(body).as_deref(), Some("123456"));
        let body2 = "<div data-onlineid=8888888>";
        assert_eq!(extract_room_id(body2).as_deref(), Some("8888888"));
        let escaped = r#"window.__CONFIG__ = "{\"room_id\":45977,\"title\":\"x\"}";"#;
        assert_eq!(extract_room_id(escaped).as_deref(), Some("45977"));
        assert!(extract_room_id("<html>nothing</html>").is_none());
    }

    /// 联网冒烟：cargo test --lib -- --ignored --nocapture douyu
    #[tokio::test]
    #[ignore = "需要访问斗鱼接口"]
    async fn live_room_probe() {
        let adapter = DouyuAdapter::new();
        let room = adapter.resolve("douyu:9999").expect("解析房间号失败");
        let info = adapter.room_info(&room).await.expect("取房间信息失败");
        println!(
            "room: {} / {} live={} did={}",
            info.host_name,
            info.room_name,
            info.is_live,
            adapter.device_id()
        );
        match adapter.stream_groups(&room, 10000).await {
            Ok(groups) => {
                println!("groups={}", groups.len());
                for group in &groups {
                    for candidate in &group.candidates {
                        println!(
                            "  line {} / {} weight={} qn={} url={}",
                            group.name,
                            candidate.name,
                            candidate.weight,
                            candidate.attrs.get("qn").cloned().unwrap_or_default(),
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
