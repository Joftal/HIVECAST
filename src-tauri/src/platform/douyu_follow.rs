//! 斗鱼关注列表：导入、批量状态发现。
//!
//! 关注列表接口一次返回 120 条，含房间号、主播名、标题、封面、在线人数、
//! show_status 与 videoLoop。批量监控只把它当作「状态发现」层；取流、画质、
//! 线路、弹幕仍然走每个房间自己的 betard / getEncryption / getH5PlayV1。
//!
//! 关键边界：
//! - 未登录时接口返回 error=-1，不能把任何房间判成离线；
//! - 返回体里的 show_status=1 还可能是轮播，必须同时满足 videoLoop=0；
//! - 列表只覆盖「关注」的房间，未导入/已取关的房间要由调用方回退单房检查。

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Deserializer};

use hivecast_ipc::{DouyuFollowingPage, LiveStatus};

use super::http;
use super::PlatformError;

const FOLLOW_API: &str = "https://www.douyu.com/wgapi/livenc/liveweb/follow/list";
const REFERER: &str = "https://www.douyu.com/directory/myFollow";
const PAGE_SIZE: u32 = 120;
/// 防止异常账号（关注数极大）把一次批量检查拖成几十页请求。
/// 超过这个页数仍没找到的房间会回退单房 betard，不会漏掉状态。
const MAX_PAGES: u32 = 20;

#[derive(Debug, Deserialize)]
struct FollowEnvelope {
    #[serde(default)]
    error: i64,
    #[serde(default)]
    msg: String,
    #[serde(default)]
    data: Option<FollowData>,
}

#[derive(Debug, Deserialize)]
struct FollowData {
    #[serde(default)]
    total: u64,
    #[serde(default, rename = "pageCount")]
    page_count: u32,
    #[serde(default, rename = "nowPage")]
    now_page: u32,
    #[serde(default)]
    list: Vec<FollowItem>,
}

#[derive(Debug, Deserialize)]
struct FollowItem {
    #[serde(default, deserialize_with = "de_id_string")]
    room_id: String,
    #[serde(default)]
    room_name: String,
    #[serde(default)]
    nickname: String,
    #[serde(default)]
    avatar_small: Option<String>,
    #[serde(default)]
    room_src: Option<String>,
    #[serde(default)]
    show_status: i64,
    #[serde(default, rename = "videoLoop")]
    video_loop: i64,
    #[serde(default)]
    show_time: i64,
    #[serde(default, deserialize_with = "de_string")]
    online: String,
    #[serde(default)]
    game_name: Option<String>,
}

impl FollowItem {
    fn into_status(self) -> Option<LiveStatus> {
        let room_id = self.room_id.trim().to_string();
        if room_id.is_empty() {
            return None;
        }
        // 轮播录像的 show_status 也是 1，必须排除，否则会 7×24 一直录。
        let is_live = self.show_status == 1 && self.video_loop == 0;
        let title = decode_html_entities(&self.room_name);
        let uname = decode_html_entities(&self.nickname);
        Some(LiveStatus {
            // 斗鱼没有独立的关注 UID；房间号就是导入、去重与批量映射的键。
            uid: room_id.clone(),
            room_id: Some(room_id),
            short_id: None,
            uname: if uname.trim().is_empty() {
                "斗鱼主播".to_string()
            } else {
                uname
            },
            title,
            avatar_url: non_empty(self.avatar_small),
            cover_url: non_empty(self.room_src),
            area_name: non_empty(self.game_name),
            is_live,
            video_loop: self.video_loop != 0,
            // show_time 在离线时是「上次开播」，不能当作本场开播时间。
            started_at: if is_live && self.show_time > 0 {
                Some(self.show_time)
            } else {
                None
            },
            online: parse_online(&self.online),
        })
    }
}

/// 拉取一页关注列表。斗鱼服务端硬上限 120 条/页（传更大的 limit 仍只返回 120）。
pub async fn fetch_follow_page(page: u32) -> Result<DouyuFollowingPage, PlatformError> {
    let page = page.max(1);
    let url = format!("{FOLLOW_API}?sort=0&cid1=0&limit={PAGE_SIZE}&page={page}");
    let envelope: FollowEnvelope = http::get_json(&url, REFERER, &[]).await?;
    if envelope.error != 0 {
        let message = envelope.msg.trim().to_string();
        return Err(PlatformError::Upstream {
            code: envelope.error,
            message: if message.is_empty() {
                "斗鱼关注列表请求失败".to_string()
            } else {
                message
            },
        });
    }
    let data = envelope
        .data
        .ok_or_else(|| PlatformError::Parse("斗鱼关注列表缺少 data".into()))?;
    let total = data.total;
    let page_count = data
        .page_count
        .max(if total > 0 {
            ((total as f64) / PAGE_SIZE as f64).ceil() as u32
        } else {
            1
        })
        .max(1);
    let items: Vec<LiveStatus> = data
        .list
        .into_iter()
        .filter_map(FollowItem::into_status)
        .collect();
    Ok(DouyuFollowingPage {
        total,
        page: data.now_page.max(page),
        page_size: PAGE_SIZE,
        page_count,
        items,
    })
}

/// 批量查一组已导入的斗鱼房间状态。
///
/// 只返回在关注列表里找到的房间；调用方必须把没返回的房间回退到单房 betard，
/// 不能把「没返回」当成离线。关注数超过 MAX_PAGES 页时也只读到上限。
pub async fn fetch_live_statuses(room_ids: &[String]) -> Result<Vec<LiveStatus>, PlatformError> {
    let wanted: HashSet<&str> = room_ids
        .iter()
        .map(|id| id.trim())
        .filter(|id| !id.is_empty())
        .collect();
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let mut found: HashMap<String, LiveStatus> = HashMap::new();
    let mut page = 1u32;
    let mut page_count = 1u32;
    while page <= page_count && page <= MAX_PAGES {
        let fetched = fetch_follow_page(page).await?;
        page_count = fetched.page_count.max(1);
        for status in fetched.items {
            if wanted.contains(status.uid.as_str()) {
                found.entry(status.uid.clone()).or_insert(status);
            }
        }
        if found.len() >= wanted.len() || page >= page_count {
            break;
        }
        page += 1;
    }
    Ok(found.into_values().collect())
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn de_id_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::String(text) => text,
        serde_json::Value::Number(number) => number.to_string(),
        _ => String::new(),
    })
}

fn de_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::String(text) => text,
        serde_json::Value::Number(number) => number.to_string(),
        _ => String::new(),
    })
}

/// 斗鱼在线人数是 "475.4万"、"2093" 这类字符串；解析失败不阻断整行导入。
fn parse_online(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let (number, factor) = if let Some(value) = raw.strip_suffix('万') {
        (value, 10_000f64)
    } else if let Some(value) = raw.strip_suffix('亿') {
        (value, 100_000_000f64)
    } else {
        (raw, 1f64)
    };
    let value: f64 = number.trim().parse().ok()?;
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    Some((value * factor).round() as u64)
}

/// 斗鱼标题里会出现 &amp;、&nbsp; 等 HTML 实体；只解码常见实体，避免引入依赖。
fn decode_html_entities(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(index) = rest.find('&') {
        out.push_str(&rest[..index]);
        let tail = &rest[index..];
        let Some(end) = tail.find(';').filter(|end| *end <= 10) else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let entity = &tail[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| {
                    entity
                        .strip_prefix('#')
                        .and_then(|dec| dec.parse::<u32>().ok())
                })
                .and_then(char::from_u32),
        };
        match decoded {
            Some(ch) => {
                out.push(ch);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(room_id: &str, show_status: i64, video_loop: i64) -> FollowItem {
        FollowItem {
            room_id: room_id.into(),
            room_name: "标题 &amp; 副标题".into(),
            nickname: "主播".into(),
            avatar_small: Some("https://a/avatar.jpg".into()),
            room_src: Some("https://a/cover.jpg".into()),
            show_status,
            video_loop,
            show_time: 1_792_050_615,
            online: "475.4万".into(),
            game_name: Some("英雄联盟".into()),
        }
    }

    #[test]
    fn live_requires_show_status_and_not_loop() {
        let live = item("9999", 1, 0).into_status().unwrap();
        assert!(live.is_live);
        assert!(!live.video_loop);
        assert_eq!(live.started_at, Some(1_792_050_615));
        assert_eq!(live.online, Some(4_754_000));

        let looped = item("9418", 1, 1).into_status().unwrap();
        assert!(!looped.is_live, "轮播录像不能当直播");
        assert!(looped.video_loop);
        assert_eq!(looped.started_at, None, "轮播没有本场开播时间");

        let offline = item("9998", 2, 0).into_status().unwrap();
        assert!(!offline.is_live);
        assert_eq!(offline.started_at, None);
    }

    #[test]
    fn decodes_titles_and_online_units() {
        assert_eq!(
            decode_html_entities("崩铁&amp;原神&nbsp;直播"),
            "崩铁&原神 直播"
        );
        assert_eq!(decode_html_entities("&#20320;&#x597d;"), "你好");
        assert_eq!(parse_online("2093"), Some(2093));
        assert_eq!(parse_online("475.4万"), Some(4_754_000));
        assert_eq!(parse_online("1.2亿"), Some(120_000_000));
        assert_eq!(parse_online(""), None);
        assert_eq!(parse_online("bad"), None);
    }

    #[test]
    fn follows_without_room_id_are_skipped() {
        let mut missing = item("", 1, 0);
        missing.room_id = String::new();
        assert!(missing.into_status().is_none());
    }

    #[test]
    fn parses_follow_envelope_with_numeric_room_id() {
        let json = r#"{
            "error": 0,
            "data": {
                "total": 2,
                "pageCount": 1,
                "nowPage": 1,
                "list": [
                    {"room_id": 9999, "room_name": "A", "nickname": "甲", "show_status": 1, "videoLoop": 0, "show_time": 100, "online": "10"},
                    {"room_id": "9418", "room_name": "B", "nickname": "乙", "show_status": 1, "videoLoop": 1, "show_time": 200, "online": "0"}
                ]
            }
        }"#;
        let envelope: FollowEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(envelope.error, 0);
        let data = envelope.data.unwrap();
        assert_eq!(data.total, 2);
        assert_eq!(data.page_count, 1);
        let statuses: Vec<LiveStatus> = data
            .list
            .into_iter()
            .filter_map(FollowItem::into_status)
            .collect();
        assert_eq!(statuses[0].uid, "9999");
        assert!(statuses[0].is_live);
        assert_eq!(statuses[1].uid, "9418");
        assert!(!statuses[1].is_live);
    }
}
