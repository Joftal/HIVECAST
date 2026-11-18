//! 抖音账号：粘贴 Cookie 校验、登录态复验与 Cookie 轮换。
//!
//! 抖音没有公开可复刻的长期续期凭证（扫码轮询还要 a_bogus / x-tt-session-dtrait），
//! 因此这里只做「粘贴 Cookie」这一条路径：
//! 1. 保存时用 profile/self 校验 sessionid 并解析 uid/昵称/头像；
//! 2. 每 30 分钟用同一份 Cookie 复验登录态，失效就标记 expired；
//! 3. 每 6 小时访问一次 live.douyin.com 首页，吸收服务端轮换的 ttwid 等 Cookie；
//! 4. 不承诺永久续期：会话失效后必须由用户重新粘贴 Cookie。
//!
//! 隐私边界：Cookie 只会发给 douyin.com / iesdouyin.com，绝不会发给 douyincdn.com
//! 这类第三方 CDN；日志与错误信息只记录平台名，不打印 Cookie。

use std::collections::BTreeMap;
use std::time::Duration;

use hivecast_ipc::{AppNotice, RefreshStatus};
use reqwest::header::{HeaderValue, COOKIE, REFERER, SET_COOKIE};
use serde::Deserialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};

use crate::account::Account;
use crate::credential;
use crate::db::{now_ts, CredentialRow, Db};
use crate::error::AppError;

const HOME: &str = "https://live.douyin.com/";
const PROFILE: &str = "https://www.douyin.com/aweme/v1/web/user/profile/self/?device_platform=webapp&aid=6383&channel=channel_pc_web&publish_video_strategy_type=2&source=channel_pc_web";
const REFERER_URL: &str = "https://www.douyin.com/";
/// 登录态复验周期。太频繁会被风控，30 分钟足够发现会话失效。
pub const REVALIDATE_INTERVAL: Duration = Duration::from_secs(30 * 60);
const REFRESH_EVERY: i64 = 6 * 60 * 60;
const RETRY_FAILED: i64 = 30 * 60;

static REFRESH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, Default, Deserialize)]
struct ProfileEnvelope {
    #[serde(default)]
    status_code: i64,
    #[serde(default)]
    status_msg: Option<String>,
    #[serde(default)]
    user: Option<Value>,
}

#[derive(Debug)]
enum Probe {
    Ok(Account),
    Invalid,
    Transient,
}

#[derive(Debug, PartialEq, Eq)]
enum RefreshOutcome {
    NoCredential,
    NotRefreshable,
    Refreshed,
    Fresh,
    Transient,
    Discarded,
}

fn dy_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(crate::platform::http::USER_AGENT)
            .timeout(Duration::from_secs(15))
            .gzip(true)
            .build()
            .expect("构建抖音 HTTP 客户端失败")
    })
}

fn header_value(value: &str) -> Option<HeaderValue> {
    HeaderValue::from_str(value).ok()
}

pub(crate) fn has_session_cookie(cookie: &str) -> bool {
    ["sessionid", "sessionid_ss"]
        .iter()
        .any(|name| credential::value(cookie, name).is_some_and(|value| !value.trim().is_empty()))
}

fn normalize_cookie(raw: &str) -> Result<String, AppError> {
    let cookie = credential::normalize(raw).map_err(AppError::User)?;
    if !has_session_cookie(&cookie) {
        return Err(AppError::User(
            "抖音 Cookie 缺少 sessionid，请在抖音网页登录后重新复制完整 Cookie".into(),
        ));
    }
    Ok(cookie)
}

fn avatar_from(user: &Value) -> Option<String> {
    for key in [
        "avatar_thumb",
        "avatar_larger",
        "avatar_medium",
        "avatar_168x168",
    ] {
        if let Some(url) = user
            .get(key)
            .and_then(|image| image.get("url_list"))
            .and_then(|list| list.as_array())
            .and_then(|list| list.first())
            .and_then(Value::as_str)
            .filter(|url| !url.is_empty())
        {
            return Some(url.to_string());
        }
    }
    None
}

fn account_from_user(user: &Value) -> Result<Account, AppError> {
    let uid = match user.get("uid") {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Number(value)) => value.to_string(),
        _ => String::new(),
    };
    if uid.is_empty() {
        return Err(AppError::User("抖音账号信息缺少 uid".into()));
    }
    let nickname = user
        .get("nickname")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    Ok(Account {
        uid: uid.clone(),
        name: if nickname.is_empty() {
            format!("抖音用户 {uid}")
        } else {
            nickname
        },
        avatar: avatar_from(user),
    })
}

async fn probe(cookie: &str) -> Probe {
    let mut request = dy_client()
        .get(PROFILE)
        .header(REFERER, REFERER_URL)
        .header("Accept-Language", "zh-CN,zh;q=0.9");
    if let Some(value) = header_value(cookie) {
        request = request.header(COOKIE, value);
    }
    let response = match request.send().await {
        Ok(response) => response,
        Err(err) => {
            tracing::warn!(platform = "douyin", error = %err, "抖音登录态复验请求失败");
            return Probe::Transient;
        }
    };
    if !response.status().is_success() {
        tracing::warn!(
            platform = "douyin",
            status = response.status().as_u16(),
            "抖音登录态复验返回非 2xx"
        );
        return Probe::Transient;
    }
    let body = match response.text().await {
        Ok(body) => body,
        Err(err) => {
            tracing::warn!(platform = "douyin", error = %err, "读取抖音复验响应失败");
            return Probe::Transient;
        }
    };
    if body.len() > 2 * 1024 * 1024 {
        return Probe::Transient;
    }
    let envelope: ProfileEnvelope = match serde_json::from_str(&body) {
        Ok(envelope) => envelope,
        Err(err) => {
            tracing::warn!(platform = "douyin", error = %err, "解析抖音复验响应失败");
            return Probe::Transient;
        }
    };
    if envelope.status_code == 8 {
        return Probe::Invalid;
    }
    if envelope.status_code != 0 {
        tracing::warn!(
            platform = "douyin",
            code = envelope.status_code,
            message = envelope.status_msg.as_deref().unwrap_or(""),
            "抖音登录态复验返回业务错误"
        );
        return Probe::Transient;
    }
    let Some(user) = envelope.user else {
        return Probe::Invalid;
    };
    match account_from_user(&user) {
        Ok(account) => Probe::Ok(account),
        Err(_) => Probe::Invalid,
    }
}

/// 校验用户粘贴的抖音 Cookie：必须是登录态，且能解析出 uid。
pub(crate) async fn validate_cookie(cookie: &str) -> Result<Account, AppError> {
    let cookie = normalize_cookie(cookie)?;
    match probe(&cookie).await {
        Probe::Ok(account) => Ok(account),
        Probe::Invalid => Err(AppError::User("抖音 Cookie 已失效或未登录".into())),
        Probe::Transient => Err(AppError::Platform {
            code: "douyin_upstream",
            message: "抖音接口暂时不可用，无法校验 Cookie，请稍后重试".into(),
        }),
    }
}

fn parse_set_cookie(raw: &str) -> Option<(String, String)> {
    let first = raw.split(';').next()?.trim();
    let (name, value) = first.split_once('=')?;
    let name = name.trim();
    let value = value.trim();
    if name.is_empty() || value.is_empty() {
        return None;
    }
    let lower = name.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "path"
            | "domain"
            | "expires"
            | "max-age"
            | "samesite"
            | "secure"
            | "httponly"
            | "priority"
            | "partitioned"
    ) {
        return None;
    }
    Some((name.to_string(), value.to_string()))
}

/// 吸收首页下发的 Cookie。只接受会随会话轮换的 ttwid 一类，避免服务端用
/// 空值/登出标记把现有 sessionid 覆盖掉。
fn absorb_rotation(headers: &reqwest::header::HeaderMap) -> BTreeMap<String, String> {
    let mut jar = BTreeMap::new();
    for value in headers.get_all(SET_COOKIE) {
        let Ok(raw) = value.to_str() else { continue };
        let Some((name, value)) = parse_set_cookie(raw) else {
            continue;
        };
        let lower = name.to_ascii_lowercase();
        if lower == "sessionid" || lower == "sessionid_ss" || lower == "sid_tt" {
            continue;
        }
        jar.insert(name, value);
    }
    jar
}

fn cookie_is_current(row: &CredentialRow) -> bool {
    credential::get("douyin").as_deref() == Some(row.cookie.as_str())
}

/// 当前落库的抖音凭据。轮换结果写回前用它复核状态，避免把已经标记过期的
/// 会话又「洗」回 ok。
fn current_row(db: &Db) -> Option<CredentialRow> {
    db.credentials()
        .ok()
        .and_then(|rows| rows.into_iter().find(|row| row.platform == "douyin"))
}

/// 只有「有 sessionid 且状态仍为 ok」才允许轮换 ttwid。轮换本身不验证
/// sessionid，不能拿它当登录态恢复的手段。
fn rotation_allowed(row: &CredentialRow) -> bool {
    row.status == "ok" && has_session_cookie(&row.cookie)
}

fn write_status(db: &Db, status: &RefreshStatus) {
    crate::account::save_refresh_status(db, "douyin", status);
}

fn notify_expired(app: &AppHandle, account_name: &str) {
    let notice = AppNotice {
        kind: "credential_expired".into(),
        room_id: String::new(),
        host_name: account_name.to_string(),
        detail: Some("douyin".into()),
        suppress: false,
    };
    if let Err(err) = app.emit(crate::recorder::EVENT_APP_NOTICE, notice) {
        tracing::warn!(error = %err, "抖音登录失效通知发送失败");
    }
}

fn mark_expired_locked(db: &Db, app: &AppHandle, row: &CredentialRow) -> bool {
    if row.status == "expired" {
        return false;
    }
    if !cookie_is_current(row) {
        tracing::info!("标记过期前发现抖音 Cookie 已更新，跳过");
        return false;
    }
    match db.update_credential_profile(
        "douyin",
        "expired",
        &row.account_name,
        row.avatar.as_deref(),
        now_ts(),
    ) {
        Ok(true) => {
            credential::set_status("douyin", "expired");
            notify_expired(app, &row.account_name);
            true
        }
        Ok(false) => false,
        Err(err) => {
            tracing::warn!(error = %err, "标记抖音账号过期失败");
            false
        }
    }
}

fn recheck_recording(app: &AppHandle, reason: &'static str) {
    if let Some(state) = app.try_state::<crate::state::AppState>() {
        state
            .recorder
            .recheck_all_quality(app, state.broker.clone(), "douyin", reason);
    }
}

/// 6 小时轮换一次：访问直播首页吸收新 ttwid，不重新登录、不改 sessionid。
async fn refresh_once(db: &Db, app: &AppHandle, force: bool) -> RefreshOutcome {
    let Some(row) = current_row(db) else {
        return RefreshOutcome::NoCredential;
    };
    // 轮换只刷新 ttwid，不能证明 sessionid 还有效。已标记过期的账号必须等
    // revalidate 探活恢复；否则这里会把 expired 重新写成 ok，让失效 Cookie
    // 又参与取流，界面也会出现「已失效 + 登录态有效」的矛盾。
    if !rotation_allowed(&row) {
        tracing::debug!(status = %row.status, "抖音登录态不可轮换，跳过 Cookie 轮换");
        return RefreshOutcome::NotRefreshable;
    }
    let before = crate::account::load_refresh_status(db, "douyin");
    let due = force
        || match before.outcome.as_str() {
            "refreshed" | "fresh" => {
                now_ts() >= before.last_refreshed_at.max(row.updated_at) + REFRESH_EVERY
            }
            "failed" => now_ts() >= before.checked_at + RETRY_FAILED,
            _ => now_ts() >= row.updated_at + REFRESH_EVERY,
        };
    if !due {
        return RefreshOutcome::Fresh;
    }

    let _guard = REFRESH_LOCK.lock().await;
    let Some(row) = current_row(db) else {
        return RefreshOutcome::NoCredential;
    };
    // 等锁期间 revalidate 可能已经把它标成过期，必须重新看一眼。
    if !rotation_allowed(&row) {
        tracing::debug!(status = %row.status, "等锁期间抖音登录态不可轮换，跳过 Cookie 轮换");
        return RefreshOutcome::NotRefreshable;
    }

    tracing::debug!(platform = "douyin", force, "开始抖音 Cookie 轮换检查");
    let mut request = dy_client()
        .get(HOME)
        .header(REFERER, REFERER_URL)
        .header("Accept-Language", "zh-CN,zh;q=0.9");
    if let Some(value) = header_value(&row.cookie) {
        request = request.header(COOKIE, value);
    }
    let response = match request.send().await {
        Ok(response) => response,
        Err(err) => {
            tracing::warn!(platform = "douyin", error = %err, "抖音 Cookie 轮换请求失败");
            let status = RefreshStatus {
                checked_at: now_ts(),
                outcome: "failed".into(),
                message: Some("抖音接口暂时不可用，Cookie 轮换稍后重试".into()),
                key_stale: false,
                last_refreshed_at: before.last_refreshed_at,
            };
            write_status(db, &status);
            return RefreshOutcome::Transient;
        }
    };
    if !response.status().is_success() {
        let status = RefreshStatus {
            checked_at: now_ts(),
            outcome: "failed".into(),
            message: Some(format!("抖音接口返回 HTTP {}", response.status().as_u16())),
            key_stale: false,
            last_refreshed_at: before.last_refreshed_at,
        };
        write_status(db, &status);
        return RefreshOutcome::Transient;
    }
    let jar = absorb_rotation(response.headers());
    tracing::debug!(
        platform = "douyin",
        rotated = ?jar.keys().collect::<Vec<_>>(),
        "抖音首页轮换 Cookie"
    );
    let _ = response.text().await;

    let mut changed = 0usize;
    let mut next_cookie = row.cookie.clone();
    for (name, value) in &jar {
        if credential::value(&next_cookie, name) != Some(value.as_str()) {
            changed += 1;
        }
    }
    tracing::debug!(platform = "douyin", changed, "抖音 Cookie 轮换差异计算完成");
    if changed > 0 {
        let extra = jar
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ");
        next_cookie = credential::merge(&row.cookie, &extra);
        let _guard = crate::account::credential_write_guard();
        // 写锁内重新读一次：用户可能在这几秒里换了账号，revalidate 也可能
        // 已经把旧会话标成过期。晚到的轮换结果既不能覆盖新 Cookie，也不能
        // 把 expired 改回 ok。
        let Some(latest) = current_row(db) else {
            return RefreshOutcome::NoCredential;
        };
        if !rotation_allowed(&latest) {
            tracing::info!("抖音 Cookie 轮换期间登录态已失效，丢弃本次结果");
            return RefreshOutcome::NotRefreshable;
        }
        if latest.cookie != row.cookie {
            tracing::info!("抖音 Cookie 轮换期间凭据已更新，丢弃本次结果");
            return RefreshOutcome::Discarded;
        }
        let mut updated = latest;
        updated.cookie = next_cookie.clone();
        updated.updated_at = now_ts();
        if let Err(err) = db.save_credential(&updated) {
            tracing::warn!(error = %err, "保存轮换后的抖音 Cookie 失败");
            return RefreshOutcome::Transient;
        }
        credential::set("douyin", &next_cookie);
        // ttwid 变了，缓存里的旧候选地址可能仍带着上一轮签名；丢掉缓存，
        // 下一次取流用新 Cookie 重新解析。正在录的 ffmpeg 不受影响。
        crate::account::clear_stream_cache(app);
    }

    let status = RefreshStatus {
        checked_at: now_ts(),
        outcome: if changed > 0 { "refreshed" } else { "fresh" }.into(),
        message: Some(if changed > 0 {
            format!("已同步 {changed} 个轮换 Cookie")
        } else {
            "登录态有效".into()
        }),
        key_stale: false,
        last_refreshed_at: if changed > 0 {
            now_ts()
        } else {
            before.last_refreshed_at
        },
    };
    {
        let _guard = crate::account::credential_write_guard();
        let Some(latest) = current_row(db) else {
            return RefreshOutcome::NoCredential;
        };
        if !rotation_allowed(&latest) {
            // 请求期间登录态失效：绝不能写「登录态有效」。
            return RefreshOutcome::NotRefreshable;
        }
        let expected = if changed > 0 {
            next_cookie.as_str()
        } else {
            row.cookie.as_str()
        };
        if latest.cookie != expected {
            tracing::info!("抖音 Cookie 轮换结果写回前凭据已更新，丢弃本次状态");
            return RefreshOutcome::Discarded;
        }
        write_status(db, &status);
    }
    crate::account::emit_account_refresh(app);
    if changed > 0 {
        tracing::info!(platform = "douyin", changed, "抖音 Cookie 已轮换");
    }
    if changed > 0 {
        RefreshOutcome::Refreshed
    } else {
        RefreshOutcome::Fresh
    }
}

/// 复验登录态：失效就标记过期，恢复就清掉过期标记并重检录制画质。
pub async fn revalidate(db: &Db, app: &AppHandle) {
    revalidate_inner(db, app, false).await;
}

async fn revalidate_inner(db: &Db, app: &AppHandle, force: bool) {
    let Some(row) = db
        .credentials()
        .ok()
        .and_then(|rows| rows.into_iter().find(|row| row.platform == "douyin"))
    else {
        return;
    };
    if !has_session_cookie(&row.cookie) {
        return;
    }
    let status = crate::account::load_refresh_status(db, "douyin");
    if !force && status.checked_at > 0 && now_ts() - status.checked_at < 300 {
        return;
    }
    match probe(&row.cookie).await {
        Probe::Ok(account) => {
            let mut recovered = false;
            {
                let _guard = crate::account::credential_write_guard();
                if !cookie_is_current(&row) {
                    tracing::info!("抖音复验期间 Cookie 已更新，丢弃本次结果");
                    return;
                }
                if row.status == "expired" {
                    let _ = db.update_credential_profile(
                        "douyin",
                        "ok",
                        &account.name,
                        account.avatar.as_deref(),
                        now_ts(),
                    );
                    credential::set_status("douyin", "ok");
                    crate::account::emit_account_refresh(app);
                    tracing::info!("抖音登录态已恢复");
                    recovered = true;
                } else if row.account_name != account.name || row.avatar != account.avatar {
                    let _ = db.update_credential_profile(
                        "douyin",
                        "ok",
                        &account.name,
                        account.avatar.as_deref(),
                        now_ts(),
                    );
                    crate::account::emit_account_refresh(app);
                }
            }
            let status = RefreshStatus {
                checked_at: now_ts(),
                outcome: "fresh".into(),
                message: Some("登录态有效".into()),
                key_stale: false,
                last_refreshed_at: status.last_refreshed_at,
            };
            write_status(db, &status);
            if recovered {
                recheck_recording(app, "douyin_revalidated");
            }
        }
        Probe::Invalid => {
            let _guard = crate::account::credential_write_guard();
            if mark_expired_locked(db, app, &row) {
                tracing::warn!(platform = "douyin", "抖音登录态已失效，已标记过期");
            }
            let status = RefreshStatus {
                checked_at: now_ts(),
                outcome: "failed".into(),
                message: Some("抖音登录态已失效，请重新粘贴 Cookie".into()),
                key_stale: true,
                last_refreshed_at: status.last_refreshed_at,
            };
            write_status(db, &status);
        }
        Probe::Transient => {}
    }
}

pub async fn refresh_now(db: &Db, app: &AppHandle) -> Result<RefreshStatus, AppError> {
    // 先复验再轮换：轮换会写 checked_at，反过来会让复验的 5 分钟闸门跳过。
    // 顺序反过来，轮换成功后的「已同步 N 个轮换 Cookie」也不会被复验覆盖成 fresh。
    revalidate_inner(db, app, true).await;
    let _ = refresh_once(db, app, true).await;
    Ok(crate::account::load_refresh_status(db, "douyin"))
}

/// 独立维护循环：先复验再轮换。顺序不能反，否则轮换写入的 checked_at
/// 会让复验的 5 分钟闸门跳过本次检查。
pub async fn maintenance_loop(db: std::sync::Arc<Db>, app: AppHandle) {
    tokio::time::sleep(Duration::from_secs(30)).await;
    loop {
        revalidate(&db, &app).await;
        let _ = refresh_once(&db, &app, false).await;
        tokio::time::sleep(REVALIDATE_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_cookie_detection_requires_a_value() {
        assert!(has_session_cookie("sessionid=abc; uid_tt=1"));
        assert!(has_session_cookie("sessionid_ss=abc"));
        assert!(!has_session_cookie("sessionid=; uid_tt=1"));
        assert!(!has_session_cookie("ttwid=abc"));
    }

    #[test]
    fn normalize_rejects_cookies_without_session() {
        assert!(normalize_cookie("ttwid=abc").is_err());
        assert_eq!(
            normalize_cookie("抖音：sessionid=abc; ttwid=def").unwrap(),
            "sessionid=abc; ttwid=def"
        );
    }

    /// 联网探针：HIVE_DOUYIN_COOKIE=... cargo test --lib douyin_auth::tests::live_rotation_probe -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "需要真实抖音 Cookie"]
    async fn live_rotation_probe() {
        let cookie = std::env::var("HIVE_DOUYIN_COOKIE").expect("缺少 HIVE_DOUYIN_COOKIE");
        let response = dy_client()
            .get(HOME)
            .header(REFERER, REFERER_URL)
            .header(COOKIE, cookie)
            .send()
            .await
            .expect("请求直播首页失败");
        println!("status={}", response.status());
        let jar = absorb_rotation(response.headers());
        println!("rotated={:?}", jar.keys().collect::<Vec<_>>());
    }

    #[test]
    fn profile_accepts_null_status_message() {
        let envelope: ProfileEnvelope = serde_json::from_str(
            r#"{"status_code":0,"status_msg":null,"user":{"uid":"1","nickname":"n"}}"#,
        )
        .unwrap();
        assert_eq!(envelope.status_code, 0);
        assert_eq!(envelope.status_msg, None);
        assert_eq!(
            account_from_user(envelope.user.as_ref().unwrap())
                .unwrap()
                .uid,
            "1"
        );
    }

    #[test]
    fn profile_parses_uid_and_avatar() {
        let user = serde_json::json!({
            "uid": 67305272492u64,
            "nickname": "执事只食芝士",
            "avatar_thumb": { "url_list": ["https://p3.douyinpic.com/avatar.jpeg"] }
        });
        let account = account_from_user(&user).unwrap();
        assert_eq!(account.uid, "67305272492");
        assert_eq!(account.name, "执事只食芝士");
        assert_eq!(
            account.avatar.as_deref(),
            Some("https://p3.douyinpic.com/avatar.jpeg")
        );
    }

    #[test]
    fn rotation_never_overwrites_login_cookies() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.append(
            SET_COOKIE,
            HeaderValue::from_static("ttwid=new; Path=/; HttpOnly"),
        );
        headers.append(
            SET_COOKIE,
            HeaderValue::from_static("sessionid=logout; Path=/"),
        );
        headers.append(
            SET_COOKIE,
            HeaderValue::from_static("sid_tt=logout; Path=/"),
        );
        let jar = absorb_rotation(&headers);
        assert_eq!(jar.get("ttwid").map(String::as_str), Some("new"));
        assert!(!jar.contains_key("sessionid"));
        assert!(!jar.contains_key("sid_tt"));
    }

    #[test]
    fn rotation_merges_only_changed_cookies() {
        let merged = credential::merge("sessionid=abc; ttwid=old", "ttwid=new");
        assert_eq!(merged, "sessionid=abc; ttwid=new");
    }

    #[test]
    fn rotation_requires_ok_status_and_session() {
        let row = |status: &str, cookie: &str| CredentialRow {
            platform: "douyin".into(),
            account_id: "douyin".into(),
            cookie: cookie.into(),
            uid: "1".into(),
            account_name: "n".into(),
            avatar: None,
            status: status.into(),
            updated_at: 0,
        };
        assert!(rotation_allowed(&row("ok", "sessionid=abc; ttwid=1")));
        assert!(!rotation_allowed(&row("expired", "sessionid=abc; ttwid=1")));
        assert!(
            !rotation_allowed(&row("ok", "ttwid=1")),
            "没有 sessionid 不算登录态"
        );
    }
}
