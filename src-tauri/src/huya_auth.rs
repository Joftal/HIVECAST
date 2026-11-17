//! 虎牙账号：扫码登录、Cookie 校验与自动续期。
//!
//! 虎牙网页登录态由一组 udb_* Cookie 组成。真正能续期的是长期凭证 udb_cred：
//! 1. 匿名设备 Cookie（udb_guiddata/__yamid_new/guid/sdid/udb_deviceid 等）先向
//!    udblgn.huya.com 申请二维码，再由客户端轮询扫码阶段；
//! 2. 扫码确认后，用服务端给的 loginTime/sign 请求 huyaCallback 换取登录 Cookie；
//! 3. checkLogin 验证 uid/昵称，udb_cred 单独加密存 credential_secret；
//! 4. 之后每 20 分钟 cookieExchange 轮换 Cookie，失败回退 credLogin；
//! 5. 每 6 小时 checkLogin 复验，失效才标记过期，避免网络抖动误踢登录态。
//!
//! 手动粘贴的 Cookie 如果没有 udb_cred，只能校验、不能自动续期；这条降级路径
//! 与斗鱼一致，不能拿旧设备的 udb_cred 去换新账号的票。

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use hivecast_ipc::{AppNotice, QrPoll, QrTicket, RefreshStatus};
use rand::Rng;
use reqwest::header::{HeaderMap, ORIGIN, REFERER, SET_COOKIE};
use reqwest::redirect::Policy;
use reqwest::{Client, Response, StatusCode};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::account::Account;
use crate::db::{now_ts, CredentialRow, CredentialSecretRow, Db};
use crate::error::AppError;

const QR_API: &str = "https://udblgn.huya.com/qrLgn";
const LOGIN_API: &str = "https://udblgn.huya.com/web";
const CALLBACK: &str = "https://l.huya.com/udb_web/udbport2.php?m=HuyaLogin&do=huyaCallback";
const CHECK_LOGIN: &str = "https://l.huya.com/udb_web/udbport2.php?m=HuyaLogin&do=checkLogin";
const MAIN_REFERER: &str = "https://www.huya.com/";
const APP_ID: &str = "5002";
const APP_VERSION: &str = "2.6";
const APP_SIGN: &str = "1ce3bf682483d03f146f58232ec10635";
const QR_TTL: i64 = 225;
const EXCHANGE_EVERY: i64 = 20 * 60;
const RETRY_TRANSIENT: i64 = 6 * 60 * 60;
const RETRY_RESCAN: i64 = 24 * 60 * 60;
const REVALIDATE_EVERY: i64 = 6 * 60 * 60;
const MAX_BODY: usize = 2 * 1024 * 1024;

/// 定时续期与手动检查共用一把锁，避免两个 cookieExchange 同时换票。
static REFRESH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
/// 扫码确认后的落库只执行一次；重复轮询直接返回已登录。
static QR_CONFIRM_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
/// 已确认的二维码只保留少量，防止集合无限增长。
static QR_CONFIRMED: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
/// 内存里的复验时间。复用 DB 的 checked_at 会和 20 分钟轮换互相覆盖。
static REVALIDATED_AT: AtomicI64 = AtomicI64::new(0);

struct QrSession {
    jar: BTreeMap<String, String>,
    created_at: i64,
}

/// 二维码只活 225 秒；用 Vec 保存少量会话，读取时顺手清理过期项。
static QR_SESSIONS: Mutex<Vec<(String, QrSession)>> = Mutex::new(Vec::new());

fn lock_sessions() -> std::sync::MutexGuard<'static, Vec<(String, QrSession)>> {
    QR_SESSIONS.lock().unwrap_or_else(|err| err.into_inner())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_else(|_| now_ts() * 1000)
}

fn hy_client() -> &'static Client {
    static CLIENT: std::sync::OnceLock<Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .user_agent(crate::platform::http::USER_AGENT)
            .timeout(Duration::from_secs(15))
            .gzip(true)
            .redirect(Policy::limited(5))
            .build()
            .expect("构建虎牙 HTTP 客户端失败")
    })
}

fn base_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(ORIGIN, "https://www.huya.com".parse().expect("固定 Origin"));
    headers.insert(REFERER, MAIN_REFERER.parse().expect("固定 Referer"));
    headers
}

fn upstream(message: impl Into<String>) -> AppError {
    AppError::Platform {
        code: "huya_upstream",
        message: message.into(),
    }
}

fn user(message: impl Into<String>) -> AppError {
    AppError::User(message.into())
}

fn random_hex(len: usize, upper: bool) -> String {
    let mut raw = String::new();
    while raw.len() < len {
        raw.push_str(&uuid::Uuid::new_v4().simple().to_string());
    }
    raw.truncate(len);
    if upper {
        raw.to_uppercase()
    } else {
        raw
    }
}

fn random_digits(len: usize) -> String {
    let mut rng = rand::thread_rng();
    (0..len)
        .map(|_| char::from(b'0' + rng.gen_range(0..10)))
        .collect()
}

/// 官方网页会用这组匿名 Cookie 把二维码绑定到「同一台设备」。
/// 实测只给前 5 个时 tryQrLogin 会直接返回 stage=6，补上 deviceid/appid/passdata 才正常。
fn new_device_jar() -> BTreeMap<String, String> {
    let guid = random_hex(32, false);
    let mut jar = BTreeMap::new();
    jar.insert("udb_guiddata".into(), guid.clone());
    jar.insert("__yamid_new".into(), random_hex(32, true));
    jar.insert("guid".into(), guid);
    jar.insert("sdid".into(), format!("csid_{}", random_hex(32, false)));
    jar.insert("PHPSESSID".into(), random_hex(26, false));
    jar.insert(
        "udb_deviceid".into(),
        format!("w_{}{}", now_ms(), random_digits(6)),
    );
    jar.insert("udb_appid".into(), APP_ID.into());
    jar.insert("udb_passdata".into(), "3".into());
    jar
}

fn parse_cookie(raw: &str) -> BTreeMap<String, String> {
    let mut jar = BTreeMap::new();
    for part in raw.split(';') {
        let Some((name, value)) = part.trim().split_once('=') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        jar.insert(name.to_string(), value.trim().to_string());
    }
    jar
}

fn jar_cookie(jar: &BTreeMap<String, String>) -> String {
    jar.iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; ")
}

fn cookie_value(cookie: &str, name: &str) -> Option<String> {
    parse_cookie(cookie)
        .get(name)
        .filter(|value| !value.is_empty())
        .cloned()
}

fn absorb_cookies(headers: &HeaderMap, jar: &mut BTreeMap<String, String>) -> bool {
    let mut changed = false;
    for value in headers.get_all(SET_COOKIE) {
        let raw = value.to_str().unwrap_or("");
        let first = raw.split(';').next().unwrap_or("");
        let Some((name, val)) = first.split_once('=') else {
            continue;
        };
        let name = name.trim();
        let val = val.trim();
        if name.is_empty() {
            continue;
        }
        let expired = val.is_empty() || raw.to_ascii_lowercase().contains("max-age=0");
        if expired {
            if jar.remove(name).is_some() {
                changed = true;
            }
            continue;
        }
        if jar.get(name).map(String::as_str) != Some(val) {
            jar.insert(name.to_string(), val.to_string());
            changed = true;
        }
    }
    changed
}

#[cfg(test)]
fn merge_cookie(base: &str, extra: &BTreeMap<String, String>) -> String {
    let mut jar = parse_cookie(base);
    for (name, value) in extra {
        jar.insert(name.clone(), value.clone());
    }
    jar_cookie(&jar)
}

fn has_login_cookie(cookie: &str) -> bool {
    cookie_value(cookie, "udb_uid").is_some() && cookie_value(cookie, "udb_biztoken").is_some()
}

fn sdid_of(cookie: &str) -> String {
    cookie_value(cookie, "sdid").unwrap_or_default()
}

async fn response_json(resp: Response) -> Result<Value, AppError> {
    let status = resp.status();
    let text = resp.text().await.map_err(|err| {
        tracing::warn!(error = %err, "读取虎牙响应失败");
        upstream("虎牙接口暂时不可用，请稍后重试")
    })?;
    if text.len() > MAX_BODY {
        return Err(upstream("虎牙接口返回内容过大"));
    }
    if !status.is_success() {
        return Err(upstream(format!("虎牙接口返回 HTTP {status}")));
    }
    serde_json::from_str(&text).map_err(|err| {
        tracing::warn!(error = %err, body = %text.chars().take(200).collect::<String>(), "虎牙响应解析失败");
        upstream("虎牙接口响应解析失败")
    })
}

fn return_code(value: &Value) -> i64 {
    value
        .get("returnCode")
        .or_else(|| value.get("return_code"))
        .and_then(|v| v.as_i64())
        .unwrap_or(-1)
}

fn message_of(value: &Value) -> String {
    value
        .get("description")
        .or_else(|| value.get("message"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("虎牙接口返回错误")
        .to_string()
}

fn data_of(value: &Value) -> &Value {
    value.get("data").unwrap_or(&Value::Null)
}

fn value_i64(value: &Value, key: &str) -> i64 {
    match value.get(key) {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

fn value_str(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn value_bool(value: &Value, key: &str) -> bool {
    match value.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0) != 0,
        Some(Value::String(s)) => s == "1" || s.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

fn page_encode(text: &str) -> String {
    url::form_urlencoded::byte_serialize(text.as_bytes()).collect()
}

fn behavior() -> String {
    let raw = json!({
        "furl": MAIN_REFERER,
        "curl": MAIN_REFERER,
        "user_action": [],
    })
    .to_string();
    page_encode(&raw)
}

fn qr_context(jar: &BTreeMap<String, String>) -> String {
    format!(
        "WB-{}-{}-{}",
        jar.get("udb_guiddata").cloned().unwrap_or_default(),
        jar.get("__yamid_new").cloned().unwrap_or_default(),
        jar.get("guid").cloned().unwrap_or_default(),
    )
}

fn request_body(jar: &BTreeMap<String, String>, uri: &str, data: Value) -> Value {
    json!({
        "uri": uri,
        "version": APP_VERSION,
        "context": qr_context(jar),
        "appId": APP_ID,
        "appSign": APP_SIGN,
        "authId": "",
        "sdid": jar.get("sdid").cloned().unwrap_or_default(),
        "lcid": "2052",
        "byPass": "3",
        "requestId": random_digits(8),
        "data": data,
    })
}

async fn post_huya(
    url: &str,
    body: &Value,
    cookie: &str,
) -> Result<(BTreeMap<String, String>, Value), AppError> {
    let resp = hy_client()
        .post(url)
        .headers(base_headers())
        .header(reqwest::header::COOKIE, cookie)
        .json(body)
        .send()
        .await
        .map_err(|err| {
            tracing::warn!(error = %err, "请求虎牙接口失败");
            upstream("虎牙接口暂时不可用，请稍后重试")
        })?;
    let mut jar = BTreeMap::new();
    absorb_cookies(resp.headers(), &mut jar);
    let value = response_json(resp).await?;
    Ok((jar, value))
}

async fn get_huya(
    url: &str,
    cookie: &str,
) -> Result<(BTreeMap<String, String>, String, StatusCode), AppError> {
    let resp = hy_client()
        .get(url)
        .headers(base_headers())
        .header(reqwest::header::COOKIE, cookie)
        .send()
        .await
        .map_err(|err| {
            tracing::warn!(error = %err, "请求虎牙接口失败");
            upstream("虎牙接口暂时不可用，请稍后重试")
        })?;
    let status = resp.status();
    let mut jar = BTreeMap::new();
    absorb_cookies(resp.headers(), &mut jar);
    let text = resp.text().await.map_err(|err| {
        tracing::warn!(error = %err, "读取虎牙响应失败");
        upstream("虎牙接口暂时不可用，请稍后重试")
    })?;
    if text.len() > MAX_BODY {
        return Err(upstream("虎牙接口返回内容过大"));
    }
    Ok((jar, text, status))
}

async fn fetch_qr_id(
    jar: &BTreeMap<String, String>,
) -> Result<(String, BTreeMap<String, String>), AppError> {
    let body = request_body(
        jar,
        "70001",
        json!({
            "behavior": behavior(),
            "type": "",
            "domainList": "",
            "page": page_encode(MAIN_REFERER),
        }),
    );
    let (extra, value) = post_huya(&format!("{QR_API}/getQrId"), &body, &jar_cookie(jar)).await?;
    if return_code(&value) != 0 {
        return Err(upstream(format!(
            "生成虎牙二维码失败：{}",
            message_of(&value)
        )));
    }
    let qr_id = value_str(data_of(&value), "qrId");
    if qr_id.is_empty() {
        return Err(upstream("虎牙二维码响应缺少 qrId"));
    }
    Ok((qr_id, extra))
}

async fn fetch_qr_png(qr_id: &str, jar: &BTreeMap<String, String>) -> Result<Vec<u8>, AppError> {
    let url = format!("{QR_API}/getQrImg?k={}&appId={APP_ID}", page_encode(qr_id));
    let resp = hy_client()
        .get(url)
        .headers(base_headers())
        .header(reqwest::header::COOKIE, jar_cookie(jar))
        .send()
        .await
        .map_err(|err| {
            tracing::warn!(error = %err, "下载虎牙二维码失败");
            upstream("虎牙二维码下载失败，请稍后重试")
        })?;
    let status = resp.status();
    let bytes = resp.bytes().await.map_err(|err| {
        tracing::warn!(error = %err, "读取虎牙二维码失败");
        upstream("虎牙二维码下载失败，请稍后重试")
    })?;
    if !status.is_success() || bytes.len() < 8 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
        return Err(upstream("虎牙二维码图片格式异常"));
    }
    if bytes.len() > MAX_BODY {
        return Err(upstream("虎牙二维码图片过大"));
    }
    Ok(bytes.to_vec())
}

fn qr_svg(png: &[u8]) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(png);
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128" width="128" height="128"><image width="128" height="128" href="data:image/png;base64,{encoded}"/></svg>"#
    )
}

/// 生成虎牙二维码。设备 Cookie 完全由客户端生成，不需要先打开网页。
pub async fn qr_start() -> Result<QrTicket, AppError> {
    let mut jar = new_device_jar();
    let (qr_id, extra) = fetch_qr_id(&jar).await?;
    for (name, value) in extra {
        jar.insert(name, value);
    }
    let png = fetch_qr_png(&qr_id, &jar).await?;
    let expires_in = QR_TTL;
    {
        let mut sessions = lock_sessions();
        let cutoff = now_ts() - 10 * 60;
        sessions.retain(|(_, session)| session.created_at >= cutoff);
        sessions.push((
            qr_id.clone(),
            QrSession {
                jar,
                created_at: now_ts(),
            },
        ));
        if sessions.len() > 16 {
            let drop = sessions.len() - 16;
            sessions.drain(0..drop);
        }
    }
    tracing::info!("虎牙扫码二维码已生成 expires_in={expires_in}");
    Ok(QrTicket {
        qrcode_key: qr_id,
        url: MAIN_REFERER.into(),
        svg: qr_svg(&png),
        expires_in,
    })
}

fn qr_state(state: &str, message: impl Into<String>, expires_in: i64) -> QrPoll {
    QrPoll {
        state: state.into(),
        message: message.into(),
        expires_in,
        credential: None,
    }
}

fn update_session(qr_id: &str, extra: &BTreeMap<String, String>) {
    let mut sessions = lock_sessions();
    if let Some((_, session)) = sessions.iter_mut().find(|(key, _)| key == qr_id) {
        for (name, value) in extra {
            session.jar.insert(name.clone(), value.clone());
        }
    }
}

fn peek_session(qr_id: &str) -> Option<QrSession> {
    let sessions = lock_sessions();
    sessions
        .iter()
        .find(|(key, _)| key == qr_id)
        .map(|(_, session)| QrSession {
            jar: session.jar.clone(),
            created_at: session.created_at,
        })
}

fn is_confirmed(qr_id: &str) -> bool {
    let confirmed = QR_CONFIRMED.lock().unwrap_or_else(|err| err.into_inner());
    confirmed.iter().any(|key| key == qr_id)
}

fn remember_confirmed(qr_id: &str) {
    let mut confirmed = QR_CONFIRMED.lock().unwrap_or_else(|err| err.into_inner());
    confirmed.push_back(qr_id.to_string());
    while confirmed.len() > 32 {
        confirmed.pop_front();
    }
}

fn expire_session(qr_id: &str) {
    let mut sessions = lock_sessions();
    sessions.retain(|(key, _)| key != qr_id);
}

/// 轮询扫码状态；成功时完成换票、校验、落库，返回 CredentialInfo。
pub async fn qr_poll(db: &Db, app: &AppHandle, qr_id: &str) -> Result<QrPoll, AppError> {
    if is_confirmed(qr_id) {
        return Ok(QrPoll {
            state: "confirmed".into(),
            message: "虎牙登录成功".into(),
            expires_in: 0,
            credential: credential_info(db, "huya"),
        });
    }
    let Some(session) = peek_session(qr_id) else {
        return Ok(qr_state("expired", "二维码已失效，请刷新", 0));
    };
    let elapsed = now_ts() - session.created_at;
    if elapsed > QR_TTL + 15 {
        expire_session(qr_id);
        return Ok(qr_state("expired", "二维码已过期，请刷新", 0));
    }
    let body = request_body(
        &session.jar,
        "70003",
        json!({
            "qrId": qr_id,
            "remember": "1",
            "domainList": "",
            "behavior": behavior(),
            "page": page_encode(MAIN_REFERER),
        }),
    );
    let (extra, value) = post_huya(
        &format!("{QR_API}/tryQrLogin"),
        &body,
        &jar_cookie(&session.jar),
    )
    .await?;
    if !extra.is_empty() {
        update_session(qr_id, &extra);
    }
    if return_code(&value) != 0 {
        let message = message_of(&value);
        return Ok(qr_state("error", message, 0));
    }
    let stage = value_i64(data_of(&value), "stage");
    let remaining = (QR_TTL - elapsed).max(0);
    match stage {
        0 | 4 => Ok(qr_state("pending", "等待扫码", remaining)),
        1 | 10 => Ok(qr_state("scanned", "已扫码，请在手机上确认登录", remaining)),
        5 => {
            expire_session(qr_id);
            Ok(qr_state("expired", "二维码已过期，请刷新", 0))
        }
        6 => {
            expire_session(qr_id);
            Ok(qr_state("expired", "二维码已失效，请刷新", 0))
        }
        7 => {
            expire_session(qr_id);
            Ok(qr_state("expired", "已取消登录授权", 0))
        }
        8 => Ok(qr_state("error", "扫码账号异常，请更换账号后重试", 0)),
        2 | 3 => {
            let mut jar = session.jar.clone();
            for (name, value) in extra {
                jar.insert(name, value);
            }
            let poll = confirm_login(db, app, qr_id, &jar, data_of(&value)).await?;
            Ok(poll)
        }
        other => Ok(qr_state(
            "error",
            format!("扫码登录失败（stage={other}）"),
            0,
        )),
    }
}

async fn confirm_login(
    db: &Db,
    app: &AppHandle,
    qr_id: &str,
    jar: &BTreeMap<String, String>,
    data: &Value,
) -> Result<QrPoll, AppError> {
    let _guard = QR_CONFIRM_LOCK.lock().await;
    if is_confirmed(qr_id) {
        return Ok(QrPoll {
            state: "confirmed".into(),
            message: "虎牙登录成功".into(),
            expires_in: 0,
            credential: credential_info(db, "huya"),
        });
    }
    let login_time = value_str(data, "loginTime");
    let sign = value_str(data, "sign");
    let mut jar = jar.clone();
    if login_time.is_empty() || sign.is_empty() {
        // 真机扫码常见分支：虎牙返回 uid + biztoken（快速登录票据），而不是
        // loginTime/sign。biztoken 本身即 udb_biztoken，直接落 Cookie 后
        // checkLogin 即可确认登录态；quickTicket 会返回 10052，不能再换一次。
        if let Err(message) = apply_quick_login(data, &mut jar) {
            return Ok(qr_state("error", message, 0));
        }
        let uid = value_str(data, "uid");
        tracing::info!(uid = %uid, "虎牙扫码返回快速登录票据，使用 uid+biztoken 落登录态");
        return finish_login(db, app, qr_id, jar, "quick").await;
    }
    let callback = format!(
        "{CALLBACK}&loginTime={}&sign={}&callback=cb",
        page_encode(&login_time),
        page_encode(&sign)
    );
    let (extra, _, status) = get_huya(&callback, &jar_cookie(&jar)).await?;
    if !status.is_success() {
        return Ok(qr_state("error", "虎牙登录回调失败，请刷新重试", 0));
    }
    for (name, value) in extra {
        jar.insert(name, value);
    }
    finish_login(db, app, qr_id, jar, "qr").await
}

/// 把扫码返回的快速登录票据写进设备 Cookie。
/// 真机返回形如 {"uid":1199573670219,"biztoken":"...","passport":"","version":"1.0","stage":2}。
fn apply_quick_login(data: &Value, jar: &mut BTreeMap<String, String>) -> Result<(), &'static str> {
    let uid = value_str(data, "uid");
    let biztoken = value_str(data, "biztoken");
    if uid.is_empty() || biztoken.is_empty() {
        return Err("虎牙登录响应缺少换票参数，请刷新二维码重试");
    }
    jar.insert("udb_uid".into(), uid);
    jar.insert("udb_biztoken".into(), biztoken);
    let passport = value_str(data, "passport");
    if !passport.is_empty() {
        jar.insert("udb_passport".into(), passport);
    }
    let version = value_str(data, "version");
    if !version.is_empty() {
        jar.insert("udb_version".into(), version);
    }
    Ok(())
}

async fn finish_login(
    db: &Db,
    app: &AppHandle,
    qr_id: &str,
    mut jar: BTreeMap<String, String>,
    method: &str,
) -> Result<QrPoll, AppError> {
    let mut cookie = jar_cookie(&jar);
    // 回调链有时要第二次请求才落全 Cookie；给服务端三次机会，避免刚扫码成功就报失败。
    let mut account = None;
    for attempt in 0..3 {
        match check_login(&cookie).await? {
            Check::LoggedIn(found) => {
                account = Some(found);
                break;
            }
            Check::LoggedOut => {
                if attempt < 2 {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
            }
            Check::Transient => {
                return Ok(qr_state(
                    "error",
                    "虎牙登录态校验暂时失败，请稍后刷新重试",
                    0,
                ));
            }
        }
    }
    let Some(account) = account else {
        return Ok(qr_state(
            "error",
            "虎牙登录成功，但没有拿到完整登录 Cookie，请刷新重试",
            0,
        ));
    };
    if let Some(cred) = cookie_value(&cookie, "udb_cred") {
        jar.insert("udb_cred".into(), cred);
        cookie = jar_cookie(&jar);
    }
    let row = crate::account::store_account(db, "huya", &cookie, &account)?;
    save_secret(db, &cookie, &sdid_of(&cookie));
    remember_confirmed(qr_id);
    expire_session(qr_id);
    crate::account::clear_stream_cache(app);
    crate::account::emit_account_refresh(app);
    tracing::info!(
        account = %account.name,
        uid = %account.uid,
        method,
        cookie_names = ?crate::credential::names(&cookie),
        "虎牙扫码登录成功"
    );
    Ok(QrPoll {
        state: "confirmed".into(),
        message: "虎牙登录成功".into(),
        expires_in: 0,
        credential: Some(crate::account::info_for_row(db, &row)),
    })
}

/// 手动粘贴 Cookie 时调用：只有当前 Cookie 带 udb_cred 才保存长期凭证。
pub(crate) fn remember_secret(db: &Db, cookie: &str) {
    save_secret(db, cookie, &sdid_of(cookie));
}

fn save_secret(db: &Db, cookie: &str, sdid: &str) {
    let cred = cookie_value(cookie, "udb_cred").unwrap_or_default();
    if cred.is_empty() {
        if let Err(err) = db.clear_credential_secret("huya") {
            tracing::warn!(error = %err, "清理虎牙长期凭证失败");
        }
        return;
    }
    let secret = CredentialSecretRow {
        platform: "huya".into(),
        refresh_token: cred,
        device_id: sdid.to_string(),
        updated_at: now_ts(),
    };
    if let Err(err) = db.save_credential_secret(&secret) {
        tracing::warn!(error = %err, "保存虎牙长期凭证失败");
    }
}

pub fn has_refresh_token(db: &Db) -> bool {
    let has_secret = db
        .credential_secret("huya")
        .ok()
        .flatten()
        .is_some_and(|secret| !secret.refresh_token.trim().is_empty());
    if !has_secret {
        return false;
    }
    db.credentials()
        .ok()
        .and_then(|rows| rows.into_iter().find(|row| row.platform == "huya"))
        .is_some_and(|row| cookie_value(&row.cookie, "udb_cred").is_some())
}

#[derive(Debug)]
enum Check {
    LoggedIn(Account),
    LoggedOut,
    Transient,
}

fn strip_jsonp(body: &str) -> &str {
    let text = body.trim();
    let Some(open) = text.find('(') else {
        return text;
    };
    if !text.ends_with(')') {
        return text;
    }
    text[open + 1..text.len() - 1].trim()
}

async fn check_login(cookie: &str) -> Result<Check, AppError> {
    let url = format!("{CHECK_LOGIN}&callback=cb");
    let (_, body, status) = get_huya(&url, cookie).await?;
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return Ok(Check::LoggedOut);
    }
    if !status.is_success() {
        return Ok(Check::Transient);
    }
    let value: Value = match serde_json::from_str(strip_jsonp(&body)) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(error = %err, "虎牙登录态响应解析失败");
            return Ok(Check::Transient);
        }
    };
    if !value_bool(&value, "isLogined") {
        return Ok(Check::LoggedOut);
    }
    let uid = value_str(&value, "uid");
    if uid.is_empty() || uid == "0" {
        return Ok(Check::LoggedOut);
    }
    let name = {
        let nick = value_str(&value, "userNick");
        let user = value_str(&value, "userName");
        if !nick.trim().is_empty() {
            nick
        } else if !user.trim().is_empty() {
            user
        } else {
            "虎牙用户".into()
        }
    };
    let avatar = {
        let logo = value_str(&value, "userLogo");
        (!logo.trim().is_empty()).then_some(logo)
    };
    Ok(Check::LoggedIn(Account { uid, name, avatar }))
}

#[derive(Debug)]
enum RefreshFailure {
    Invalid,
    Transient,
}

fn classify_return(value: &Value) -> Result<(), RefreshFailure> {
    match return_code(value) {
        0 => Ok(()),
        40002 | 40003 | 40004 | 40005 => Err(RefreshFailure::Invalid),
        _ => Err(RefreshFailure::Transient),
    }
}

async fn cred_login(
    cookie: &str,
    sdid: &str,
) -> Result<(BTreeMap<String, String>, Value), RefreshFailure> {
    let body = json!({
        "uri": "70017",
        "version": APP_VERSION,
        "context": "1",
        "appId": APP_ID,
        "appSign": APP_SIGN,
        "lcid": "2052",
        "sdid": sdid,
        "data": { "page": page_encode(MAIN_REFERER) },
    });
    let (jar, value) = post_huya(&format!("{LOGIN_API}/credLogin"), &body, cookie)
        .await
        .map_err(|_| RefreshFailure::Transient)?;
    classify_return(&value)?;
    Ok((jar, value))
}

async fn cookie_exchange(
    cookie: &str,
    sdid: &str,
) -> Result<(BTreeMap<String, String>, Value, bool), RefreshFailure> {
    let body = json!({
        "uri": "70015",
        "version": APP_VERSION,
        "context": "1",
        "appId": APP_ID,
        "appSign": APP_SIGN,
        "lcid": "2052",
        "traceid": uuid::Uuid::new_v4().to_string(),
        "sdid": sdid,
        "data": { "page": page_encode(MAIN_REFERER) },
    });
    let (jar, value) = post_huya(&format!("{LOGIN_API}/cookieExchange"), &body, cookie)
        .await
        .map_err(|_| RefreshFailure::Transient)?;
    classify_return(&value)?;
    let write_flag = value_bool(data_of(&value), "writeCookieFlag");
    Ok((jar, value, write_flag))
}

/// 手动粘贴的 Cookie：有 udb_cred 时先换票补 udb_uid/udb_biztoken，再检查登录态。
pub(crate) async fn prepare_cookie(cookie: &str) -> Result<(Account, String), AppError> {
    let mut jar = parse_cookie(cookie);
    let mut current = cookie.to_string();
    if !has_login_cookie(&current) && cookie_value(&current, "udb_cred").is_some() {
        let sdid = sdid_of(&current);
        match cred_login(&current, &sdid).await {
            Ok((extra, _)) => {
                for (name, value) in extra {
                    jar.insert(name, value);
                }
                current = jar_cookie(&jar);
            }
            Err(RefreshFailure::Invalid) => {
                return Err(user("虎牙 Cookie 已失效或未登录"));
            }
            Err(RefreshFailure::Transient) => {
                return Err(upstream("虎牙接口暂时不可用，无法校验 Cookie"));
            }
        }
    }
    match check_login(&current).await? {
        Check::LoggedIn(account) => Ok((account, current)),
        Check::LoggedOut => Err(user("虎牙 Cookie 已失效或未登录")),
        Check::Transient => Err(upstream("虎牙接口暂时不可用，无法校验 Cookie")),
    }
}

fn next_refresh_at(status: &RefreshStatus, updated_at: i64) -> i64 {
    match status.outcome.as_str() {
        "refreshed" | "fresh" => status.checked_at.max(updated_at) + EXCHANGE_EVERY,
        "failed" | "not_refreshable" => {
            status.checked_at
                + if status.key_stale {
                    RETRY_RESCAN
                } else {
                    RETRY_TRANSIENT
                }
        }
        _ => updated_at.max(status.checked_at) + EXCHANGE_EVERY,
    }
}

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

fn write_status(db: &Db, status: &RefreshStatus) {
    crate::account::save_refresh_status(db, "huya", status);
}

fn cookie_is_current(row: &CredentialRow) -> bool {
    crate::credential::get("huya").as_deref() == Some(row.cookie.as_str())
}

fn notify_expired(app: &AppHandle, account_name: &str) {
    let notice = AppNotice {
        kind: "credential_expired".into(),
        room_id: String::new(),
        host_name: account_name.to_string(),
        detail: Some("huya".into()),
        suppress: false,
    };
    if let Err(err) = app.emit(crate::recorder::EVENT_APP_NOTICE, notice) {
        tracing::warn!(error = %err, "虎牙登录失效通知发送失败");
    }
}

fn mark_expired_locked(db: &Db, app: &AppHandle, row: &CredentialRow) -> bool {
    if row.status == "expired" {
        return false;
    }
    if !cookie_is_current(row) {
        tracing::info!("标记过期前发现虎牙 Cookie 已更新，跳过");
        return false;
    }
    match db.update_credential_profile(
        "huya",
        "expired",
        &row.account_name,
        row.avatar.as_deref(),
        now_ts(),
    ) {
        Ok(true) => {
            crate::credential::set_status("huya", "expired");
            notify_expired(app, &row.account_name);
            true
        }
        Ok(false) => false,
        Err(err) => {
            tracing::warn!(error = %err, "标记虎牙账号过期失败");
            false
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum RefreshOutcome {
    NoCredential,
    NotRefreshable,
    Fresh,
    Refreshed,
    Invalid,
    Transient,
    Discarded,
}

async fn refresh_once(db: &Db, app: &AppHandle, force: bool) -> RefreshOutcome {
    let Some(row) = db
        .credentials()
        .ok()
        .and_then(|rows| rows.into_iter().find(|row| row.platform == "huya"))
    else {
        return RefreshOutcome::NoCredential;
    };
    let Some(secret) = db.credential_secret("huya").ok().flatten() else {
        return RefreshOutcome::NotRefreshable;
    };
    if secret.refresh_token.trim().is_empty() {
        return RefreshOutcome::NotRefreshable;
    }
    let before = crate::account::load_refresh_status(db, "huya");
    if !refresh_due(
        force,
        row.status == "expired",
        &before,
        row.updated_at,
        now_ts(),
    ) {
        return RefreshOutcome::Fresh;
    }

    let _guard = REFRESH_LOCK.lock().await;
    // 等锁期间用户可能重新扫码/删除，重新读凭据与长期凭证。
    let Some(row) = db
        .credentials()
        .ok()
        .and_then(|rows| rows.into_iter().find(|row| row.platform == "huya"))
    else {
        return RefreshOutcome::NoCredential;
    };
    let Some(secret) = db.credential_secret("huya").ok().flatten() else {
        return RefreshOutcome::NotRefreshable;
    };
    let udb_cred = secret.refresh_token;
    if udb_cred.trim().is_empty() {
        return RefreshOutcome::NotRefreshable;
    }
    let sdid = if secret.device_id.trim().is_empty() {
        sdid_of(&row.cookie)
    } else {
        secret.device_id.clone()
    };

    let mut jar = parse_cookie(&row.cookie);
    let mut merged = row.cookie.clone();
    let mut rotated = false;
    let mut exchange_ok = false;

    match cookie_exchange(&row.cookie, &sdid).await {
        Ok((extra, value, write_flag)) => {
            exchange_ok = true;
            let changed = {
                let before_len = jar.len();
                for (name, value) in &extra {
                    jar.insert(name.clone(), value.clone());
                }
                jar.len() != before_len
                    || extra.iter().any(|(name, value)| {
                        row.cookie
                            .split(';')
                            .filter_map(|part| part.trim().split_once('='))
                            .any(|(k, v)| k == name && v != value)
                    })
            };
            rotated = write_flag && changed;
            merged = jar_cookie(&jar);
            tracing::debug!(
                write_flag,
                changed,
                cookies = extra.len(),
                return_code = return_code(&value),
                "虎牙 cookieExchange 完成"
            );
        }
        Err(RefreshFailure::Invalid) => {
            tracing::info!("虎牙 cookieExchange 票据失效，尝试 credLogin");
        }
        Err(RefreshFailure::Transient) => {
            tracing::warn!("虎牙 cookieExchange 临时失败，尝试 credLogin");
        }
    }

    // exchange 即使返回 0 也可能只是「没登录」；只有 checkLogin 才算数。
    let mut account = None;
    match check_login(&merged).await {
        Ok(Check::LoggedIn(found)) => account = Some(found),
        Ok(Check::LoggedOut) => {}
        Ok(Check::Transient) => {
            return record_transient(db, &row, &before, "虎牙登录态校验暂时失败，稍后自动重试")
                .await;
        }
        Err(_) => {
            return record_transient(db, &row, &before, "虎牙登录态校验暂时失败，稍后自动重试")
                .await;
        }
    }

    // cookieExchange 可能轮换掉 udb_cred 本身；长期凭证仍单独保存在密钥表里，
    // credLogin 需要它。只在当前 Cookie 缺失时补回，绝不用旧设备的凭证覆盖新账号。
    if cookie_value(&merged, "udb_cred").is_none() {
        jar.insert("udb_cred".into(), udb_cred.clone());
        merged = jar_cookie(&jar);
    }

    if account.is_none() && (cookie_value(&merged, "udb_cred").is_some() || exchange_ok) {
        match cred_login(&merged, &sdid).await {
            Ok((extra, _)) => {
                for (name, value) in extra {
                    jar.insert(name, value);
                }
                merged = jar_cookie(&jar);
                match check_login(&merged).await {
                    Ok(Check::LoggedIn(found)) => account = Some(found),
                    Ok(Check::LoggedOut) => {}
                    Ok(Check::Transient) | Err(_) => {
                        return record_transient(
                            db,
                            &row,
                            &before,
                            "虎牙登录态校验暂时失败，稍后自动重试",
                        )
                        .await;
                    }
                }
            }
            Err(RefreshFailure::Invalid) => {}
            Err(RefreshFailure::Transient) => {
                return record_transient(db, &row, &before, "虎牙续期临时失败，稍后自动重试").await;
            }
        }
    }

    let Some(account) = account else {
        let guard = crate::account::credential_write_guard();
        if !cookie_is_current(&row) {
            drop(guard);
            tracing::info!("虎牙续期失败期间 Cookie 已更新，丢弃本次结果");
            return RefreshOutcome::Discarded;
        }
        let status = RefreshStatus {
            checked_at: now_ts(),
            outcome: "failed".into(),
            message: Some("虎牙长期凭证已失效，请重新扫码登录".into()),
            key_stale: true,
            last_refreshed_at: before.last_refreshed_at,
        };
        write_status(db, &status);
        mark_expired_locked(db, app, &row);
        drop(guard);
        return RefreshOutcome::Invalid;
    };

    let changed = merged != row.cookie;
    let was_expired = row.status == "expired";
    {
        let _guard = crate::account::credential_write_guard();
        if !cookie_is_current(&row) {
            tracing::info!("虎牙续期期间 Cookie 已更新，丢弃本轮结果");
            return RefreshOutcome::Discarded;
        }
        let updated = CredentialRow {
            cookie: merged.clone(),
            status: "ok".into(),
            account_name: account.name.clone(),
            avatar: account.avatar.clone(),
            updated_at: now_ts(),
            ..row.clone()
        };
        if let Err(err) = db.save_credential(&updated) {
            tracing::warn!(error = %err, "保存虎牙续期 Cookie 失败");
            return RefreshOutcome::Transient;
        }
        crate::credential::set("huya", &merged);
        crate::credential::set_status("huya", "ok");
        // 长期凭证与 Cookie 必须在同一把写锁内落库，避免扫码登录刚好插进来后
        // 本次旧结果把新账号的 udb_cred 覆盖掉。
        save_secret(db, &merged, &sdid);
    }
    let status = RefreshStatus {
        checked_at: now_ts(),
        outcome: if changed || rotated {
            "refreshed"
        } else {
            "fresh"
        }
        .into(),
        message: Some(if changed || rotated {
            "已同步虎牙登录态".into()
        } else {
            "虎牙登录态有效".into()
        }),
        key_stale: false,
        last_refreshed_at: if changed || rotated {
            now_ts()
        } else {
            before.last_refreshed_at
        },
    };
    write_status(db, &status);
    crate::account::clear_stream_cache(app);
    crate::account::emit_account_refresh(app);
    tracing::info!(
        changed,
        rotated,
        udb_cred_len = udb_cred.len(),
        "虎牙 Cookie 自动续期完成"
    );
    if was_expired {
        if let Some(state) = app.try_state::<crate::state::AppState>() {
            state
                .recorder
                .recheck_all_quality(app, state.broker.clone(), "huya_refreshed");
        }
    }
    if changed || rotated {
        RefreshOutcome::Refreshed
    } else {
        RefreshOutcome::Fresh
    }
}

async fn record_transient(
    db: &Db,
    row: &CredentialRow,
    before: &RefreshStatus,
    message: &str,
) -> RefreshOutcome {
    let _guard = crate::account::credential_write_guard();
    if !cookie_is_current(row) {
        tracing::info!("虎牙续期失败期间 Cookie 已更新，丢弃本次结果");
        return RefreshOutcome::Discarded;
    }
    let status = RefreshStatus {
        checked_at: now_ts(),
        outcome: "failed".into(),
        message: Some(message.into()),
        key_stale: false,
        last_refreshed_at: before.last_refreshed_at,
    };
    write_status(db, &status);
    RefreshOutcome::Transient
}

/// 设置页「立即检查」：强制走一遍续期链路并返回最新状态。
pub async fn refresh_now(db: &Db, app: &AppHandle) -> Result<RefreshStatus, AppError> {
    let outcome = refresh_once(db, app, true).await;
    match outcome {
        RefreshOutcome::NoCredential => Err(user("尚未登录虎牙")),
        RefreshOutcome::NotRefreshable => {
            let status = RefreshStatus {
                checked_at: now_ts(),
                outcome: "not_refreshable".into(),
                message: Some("虎牙 Cookie 没有长期凭证，请扫码登录后启用自动续期".into()),
                key_stale: false,
                last_refreshed_at: 0,
            };
            write_status(db, &status);
            Ok(status)
        }
        _ => Ok(crate::account::load_refresh_status(db, "huya")),
    }
}

/// 每 20 分钟轮换一次；失败按 6 小时/24 小时退避，只在到期时才发请求。
pub async fn maintenance_loop(db: std::sync::Arc<Db>, app: AppHandle) {
    loop {
        let outcome = refresh_once(&db, &app, false).await;
        if outcome == RefreshOutcome::NoCredential || outcome == RefreshOutcome::NotRefreshable {
            tokio::time::sleep(Duration::from_secs(30 * 60)).await;
        } else {
            tokio::time::sleep(Duration::from_secs(EXCHANGE_EVERY as u64)).await;
        }
    }
}

/// 每 6 小时独立复验一次。cookieExchange 失败不一定代表登录失效，
/// 所以用 checkLogin 单独判断，避免把网络问题写成「需要重新登录」。
pub async fn revalidate(db: &Db, app: &AppHandle) {
    let Some(row) = db
        .credentials()
        .ok()
        .and_then(|rows| rows.into_iter().find(|row| row.platform == "huya"))
    else {
        return;
    };
    if !has_login_cookie(&row.cookie) {
        return;
    }
    let now = now_ts();
    let last = REVALIDATED_AT.load(Ordering::Relaxed);
    if last > 0 && now - last < REVALIDATE_EVERY {
        return;
    }
    REVALIDATED_AT.store(now, Ordering::Relaxed);
    match check_login(&row.cookie).await {
        Ok(Check::LoggedIn(account)) => {
            let guard = crate::account::credential_write_guard();
            if !cookie_is_current(&row) {
                drop(guard);
                return;
            }
            let recovered = row.status == "expired";
            if recovered {
                let _ = db.update_credential_profile(
                    "huya",
                    "ok",
                    &account.name,
                    account.avatar.as_deref(),
                    now_ts(),
                );
                crate::credential::set_status("huya", "ok");
            }
            let mut status = crate::account::load_refresh_status(db, "huya");
            status.checked_at = now_ts();
            if status.outcome == "never" || status.outcome == "failed" {
                status.outcome = "fresh".into();
                status.message = Some("虎牙登录态有效".into());
                status.key_stale = false;
            }
            crate::account::save_refresh_status(db, "huya", &status);
            drop(guard);
            if recovered {
                crate::account::emit_account_refresh(app);
                crate::account::clear_stream_cache(app);
                if let Some(state) = app.try_state::<crate::state::AppState>() {
                    state.recorder.recheck_all_quality(
                        app,
                        state.broker.clone(),
                        "huya_revalidated",
                    );
                }
            }
        }
        Ok(Check::LoggedOut) => {
            let guard = crate::account::credential_write_guard();
            if !cookie_is_current(&row) {
                drop(guard);
                return;
            }
            let status = RefreshStatus {
                checked_at: now_ts(),
                outcome: "failed".into(),
                message: Some("虎牙登录态已失效，请重新扫码登录".into()),
                key_stale: true,
                last_refreshed_at: 0,
            };
            write_status(db, &status);
            mark_expired_locked(db, app, &row);
            drop(guard);
        }
        Ok(Check::Transient) | Err(_) => {}
    }
}

/// 登录确认后给命令层用：把刚写入的账号信息转成界面条目。
fn credential_info(db: &Db, platform: &str) -> Option<hivecast_ipc::CredentialInfo> {
    let row = db
        .credentials()
        .ok()?
        .into_iter()
        .find(|row| row.platform == platform)?;
    Some(crate::account::info_for_row(db, &row))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_jar_has_required_binding_cookies() {
        let jar = new_device_jar();
        for name in [
            "udb_guiddata",
            "__yamid_new",
            "guid",
            "sdid",
            "PHPSESSID",
            "udb_deviceid",
            "udb_appid",
            "udb_passdata",
        ] {
            assert!(
                jar.get(name).is_some_and(|value| !value.is_empty()),
                "{name}"
            );
        }
        assert!(jar["udb_guiddata"].len() == 32);
        assert!(jar["__yamid_new"].len() == 32);
        assert!(jar["sdid"].starts_with("csid_"));
        assert!(jar["udb_deviceid"].starts_with("w_"));
        assert!(jar["udb_deviceid"].len() == 21);
    }

    #[test]
    fn cookie_merge_keeps_unrelated_values_and_drops_deletions() {
        let base = "udb_uid=1; udb_biztoken=old; foo=bar";
        let mut jar = BTreeMap::new();
        jar.insert("udb_biztoken".into(), "new".into());
        jar.insert("PHPSESSID".into(), "x".into());
        let merged = merge_cookie(base, &jar);
        assert!(merged.contains("udb_uid=1"));
        assert!(merged.contains("udb_biztoken=new"));
        assert!(merged.contains("foo=bar"));
        assert!(merged.contains("PHPSESSID=x"));
    }

    #[test]
    fn parse_cookie_ignores_malformed_parts() {
        let jar = parse_cookie("a=1; broken; b = 2 ; =x; c=");
        assert_eq!(jar.get("a").map(String::as_str), Some("1"));
        assert_eq!(jar.get("b").map(String::as_str), Some("2"));
        assert_eq!(jar.get("c").map(String::as_str), Some(""));
        assert!(!jar.contains_key(""));
    }

    #[test]
    fn jsonp_body_is_unwrapped() {
        assert_eq!(
            strip_jsonp(r#"cb({"isLogined":false})"#),
            r#"{"isLogined":false}"#
        );
        assert_eq!(
            strip_jsonp(r#"{"isLogined":true}"#),
            r#"{"isLogined":true}"#
        );
    }

    #[test]
    fn cookie_headers_absorb_set_cookie_and_deletion() {
        let mut headers = HeaderMap::new();
        headers.append(
            SET_COOKIE,
            "udb_uid=42; Path=/; Domain=huya.com".parse().unwrap(),
        );
        headers.append(SET_COOKIE, "udb_cred=; Max-Age=0; Path=/".parse().unwrap());
        let mut jar = BTreeMap::new();
        jar.insert("udb_cred".into(), "old".into());
        assert!(absorb_cookies(&headers, &mut jar));
        assert_eq!(jar.get("udb_uid").map(String::as_str), Some("42"));
        assert!(!jar.contains_key("udb_cred"));
    }

    #[test]
    fn refresh_due_uses_exchange_window_and_backoff() {
        let now = 1_000_000;
        let mut status = RefreshStatus {
            checked_at: now - 60,
            outcome: "fresh".into(),
            ..RefreshStatus::default()
        };
        assert!(!refresh_due(false, false, &status, 0, now));
        status.checked_at = now - EXCHANGE_EVERY - 1;
        assert!(refresh_due(false, false, &status, 0, now));
        status.outcome = "failed".into();
        status.key_stale = false;
        status.checked_at = now - 60;
        assert!(!refresh_due(false, false, &status, 0, now));
        status.checked_at = now - RETRY_TRANSIENT - 1;
        assert!(refresh_due(false, false, &status, 0, now));
        status.key_stale = true;
        status.checked_at = now - RETRY_TRANSIENT - 1;
        assert!(!refresh_due(false, false, &status, 0, now));
        assert!(refresh_due(true, false, &status, 0, now));
    }

    #[test]
    fn login_cookie_requires_uid_and_biztoken() {
        assert!(has_login_cookie("udb_uid=1; udb_biztoken=t"));
        assert!(!has_login_cookie("udb_uid=1"));
        assert!(!has_login_cookie("udb_biztoken=t"));
    }

    #[test]
    fn quick_login_ticket_fills_device_cookie() {
        let mut jar = BTreeMap::new();
        apply_quick_login(
            &json!({
                "uid": 1199573670219_i64,
                "biztoken": "token-value",
                "passport": "passport-value",
                "version": "1.0",
                "stage": 2
            }),
            &mut jar,
        )
        .expect("快速登录票据应可用");
        assert_eq!(
            jar.get("udb_uid").map(String::as_str),
            Some("1199573670219")
        );
        assert_eq!(
            jar.get("udb_biztoken").map(String::as_str),
            Some("token-value")
        );
        assert_eq!(
            jar.get("udb_passport").map(String::as_str),
            Some("passport-value")
        );
        assert_eq!(jar.get("udb_version").map(String::as_str), Some("1.0"));
    }

    #[test]
    fn quick_login_ticket_requires_uid_and_token() {
        let mut jar = BTreeMap::new();
        assert!(apply_quick_login(&json!({ "uid": 1 }), &mut jar).is_err());
        assert!(apply_quick_login(&json!({ "biztoken": "t" }), &mut jar).is_err());
        assert!(!jar.contains_key("udb_uid"));
        assert!(!jar.contains_key("udb_biztoken"));
    }

    #[tokio::test]
    #[ignore = "访问虎牙真实接口，手动运行"]
    async fn live_qr_start_returns_png_svg() {
        let ticket = qr_start().await.expect("生成虎牙二维码");
        assert!(!ticket.qrcode_key.is_empty());
        assert!(ticket.svg.contains("data:image/png;base64,"));
        assert!(ticket.svg.starts_with("<svg"));
        assert_eq!(ticket.expires_in, QR_TTL);
    }

    #[test]
    fn callback_url_encodes_ticket() {
        let encoded = page_encode("abc+/=");
        assert!(!encoded.contains('+'));
        assert!(!encoded.contains('/'));
        assert!(!encoded.contains('='));
    }
}
