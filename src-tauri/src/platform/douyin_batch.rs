//! 抖音关注页批量直播状态。
//!
//! 抖音单房检查要「页面 GET 拿 ttwid + webcast/room/web/enter」两个请求；
//! 关注列表里的房间可以走 follow/feed，一次带 20 个内部 room_id，只返回在播的。
//! 这里只做状态发现：录制、画质、线路、弹幕仍然由各自房间独立请求。
//!
//! 边界：
//! - 必须登录：匿名请求只会得到 blocked / 空 body；
//! - 没返回的房间不能一律判离线：离线关注和取关都会缺失，调用方要结合
//!   「上一轮是否在播」决定回退单房还是保持离线；
//! - rawdata 可能只有 additional_stream_url，解析不出 id/status 时计入
//!   unresolved，调用方不能拿这一批的缺失项做离线推断。

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use hivecast_ipc::LiveStatus;

use super::{abogus, http, PlatformError};
use crate::credential;

const FEED_API: &str = "https://www.douyin.com/aweme/v1/web/follow/feed/";
const REFERER: &str = "https://www.douyin.com/follow";
/// 实测 20 个 room_id 一批可接受；再多也没有收益，反而更容易触发风控。
pub const BATCH_SIZE: usize = 20;

#[derive(Debug, Default)]
pub struct BatchStatuses {
    pub statuses: Vec<LiveStatus>,
    /// 返回项里无法解析出房间 ID/状态的个数。大于 0 时，缺失房间不能判离线。
    pub unresolved: usize,
}

/// 批量查一组已导入的抖音关注房状态。
///
/// room_ids 是内部 19 位 room_id，不是 HIVECAST 的 room_key（web_rid）。
/// 只返回接口里出现的房间；调用方必须自己决定缺失项怎么处理。
pub async fn fetch_live_statuses(room_ids: &[String]) -> Result<BatchStatuses, PlatformError> {
    let cookie = credential::cookie_for_url(REFERER)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| PlatformError::Upstream {
            code: 0,
            message: "抖音未登录，无法使用关注批量状态".into(),
        })?;

    let mut seen = HashSet::new();
    let ids: Vec<String> = room_ids
        .iter()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .filter(|id| seen.insert(id.clone()))
        .collect();
    if ids.is_empty() {
        return Ok(BatchStatuses::default());
    }

    let mut by_id: HashMap<String, LiveStatus> = HashMap::new();
    let mut unresolved = 0usize;
    for chunk in ids.chunks(BATCH_SIZE) {
        let fetched = fetch_chunk(chunk, &cookie).await?;
        unresolved += fetched.unresolved;
        for status in fetched.statuses {
            by_id.entry(status.uid.clone()).or_insert(status);
        }
    }
    Ok(BatchStatuses {
        statuses: by_id.into_values().collect(),
        unresolved,
    })
}

async fn fetch_chunk(room_ids: &[String], cookie: &str) -> Result<BatchStatuses, PlatformError> {
    let params = format!(
        "device_platform=webapp&aid=6383&channel=channel_pc_web&cursor=0&level=3&count=20&pull_type=0&refresh_type=0&room_ids={}",
        room_ids.join(",")
    );
    let signed = abogus::sign(&params, "", &abogus::SignOptions::now());
    let url = format!("{FEED_API}?{signed}");
    let cookie = reqwest::header::HeaderValue::from_str(cookie)
        .map_err(|err| PlatformError::Network(format!("抖音 Cookie 头无效：{err}")))?;

    let response = http::client()
        .get(&url)
        .header("User-Agent", abogus::SIGN_UA)
        .header("Referer", REFERER)
        .header("Accept-Language", "zh-CN,zh;q=0.9")
        .header(reqwest::header::COOKIE, cookie)
        .send()
        .await
        .map_err(|err| PlatformError::Network(format!("{FEED_API} → {err}")))?;

    if !response.status().is_success() {
        let status = response.status().as_u16();
        // 429/412 是平台明确的限流信号，交给监控做整平台冷却；
        // 其他 5xx 只降级单房，不让整批停 30 秒。
        if matches!(status, 412 | 429) {
            return Err(PlatformError::Upstream {
                code: status as i64,
                message: format!("抖音关注批量状态 HTTP {status}"),
            });
        }
        return Err(PlatformError::Network(format!(
            "{FEED_API} → HTTP {status}"
        )));
    }

    let body = response
        .text()
        .await
        .map_err(|err| PlatformError::Parse(format!("{FEED_API} → {err}")))?;
    if body.trim().is_empty() {
        return Err(PlatformError::Upstream {
            code: 0,
            message: "抖音关注批量状态返回空响应（Cookie、a_bogus 或风控校验失败）".into(),
        });
    }
    let root: Value = serde_json::from_str(&body)
        .map_err(|err| PlatformError::Parse(format!("{FEED_API} → {err}")))?;
    parse_feed_payload(&root)
}

fn parse_feed_payload(root: &Value) -> Result<BatchStatuses, PlatformError> {
    let code = root.get("status_code").and_then(Value::as_i64).unwrap_or(0);
    if code != 0 {
        let message = root
            .get("status_msg")
            .and_then(Value::as_str)
            .unwrap_or("抖音关注批量状态请求失败");
        return Err(PlatformError::Upstream {
            code,
            message: message.to_string(),
        });
    }

    let items = root
        .get("data")
        .and_then(|data| {
            data.as_array()
                .cloned()
                .or_else(|| data.get("data").and_then(Value::as_array).cloned())
                .or_else(|| data.get("rooms").and_then(Value::as_array).cloned())
        })
        .unwrap_or_default();

    let mut statuses = Vec::new();
    let mut unresolved = 0usize;
    for item in &items {
        let Some(raw) = room_rawdata(item) else {
            unresolved += 1;
            continue;
        };
        match parse_room(&raw) {
            Some(status) => statuses.push(status),
            None => unresolved += 1,
        }
    }
    Ok(BatchStatuses {
        statuses,
        unresolved,
    })
}

/// follow/feed 的一项可能是 {cell_room:{rawdata:"json"}}，也可能是已经展开的房间对象。
fn room_rawdata(item: &Value) -> Option<Value> {
    let cell = item.get("cell_room").unwrap_or(item);
    let raw = cell.get("rawdata").unwrap_or(cell);
    match raw {
        Value::String(text) => serde_json::from_str(text).ok(),
        Value::Object(_) => Some(raw.clone()),
        _ => None,
    }
}

fn parse_room(raw: &Value) -> Option<LiveStatus> {
    let room_id = text(raw, &["id_str", "id", "room_id"]).or_else(|| {
        raw.get("room")
            .and_then(|room| text(room, &["id_str", "id"]))
    })?;
    if room_id.is_empty() {
        return None;
    }
    let status_code = raw
        .get("status")
        .and_then(Value::as_i64)
        .or_else(|| raw.get("room_status").and_then(Value::as_i64))
        .unwrap_or(0);
    let is_live = status_code == 2;
    let owner = raw.get("owner");
    let owner_name = owner
        .and_then(|owner| text(owner, &["nickname", "name"]))
        .unwrap_or_default();
    let title = text(raw, &["title", "room_name"]).unwrap_or_default();
    let avatar_url = owner
        .and_then(|owner| first_url(owner.get("avatar_thumb")))
        .or_else(|| owner.and_then(|owner| first_url(owner.get("avatar_medium"))));
    let cover_url = first_url(raw.get("cover"));
    let online = raw.get("user_count").and_then(Value::as_u64).or_else(|| {
        raw.get("user_count_str")
            .and_then(Value::as_str)
            .and_then(parse_count)
    });
    let started_at = if is_live {
        raw.get("start_time")
            .and_then(Value::as_i64)
            .or_else(|| raw.get("create_time").and_then(Value::as_i64))
            .map(epoch_seconds)
            .filter(|value| *value > 0)
    } else {
        None
    };
    Some(LiveStatus {
        // 批量映射用内部 room_id，和监控里的 room_key 完全一致。
        uid: room_id.clone(),
        room_id: Some(room_id),
        short_id: owner
            .and_then(|owner| text(owner, &["short_id", "web_rid"]))
            .filter(|value| !value.trim().is_empty()),
        uname: if owner_name.trim().is_empty() {
            "抖音主播".to_string()
        } else {
            owner_name
        },
        title,
        avatar_url,
        cover_url,
        area_name: None,
        is_live,
        video_loop: false,
        started_at,
        online,
    })
}

fn text(value: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        let Some(field) = value.get(key) else {
            continue;
        };
        let text = match field {
            Value::String(text) => text.trim().to_string(),
            Value::Number(number) => number.to_string(),
            _ => continue,
        };
        if !text.is_empty() {
            return Some(text);
        }
    }
    None
}

fn first_url(value: Option<&Value>) -> Option<String> {
    let value = value?;
    match value {
        Value::String(text) => non_empty(text.clone()),
        Value::Array(items) => items.iter().find_map(|item| first_url(Some(item))),
        Value::Object(map) => map
            .get("url_list")
            .and_then(Value::as_array)
            .and_then(|items| items.iter().find_map(|item| first_url(Some(item))))
            .or_else(|| map.get("url").and_then(|url| first_url(Some(url)))),
        _ => None,
    }
}

fn non_empty(value: String) -> Option<String> {
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
}

/// start_time/create_time 有时是毫秒时间戳，统一成秒。
fn epoch_seconds(value: i64) -> i64 {
    if value > 10_000_000_000 {
        value / 1000
    } else {
        value
    }
}

/// 抖音会返回 "1.2万" 这类在线人数；解析失败不阻断房间状态。
fn parse_count(raw: &str) -> Option<u64> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(data: Value) -> Value {
        serde_json::json!({ "status_code": 0, "data": data })
    }

    #[test]
    fn parses_cell_room_rawdata_and_ignores_offline() {
        let raw = serde_json::json!({
            "id_str": "7708331906513767187",
            "status": 2,
            "title": "再见一面",
            "user_count": 1234,
            "start_time": 1_700_000_000,
            "owner": {
                "nickname": "不吃🍐",
                "web_rid": "938611933111",
                "avatar_thumb": { "url_list": ["https://example.com/a.jpg"] }
            },
            "cover": { "url_list": ["https://example.com/c.jpg"] }
        });
        let root = envelope(serde_json::json!([
            { "cell_room": { "rawdata": raw.to_string() } }
        ]));
        let parsed = parse_feed_payload(&root).expect("解析");
        assert_eq!(parsed.statuses.len(), 1);
        assert_eq!(parsed.unresolved, 0);
        let status = &parsed.statuses[0];
        assert_eq!(status.uid, "7708331906513767187");
        assert_eq!(status.room_id.as_deref(), Some("7708331906513767187"));
        assert_eq!(status.uname, "不吃🍐");
        assert!(status.is_live);
        assert_eq!(status.online, Some(1234));
        assert_eq!(status.started_at, Some(1_700_000_000));
    }

    #[test]
    fn malformed_rawdata_is_counted_as_unresolved() {
        let root = envelope(serde_json::json!([
            { "cell_room": { "rawdata": "{\"additional_stream_url\":{}}" } }
        ]));
        let parsed = parse_feed_payload(&root).expect("解析");
        assert!(parsed.statuses.is_empty());
        assert_eq!(parsed.unresolved, 1);
    }

    #[test]
    fn timestamps_in_milliseconds_are_normalized() {
        assert_eq!(epoch_seconds(1_700_000_000), 1_700_000_000);
        assert_eq!(epoch_seconds(1_700_000_000_000), 1_700_000_000);
    }

    #[test]
    fn non_zero_status_code_is_an_error_not_offline() {
        let root = serde_json::json!({ "status_code": 10011, "status_msg": "blocked" });
        let err = parse_feed_payload(&root).expect_err("应报错");
        assert!(err.to_string().contains("blocked"));
    }

    #[test]
    fn empty_data_means_no_live_rooms_not_an_error() {
        let root = envelope(Value::Null);
        let parsed = parse_feed_payload(&root).expect("解析");
        assert!(parsed.statuses.is_empty());
        assert_eq!(parsed.unresolved, 0);
    }
}
