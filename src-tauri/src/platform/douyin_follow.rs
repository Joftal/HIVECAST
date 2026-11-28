//! 抖音关注列表与关注池。
//!
//! 抖音关注项只有在播时才带 room_data，离线关注没有 web_rid，也没有可批量
//! 查询的内部 room_id，因此不能直接建房。这里把关注项先放进 douyin_follow
//! 池：在播且拿得到 web_rid 的立即转成正式房间；其余留在池里，由一个低频
//! 后台任务顺序翻完关注列表，开播后自动转正。
//!
//! 关注列表分页靠 max_time = 上一页 min_time，不是 offset；每页固定 20 条。
//! 全量扫描 1531 个关注约 77 个请求，只在还有待转正关注时执行；扫描节奏跟随
//! 设置里的「监控检测间隔」，0（关闭自动检测）时一并暂停。

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::Context;
use serde_json::Value;
use tauri::{AppHandle, Emitter};

use hivecast_ipc::{DouyinFollow, DouyinFollowingPage, RoomRecord};

use super::douyin::DouyinAdapter;
use super::douyin_batch;
use super::{abogus, http, PlatformAdapter, PlatformError, RoomRef};
use crate::credential;
use crate::db::{now_ts, Db};
use crate::recorder::EVENT_ROOMS_CHANGED;

const FOLLOW_API: &str = "https://www.douyin.com/aweme/v1/web/user/following/list/";
const PROFILE_API: &str = "https://www.douyin.com/aweme/v1/web/user/profile/self/?device_platform=webapp&aid=6383&channel=channel_pc_web&publish_video_strategy_type=2&source=channel_pc_web";
const REFERER: &str = "https://www.douyin.com/follow";
/// 设置变更的检查粒度：间隔改小或关闭后，不必等完旧的睡眠周期。
const POOL_TICK: Duration = Duration::from_secs(2);
/// 启动后的首次检查上限：监控间隔很长时也别让关注池干等一整个周期。
const POOL_STARTUP_MAX: Duration = Duration::from_secs(25);
const PROFILE_TTL: Duration = Duration::from_secs(600);

#[derive(Debug, Default)]
pub(crate) struct PoolStats {
    pages: usize,
    matched: usize,
    promoted: usize,
}

#[derive(Debug, Clone)]
struct Identity {
    uid: String,
    sec_uid: String,
}

/// 自己的 uid/sec_uid 在翻页时是固定参数，缓存起来避免每页都打一次 profile。
static IDENTITY: Mutex<Option<(u64, Identity, Instant)>> = Mutex::new(None);

fn cookie_hash(cookie: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    cookie.hash(&mut hasher);
    hasher.finish()
}

fn login_cookie() -> Result<String, PlatformError> {
    credential::cookie_for_url(REFERER)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| PlatformError::Upstream {
            code: 0,
            message: "抖音未登录，无法读取关注列表".into(),
        })
}

async fn get_json(url: &str, cookie: &str) -> Result<Value, PlatformError> {
    let cookie = reqwest::header::HeaderValue::from_str(cookie)
        .map_err(|err| PlatformError::Network(format!("抖音 Cookie 头无效：{err}")))?;
    let response = http::client()
        .get(url)
        .header("User-Agent", abogus::SIGN_UA)
        .header("Referer", REFERER)
        .header("Accept-Language", "zh-CN,zh;q=0.9")
        .header(reqwest::header::COOKIE, cookie)
        .send()
        .await
        .map_err(|err| PlatformError::Network(format!("{url} → {err}")))?;

    let status = response.status();
    if !status.is_success() {
        let code = status.as_u16();
        if matches!(code, 401 | 403) {
            return Err(PlatformError::Upstream {
                code: code as i64,
                message: "抖音登录态已失效，请重新登录".into(),
            });
        }
        if matches!(code, 412 | 429) {
            return Err(PlatformError::Upstream {
                code: code as i64,
                message: format!("抖音关注列表请求被限流（HTTP {code}）"),
            });
        }
        return Err(PlatformError::Network(format!("{url} → HTTP {code}")));
    }

    let body = response
        .text()
        .await
        .map_err(|err| PlatformError::Parse(format!("{url} → {err}")))?;
    if body.trim().is_empty() {
        return Err(PlatformError::Upstream {
            code: 0,
            message: "抖音关注列表返回空响应（Cookie 或风控校验失败）".into(),
        });
    }
    serde_json::from_str(&body).map_err(|err| PlatformError::Parse(format!("{url} → {err}")))
}

async fn identity(cookie: &str) -> Result<Identity, PlatformError> {
    let hash = cookie_hash(cookie);
    if let Ok(guard) = IDENTITY.lock() {
        if let Some((saved_hash, saved, at)) = guard.as_ref() {
            if *saved_hash == hash && at.elapsed() < PROFILE_TTL {
                return Ok(saved.clone());
            }
        }
    }

    let root = get_json(PROFILE_API, cookie).await?;
    let code = root.get("status_code").and_then(Value::as_i64).unwrap_or(0);
    if code != 0 {
        let message = root
            .get("status_msg")
            .and_then(Value::as_str)
            .unwrap_or("抖音登录态已失效，请重新登录");
        return Err(PlatformError::Upstream {
            code,
            message: message.to_string(),
        });
    }
    let user = root.get("user").unwrap_or(&Value::Null);
    let uid = douyin_batch::text(user, &["uid"])
        .ok_or_else(|| PlatformError::Parse("抖音个人信息缺少 uid，无法读取关注列表".into()))?;
    let sec_uid = douyin_batch::text(user, &["sec_uid"])
        .ok_or_else(|| PlatformError::Parse("抖音个人信息缺少 sec_uid，无法读取关注列表".into()))?;
    let identity = Identity { uid, sec_uid };
    if let Ok(mut guard) = IDENTITY.lock() {
        *guard = Some((hash, identity.clone(), Instant::now()));
    }
    Ok(identity)
}

/// 读取关注列表的一页。cursor 是上一页返回的 min_time；首页传 None。
pub async fn fetch_follow_page(cursor: Option<&str>) -> Result<DouyinFollowingPage, PlatformError> {
    let cookie = login_cookie()?;
    let me = identity(&cookie).await?;
    let max_time = cursor.map(str::trim).filter(|value| !value.is_empty());
    let params = format!(
        "device_platform=webapp&aid=6383&channel=channel_pc_web&user_id={}&sec_user_id={}&offset=0&min_time=0&max_time={}&count=20&source_type=2&gps_access=0&address_book_access=0&is_top=1",
        me.uid,
        me.sec_uid,
        max_time.unwrap_or("0")
    );
    let url = format!("{FOLLOW_API}?{params}");
    let root = get_json(&url, &cookie).await?;
    parse_follow_page(&root, max_time)
}

fn parse_follow_page(
    root: &Value,
    requested_cursor: Option<&str>,
) -> Result<DouyinFollowingPage, PlatformError> {
    let code = root.get("status_code").and_then(Value::as_i64).unwrap_or(0);
    if code != 0 {
        let message = root
            .get("status_msg")
            .and_then(Value::as_str)
            .unwrap_or("抖音关注列表请求失败");
        return Err(PlatformError::Upstream {
            code,
            message: message.to_string(),
        });
    }

    let data = root.get("data").unwrap_or(&Value::Null);
    let items = data
        .get("followings")
        .or_else(|| root.get("followings"))
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(parse_follow_item).collect())
        .unwrap_or_default();
    // 真实接口把 followings/has_more/min_time 放在响应根上（没有 data 包裹）；
    // 早期固定件与部分风控返回会包一层 data，所以两处都读，优先 data。
    let pick = |key: &str| data.get(key).or_else(|| root.get(key));
    let next_cursor = pick("min_time")
        .or_else(|| pick("max_time"))
        .and_then(number_text);
    let has_more_raw = pick("has_more")
        .and_then(|value| match value {
            Value::Bool(value) => Some(*value),
            Value::Number(number) => Some(number.as_i64().unwrap_or(0) != 0),
            _ => None,
        })
        .unwrap_or(false);
    // 最后一页的 min_time 可能为 0 或与当前游标相同；两种情况都说明翻完了。
    let next_cursor = next_cursor.filter(|value| {
        !value.is_empty() && value != "0" && Some(value.as_str()) != requested_cursor
    });
    let has_more = has_more_raw && next_cursor.is_some();
    Ok(DouyinFollowingPage {
        items,
        next_cursor,
        has_more,
    })
}

fn parse_follow_item(item: &Value) -> Option<DouyinFollow> {
    let uid = douyin_batch::text(item, &["uid", "sec_uid"]).or_else(|| {
        item.get("user")
            .and_then(|user| douyin_batch::text(user, &["uid"]))
    })?;
    let sec_uid = douyin_batch::text(item, &["sec_uid"]).unwrap_or_default();
    let uname = douyin_batch::text(item, &["nickname"])
        .or_else(|| {
            item.get("user")
                .and_then(|user| douyin_batch::text(user, &["nickname"]))
        })
        .unwrap_or_else(|| "抖音主播".to_string());
    let avatar_url = douyin_batch::first_url(item.get("avatar_medium"))
        .or_else(|| douyin_batch::first_url(item.get("avatar_thumb")))
        .or_else(|| {
            item.get("user")
                .and_then(|user| douyin_batch::first_url(user.get("avatar_medium")))
        });
    let is_live = item
        .get("live_status")
        .and_then(Value::as_i64)
        .map(|value| value == 1)
        .unwrap_or(false);
    let listed_room_id = douyin_batch::text(item, &["room_id"]).filter(|value| value != "0");
    let mut follow = DouyinFollow {
        uid,
        sec_uid,
        uname,
        avatar_url,
        is_live,
        web_rid: None,
        internal_room_id: listed_room_id,
        title: String::new(),
        cover_url: None,
        area_name: None,
        online: None,
        started_at: None,
        in_pool: false,
        monitor: true,
        auto_record: false,
        promoted_room_id: None,
    };

    // room_data 是字符串或对象；能解析就补标题/封面/在线人数与实际 web_rid。
    // 真实 following/list 的 room_data 只有 owner/stream_url，没有 id_str；
    // 这里把外层 room_id 补进去，否则 parse_room 会因为缺房间号整项丢弃，
    // 关注池就永远拿不到 web_rid、无法转正。
    if let Some(mut raw) = room_data(item) {
        let has_id = ["id_str", "id", "room_id"]
            .iter()
            .any(|key| raw.get(*key).is_some_and(|value| !value.is_null()));
        if !has_id {
            if let Some(listed) = douyin_batch::text(item, &["room_id"]) {
                if let Some(object) = raw.as_object_mut() {
                    object.insert("id_str".to_string(), Value::String(listed));
                }
            }
        }
        if let Some(status) = douyin_batch::parse_room(&raw) {
            follow.is_live = follow.is_live || status.is_live;
            follow.internal_room_id = Some(status.uid);
            if status.short_id.is_some() {
                follow.web_rid = status.short_id;
            }
            if !status.title.trim().is_empty() {
                follow.title = status.title;
            }
            follow.cover_url = status.cover_url;
            follow.area_name = status.area_name;
            follow.online = status.online;
            follow.started_at = status.started_at;
            if !status.uname.trim().is_empty() && follow.uname == "抖音主播" {
                follow.uname = status.uname;
            }
        }
    }
    if !follow.is_live {
        follow.started_at = None;
        follow.online = None;
    }
    Some(follow)
}

fn room_data(item: &Value) -> Option<Value> {
    match item.get("room_data") {
        Some(Value::String(text)) => serde_json::from_str(text).ok(),
        Some(Value::Object(_)) => item.get("room_data").cloned(),
        _ => None,
    }
}

fn number_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => {
            let text = text.trim().to_string();
            (!text.is_empty()).then_some(text)
        }
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// 关注池后台刷新：只扫还没转正的项，开播且拿到 web_rid 就转成正式房间。
pub async fn refresh_pool(db: &Db, app: &AppHandle) -> anyhow::Result<PoolStats> {
    let pending = db.pending_douyin_follows().context("读取抖音关注池失败")?;
    if pending.is_empty() {
        tracing::trace!(
            target: "hivecast_lib::douyin_follow",
            "抖音关注池为空，跳过扫描"
        );
        return Ok(PoolStats::default());
    }
    let pending_total = pending.len();
    let cookie = match login_cookie() {
        Ok(cookie) => cookie,
        Err(err) => {
            tracing::debug!(
                target: "hivecast_lib::douyin_follow",
                pending = pending_total,
                error = %err,
                "抖音关注池有待转正关注，但登录态不可用，跳过扫描"
            );
            return Ok(PoolStats::default());
        }
    };
    let _ = identity(&cookie).await?;
    tracing::debug!(
        target: "hivecast_lib::douyin_follow",
        pending = pending_total,
        "抖音关注池开始扫描待转正关注"
    );
    let pending_by_uid: HashMap<String, DouyinFollow> = pending
        .into_iter()
        .map(|item| (item.uid.clone(), item))
        .collect();
    let mut remaining: HashSet<String> = pending_by_uid.keys().cloned().collect();
    let mut cursor: Option<String> = None;
    let mut stats = PoolStats::default();
    let started = Instant::now();

    loop {
        let page = fetch_follow_page(cursor.as_deref()).await?;
        stats.pages += 1;
        for item in page.items {
            let Some(saved) = pending_by_uid.get(&item.uid) else {
                continue;
            };
            remaining.remove(&item.uid);
            stats.matched += 1;
            db.update_douyin_follow_status(&item)
                .context("写回抖音关注状态失败")?;
            if item.is_live && item.web_rid.is_some() {
                match promote(db, app, &item, saved.monitor, saved.auto_record).await {
                    Ok(Some(_)) => stats.promoted += 1,
                    Ok(None) => {}
                    Err(err) => {
                        tracing::warn!(
                            target: "hivecast_lib::douyin_follow",
                            uid = %item.uid,
                            error = %err,
                            "抖音关注转正失败，保留在关注池下一轮重试"
                        );
                    }
                }
            }
        }
        if remaining.is_empty() || !page.has_more {
            break;
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
        // 页间留一点间隔，别把 77 页在一秒内打满。
        tokio::time::sleep(Duration::from_millis(120)).await;
    }

    tracing::info!(
        target: "hivecast_lib::douyin_follow",
        pending = pending_total,
        pages = stats.pages,
        matched = stats.matched,
        promoted = stats.promoted,
        remaining = remaining.len(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "抖音关注池扫描完成"
    );
    Ok(stats)
}

async fn promote(
    db: &Db,
    app: &AppHandle,
    item: &DouyinFollow,
    monitor: bool,
    auto_record: bool,
) -> anyhow::Result<Option<String>> {
    let Some(web_rid) = item.web_rid.clone() else {
        return Ok(None);
    };
    let internal_room_id = item.internal_room_id.clone();
    let canonical_url = format!("https://live.douyin.com/{web_rid}");
    let reference = RoomRef {
        platform: "douyin".to_string(),
        room_key: web_rid.clone(),
        canonical_url: canonical_url.clone(),
        input: canonical_url.clone(),
    };
    let info = DouyinAdapter
        .room_info(&reference)
        .await
        .with_context(|| format!("抖音房间 {web_rid} 信息请求失败"))?;
    if !info.is_live {
        return Ok(None);
    }

    if let Some(room) = db.find_room("douyin", &web_rid)? {
        db.update_room_info(&room.id, &info)?;
        // 已有房间可能是先手动添加的，batch_follow 仍为 0；关联时一起补上，
        // 否则关注转正了却继续走单房轮询。
        db.mark_douyin_batch_room(&room.id, Some(&item.uid), internal_room_id.as_deref())?;
        db.mark_douyin_follow_promoted(&item.uid, &room.id)?;
        let _ = app.emit(EVENT_ROOMS_CHANGED, ());
        tracing::info!(
            target: "hivecast_lib::douyin_follow",
            uid = %item.uid,
            room = %web_rid,
            room_id = %room.id,
            batch_follow = true,
            "抖音关注开播，已关联到已有房间"
        );
        return Ok(Some(room.id));
    }

    let now = now_ts();
    let draft = RoomRecord {
        id: uuid::Uuid::new_v4().to_string(),
        platform: "douyin".to_string(),
        room_key: web_rid.clone(),
        canonical_url: canonical_url.clone(),
        title: info.room_name.clone(),
        host_name: info.host_name.clone(),
        avatar_url: info.avatar_url.clone(),
        cover_url: info.cover_url.clone(),
        area_name: info.area_name.clone(),
        is_live: true,
        started_at: info.started_at,
        online: info.online,
        auto_record,
        monitor_paused: !monitor,
        record_qn: 0,
        view_qn: 0,
        record_line: None,
        last_checked_at: Some(now),
        last_session_id: None,
        last_session_started_at: None,
        last_session_duration_ms: 0,
        last_session_status: None,
        last_session_reason: None,
        last_session_file_size: 0,
        last_session_quality: None,
        removed_at: None,
        note: String::new(),
        tags: String::new(),
        title_include: None,
        title_exclude: None,
        split_max_duration_sec: None,
        split_max_size_mb: None,
        notify_live: None,
        notify_error: None,
        created_at: now,
        updated_at: now,
        platform_uid: Some(item.uid.clone()),
        batch_follow: true,
        internal_room_id,
    };
    let room = db.insert_room(&draft, &info)?;
    db.mark_douyin_follow_promoted(&item.uid, &room.id)?;
    let _ = app.emit(EVENT_ROOMS_CHANGED, ());
    tracing::info!(
        target: "hivecast_lib::douyin_follow",
        uid = %item.uid,
        room = %web_rid,
        room_id = %room.id,
        auto_record,
        monitor,
        "抖音关注开播，已转成正式房间"
    );
    Ok(Some(room.id))
}

fn pool_interval_from_secs(secs: u32) -> Option<Duration> {
    (secs > 0).then(|| Duration::from_secs(secs as u64))
}

fn pool_interval(db: &Db) -> Option<Duration> {
    pool_interval_from_secs(crate::settings::load(db).monitor_interval_sec)
}

/// 首次检查最多等 25 秒；监控间隔更短时按用户的间隔来。
fn pool_first_delay(interval: Duration) -> Duration {
    interval.min(POOL_STARTUP_MAX)
}

/// 关注池后台循环：启动后先检查一次，之后跟随「监控检测间隔」。
/// 间隔为 0（关闭自动检测）时暂停检查，改回正数后重新开始。
pub async fn pool_loop(db: std::sync::Arc<Db>, app: AppHandle) {
    let mut interval = pool_interval(&db);
    let mut next_at = interval.map(|delay| Instant::now() + pool_first_delay(delay));
    match interval {
        Some(delay) => tracing::info!(
            target: "hivecast_lib::douyin_follow",
            interval_sec = delay.as_secs(),
            "抖音关注池后台检查已启动"
        ),
        None => tracing::info!(
            target: "hivecast_lib::douyin_follow",
            "监控检测已关闭，抖音关注池暂停检查"
        ),
    }
    let mut settings_at = Instant::now();
    loop {
        if settings_at.elapsed() >= POOL_TICK {
            let next = pool_interval(&db);
            if next != interval {
                match next {
                    Some(delay) => {
                        tracing::info!(
                            target: "hivecast_lib::douyin_follow",
                            interval_sec = delay.as_secs(),
                            "抖音关注池检查间隔已更新"
                        );
                        next_at = Some(Instant::now() + delay);
                    }
                    None => {
                        tracing::info!(
                            target: "hivecast_lib::douyin_follow",
                            "监控检测已关闭，抖音关注池暂停检查"
                        );
                        next_at = None;
                    }
                }
                interval = next;
            }
            settings_at = Instant::now();
        }
        let mut sleep_for = POOL_TICK;
        if let Some(delay) = interval {
            let now = Instant::now();
            let due = next_at.is_none_or(|at| now >= at);
            if due {
                if let Err(err) = refresh_pool(&db, &app).await {
                    tracing::warn!(
                        target: "hivecast_lib::douyin_follow",
                        error = %err,
                        "抖音关注池扫描失败"
                    );
                }
                next_at = Some(Instant::now() + delay);
            }
            // 睡到下一个拍点，而不是固定 2 秒一跳：5 秒间隔不能被量化成 6 秒。
            if let Some(at) = next_at {
                let now = Instant::now();
                if at > now {
                    sleep_for = (at - now).min(POOL_TICK);
                }
            }
        }
        tokio::time::sleep(sleep_for).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_offline_follow_without_room_ids() {
        let root = serde_json::json!({
            "status_code": 0,
            "data": {
                "followings": [{
                    "uid": "123",
                    "sec_uid": "MS4wLjAB",
                    "nickname": "离线主播",
                    "live_status": 0,
                    "room_id": 0,
                    "room_data": null
                }],
                "has_more": true,
                "min_time": "1700000000"
            }
        });
        let page = parse_follow_page(&root, None).expect("解析");
        assert_eq!(page.items.len(), 1);
        assert!(!page.items[0].is_live);
        assert!(page.items[0].web_rid.is_none());
        assert!(page.items[0].internal_room_id.is_none());
        assert_eq!(page.next_cursor.as_deref(), Some("1700000000"));
        assert!(page.has_more);
    }

    #[test]
    fn parses_live_follow_with_room_data() {
        let room = serde_json::json!({
            "id_str": "7708331906513767187",
            "status": 2,
            "title": "直播标题",
            "user_count": 1234,
            "start_time": 1_700_000_000,
            "owner": {
                "nickname": "在播主播",
                "web_rid": "938611933111",
                "avatar_thumb": { "url_list": ["https://example.com/a.jpg"] }
            },
            "cover": { "url_list": ["https://example.com/c.jpg"] }
        });
        let root = serde_json::json!({
            "status_code": 0,
            "data": {
                "followings": [{
                    "uid": "456",
                    "sec_uid": "MS4wLjAB2",
                    "nickname": "在播主播",
                    "live_status": 1,
                    "room_id": "7708331906513767187",
                    "room_data": room.to_string()
                }],
                "has_more": false,
                "min_time": 0
            }
        });
        let page = parse_follow_page(&root, None).expect("解析");
        let item = &page.items[0];
        assert!(item.is_live);
        assert_eq!(item.web_rid.as_deref(), Some("938611933111"));
        assert_eq!(
            item.internal_room_id.as_deref(),
            Some("7708331906513767187")
        );
        assert_eq!(item.title, "直播标题");
        assert_eq!(item.online, Some(1234));
        assert_eq!(item.started_at, Some(1_700_000_000));
        assert!(!page.has_more);
        assert!(page.next_cursor.is_none());
    }

    #[test]
    fn live_room_data_without_room_id_keeps_web_rid() {
        // 真实 following/list 的 room_data 没有 id_str，房间号只在外层。
        let room = serde_json::json!({
            "status": 2,
            "user_count": 5957,
            "title": "",
            "owner": { "web_rid": "205048140143" }
        });
        let root = serde_json::json!({
            "status_code": 0,
            "followings": [{
                "uid": "3250618555442916",
                "nickname": "偷心九月天",
                "live_status": 1,
                "room_id": 7708405304424041000i64,
                "room_data": room.to_string()
            }],
            "has_more": true,
            "min_time": "1787985326"
        });
        let page = parse_follow_page(&root, None).expect("解析");
        let item = &page.items[0];
        assert!(item.is_live);
        assert_eq!(item.web_rid.as_deref(), Some("205048140143"));
        assert_eq!(
            item.internal_room_id.as_deref(),
            Some("7708405304424041000")
        );
    }

    #[test]
    fn parses_root_level_page_from_real_api() {
        // 真实 following/list 响应把 followings/has_more/min_time 放在根上。
        let root = serde_json::json!({
            "status_code": 0,
            "followings": [{
                "uid": "3250618555442916",
                "sec_uid": "MS4wLjAB3",
                "nickname": "偷心九月天",
                "live_status": 1,
                "room_id": "7708405304424041000",
                "room_data": null
            }],
            "has_more": true,
            "min_time": "1787985326",
            "max_time": "1792743236"
        });
        let page = parse_follow_page(&root, None).expect("解析");
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].uid, "3250618555442916");
        assert_eq!(page.next_cursor.as_deref(), Some("1787985326"));
        assert!(page.has_more);
    }

    #[test]
    fn same_cursor_stops_pagination() {
        let root = serde_json::json!({
            "status_code": 0,
            "data": { "followings": [], "has_more": true, "min_time": "1700000000" }
        });
        let page = parse_follow_page(&root, Some("1700000000")).expect("解析");
        assert!(!page.has_more);
        assert!(page.next_cursor.is_none());
    }

    #[test]
    fn non_zero_status_code_is_an_error() {
        let root = serde_json::json!({ "status_code": 8, "status_msg": "登录态失效" });
        let err = parse_follow_page(&root, None).expect_err("应报错");
        assert!(err.to_string().contains("登录态失效"));
    }

    #[test]
    fn pool_interval_follows_monitor_setting_and_zero_disables() {
        assert_eq!(pool_interval_from_secs(0), None);
        assert_eq!(pool_interval_from_secs(45), Some(Duration::from_secs(45)));
        assert_eq!(
            pool_interval_from_secs(3600),
            Some(Duration::from_secs(3600))
        );
    }

    #[test]
    fn pool_first_scan_caps_long_intervals() {
        assert_eq!(
            pool_first_delay(Duration::from_secs(5)),
            Duration::from_secs(5)
        );
        assert_eq!(
            pool_first_delay(Duration::from_secs(30)),
            Duration::from_secs(25)
        );
        assert_eq!(
            pool_first_delay(Duration::from_secs(3600)),
            Duration::from_secs(25)
        );
    }
}
