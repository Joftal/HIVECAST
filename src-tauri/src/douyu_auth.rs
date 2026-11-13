//! 斗鱼账号：扫码登录、Cookie 校验与自动续期。
//!
//! 斗鱼和 B 站不同：主站登录 Cookie（acf_*）大约只有 6 天有效期，网页端不会被动轮换。
//! 真正能续期的是扫码确认时 passport 下发的长期凭证 LTP0（数月有效）：
//! 1. 扫码生成 + 轮询，确认后跟随 data.url 的回调链，收集 Set-Cookie；
//! 2. 必需 acf_* 齐全才算登录成功，LTP0 / dy_did 单独加密存 credential_secret；
//! 3. 每 3 天用「只带 LTP0」的 safeAuth 链换一套新 acf_*，再用 follow/top3 探针确认登录态；
//! 4. 临时失败 6 小时后重试，LTP0 明确失效 24 小时后重试并提示重新扫码。
//!
//! 手动粘贴的 Cookie 没有 LTP0，只能校验、不能自动续期；这是一条明确的降级路径，
//! 不能把旧 LTP0 套到新 Cookie 上。

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hivecast_ipc::{AppNotice, QrPoll, QrTicket, RefreshStatus};
use reqwest::header::{HeaderMap, LOCATION, REFERER, SET_COOKIE};
use reqwest::redirect::Policy;
use reqwest::{Client, RequestBuilder, StatusCode};
use serde_json::Value;
use tauri::{AppHandle, Emitter};
use url::Url;

use crate::account::Account;
use crate::db::{now_ts, CredentialRow, CredentialSecretRow, Db};
use crate::error::AppError;

const PASSPORT: &str = "https://passport.douyu.com";
const GENERATE: &str = "https://passport.douyu.com/scan/generateCode";
const POLL: &str = "https://passport.douyu.com/japi/scan/auth";
const SAFE_AUTH: &str = "https://passport.douyu.com/lapi/passport/iframe/safeAuth";
const PROBE: &str = "https://www.douyu.com/wgapi/livenc/liveweb/follow/top3";
const LOGIN_REFERER: &str = "https://passport.douyu.com/index/login";
const FOLLOW_REFERER: &str = "https://www.douyu.com/directory/myFollow";
const MAIN_REFERER: &str = "https://www.douyu.com/";

const REQUIRED_COOKIES: [&str; 7] = [
    "acf_uid",
    "acf_auth",
    "acf_stk",
    "acf_ltkid",
    "acf_username",
    "acf_biz",
    "acf_ct",
];
const DEVICE_COOKIES: [&str; 3] = ["dy_did", "acf_did", "acf_devid"];
const REFRESH_EVERY: i64 = 3 * 24 * 60 * 60;
const RETRY_TRANSIENT: i64 = 6 * 60 * 60;
const RETRY_RESCAN: i64 = 24 * 60 * 60;
const MAX_BODY: usize = 1024 * 1024;

/// 定时与手动续期共用一把锁，避免两个 safeAuth 同时换票。
static REFRESH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_else(|_| now_ts() * 1000)
}

fn dy_client() -> &'static Client {
    static CLIENT: std::sync::OnceLock<Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .user_agent(crate::platform::http::USER_AGENT)
            .timeout(Duration::from_secs(15))
            .gzip(true)
            .redirect(Policy::none())
            .build()
            .expect("构建斗鱼 HTTP 客户端失败")
    })
}

fn base_headers(referer: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Ok(value) = reqwest::header::HeaderValue::from_str(referer) {
        headers.insert(REFERER, value);
    }
    headers.insert(
        reqwest::header::ORIGIN,
        reqwest::header::HeaderValue::from_static("https://passport.douyu.com"),
    );
    headers.insert(
        reqwest::header::HeaderName::from_static("x-requested-with"),
        reqwest::header::HeaderValue::from_static("XMLHttpRequest"),
    );
    headers
}

/// 解析 Set-Cookie：只取 name=value，属性（Path/Expires/HttpOnly 等）全部丢弃。
/// 空值不覆盖已有值，避免平台用 name=; Max-Age=0 清 Cookie 时把登录态误删。
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

fn absorb_cookies(headers: &HeaderMap, jar: &mut BTreeMap<String, String>) -> bool {
    let mut any = false;
    for value in headers.get_all(SET_COOKIE) {
        let Ok(raw) = value.to_str() else { continue };
        if let Some((name, value)) = parse_set_cookie(raw) {
            jar.insert(name, value);
            any = true;
        }
    }
    any
}

fn cookie_header(jar: &BTreeMap<String, String>) -> Option<String> {
    if jar.is_empty() {
        return None;
    }
    Some(
        jar.iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; "),
    )
}

fn host_allowed(url: &Url) -> bool {
    url.scheme() == "https"
        && url
            .host_str()
            .is_some_and(|host| host == "douyu.com" || host.ends_with(".douyu.com"))
}

fn resolve_callback(base: &str, raw: &str) -> Result<Url, AppError> {
    let url = Url::parse(raw)
        .or_else(|_| Url::parse(base).and_then(|b| b.join(raw)))
        .map_err(|err| AppError::User(format!("斗鱼登录回调地址无效：{err}")))?;
    if !host_allowed(&url) {
        return Err(AppError::User("斗鱼登录回调跳转到了非斗鱼域名".into()));
    }
    Ok(url)
}

/// 手动跟随重定向：扫码回调与 safeAuth 都会在中间跳转里下发登录态，
/// 只看最后一跳会丢 Cookie；同时限制跳数和非斗鱼域名，避免被带到任意站点。
async fn follow_chain(
    mut url: Url,
    jar: &mut BTreeMap<String, String>,
    referer: &str,
) -> Result<(StatusCode, Url, HeaderMap, String), AppError> {
    for _ in 0..9 {
        let mut req = dy_client().get(url.clone()).headers(base_headers(referer));
        if let Some(cookie) = cookie_header(jar) {
            req = req.header(reqwest::header::COOKIE, cookie);
        }
        let resp = req
            .send()
            .await
            .map_err(|err| AppError::User(format!("斗鱼请求失败：{}", err.without_url())))?;
        let status = resp.status();
        let headers = resp.headers().clone();
        absorb_cookies(&headers, jar);
        if status.is_redirection() {
            let Some(location) = headers.get(LOCATION).and_then(|v| v.to_str().ok()) else {
                return Err(AppError::User("斗鱼登录跳转缺少 Location".into()));
            };
            if let Ok(next) = url.join(location) {
                if !host_allowed(&next) {
                    return Err(AppError::User("斗鱼登录跳转到了非斗鱼域名".into()));
                }
                url = next;
                continue;
            }
            return Err(AppError::User("斗鱼登录跳转地址无效".into()));
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|err| AppError::User(format!("读取斗鱼响应失败：{}", err.without_url())))?;
        if bytes.len() > MAX_BODY {
            return Err(AppError::User("斗鱼响应过大".into()));
        }
        let body = String::from_utf8_lossy(&bytes).into_owned();
        return Ok((status, url, headers, body));
    }
    Err(AppError::User("斗鱼登录跳转次数过多".into()))
}

async fn send_json(req: RequestBuilder) -> Result<(StatusCode, HeaderMap, Value), AppError> {
    let resp = req
        .send()
        .await
        .map_err(|err| AppError::User(format!("斗鱼请求失败：{}", err.without_url())))?;
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp
        .bytes()
        .await
        .map_err(|err| AppError::User(format!("读取斗鱼响应失败：{}", err.without_url())))?;
    if bytes.len() > MAX_BODY {
        return Err(AppError::User("斗鱼响应过大".into()));
    }
    let value = serde_json::from_slice(&bytes)
        .map_err(|_| AppError::User("斗鱼接口返回的不是 JSON，可能触发了风控".into()))?;
    Ok((status, headers, value))
}

fn value_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn value_i64(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    })
}

fn poll_state(error: i64) -> &'static str {
    match error {
        0 => "confirmed",
        1 => "scanned",
        -1 => "expired",
        -2 => "pending",
        _ => "error",
    }
}

fn poll_message(error: i64, fallback: &str) -> String {
    match error {
        0 => "登录成功".into(),
        1 => "已扫码，请在手机端确认".into(),
        -1 => "二维码已过期，请刷新".into(),
        -2 => "等待扫码".into(),
        _ if fallback.trim().is_empty() => "扫码状态异常，请刷新二维码".into(),
        _ => fallback.to_string(),
    }
}

fn qr_error(message: impl Into<String>) -> QrPoll {
    QrPoll {
        state: "error".into(),
        message: message.into(),
        expires_in: 0,
        credential: None,
    }
}

/// 让扫码状态可单测，也避免把任意字符直接拼进 URL。
fn valid_code(code: &str) -> bool {
    let len = code.len();
    (8..=64).contains(&len)
        && code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub async fn qr_start() -> Result<QrTicket, AppError> {
    let resp = dy_client()
        .post(GENERATE)
        .headers(base_headers(LOGIN_REFERER))
        .form(&[("client_id", "1"), ("isMultiAccount", "0")])
        .send()
        .await
        .map_err(|err| AppError::User(format!("斗鱼二维码生成失败：{}", err.without_url())))?;
    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|err| AppError::User(format!("读取斗鱼二维码失败：{}", err.without_url())))?;
    if !status.is_success() || bytes.len() > MAX_BODY {
        return Err(AppError::User(format!(
            "斗鱼二维码接口异常（HTTP {}）",
            status.as_u16()
        )));
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| AppError::User("斗鱼二维码接口返回格式异常".into()))?;
    let error = value_i64(&value, "error")
        .ok_or_else(|| AppError::User("斗鱼二维码接口缺少 error 字段，可能触发了风控".into()))?;
    if error != 0 {
        return Err(AppError::User(format!(
            "斗鱼二维码生成失败：{}",
            value_str(&value, "msg").unwrap_or("请稍后重试")
        )));
    }
    let data = value
        .get("data")
        .and_then(Value::as_object)
        .ok_or_else(|| AppError::User("斗鱼二维码返回缺少 data".into()))?;
    let code = data
        .get("code")
        .and_then(Value::as_str)
        .filter(|code| valid_code(code))
        .ok_or_else(|| AppError::User("斗鱼二维码 code 无效".into()))?;
    let url = data
        .get("url")
        .and_then(Value::as_str)
        .filter(|url| !url.trim().is_empty())
        .ok_or_else(|| AppError::User("斗鱼二维码内容为空".into()))?;
    let expires_in = data
        .get("expire")
        .and_then(Value::as_i64)
        .unwrap_or(300)
        .clamp(60, 900);
    let svg = crate::account::render_qr(url)?;
    tracing::info!(expires_in, "斗鱼扫码二维码已生成");
    Ok(QrTicket {
        qrcode_key: code.to_string(),
        url: url.to_string(),
        svg,
        expires_in,
    })
}

pub async fn qr_poll(db: &Db, app: &AppHandle, code: &str) -> Result<QrPoll, AppError> {
    if !valid_code(code) {
        return Ok(qr_error("二维码参数无效，请刷新"));
    }
    let url = format!("{POLL}?time={}&code={}", now_ms(), code);
    let (status, headers, value) =
        send_json(dy_client().get(url).headers(base_headers(LOGIN_REFERER))).await?;
    if status == StatusCode::TOO_MANY_REQUESTS {
        return Ok(qr_error("请求过于频繁，请稍后再试"));
    }
    if !status.is_success() {
        return Ok(qr_error(format!(
            "斗鱼扫码接口异常（HTTP {}）",
            status.as_u16()
        )));
    }
    let error = value_i64(&value, "error").unwrap_or(-99);
    let state = poll_state(error);
    if state != "confirmed" {
        return Ok(QrPoll {
            state: state.into(),
            message: poll_message(error, value_str(&value, "msg").unwrap_or("")),
            expires_in: 0,
            credential: None,
        });
    }

    let callback = value
        .get("data")
        .and_then(|data| data.get("url"))
        .and_then(Value::as_str)
        .filter(|url| !url.trim().is_empty())
        .ok_or_else(|| AppError::User("斗鱼已确认登录，但没有返回回调地址".into()))?;
    let callback = resolve_callback(PASSPORT, callback)?;

    let mut jar = BTreeMap::new();
    absorb_cookies(&headers, &mut jar);
    let (final_status, final_url, _, _) = follow_chain(callback, &mut jar, MAIN_REFERER).await?;
    if !final_status.is_success() {
        return Ok(qr_error(format!(
            "斗鱼登录回调失败（HTTP {}）",
            final_status.as_u16()
        )));
    }
    let merged = merge_login_cookies(&jar);
    if !has_required_cookies(&merged) {
        tracing::warn!(
            final_url = %final_url,
            cookie_names = ?credential_names(&merged),
            "斗鱼扫码回调缺少登录 Cookie"
        );
        return Ok(qr_error("登录没有完成：斗鱼未返回完整登录凭证，请重新扫码"));
    }
    let account = profile_from_cookie(&merged);
    let row = crate::account::store_account(db, "douyu", &merged, &account)?;

    let ltp0 = cookie_value(&merged, "LTP0").or_else(|| cookie_value(&merged, "ltp0"));
    let ltp0 = ltp0.or_else(|| {
        jar.iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("LTP0"))
            .map(|(_, value)| value.clone())
    });
    let device = existing_device(&merged)
        .or_else(|| {
            jar.iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("dy_did"))
                .map(|(_, value)| value.clone())
        })
        .unwrap_or_default();
    if let Some(ltp0) = ltp0 {
        let secret = CredentialSecretRow {
            platform: "douyu".into(),
            refresh_token: ltp0,
            device_id: device,
            updated_at: now_ts(),
        };
        if let Err(err) = db.save_credential_secret(&secret) {
            tracing::warn!(error = %err, "保存斗鱼长期凭证失败");
        }
    } else {
        let _ = db.clear_credential_secret("douyu");
        tracing::warn!("斗鱼扫码登录未拿到 LTP0，自动续期不可用");
    }
    let _ = db.clear_credential_refresh("douyu");
    crate::account::emit_account_refresh(app);
    tracing::info!(
        account = %row.account_name,
        uid = %row.uid,
        refreshable = has_refresh_token(db),
        "斗鱼扫码登录成功"
    );
    let info = crate::account::info_for_row(db, &row);
    let message = if info.refreshable {
        "登录成功，已启用 Cookie 自动续期".to_string()
    } else {
        "登录成功，但没有拿到长期凭证，自动续期不可用；可稍后重新扫码".to_string()
    };
    Ok(QrPoll {
        state: "confirmed".into(),
        message,
        expires_in: 0,
        credential: Some(info),
    })
}

fn merge_login_cookies(jar: &BTreeMap<String, String>) -> String {
    let mut extra = String::new();
    for (name, value) in jar {
        if name.eq_ignore_ascii_case("LTP0") {
            continue;
        }
        if !extra.is_empty() {
            extra.push_str("; ");
        }
        extra.push_str(name);
        extra.push('=');
        extra.push_str(value);
    }
    extra
}

fn credential_names(cookie: &str) -> Vec<String> {
    crate::credential::names(cookie)
}

fn cookie_value(cookie: &str, name: &str) -> Option<String> {
    crate::credential::value(cookie, name)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn has_required_cookies(cookie: &str) -> bool {
    REQUIRED_COOKIES
        .iter()
        .all(|name| cookie_value(cookie, name).is_some())
}

fn existing_device(cookie: &str) -> Option<String> {
    DEVICE_COOKIES
        .iter()
        .find_map(|name| cookie_value(cookie, name))
}

fn is_device_cookie(name: &str) -> bool {
    DEVICE_COOKIES
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(name))
}

fn url_decode(raw: &str) -> String {
    let wrapped = format!("v={raw}");
    url::form_urlencoded::parse(wrapped.as_bytes())
        .next()
        .map(|(_, value)| value.into_owned())
        .unwrap_or_else(|| raw.to_string())
}

fn profile_from_cookie(cookie: &str) -> Account {
    let uid = cookie_value(cookie, "acf_uid").unwrap_or_default();
    let name = cookie_value(cookie, "acf_nickname")
        .or_else(|| cookie_value(cookie, "acf_username"))
        .map(|name| url_decode(&name))
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| format!("斗鱼用户 {uid}"));
    Account {
        uid,
        name,
        avatar: None,
    }
}

enum Probe {
    Ok,
    Invalid,
    Transient,
}

fn classify_probe(status: StatusCode, body: &str) -> Probe {
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return Probe::Invalid;
    }
    if !status.is_success() {
        return Probe::Transient;
    }
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        // 风控页 / HTML 登录页没有 error 字段：不能当成确定失效，否则网络抖动会误踢登录态。
        return Probe::Transient;
    };
    match value_i64(&value, "error") {
        Some(0) => Probe::Ok,
        Some(_) => Probe::Invalid,
        None => Probe::Transient,
    }
}

async fn probe(cookie: &str) -> Probe {
    let resp = dy_client()
        .get(PROBE)
        .headers(base_headers(MAIN_REFERER))
        .header(reqwest::header::COOKIE, cookie)
        .send()
        .await;
    let Ok(resp) = resp else {
        return Probe::Transient;
    };
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    classify_probe(status, &body)
}

/// 校验用户粘贴的斗鱼 Cookie：必须能通过 follow/top3 探针，并尽量拿到昵称。
pub(crate) async fn validate_cookie(cookie: &str) -> Result<Account, AppError> {
    match probe(cookie).await {
        Probe::Ok => Ok(profile_from_cookie(cookie)),
        Probe::Invalid => Err(AppError::User("斗鱼 Cookie 已失效或未登录".into())),
        Probe::Transient => Err(AppError::Platform {
            code: "douyu_upstream",
            message: "斗鱼接口暂时不可用，无法校验 Cookie，请稍后重试".into(),
        }),
    }
}

pub fn has_refresh_token(db: &Db) -> bool {
    db.credential_secret("douyu")
        .ok()
        .flatten()
        .is_some_and(|secret| !secret.refresh_token.trim().is_empty())
}

fn next_refresh_at(status: &RefreshStatus, updated_at: i64) -> i64 {
    match status.outcome.as_str() {
        "refreshed" | "fresh" => status.last_refreshed_at.max(updated_at) + REFRESH_EVERY,
        "failed" | "not_refreshable" => {
            status.checked_at
                + if status.key_stale {
                    RETRY_RESCAN
                } else {
                    RETRY_TRANSIENT
                }
        }
        _ => updated_at.max(status.last_refreshed_at) + REFRESH_EVERY,
    }
}

/// 到期续期之外，还有一个必须立即处理的场景：Cookie 在 3 天周期内先失效，
/// revalidate 已把账号标记为 expired。此时不能继续等 next_refresh_at，否则
/// 要再等满 3 天才会尝试用 LTP0 恢复。失败的续期仍按 6 小时/24 小时退避。
fn refresh_due(
    force: bool,
    expired: bool,
    before: &RefreshStatus,
    updated_at: i64,
    now: i64,
) -> bool {
    if force {
        return true;
    }
    if expired && before.outcome != "failed" {
        return true;
    }
    now >= next_refresh_at(before, updated_at)
}

fn refreshed_status(message: &str, last_refreshed_at: i64, key_stale: bool) -> RefreshStatus {
    RefreshStatus {
        checked_at: now_ts(),
        outcome: if key_stale { "failed" } else { "refreshed" }.into(),
        message: Some(message.to_string()),
        key_stale,
        last_refreshed_at,
    }
}

fn write_status(db: &Db, status: &RefreshStatus) {
    crate::account::save_refresh_status(db, "douyu", status);
}

fn notify_expired(app: &AppHandle, account_name: &str) {
    let notice = AppNotice {
        kind: "credential_expired".into(),
        room_id: String::new(),
        host_name: account_name.to_string(),
        detail: Some("douyu".into()),
        suppress: false,
    };
    if let Err(err) = app.emit(crate::recorder::EVENT_APP_NOTICE, notice) {
        tracing::warn!(error = %err, "斗鱼登录失效通知发送失败");
    }
}

fn mark_expired(db: &Db, app: &AppHandle, row: &CredentialRow) -> bool {
    let _guard = crate::account::credential_write_guard();
    if row.status == "expired" {
        return false;
    }
    match db.update_credential_profile(
        "douyu",
        "expired",
        &row.account_name,
        row.avatar.as_deref(),
        now_ts(),
    ) {
        Ok(true) => {
            crate::credential::set_status("douyu", "expired");
            notify_expired(app, &row.account_name);
            true
        }
        Ok(false) => false,
        Err(err) => {
            tracing::warn!(error = %err, "标记斗鱼账号过期失败");
            false
        }
    }
}

fn merge_refreshed_cookie(base: &str, jar: &BTreeMap<String, String>, keep_device: bool) -> String {
    let mut extra = String::new();
    for (name, value) in jar {
        if name.eq_ignore_ascii_case("LTP0") {
            continue;
        }
        if keep_device && is_device_cookie(name) {
            continue;
        }
        if !extra.is_empty() {
            extra.push_str("; ");
        }
        extra.push_str(name);
        extra.push('=');
        extra.push_str(value);
    }
    let merged = crate::credential::merge(base, &extra);
    crate::credential::without(&merged, "LTP0")
}

/// safeAuth 只带 LTP0（以及配套设备号）换票；返回探针结果与本次响应累积的 Cookie。
async fn safe_auth(ltp0: &str, device: &str) -> (Probe, StatusCode, BTreeMap<String, String>) {
    let mut jar = BTreeMap::new();
    jar.insert("LTP0".to_string(), ltp0.to_string());
    if !device.trim().is_empty() {
        jar.insert("dy_did".to_string(), device.to_string());
    }
    let url = format!(
        "{SAFE_AUTH}?client_id=1&t={}&_={}&callback=cb",
        now_ms(),
        now_ms()
    );
    let Ok(url) = Url::parse(&url) else {
        return (Probe::Transient, StatusCode::BAD_GATEWAY, jar);
    };
    let result = follow_chain(url, &mut jar, FOLLOW_REFERER).await;
    let Ok((status, final_url, _, body)) = result else {
        return (Probe::Transient, StatusCode::BAD_GATEWAY, jar);
    };
    if status != StatusCode::OK {
        let probe = if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            Probe::Invalid
        } else {
            Probe::Transient
        };
        return (probe, status, jar);
    }
    let merged = merge_refreshed_cookie("", &jar, false);
    if has_required_cookies(&merged) {
        // 换票链自己有结果后再打一次业务探针，避免把风控页/登录页当成成功。
        let probe = probe(&merged).await;
        return (probe, status, jar);
    }
    let lower = body.to_ascii_lowercase();
    if final_url.as_str().contains("/login")
        || lower.contains("未登录")
        || lower.contains("请登录")
        || lower.contains("not login")
    {
        (Probe::Invalid, status, jar)
    } else {
        (Probe::Transient, status, jar)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum RefreshOutcome {
    NoCredential,
    NotRefreshable,
    Refreshed,
    Invalid,
    Transient,
    Discarded,
}

async fn refresh_once(db: &Db, app: &AppHandle, force: bool) -> RefreshOutcome {
    let Some(row) = db
        .credentials()
        .ok()
        .and_then(|rows| rows.into_iter().find(|row| row.platform == "douyu"))
    else {
        return RefreshOutcome::NoCredential;
    };
    let Some(secret) = db.credential_secret("douyu").ok().flatten() else {
        // 手动 Cookie 每次维护循环都会走到这里；只写一次状态，避免把「上次检查」
        // 每 30 分钟刷成当前时间，也让 revalidate 能按真实时间定期探活。
        let current = crate::account::load_refresh_status(db, "douyu");
        if current.outcome != "not_refreshable" || current.checked_at == 0 {
            let status = RefreshStatus {
                checked_at: now_ts(),
                outcome: "not_refreshable".into(),
                message: Some("当前 Cookie 没有长期凭证，无法自动续期；请扫码登录".into()),
                key_stale: false,
                last_refreshed_at: 0,
            };
            write_status(db, &status);
        }
        return RefreshOutcome::NotRefreshable;
    };
    if secret.refresh_token.trim().is_empty() {
        return RefreshOutcome::NotRefreshable;
    }
    let before = crate::account::load_refresh_status(db, "douyu");
    if !refresh_due(
        force,
        row.status == "expired",
        &before,
        row.updated_at,
        now_ts(),
    ) {
        return RefreshOutcome::Refreshed;
    }

    let _guard = REFRESH_LOCK.lock().await;
    // 等锁期间用户可能重新扫码/删除；重新读一次凭据与 LTP0，避免拿旧票据换票。
    let Some(row) = db
        .credentials()
        .ok()
        .and_then(|rows| rows.into_iter().find(|row| row.platform == "douyu"))
    else {
        return RefreshOutcome::NoCredential;
    };
    let Some(secret) = db.credential_secret("douyu").ok().flatten() else {
        return RefreshOutcome::NotRefreshable;
    };
    let ltp0 = secret.refresh_token.clone();
    if ltp0.trim().is_empty() {
        return RefreshOutcome::NotRefreshable;
    }

    let (probe_result, chain_status, jar) = safe_auth(&ltp0, &secret.device_id).await;
    match probe_result {
        Probe::Ok => {}
        Probe::Invalid => {
            let status = RefreshStatus {
                checked_at: now_ts(),
                outcome: "failed".into(),
                message: Some("斗鱼长期凭证已失效，请重新扫码登录".into()),
                key_stale: true,
                last_refreshed_at: before.last_refreshed_at,
            };
            write_status(db, &status);
            mark_expired(db, app, &row);
            return RefreshOutcome::Invalid;
        }
        Probe::Transient => {
            let status = RefreshStatus {
                checked_at: now_ts(),
                outcome: "failed".into(),
                message: Some("斗鱼续期临时失败，稍后自动重试".into()),
                key_stale: false,
                last_refreshed_at: before.last_refreshed_at,
            };
            write_status(db, &status);
            return RefreshOutcome::Transient;
        }
    }
    if chain_status != StatusCode::OK {
        return RefreshOutcome::Transient;
    }

    let keep_device = existing_device(&row.cookie).is_some();
    let merged = merge_refreshed_cookie(&row.cookie, &jar, keep_device);
    if !has_required_cookies(&merged) {
        let status = RefreshStatus {
            checked_at: now_ts(),
            outcome: "failed".into(),
            message: Some("斗鱼续期没有返回完整登录 Cookie，稍后自动重试".into()),
            key_stale: false,
            last_refreshed_at: before.last_refreshed_at,
        };
        write_status(db, &status);
        return RefreshOutcome::Transient;
    }

    // 写回前再次确认 LTP0 没有被换掉；否则这是上一份凭据的续期结果，必须丢弃。
    let current_ltp0 = db
        .credential_secret("douyu")
        .ok()
        .flatten()
        .map(|secret| secret.refresh_token)
        .unwrap_or_default();
    if current_ltp0 != ltp0 {
        tracing::warn!("斗鱼续期期间长期凭证已变化，丢弃本次结果");
        return RefreshOutcome::Discarded;
    }

    let was_expired = row.status == "expired";
    let now = now_ts();
    let updated = CredentialRow {
        cookie: merged.clone(),
        status: "ok".into(),
        updated_at: now,
        ..row
    };
    if let Err(err) = db.save_credential(&updated) {
        tracing::warn!(error = %err, "保存斗鱼续期 Cookie 失败");
        return RefreshOutcome::Transient;
    }
    crate::credential::set("douyu", &merged);
    if was_expired {
        crate::credential::set_status("douyu", "ok");
    }
    if !keep_device {
        if let Some(device) = existing_device(&merged) {
            let secret = CredentialSecretRow {
                platform: "douyu".into(),
                refresh_token: ltp0.clone(),
                device_id: device,
                updated_at: now,
            };
            let _ = db.save_credential_secret(&secret);
        }
    }
    let status = refreshed_status("已自动续期", now, false);
    write_status(db, &status);
    crate::account::clear_stream_cache(app);
    crate::account::emit_account_refresh(app);
    tracing::info!("斗鱼 Cookie 自动续期完成");
    RefreshOutcome::Refreshed
}

/// 设置页「立即检查」：强制走一遍续期链路并返回最新状态。
pub async fn refresh_now(db: &Db, app: &AppHandle) -> Result<RefreshStatus, AppError> {
    let has_credential = db
        .credentials()
        .map(|rows| rows.iter().any(|row| row.platform == "douyu"))
        .unwrap_or(false);
    if !has_credential {
        return Err(AppError::User("尚未登录斗鱼".into()));
    }
    let outcome = refresh_once(db, app, true).await;
    if outcome == RefreshOutcome::NoCredential {
        return Err(AppError::User("尚未登录斗鱼".into()));
    }
    Ok(crate::account::load_refresh_status(db, "douyu"))
}

/// 运行中定期复验斗鱼登录态：没有 LTP0 的手动 Cookie 也能被发现失效。
pub async fn revalidate(db: &Db, app: &AppHandle) {
    let Some(row) = db
        .credentials()
        .ok()
        .and_then(|rows| rows.into_iter().find(|row| row.platform == "douyu"))
    else {
        return;
    };
    let status = crate::account::load_refresh_status(db, "douyu");
    if status.checked_at > 0 && now_ts() - status.checked_at < 300 {
        return;
    }
    match probe(&row.cookie).await {
        Probe::Ok => {
            if row.status == "expired" {
                let _guard = crate::account::credential_write_guard();
                let _ = db.update_credential_profile(
                    "douyu",
                    "ok",
                    &row.account_name,
                    row.avatar.as_deref(),
                    now_ts(),
                );
                crate::credential::set_status("douyu", "ok");
                crate::account::emit_account_refresh(app);
                tracing::info!("斗鱼登录态已恢复");
            }
        }
        Probe::Invalid => {
            if mark_expired(db, app, &row) {
                tracing::warn!("斗鱼登录态已失效，已标记过期");
            }
        }
        Probe::Transient => {}
    }
}

/// 独立维护循环：不动 B 站的 refresh_loop，避免斗鱼失败影响 B 站续期节奏。
pub async fn maintenance_loop(db: std::sync::Arc<Db>, app: AppHandle) {
    tokio::time::sleep(Duration::from_secs(30)).await;
    loop {
        // 先探活：手动 Cookie 没有 LTP0，refresh_once 只会返回 not_refreshable，
        // 若先跑它会把 checked_at 刷新到当前，导致 revalidate 永远被 5 分钟闸门跳过。
        revalidate(&db, &app).await;
        let _ = refresh_once(&db, &app, false).await;
        tokio::time::sleep(Duration::from_secs(30 * 60)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe_status(outcome: &str, checked_at: i64, last: i64) -> RefreshStatus {
        RefreshStatus {
            checked_at,
            outcome: outcome.into(),
            message: None,
            key_stale: false,
            last_refreshed_at: last,
        }
    }

    #[test]
    fn expired_credential_refreshes_before_the_three_day_cycle() {
        let fresh = probe_status("refreshed", 100, 100);
        assert!(refresh_due(false, true, &fresh, 100, 200));
        assert!(!refresh_due(false, false, &fresh, 100, 200));
    }

    #[test]
    fn failed_refresh_keeps_backoff_even_when_expired() {
        let mut failed = probe_status("failed", 100, 100);
        failed.key_stale = true;
        assert!(!refresh_due(false, true, &failed, 100, 200));
        assert!(refresh_due(true, true, &failed, 100, 200));
    }

    #[test]
    fn douyu_poll_codes_map_to_states() {
        assert_eq!(poll_state(-2), "pending");
        assert_eq!(poll_state(1), "scanned");
        assert_eq!(poll_state(0), "confirmed");
        assert_eq!(poll_state(-1), "expired");
        assert_eq!(poll_state(99), "error");
    }

    #[test]
    fn set_cookie_parsing_skips_attributes_and_empty_values() {
        assert_eq!(
            parse_set_cookie("acf_uid=42; Path=/; HttpOnly"),
            Some(("acf_uid".into(), "42".into()))
        );
        assert_eq!(parse_set_cookie("acf_uid=; Max-Age=0"), None);
        assert_eq!(parse_set_cookie("Path=/"), None);
    }

    #[test]
    fn required_cookie_fields_are_enforced() {
        let good =
            "acf_uid=1; acf_auth=a; acf_stk=b; acf_ltkid=c; acf_username=u; acf_biz=d; acf_ct=e";
        assert!(has_required_cookies(good));
        assert!(!has_required_cookies("acf_uid=1; acf_auth=a"));
    }

    #[test]
    fn device_cookie_detection_covers_three_names() {
        assert_eq!(existing_device("dy_did=d1").as_deref(), Some("d1"));
        assert_eq!(existing_device("acf_did=d2").as_deref(), Some("d2"));
        assert_eq!(existing_device("acf_devid=d3").as_deref(), Some("d3"));
        assert_eq!(existing_device("acf_uid=1"), None);
        assert!(is_device_cookie("dy_did"));
        assert!(!is_device_cookie("acf_uid"));
    }

    #[test]
    fn profile_uses_nickname_and_decodes_it() {
        let account =
            profile_from_cookie("acf_uid=42; acf_nickname=%E6%96%97%E9%B1%BC%E7%94%A8%E6%88%B7");
        assert_eq!(account.uid, "42");
        assert_eq!(account.name, "斗鱼用户");
    }

    #[test]
    fn refresh_schedule_starts_three_days_after_login_and_retries_six_hours() {
        let now = 1_000_000;
        assert_eq!(
            next_refresh_at(&probe_status("never", 0, 0), now),
            now + REFRESH_EVERY
        );
        assert_eq!(
            next_refresh_at(&probe_status("failed", now, 0), now),
            now + RETRY_TRANSIENT
        );
        let mut stale = probe_status("failed", now, 0);
        stale.key_stale = true;
        assert_eq!(next_refresh_at(&stale, now), now + RETRY_RESCAN);
        assert_eq!(
            next_refresh_at(&probe_status("refreshed", now, now), now),
            now + REFRESH_EVERY
        );
    }

    #[test]
    fn callback_must_stay_on_douyu_https() {
        assert!(resolve_callback(PASSPORT, "https://www.douyu.com/foo").is_ok());
        assert!(resolve_callback(PASSPORT, "https://douyu.com/foo").is_ok());
        assert!(resolve_callback(PASSPORT, "https://evil.com/foo").is_err());
        assert!(resolve_callback(PASSPORT, "http://www.douyu.com/foo").is_err());
        assert!(resolve_callback(PASSPORT, "//www.douyu.com/foo").is_ok());
    }

    #[test]
    fn probe_classification_does_not_treat_risk_pages_as_expired() {
        assert!(matches!(
            classify_probe(StatusCode::OK, r#"{"error":0}"#),
            Probe::Ok
        ));
        assert!(matches!(
            classify_probe(StatusCode::OK, r#"{"error":1}"#),
            Probe::Invalid
        ));
        assert!(matches!(
            classify_probe(StatusCode::OK, "<html>risk</html>"),
            Probe::Transient
        ));
        assert!(matches!(
            classify_probe(StatusCode::INTERNAL_SERVER_ERROR, ""),
            Probe::Transient
        ));
    }

    #[test]
    fn merge_refresh_keeps_existing_device_when_requested() {
        let jar = BTreeMap::from([
            ("acf_uid".to_string(), "1".to_string()),
            ("dy_did".to_string(), "new-device".to_string()),
            ("LTP0".to_string(), "secret".to_string()),
        ]);
        let merged = merge_refreshed_cookie("acf_uid=1; dy_did=old-device", &jar, true);
        assert!(merged.contains("dy_did=old-device"));
        assert!(!merged.contains("new-device"));
        assert!(!merged.contains("LTP0"));
    }
}
