//! 平台账号：B 站扫码登录、Cookie 校验与落库。
//!
//! 扫码流程（对齐 passport 的 web 端）：
//! 1. generate 拿 qrcode_key + 二维码内容；
//! 2. 轮询 poll，data.code 为 0 时登录成功；登录态可能在 Set-Cookie，也可能在 data.url 的查询串里；
//! 3. 两条路径都取、合并并校验 SESSDATA/bili_jct/DedeUserID，再打一次 nav 确认身份，最后落库；
//! 4. 扫码返回的 refresh_token 以 ac_time_value 并入 Cookie，之后由 refresh_loop 自动续期。
//!
//! 斗鱼有独立的扫码/续期模块；抖音、虎牙走粘贴 Cookie（抖音会校验并定期复验）。

use std::time::Duration;

use hivecast_ipc::{AppNotice, CredentialInfo, QrPoll, QrTicket, RefreshStatus};
use rsa::pkcs8::DecodePublicKey;
use rsa::{Oaep, RsaPublicKey};
use serde::Deserialize;
use sha2::Sha256;
use tauri::{Emitter, Manager};

use crate::credential;
use crate::db::{now_ts, CredentialRefreshRow, CredentialRow, Db};
use crate::error::AppError;
use crate::platform::{http, PlatformError};

const PASSPORT: &str = "https://passport.bilibili.com";
const API: &str = "https://api.bilibili.com";
/// 运行中定期复验登录态：B 站 Cookie 可能在长时间录制中失效，只在启动时查一次不够。
pub const REVALIDATE_INTERVAL: Duration = Duration::from_secs(30 * 60);
/// 续期检查周期。真正是否轮换由 cookie/info 的 data.refresh 决定，这里只控制问的频率。
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(12 * 60 * 60);
const REFERER: &str = "https://www.bilibili.com/";
/// B 站 Web 前端的 Cookie 刷新公钥（1024 位，PKCS#1 OAEP + SHA-256）。
const REFRESH_PUBLIC_KEY: &str = "-----BEGIN PUBLIC KEY-----
MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDLgd2OAkcGVtoE3ThUREbio0Eg
Uc/prcajMKXvkCKFCWhJYJcLkcM2DKKcSeFpD/j6Boy538YXnR6VhcuUJOhH2x71
nzPjfdTcqMz7djHum0qSZA0AyCBDABUqCrfNgCiJ00Ra7GmRj+YCK1NJEuewlb40
JNrRuoEUXpabUzGB8QIDAQAB
-----END PUBLIC KEY-----";
/// 定时与手动续期共用一把锁：两个请求同时轮换会让 token 互相作废。
static REFRESH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
/// 登录/换 Cookie 后唤醒后台续期循环，不必等下一个 12 小时窗口。
static REFRESH_WAKE: tokio::sync::Notify = tokio::sync::Notify::const_new();
/// 凭据的「比较 + 写回」必须原子：续期、重新扫码、手动保存、删除都可能并发。
static CREDENTIAL_WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());
/// 账号凭据或续期状态发生变化时通知前端重新拉取，替代靠猜时间的 setTimeout。
pub const EVENT_ACCOUNT_REFRESH: &str = "account://refresh";

pub(crate) fn credential_write_guard() -> std::sync::MutexGuard<'static, ()> {
    CREDENTIAL_WRITE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Debug, Deserialize)]
struct Envelope<T> {
    code: i64,
    #[serde(default)]
    message: Option<String>,
    data: Option<T>,
}

#[derive(Debug, Deserialize)]
struct QrData {
    url: String,
    qrcode_key: String,
}

#[derive(Debug, Deserialize)]
struct PollData {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    message: String,
    /// 距离二维码失效还剩多少秒；接口不保证返回，缺省按 0（本地以 QR_TTL_SECONDS 为准）。
    #[serde(default)]
    expires_in: i64,
    /// 确认态才有：登录态也可能只放在这个跨域 URL 的查询串里。
    #[serde(default)]
    url: String,
    /// 后续自动续期用的 refresh_token，等价于 Web Cookie 的 ac_time_value。
    #[serde(default)]
    refresh_token: String,
    /// 服务端时间戳，仅用于日志诊断。
    #[serde(default)]
    timestamp: i64,
}

#[derive(Debug, Deserialize)]
struct CookieInfoData {
    #[serde(default)]
    refresh: bool,
}

#[derive(Debug, Deserialize)]
struct RefreshData {
    #[serde(default)]
    refresh_token: String,
}

#[derive(Debug, Deserialize)]
struct NavData {
    #[serde(default, rename = "isLogin")]
    is_login: bool,
    #[serde(default)]
    uname: String,
    #[serde(default)]
    mid: i64,
    #[serde(default)]
    face: String,
}

/// 账号身份（校验通过后用来填展示信息）。
#[derive(Debug, Clone, Default)]
pub struct Account {
    pub uid: String,
    pub name: String,
    pub avatar: Option<String>,
}

pub fn info_from_row(row: &CredentialRow) -> CredentialInfo {
    let refreshable = row.platform == "bilibili" && credential::has_refresh_token(&row.cookie);
    info_with_refresh(row, refreshable, RefreshStatus::default())
}

/// 读库后组装界面条目：续期能力与最近一次检查结果都按平台取。
pub(crate) fn info_for_row(db: &Db, row: &CredentialRow) -> CredentialInfo {
    let refreshable = match row.platform.as_str() {
        "bilibili" => credential::has_refresh_token(&row.cookie),
        "douyu" => crate::douyu_auth::has_refresh_token(db),
        "douyin" => crate::douyin_auth::has_session_cookie(&row.cookie),
        _ => false,
    };
    info_with_refresh(row, refreshable, load_refresh_status(db, &row.platform))
}

fn info_with_refresh(
    row: &CredentialRow,
    refreshable: bool,
    refresh_status: RefreshStatus,
) -> CredentialInfo {
    CredentialInfo {
        platform: row.platform.clone(),
        account_id: row.account_id.clone(),
        account_name: row.account_name.clone(),
        uid: row.uid.clone(),
        avatar: row.avatar.clone(),
        cookie_names: credential::names(&row.cookie),
        status: row.status.clone(),
        refreshable,
        refresh_status,
        updated_at: row.updated_at,
    }
}

fn refresh_status_from_row(row: CredentialRefreshRow) -> RefreshStatus {
    RefreshStatus {
        checked_at: row.checked_at,
        outcome: row.outcome,
        message: row.message,
        key_stale: row.key_stale,
        last_refreshed_at: row.last_refreshed_at,
    }
}

/// 读取某平台最近一次续期结果；没有记录时返回 never。
pub(crate) fn load_refresh_status(db: &Db, platform: &str) -> RefreshStatus {
    match db.credential_refresh(platform) {
        Ok(Some(row)) => refresh_status_from_row(row),
        Ok(None) => RefreshStatus::default(),
        Err(err) => {
            tracing::warn!(platform, error = %err, "读取续期状态失败，按未检查处理");
            RefreshStatus::default()
        }
    }
}

pub(crate) fn save_refresh_status(db: &Db, platform: &str, status: &RefreshStatus) {
    let row = CredentialRefreshRow {
        platform: platform.to_string(),
        checked_at: status.checked_at,
        outcome: status.outcome.clone(),
        message: status.message.clone(),
        key_stale: status.key_stale,
        last_refreshed_at: status.last_refreshed_at,
    };
    if let Err(err) = db.save_credential_refresh(&row) {
        tracing::warn!(platform, error = %err, "写入续期状态失败");
    }
}

pub(crate) fn short_error(message: &str) -> String {
    let mut out: String = message.chars().take(180).collect();
    if message.chars().count() > 180 {
        out.push('…');
    }
    out.replace('\n', " ")
}

/// 请求后台循环立即检查一次（登录、换 Cookie 之后调用）。
fn wake_refresh_loop(reason: &'static str) {
    tracing::info!(platform = "bilibili", reason, "已请求立即检查 Cookie 续期");
    REFRESH_WAKE.notify_one();
}

fn correspond_key_stale(err: &PlatformError) -> bool {
    match err {
        // correspondPath 被拒绝时 B 站会返回 404/412；其他网络错误仍按普通失败处理。
        PlatformError::Network(message) => {
            message.contains("HTTP 404") || message.contains("HTTP 412")
        }
        _ => false,
    }
}

fn platform_name(platform: &str) -> &'static str {
    match platform {
        "bilibili" => "哔哩哔哩",
        "douyu" => "斗鱼",
        "huya" => "虎牙",
        "douyin" => "抖音",
        _ => "该平台",
    }
}

/// B 站二维码固定 180 秒有效；生成接口不回这个值，所以按官方值兜底。
pub const QR_TTL_SECONDS: i64 = 180;

/// 开始扫码：返回二维码内容、key 与后端渲染好的 SVG。
pub async fn bilibili_qr_start() -> Result<QrTicket, AppError> {
    tracing::info!(platform = "bilibili", "开始获取登录二维码");
    let url = format!("{PASSPORT}/x/passport-login/web/qrcode/generate?source=main-fe-header");
    let envelope: Envelope<QrData> = http::get_json(&url, REFERER, &[])
        .await
        .map_err(AppError::from)?;
    let data = match (envelope.code, envelope.data) {
        (0, Some(data)) => data,
        (code, _) => {
            tracing::warn!(platform = "bilibili", code, "获取登录二维码失败");
            return Err(AppError::User(format!(
                "生成二维码失败（code {code}）{}",
                envelope.message.unwrap_or_default()
            )));
        }
    };
    let svg = render_qr(&data.url)?;
    tracing::info!(
        platform = "bilibili",
        expires_in = QR_TTL_SECONDS,
        "登录二维码已生成"
    );
    Ok(QrTicket {
        qrcode_key: data.qrcode_key,
        url: data.url,
        svg,
        expires_in: QR_TTL_SECONDS,
    })
}

pub(crate) fn render_qr(content: &str) -> Result<String, AppError> {
    use qrcode::render::svg;
    let code = qrcode::QrCode::new(content.as_bytes())
        .map_err(|err| AppError::Internal(format!("二维码编码失败：{err}")))?;
    Ok(code
        .render::<svg::Color>()
        .min_dimensions(232, 232)
        .quiet_zone(true)
        .dark_color(svg::Color("#111114"))
        .light_color(svg::Color("#ffffff"))
        .build())
}

/// 轮询地址必须编码 key：它来自服务端，但命令边界不能假设内容安全。
fn qr_poll_url(key: &str) -> Result<String, AppError> {
    if key.len() > 256 {
        tracing::warn!(platform = "bilibili", len = key.len(), "qrcode_key 过长");
        return Err(AppError::User("qrcode_key 不合法".into()));
    }
    let mut url = reqwest::Url::parse(&format!("{PASSPORT}/x/passport-login/web/qrcode/poll"))
        .map_err(|err| AppError::Internal(format!("生成扫码轮询地址失败：{err}")))?;
    url.query_pairs_mut()
        .append_pair("qrcode_key", key)
        .append_pair("source", "main-fe-header");
    Ok(url.to_string())
}

fn qr_error(message: impl Into<String>) -> QrPoll {
    QrPoll {
        state: "error".into(),
        message: message.into(),
        expires_in: 0,
        credential: None,
    }
}

/// 轮询扫码状态；已确认时顺带把登录态落库。
pub async fn bilibili_qr_poll(db: &Db, qrcode_key: &str) -> Result<QrPoll, AppError> {
    let key = qrcode_key.trim();
    if key.is_empty() {
        tracing::warn!(platform = "bilibili", "扫码轮询缺少 qrcode_key");
        return Err(AppError::User("缺少 qrcode_key".into()));
    }
    let url = qr_poll_url(key)?;
    let fetched: http::FetchedWithHeaders<Envelope<PollData>> =
        http::get_json_with_headers(&url, REFERER, &[])
            .await
            .map_err(AppError::from)?;

    let expires_in = fetched
        .data
        .data
        .as_ref()
        .map(|data| data.expires_in.max(0))
        .unwrap_or(0);

    let (state, message) = match (fetched.data.code, fetched.data.data.as_ref()) {
        (0, Some(data)) if data.code == 0 => ("confirmed", "登录成功".to_string()),
        (0, Some(data)) => match data.code {
            86101 => ("pending", "等待扫码".to_string()),
            86090 => ("scanned", "已扫码，请在手机上确认".to_string()),
            86038 => ("expired", "二维码已失效，请刷新".to_string()),
            other => ("pending", format!("{}（{other}）", data.message)),
        },
        (code, _) => (
            "error",
            format!(
                "轮询失败（code {code}）{}",
                fetched.data.message.unwrap_or_default()
            ),
        ),
    };

    if state == "expired" || state == "error" {
        tracing::warn!(platform = "bilibili", state, message = %message, "扫码状态异常");
    }
    if state == "confirmed" {
        tracing::info!(platform = "bilibili", "扫码已确认，开始校验登录态");
        let poll_data = fetched
            .data
            .data
            .as_ref()
            .ok_or_else(|| AppError::User("登录响应缺少 data".into()))?;
        // 两个载体都取：实测不同客户端/风控下 Set-Cookie 可能不完整，而 data.url 里一定带登录态。
        let from_header = cookies_from_set_cookie(&fetched.set_cookies);
        let from_url = cookies_from_poll_url(&poll_data.url);
        let landing = credential::merge(&from_header, &from_url);
        // 关键：先校验本次登录自己带来的 Cookie，再与旧账号合并。
        // 否则换号时旧号的 bili_jct/DedeUserID 会把缺失字段补齐，拼出混号 Cookie。
        if !has_login_cookies(&landing) {
            tracing::warn!(
                platform = "bilibili",
                has_header = !from_header.is_empty(),
                has_url = !from_url.is_empty(),
                "扫码成功但本次登录 Cookie 不完整"
            );
            return Ok(qr_error(
                "登录信息不完整（缺少 SESSDATA/bili_jct/DedeUserID），请刷新二维码",
            ));
        }
        let mut base = credential::get("bilibili").unwrap_or_default();
        if poll_data.refresh_token.is_empty() {
            // 重新扫码换号时，旧号的 ac_time_value 不能留给新号。
            base = credential::without(&base, "ac_time_value");
        }
        let mut merged = credential::merge(&base, &landing);
        if !poll_data.refresh_token.is_empty() {
            merged = with_refresh_token(&merged, &poll_data.refresh_token);
        }
        tracing::info!(
            platform = "bilibili",
            from_header = !from_header.is_empty(),
            from_url = !from_url.is_empty(),
            refreshable = !poll_data.refresh_token.is_empty(),
            timestamp = poll_data.timestamp,
            "扫码确认，登录态来源已合并"
        );
        let account = match bilibili_nav(&merged).await {
            Ok(account) => account,
            Err(err) => {
                tracing::warn!(platform = "bilibili", error = %err, "扫码成功但账号校验失败");
                return Ok(qr_error(format!("登录校验失败：{err}")));
            }
        };
        let row = match store_account(db, "bilibili", &merged, &account) {
            Ok(row) => row,
            Err(err) => {
                tracing::warn!(platform = "bilibili", error = %err, "扫码成功但保存账号失败");
                return Ok(qr_error(format!("保存登录状态失败：{err}")));
            }
        };
        wake_refresh_loop("扫码登录");
        tracing::info!(platform = "bilibili", account = %row.account_name, uid = %row.uid, "扫码登录成功并已保存");
        return Ok(QrPoll {
            state: state.to_string(),
            message: format!("已登录：{}", row.account_name),
            expires_in: 0,
            credential: Some(info_from_row(&row)),
        });
    }

    Ok(QrPoll {
        state: state.to_string(),
        message,
        expires_in,
        credential: None,
    })
}

/// Set-Cookie 只取 name=value 部分；属性（Path/Expires）对请求头没用。
fn cookies_from_set_cookie(headers: &[String]) -> String {
    let pairs: Vec<String> = headers
        .iter()
        .filter_map(|raw| {
            let first = raw.split(';').next()?.trim();
            let (name, value) = first.split_once('=')?;
            let name = name.trim();
            if name.is_empty()
                || name.eq_ignore_ascii_case("path")
                || name.eq_ignore_ascii_case("expires")
            {
                return None;
            }
            Some(format!("{name}={}", value.trim()))
        })
        .collect();
    pairs.join("; ")
}

/// 从 poll 的 data.url 查询串里取登录态。
///
/// 不做 URL 解码：SESSDATA 里的 %2C 是服务端下发的原值，解码后反而会和
/// Set-Cookie 里的值不一致。只认 B 站登录会用到的几个字段，避免把无关参数写进 Cookie。
fn cookies_from_poll_url(raw: &str) -> String {
    let Some((_, query)) = raw.split_once('?') else {
        return String::new();
    };
    let query = query.split('#').next().unwrap_or(query);
    let mut pairs: Vec<(String, String)> = Vec::new();
    for chunk in query.split('&') {
        let Some((name, value)) = chunk.split_once('=') else {
            continue;
        };
        if !matches!(
            name,
            "SESSDATA" | "bili_jct" | "DedeUserID" | "DedeUserID__ckMd5" | "sid"
        ) {
            continue;
        }
        if value.is_empty() {
            continue;
        }
        match pairs.iter_mut().find(|(existing, _)| existing == name) {
            Some(slot) => slot.1 = value.to_string(),
            None => pairs.push((name.to_string(), value.to_string())),
        }
    }
    pairs
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// 登录态的最低要求：三项齐全才敢落库，否则后续 nav/续期都会半残。
fn has_login_cookies(cookie: &str) -> bool {
    ["SESSDATA", "bili_jct", "DedeUserID"]
        .iter()
        .all(|name| credential::value(cookie, name).is_some_and(|value| !value.is_empty()))
}

fn with_refresh_token(cookie: &str, token: &str) -> String {
    credential::merge(cookie, &format!("ac_time_value={token}"))
}

/// 从 correspond 页里抠出一次性 refresh_csrf。
fn parse_refresh_csrf(html: &str) -> Option<String> {
    let marker = "id=\"1-name\"";
    let start = html.find(marker)?;
    let rest = &html[start + marker.len()..];
    let content_start = rest.find('>')? + 1;
    let content = &rest[content_start..];
    let end = content.find("</div>")?;
    let csrf = content[..end].trim();
    (!csrf.is_empty()).then(|| csrf.to_string())
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn correspond_path_with_key(key: &RsaPublicKey, now_ms: i64) -> Result<String, AppError> {
    let mut rng = rand::thread_rng();
    let message = format!("refresh_{now_ms}");
    let encrypted = key
        .encrypt(&mut rng, Oaep::new::<Sha256>(), message.as_bytes())
        .map_err(|err| AppError::Internal(format!("生成 correspondPath 失败：{err}")))?;
    Ok(encode_hex(&encrypted))
}

fn correspond_path(now_ms: i64) -> Result<String, AppError> {
    let key = RsaPublicKey::from_public_key_pem(REFRESH_PUBLIC_KEY)
        .map_err(|err| AppError::Internal(format!("B 站续期公钥无效：{err}")))?;
    correspond_path_with_key(&key, now_ms)
}

/// 一次续期检查的结果；调度层据此决定下一次检查的间隔。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// 本机没有 B 站账号
    NoCredential,
    /// 没有 ac_time_value 或 bili_jct：只能重新扫码
    NotRefreshable,
    /// cookie/info 说还不需要刷新
    Fresh,
    /// 已完成轮换并写回
    Refreshed,
    /// Cookie 已轮换并写回，但 confirm 接口失败：新 Cookie 可用，旧 refresh token 可能未作废
    ConfirmFailed,
    /// 请求期间用户重新登录/删除了账号，本轮结果丢弃
    Discarded,
}

/// 续期时发现 Cookie 已失效：立即把账号标成过期，不等下一次 30 分钟复验。
fn mark_credential_expired(db: &Db, row: &CredentialRow) -> bool {
    let _guard = credential_write_guard();
    if credential::get("bilibili").as_deref() != Some(row.cookie.as_str()) {
        tracing::info!(platform = "bilibili", "标记过期前发现 Cookie 已更新，跳过");
        return false;
    }
    match db.update_credential_profile(
        "bilibili",
        "expired",
        &row.account_name,
        row.avatar.as_deref(),
        now_ts(),
    ) {
        Ok(true) => {
            credential::set_status("bilibili", "expired");
            true
        }
        Ok(false) => {
            tracing::warn!(platform = "bilibili", "标记过期时账号已被删除");
            false
        }
        Err(err) => {
            tracing::warn!(platform = "bilibili", error = %err, "标记账号过期失败");
            false
        }
    }
}

fn notify_credential_expired(app: &tauri::AppHandle, account_name: &str) {
    let notice = AppNotice {
        kind: "credential_expired".into(),
        room_id: String::new(),
        host_name: account_name.to_string(),
        detail: Some("bilibili".into()),
        suppress: false,
    };
    if let Err(err) = app.emit(crate::recorder::EVENT_APP_NOTICE, notice) {
        tracing::warn!(platform = "bilibili", error = %err, "账号过期通知发送失败");
    }
}

/// 检查并按需刷新 B 站 Web Cookie，同时把本次检查结果落到 credential_refresh。
///
/// 流程见 docs/134：cookie/info → correspondPath → cookie/refresh → confirm/refresh。
/// 所有失败都只返回错误、不写 Cookie；唯一例外是 confirm/refresh 失败——新 Cookie 已经
/// 轮换成功，必须保留，否则新旧两边都可能失效。
pub async fn refresh_bilibili_cookie(
    db: &Db,
    app: &tauri::AppHandle,
) -> Result<RefreshOutcome, AppError> {
    let checked_cookie = credential::get("bilibili");
    let result = refresh_bilibili_cookie_inner(db, app).await;
    // 续期期间用户可能重新扫码/保存/删除；结果只允许写到「本次检查的那份凭据」上。
    // Refreshed 时 inner 已经写回新 Cookie，所以 expected 要取写回后的值。
    let expected = match &result {
        Ok(RefreshOutcome::Refreshed) | Ok(RefreshOutcome::ConfirmFailed) => {
            credential::get("bilibili")
        }
        Ok(RefreshOutcome::NoCredential) => None,
        _ => checked_cookie,
    };
    if record_refresh_result_checked(db, &result, expected.as_deref()) {
        emit_account_refresh(app);
    }
    result
}

pub(crate) fn emit_account_refresh(app: &tauri::AppHandle) {
    if let Err(err) = app.emit(EVENT_ACCOUNT_REFRESH, ()) {
        tracing::warn!(error = %err, "账号刷新事件发送失败");
    }
}

/// 只有当前凭据仍等于 expected 时才落续期状态，避免旧请求覆盖新账号的状态。
fn record_refresh_result_checked(
    db: &Db,
    result: &Result<RefreshOutcome, AppError>,
    expected: Option<&str>,
) -> bool {
    let _guard = credential_write_guard();
    let current = credential::get("bilibili");
    if current.as_deref() != expected {
        tracing::info!(
            platform = "bilibili",
            "续期期间账号已变化，丢弃本次续期状态"
        );
        return false;
    }
    write_refresh_result(db, result);
    true
}

#[cfg(test)]
fn record_refresh_result(db: &Db, result: &Result<RefreshOutcome, AppError>) {
    let _guard = credential_write_guard();
    write_refresh_result(db, result);
}

fn write_refresh_result(db: &Db, result: &Result<RefreshOutcome, AppError>) {
    match result {
        Ok(RefreshOutcome::NoCredential) => {
            if let Err(err) = db.clear_credential_refresh("bilibili") {
                tracing::warn!(error = %err, "清除续期状态失败");
            }
        }
        Ok(RefreshOutcome::Discarded) => {
            // 请求期间账号已被替换：本轮结果不属于新账号，保留新账号自己的状态。
        }
        Ok(outcome) => {
            let previous = load_refresh_status(db, "bilibili");
            let now = now_ts();
            let (name, message) = match outcome {
                RefreshOutcome::NotRefreshable => (
                    "not_refreshable",
                    Some("Cookie 缺少 ac_time_value 或 bili_jct，无法自动续期".to_string()),
                ),
                RefreshOutcome::Fresh => ("fresh", None),
                RefreshOutcome::Refreshed => ("refreshed", None),
                RefreshOutcome::ConfirmFailed => (
                    "confirm_failed",
                    Some("Cookie 已轮换，但确认续期失败；若之后提示未登录，请重新扫码".to_string()),
                ),
                RefreshOutcome::NoCredential | RefreshOutcome::Discarded => unreachable!(),
            };
            let status = RefreshStatus {
                checked_at: now,
                outcome: name.to_string(),
                message,
                key_stale: false,
                last_refreshed_at: if matches!(
                    outcome,
                    RefreshOutcome::Refreshed | RefreshOutcome::ConfirmFailed
                ) {
                    now
                } else {
                    previous.last_refreshed_at
                },
            };
            save_refresh_status(db, "bilibili", &status);
        }
        Err(err) => {
            let key_stale = matches!(
                err,
                AppError::Platform { code, .. } if *code == "refresh_key_stale"
            );
            let status = RefreshStatus {
                checked_at: now_ts(),
                outcome: if key_stale { "key_stale" } else { "failed" }.to_string(),
                message: Some(short_error(&err.to_string())),
                key_stale,
                last_refreshed_at: load_refresh_status(db, "bilibili").last_refreshed_at,
            };
            save_refresh_status(db, "bilibili", &status);
        }
    }
}

async fn refresh_bilibili_cookie_inner(
    db: &Db,
    app: &tauri::AppHandle,
) -> Result<RefreshOutcome, AppError> {
    let _guard = REFRESH_LOCK.lock().await;

    let rows = db.credentials()?;
    let Some(row) = rows.into_iter().find(|row| row.platform == "bilibili") else {
        return Ok(RefreshOutcome::NoCredential);
    };
    let original = row.cookie.clone();
    let Some(old_token) = credential::value(&original, "ac_time_value")
        .filter(|token| !token.is_empty())
        .map(str::to_string)
    else {
        tracing::debug!(platform = "bilibili", "没有 ac_time_value，跳过自动续期");
        return Ok(RefreshOutcome::NotRefreshable);
    };
    let Some(old_jct) = credential::value(&original, "bili_jct")
        .filter(|token| !token.is_empty())
        .map(str::to_string)
    else {
        tracing::debug!(platform = "bilibili", "没有 bili_jct，跳过自动续期");
        return Ok(RefreshOutcome::NotRefreshable);
    };

    let info_url = format!("{PASSPORT}/x/passport-login/web/cookie/info");
    let info: Envelope<CookieInfoData> =
        http::get_json(&info_url, REFERER, &[("Cookie", &original)])
            .await
            .map_err(AppError::from)?;
    let need_refresh = match (info.code, info.data) {
        (0, Some(data)) => data.refresh,
        (-101, _) => {
            let was_ok = row.status == "ok";
            if mark_credential_expired(db, &row) {
                clear_stream_cache(app);
                if was_ok {
                    notify_credential_expired(app, &row.account_name);
                }
                emit_account_refresh(app);
            }
            return Err(AppError::User("Cookie 已失效或未登录".into()));
        }
        (code, _) => {
            return Err(AppError::Platform {
                code: "platform_upstream",
                message: format!(
                    "检查 Cookie 续期失败（code {code}）{}",
                    info.message.unwrap_or_default()
                ),
            });
        }
    };
    if !need_refresh {
        tracing::debug!(platform = "bilibili", "cookie/info 表示暂不需要续期");
        return Ok(RefreshOutcome::Fresh);
    }

    let now_ms = chrono::Utc::now().timestamp_millis();
    let path = correspond_path(now_ms)?;
    let correspond_url = format!("https://www.bilibili.com/correspond/1/{path}");
    // 参考公开实现：correspond 与刷新请求用随机 buvid3，避免设备指纹干扰。
    let buvid3 = credential::value(&original, "buvid3")
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let probe_cookie = credential::merge(&original, &format!("buvid3={buvid3}"));
    let (html, _) =
        match http::get_text(&correspond_url, REFERER, &[("Cookie", &probe_cookie)]).await {
            Ok(value) => value,
            Err(err) if correspond_key_stale(&err) => {
                tracing::warn!(
                    platform = "bilibili",
                    error = %err,
                    "续期公钥疑似失效：correspond 页面拒绝请求"
                );
                return Err(AppError::Platform {
                    code: "refresh_key_stale",
                    message: "B 站续期接口已变更（correspond 页面不可用）".into(),
                });
            }
            Err(err) => return Err(AppError::from(err)),
        };
    let refresh_csrf = parse_refresh_csrf(&html).ok_or_else(|| {
        tracing::warn!(
            platform = "bilibili",
            bytes = html.len(),
            "续期页面里没有 refresh_csrf，续期接口可能已变更"
        );
        AppError::Platform {
            code: "refresh_key_stale",
            message: "B 站续期页面结构已变更（未找到 refresh_csrf）".into(),
        }
    })?;

    let refresh_url = format!("{PASSPORT}/x/passport-login/web/cookie/refresh");
    let refresh_form = [
        ("csrf", old_jct.clone()),
        ("refresh_csrf", refresh_csrf),
        ("refresh_token", old_token.clone()),
        ("source", "main_web".to_string()),
    ];
    let refreshed: http::FetchedWithHeaders<Envelope<RefreshData>> =
        http::post_form_json_with_headers(
            &refresh_url,
            REFERER,
            "https://www.bilibili.com",
            &probe_cookie,
            &refresh_form,
        )
        .await
        .map_err(AppError::from)?;
    if refreshed.data.code != 0 {
        return Err(AppError::Platform {
            code: "platform_upstream",
            message: format!(
                "刷新 Cookie 失败（code {}）{}",
                refreshed.data.code,
                refreshed.data.message.unwrap_or_default()
            ),
        });
    }

    let new_cookie = cookies_from_set_cookie(&refreshed.set_cookies);
    // 只校验本次响应自己带来的字段。若拿 original 合并后再校验，旧 Cookie 永远
    // 三项齐全，会把「refresh 响应完全没给 Set-Cookie」误判成刷新成功。
    if !has_login_cookies(&new_cookie) {
        tracing::warn!(
            platform = "bilibili",
            has_header = !refreshed.set_cookies.is_empty(),
            "续期响应缺少完整的登录 Cookie"
        );
        return Err(AppError::Platform {
            code: "platform_upstream",
            message: "刷新响应缺少完整的 Set-Cookie（SESSDATA/bili_jct/DedeUserID）".into(),
        });
    }
    let new_token = refreshed
        .data
        .data
        .map(|data| data.refresh_token)
        .unwrap_or_default();
    let mut merged = credential::merge(&original, &new_cookie);
    if new_token.is_empty() {
        // 服务端没给新 token：旧 token 在 confirm 后作废，不能再留在 Cookie 里。
        merged = credential::without(&merged, "ac_time_value");
    } else {
        merged = with_refresh_token(&merged, &new_token);
    }
    let new_jct = credential::value(&merged, "bili_jct")
        .unwrap_or_default()
        .to_string();

    // 确认接口必须用「新 bili_jct + 旧 refresh_token」，顺序错了会 400。
    let confirm_url = format!("{PASSPORT}/x/passport-login/web/confirm/refresh");
    let confirm_form = [("csrf", new_jct), ("refresh_token", old_token.clone())];
    let mut confirm_failed = false;
    match http::post_form_json_with_headers::<Envelope<serde_json::Value>>(
        &confirm_url,
        REFERER,
        "https://www.bilibili.com",
        &merged,
        &confirm_form,
    )
    .await
    {
        Ok(confirmed) if confirmed.data.code == 0 => {}
        Ok(confirmed) => {
            confirm_failed = true;
            tracing::warn!(
                platform = "bilibili",
                code = confirmed.data.code,
                message = %confirmed.data.message.unwrap_or_default(),
                "确认续期失败，保留已轮换的新 Cookie 等下次重试"
            )
        }
        Err(err) => {
            confirm_failed = true;
            tracing::warn!(
                platform = "bilibili",
                error = %err,
                "确认续期请求失败，保留已轮换的新 Cookie 等下次重试"
            )
        }
    }

    // 请求期间用户可能重新扫码/保存/删除账号；比较与写回必须持有同一把写锁，
    // 否则刚比对完就可能被新登录覆盖，反过来把新登录冲掉。
    let refreshed_row = {
        let _guard = credential_write_guard();
        if credential::get("bilibili").as_deref() != Some(original.as_str()) {
            tracing::info!(
                platform = "bilibili",
                "续期期间 Cookie 已更新，丢弃本轮结果"
            );
            return Ok(RefreshOutcome::Discarded);
        }
        let refreshed_row = CredentialRow {
            cookie: merged,
            status: "ok".to_string(),
            updated_at: now_ts(),
            ..row
        };
        db.save_credential(&refreshed_row)?;
        credential::set("bilibili", &refreshed_row.cookie);
        refreshed_row
    };
    clear_stream_cache(app);
    tracing::info!(
        platform = "bilibili",
        account = %refreshed_row.account_name,
        refreshable = credential::has_refresh_token(&refreshed_row.cookie),
        "B 站 Cookie 续期成功"
    );
    Ok(if confirm_failed {
        RefreshOutcome::ConfirmFailed
    } else {
        RefreshOutcome::Refreshed
    })
}

/// 后台续期循环：正常每 12 小时问一次，失败按 1h → 6h → 24h 退避，并加抖动。
pub async fn refresh_loop(db: std::sync::Arc<Db>, app: tauri::AppHandle) {
    use rand::Rng;

    let mut failures = 0u32;
    loop {
        match refresh_bilibili_cookie(&db, &app).await {
            Ok(outcome) => {
                failures = 0;
                if outcome == RefreshOutcome::Refreshed {
                    tracing::info!(platform = "bilibili", "后台续期完成");
                }
            }
            Err(err) => {
                failures = failures.saturating_add(1);
                tracing::warn!(
                    platform = "bilibili",
                    failures,
                    error = %err,
                    "自动续期暂时失败，稍后重试"
                );
            }
        }
        let base = match failures {
            0 => REFRESH_INTERVAL,
            1 => Duration::from_secs(60 * 60),
            2 => Duration::from_secs(6 * 60 * 60),
            _ => Duration::from_secs(24 * 60 * 60),
        };
        // 抖动只用于错开各客户端的请求，不改变业务语义。
        let jitter = if failures == 0 { 30 * 60 } else { 5 * 60 };
        let wait = base + Duration::from_secs(rand::thread_rng().gen_range(0..jitter));
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = REFRESH_WAKE.notified() => {
                tracing::info!(platform = "bilibili", "续期循环被唤醒，立即重新检查");
            }
        }
    }
}

/// 手动「立即检查」：直接跑一次完整续期并返回落库后的状态。
pub async fn refresh_now(db: &Db, app: &tauri::AppHandle) -> Result<RefreshStatus, AppError> {
    tracing::info!(platform = "bilibili", "手动检查 Cookie 续期");
    match refresh_bilibili_cookie(db, app).await? {
        RefreshOutcome::NoCredential => Err(AppError::User("尚未登录 B 站".into())),
        _ => Ok(load_refresh_status(db, "bilibili")),
    }
}

/// 用指定 Cookie 查当前身份；未登录时返回中文原因，方便直接展示。
pub async fn bilibili_nav(cookie: &str) -> Result<Account, AppError> {
    let url = format!("{API}/x/web-interface/nav");
    let envelope: Envelope<NavData> = http::get_json(&url, REFERER, &[("Cookie", cookie)])
        .await
        .map_err(AppError::from)?;
    let data = match (envelope.code, envelope.data) {
        (0, Some(data)) if data.is_login => data,
        (code, _) => {
            let message = if code == -101 {
                "Cookie 已失效或未登录".to_string()
            } else {
                format!(
                    "Cookie 无效（code {code}）{}",
                    envelope.message.unwrap_or_default()
                )
            };
            // 只有明确的「未登录」才算凭据失效；平台临时报错不能把用户有效的
            // Cookie 标成过期，否则每 30 分钟的复验会误报。
            return if code == -101 {
                Err(AppError::User(message))
            } else {
                Err(AppError::Platform {
                    code: "platform_upstream",
                    message,
                })
            };
        }
    };
    Ok(Account {
        uid: data.mid.to_string(),
        name: if data.uname.is_empty() {
            format!("UID {}", data.mid)
        } else {
            data.uname
        },
        avatar: Some(data.face).filter(|face| !face.is_empty()),
    })
}

pub(crate) fn store_account(
    db: &Db,
    platform: &str,
    cookie: &str,
    account: &Account,
) -> Result<CredentialRow, AppError> {
    tracing::debug!(platform, account = %account.name, uid = %account.uid, "保存平台凭据");
    let _guard = credential_write_guard();
    let row = CredentialRow {
        platform: platform.to_string(),
        account_id: String::new(),
        cookie: cookie.to_string(),
        uid: account.uid.clone(),
        account_name: account.name.clone(),
        avatar: account.avatar.clone(),
        status: "ok".to_string(),
        updated_at: now_ts(),
    };
    db.save_credential(&row).map_err(|err| {
        tracing::warn!(platform, account = %account.name, error = %err, "保存平台凭据失败");
        err
    })?;
    credential::set(platform, cookie);
    // 新登录/新 Cookie 的续期状态从零开始，不能沿用上一份凭据的检查结果。
    if let Err(err) = db.clear_credential_refresh(platform) {
        tracing::warn!(platform, error = %err, "重置续期状态失败");
    }
    Ok(row)
}

/// 保存用户粘贴的 Cookie：先规范化，B 站再验一次身份，其余平台直接存。
pub async fn save_cookie(db: &Db, platform: &str, raw: &str) -> Result<CredentialInfo, AppError> {
    let platform = platform.trim();
    if !matches!(platform, "bilibili" | "douyu" | "huya" | "douyin") {
        tracing::warn!(platform, "保存 Cookie 被拒绝：平台不受支持");
        return Err(AppError::User(format!("不支持的平台：{platform}")));
    }
    let cookie = credential::normalize(raw).map_err(|err| {
        tracing::warn!(platform, error = %err, "Cookie 格式不规范");
        AppError::User(err)
    })?;

    let account = if platform == "bilibili" {
        bilibili_nav(&cookie).await?
    } else if platform == "douyu" {
        crate::douyu_auth::validate_cookie(&cookie).await?
    } else if platform == "douyin" {
        crate::douyin_auth::validate_cookie(&cookie).await?
    } else {
        Account {
            uid: String::new(),
            name: format!("{} Cookie", platform_name(platform)),
            avatar: None,
        }
    };

    let row = store_account(db, platform, &cookie, &account)?;
    if platform == "bilibili" {
        wake_refresh_loop("保存 Cookie");
    } else if platform == "douyu" {
        // 手动粘贴的 Cookie 没有 LTP0：旧凭据留下的长期凭证必须清掉，
        // 否则续期循环会拿旧账号的 LTP0 去换票，可能把新 Cookie 污染成旧账号。
        if let Err(err) = db.clear_credential_secret("douyu") {
            tracing::warn!(error = %err, "清理斗鱼长期凭证失败");
        }
    }
    tracing::info!(platform, account = %row.account_name, "Cookie 已保存");
    Ok(info_for_row(db, &row))
}

pub fn delete(db: &Db, platform: &str) -> Result<(), AppError> {
    let _guard = credential_write_guard();
    db.delete_credential(platform).map_err(|err| {
        tracing::warn!(platform, error = %err, "删除平台凭据失败");
        err
    })?;
    credential::remove(platform);
    tracing::info!(platform, "已删除平台凭据");
    Ok(())
}

pub fn list(db: &Db) -> Result<Vec<CredentialInfo>, AppError> {
    Ok(db
        .credentials()?
        .iter()
        .map(|row| info_for_row(db, row))
        .collect())
}

/// 启动时与运行中定期复验 B 站登录态：过期就标 expired，设置页会提示重新扫码。
/// 登录态变化后清空取流缓存，避免继续使用旧画质/旧 Cookie 换来的候选地址。
pub(crate) fn clear_stream_cache(app: &tauri::AppHandle) {
    if let Some(state) = app.try_state::<crate::state::AppState>() {
        state.broker.clear_groups();
    }
}

pub async fn revalidate(db: &Db, app: &tauri::AppHandle) {
    let rows = match db.credentials() {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!(error = %err, "读取平台凭据失败，跳过登录态复验");
            return;
        }
    };
    let mut checked = 0usize;
    for row in rows {
        if row.platform != "bilibili" {
            continue;
        }
        checked += 1;
        // 复验用的是发起请求时读到的 cookie。用户可能在这期间重新登录或删除了账号，
        // 此时不能再把旧 cookie 或旧状态写回去，否则会把新凭据覆盖掉。
        // 必须在请求返回后再检查一次，不能在 await 之前取快照。
        match bilibili_nav(&row.cookie).await {
            Ok(account) => {
                // 比较与写回必须在同一把写锁内：否则刚判断完 Cookie 未变，
                // 新登录就可能插进来，随后被这次旧复验覆盖账号信息。
                let _guard = credential_write_guard();
                let cookie_unchanged =
                    credential::get("bilibili").as_deref() == Some(row.cookie.as_str());
                if !cookie_unchanged {
                    tracing::info!(
                        platform = "bilibili",
                        "登录态复验期间 Cookie 已更新，丢弃本次复验结果"
                    );
                    continue;
                }
                if account.name == row.account_name && row.status == "ok" {
                    continue;
                }
                match db.update_credential_profile(
                    "bilibili",
                    "ok",
                    &account.name,
                    account.avatar.as_deref(),
                    now_ts(),
                ) {
                    Ok(true) => {
                        credential::set_status("bilibili", "ok");
                        tracing::info!(
                            platform = "bilibili",
                            account = %account.name,
                            "B 站登录态复验通过，已更新账号信息"
                        );
                        clear_stream_cache(app);
                        emit_account_refresh(app);
                    }
                    Ok(false) => {
                        tracing::warn!(platform = "bilibili", "复验通过但账号已被删除，跳过写回")
                    }
                    Err(err) => tracing::warn!(
                        platform = "bilibili",
                        account = %account.name,
                        error = %err,
                        "更新复验后的账号信息失败"
                    ),
                }
            }
            Err(err) => {
                let was_ok = row.status == "ok";
                if was_ok {
                    tracing::warn!(
                        platform = "bilibili",
                        account = %row.account_name,
                        error = %err,
                        "B 站登录态复验失败"
                    );
                } else {
                    // 已经标记过期后每 30 分钟仍会复验一次：失败是预期内的，
                    // 用 DEBUG 避免把用户的日志刷满同一条警告。
                    tracing::debug!(
                        platform = "bilibili",
                        account = %row.account_name,
                        error = %err,
                        "B 站登录态仍为过期"
                    );
                }
                // 网络抖动、平台临时错误不代表 Cookie 失效：保留上次状态，等下一轮。
                if !matches!(err, AppError::User(_)) {
                    tracing::warn!(platform = "bilibili", "登录态复验暂时不可用，保留原状态");
                    continue;
                }
                let _guard = credential_write_guard();
                let cookie_unchanged =
                    credential::get("bilibili").as_deref() == Some(row.cookie.as_str());
                if !cookie_unchanged {
                    tracing::info!(
                        platform = "bilibili",
                        "登录态复验期间 Cookie 已更新，不标记过期"
                    );
                    continue;
                }
                let account_name = row.account_name.clone();
                match db.update_credential_profile(
                    "bilibili",
                    "expired",
                    &account_name,
                    row.avatar.as_deref(),
                    now_ts(),
                ) {
                    Ok(true) => {
                        credential::set_status("bilibili", "expired");
                        clear_stream_cache(app);
                        emit_account_refresh(app);
                    }
                    Ok(false) => tracing::warn!(
                        platform = "bilibili",
                        account = %account_name,
                        "复验失败但账号已被删除，跳过写回"
                    ),
                    Err(err) => tracing::warn!(
                        platform = "bilibili",
                        account = %account_name,
                        error = %err,
                        "标记账号过期失败"
                    ),
                }
                // 只在「从正常变成过期」时播报，避免每次复验都弹一次。
                if was_ok {
                    let notice = AppNotice {
                        kind: "credential_expired".into(),
                        room_id: String::new(),
                        host_name: account_name,
                        detail: Some("bilibili".into()),
                        suppress: false,
                    };
                    if let Err(err) = app.emit(crate::recorder::EVENT_APP_NOTICE, notice) {
                        tracing::warn!(platform = "bilibili", error = %err, "账号过期通知发送失败");
                    }
                }
            }
        }
    }
    if checked > 0 {
        tracing::info!(checked, "登录态复验完成");
    }
    // 复验只改状态，不改 Cookie；这里不要整表重载内存凭据，否则会和用户
    // 同时保存的新 Cookie 抢写，可能把刚保存的凭据冲回旧值。
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_cookie_pairs_from_set_cookie() {
        let headers = vec![
            "SESSDATA=abc%2C123; Path=/; Domain=.bilibili.com; Expires=Wed, 01 Jan 2027 00:00:00 GMT"
                .to_string(),
            "bili_jct=token; Path=/".to_string(),
            "DedeUserID=42; Path=/".to_string(),
        ];
        assert_eq!(
            cookies_from_set_cookie(&headers),
            "SESSDATA=abc%2C123; bili_jct=token; DedeUserID=42"
        );
    }

    #[test]
    fn merges_set_cookie_into_existing() {
        let base = "SESSDATA=old; dy_did=keep";
        let extra = cookies_from_set_cookie(&["SESSDATA=new; Path=/".to_string()]);
        assert_eq!(credential::merge(base, &extra), "SESSDATA=new; dy_did=keep");
    }

    #[test]
    fn parses_poll_expires_in() {
        // B 站 poll 返回的 data 里带 expires_in（剩余秒数），缺字段时按 0 处理。
        let with: Envelope<PollData> = serde_json::from_str(
            r#"{"code":0,"data":{"code":86101,"message":"","expires_in":145}}"#,
        )
        .unwrap();
        assert_eq!(with.data.unwrap().expires_in, 145);
        let without: Envelope<PollData> =
            serde_json::from_str(r#"{"code":0,"data":{"code":86101}}"#).unwrap();
        assert_eq!(without.data.unwrap().expires_in, 0);
        assert_eq!(QR_TTL_SECONDS, 180);
    }

    #[test]
    fn parses_poll_refresh_fields() {
        let poll: Envelope<PollData> = serde_json::from_str(
            r#"{"code":0,"data":{"code":0,"url":"https://passport.biligame.com/crossDomain?SESSDATA=a%2Cb&bili_jct=j&DedeUserID=42","refresh_token":"tok","timestamp":1700000000000}}"#,
        )
        .unwrap();
        let data = poll.data.unwrap();
        assert_eq!(data.refresh_token, "tok");
        assert_eq!(data.timestamp, 1_700_000_000_000);
        assert!(data.url.contains("SESSDATA="));
    }

    #[test]
    fn keeps_raw_percent_encoding_from_poll_url() {
        let url = "https://passport.biligame.com/crossDomain?SESSDATA=a%2Cb&bili_jct=j&DedeUserID=42&DedeUserID__ckMd5=m&sid=s&ignored=x";
        assert_eq!(
            cookies_from_poll_url(url),
            "SESSDATA=a%2Cb; bili_jct=j; DedeUserID=42; DedeUserID__ckMd5=m; sid=s"
        );
        // 没有查询串 / 缺字段都不能伪造 Cookie。
        assert_eq!(cookies_from_poll_url("https://example.com"), "");
        assert_eq!(cookies_from_poll_url("https://example.com?SESSDATA="), "");
    }

    #[test]
    fn poll_cookie_keeps_last_duplicate() {
        let url = "https://example.com?SESSDATA=old&SESSDATA=new&bili_jct=j";
        assert_eq!(cookies_from_poll_url(url), "SESSDATA=new; bili_jct=j");
    }

    #[test]
    fn refresh_token_is_replaced_not_appended() {
        let cookie = "SESSDATA=a; ac_time_value=old; bili_jct=j; DedeUserID=42";
        assert_eq!(
            with_refresh_token(cookie, "new"),
            "SESSDATA=a; ac_time_value=new; bili_jct=j; DedeUserID=42"
        );
        assert_eq!(
            credential::without(cookie, "ac_time_value"),
            "SESSDATA=a; bili_jct=j; DedeUserID=42"
        );
        assert!(has_login_cookies(cookie));
        assert!(!has_login_cookies("SESSDATA=a; bili_jct=j"));
    }

    #[test]
    fn refreshable_flag_needs_bilibili_and_both_tokens() {
        let row = |platform: &str, cookie: &str| CredentialRow {
            platform: platform.to_string(),
            account_id: String::new(),
            cookie: cookie.to_string(),
            uid: "42".into(),
            account_name: "tester".into(),
            avatar: None,
            status: "ok".into(),
            updated_at: 0,
        };
        assert!(
            info_from_row(&row(
                "bilibili",
                "SESSDATA=a; bili_jct=j; DedeUserID=42; ac_time_value=t"
            ))
            .refreshable
        );
        assert!(
            !info_from_row(&row("bilibili", "SESSDATA=a; bili_jct=j; DedeUserID=42")).refreshable
        );
        assert!(
            !info_from_row(&row(
                "douyu",
                "SESSDATA=a; bili_jct=j; DedeUserID=42; ac_time_value=t"
            ))
            .refreshable
        );
    }

    #[test]
    fn parses_refresh_csrf_from_correspond_page() {
        let html = r#"<html><body><div id="1-name">  abc123  </div></body></html>"#;
        assert_eq!(parse_refresh_csrf(html).as_deref(), Some("abc123"));
        assert_eq!(parse_refresh_csrf("<html>404</html>"), None);
    }

    #[test]
    fn correspond_path_encrypts_refresh_timestamp() {
        let mut rng = rand::thread_rng();
        let private = rsa::RsaPrivateKey::new(&mut rng, 1024).unwrap();
        let public = private.to_public_key();
        let path = correspond_path_with_key(&public, 1_700_000_000_123).unwrap();
        assert_eq!(path.len(), 256);
        let bytes: Vec<u8> = (0..path.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&path[i..i + 2], 16).unwrap())
            .collect();
        let plain = private.decrypt(Oaep::new::<Sha256>(), &bytes).unwrap();
        assert_eq!(String::from_utf8(plain).unwrap(), "refresh_1700000000123");
        // 测试里顺手证明 PEM 公钥可解析（真实路径不走这里）。
        assert!(RsaPublicKey::from_public_key_pem(REFRESH_PUBLIC_KEY).is_ok());
    }

    /// 联网冒烟：cargo test --lib account -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "需要访问 B 站 passport 接口"]
    async fn qr_generate_returns_svg() {
        let ticket = bilibili_qr_start().await.expect("生成二维码失败");
        assert!(ticket.qrcode_key.len() > 8);
        assert!(ticket.svg.contains("<svg"));
        println!("svg head = {}", &ticket.svg[..ticket.svg.len().min(160)]);
        println!("qr url = {}", ticket.url);
        println!("svg bytes = {}", ticket.svg.len());
        let poll = bilibili_qr_poll(&test_db(), &ticket.qrcode_key)
            .await
            .expect("轮询失败");
        println!("poll = {} {}", poll.state, poll.message);
        assert!(matches!(poll.state.as_str(), "pending" | "scanned"));
    }

    #[test]
    fn qr_poll_url_encodes_qrcode_key() {
        let url = qr_poll_url("abc&source=evil").unwrap();
        assert!(url.contains("qrcode_key=abc%26source%3Devil"));
        assert_eq!(url.matches("source=").count(), 1);
        assert!(qr_poll_url(&"x".repeat(257)).is_err());
        assert_eq!(qr_error("bad").state, "error");
    }

    #[test]
    fn qr_landing_needs_its_own_identity_fields() {
        let old = "SESSDATA=old; bili_jct=oldj; DedeUserID=1";
        let landing = "SESSDATA=new";
        // 本次登录只给了 SESSDATA：不能借旧账号的 bili_jct/DedeUserID 通过校验。
        assert!(!has_login_cookies(landing));
        assert!(has_login_cookies(&credential::merge(old, landing)));
        // 完整 landing 才允许合并。
        let full = "SESSDATA=new; bili_jct=newj; DedeUserID=2";
        assert!(has_login_cookies(full));
    }

    #[test]
    fn refresh_response_must_carry_its_own_login_cookies() {
        let old = "SESSDATA=old; bili_jct=oldj; DedeUserID=1";
        let partial = cookies_from_set_cookie(&["SESSDATA=new; Path=/".to_string()]);
        assert!(!has_login_cookies(&partial));
        assert!(has_login_cookies(&credential::merge(old, &partial)));
        let full = cookies_from_set_cookie(&[
            "SESSDATA=new; Path=/".to_string(),
            "bili_jct=newj; Path=/".to_string(),
            "DedeUserID=2; Path=/".to_string(),
        ]);
        assert!(has_login_cookies(&full));
    }

    #[test]
    fn refresh_status_defaults_to_never() {
        let status = RefreshStatus::default();
        assert_eq!(status.checked_at, 0);
        assert_eq!(status.outcome, "never");
        assert!(!status.key_stale);
        assert_eq!(status.last_refreshed_at, 0);
    }

    #[test]
    fn refresh_status_records_results_and_keeps_last_rotation() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("hivecast-refresh-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("refresh.db")).unwrap();

        save_refresh_status(
            &db,
            "bilibili",
            &RefreshStatus {
                checked_at: 10,
                outcome: "fresh".into(),
                message: None,
                key_stale: false,
                last_refreshed_at: 111,
            },
        );
        record_refresh_result(&db, &Ok(RefreshOutcome::Fresh));
        let status = load_refresh_status(&db, "bilibili");
        assert_eq!(status.outcome, "fresh");
        assert!(status.checked_at > 10);
        assert_eq!(status.last_refreshed_at, 111);

        // 接口变更：标记 key_stale，但保留已经发生过的轮换时间。
        record_refresh_result(
            &db,
            &Err(AppError::Platform {
                code: "refresh_key_stale",
                message: "续期接口已变更".into(),
            }),
        );
        let status = load_refresh_status(&db, "bilibili");
        assert_eq!(status.outcome, "key_stale");
        assert!(status.key_stale);
        assert_eq!(status.last_refreshed_at, 111);
        assert!(status
            .message
            .as_deref()
            .unwrap()
            .contains("续期接口已变更"));

        // 成功轮换后 last_refreshed_at 跟随本次检查时间。
        record_refresh_result(&db, &Ok(RefreshOutcome::Refreshed));
        let status = load_refresh_status(&db, "bilibili");
        assert_eq!(status.outcome, "refreshed");
        assert!(!status.key_stale);
        assert_eq!(status.last_refreshed_at, status.checked_at);

        // confirm 失败：Cookie 已轮换，仍推进轮换时间，但状态明确标成 confirm_failed。
        record_refresh_result(&db, &Ok(RefreshOutcome::ConfirmFailed));
        let status = load_refresh_status(&db, "bilibili");
        assert_eq!(status.outcome, "confirm_failed");
        assert_eq!(status.last_refreshed_at, status.checked_at);
        assert!(status.message.as_deref().unwrap().contains("确认续期失败"));

        // 被丢弃的结果不能覆盖新账号的状态。
        record_refresh_result(&db, &Ok(RefreshOutcome::Discarded));
        assert_eq!(
            load_refresh_status(&db, "bilibili").outcome,
            "confirm_failed"
        );

        // 账号不存在时清空旧状态，下次登录从 never 开始。
        record_refresh_result(&db, &Ok(RefreshOutcome::NoCredential));
        let status = load_refresh_status(&db, "bilibili");
        assert_eq!(status.outcome, "never");
        assert_eq!(status.checked_at, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn correspond_key_stale_only_matches_404_or_412() {
        let stale =
            |message: &str| correspond_key_stale(&PlatformError::Network(message.to_string()));
        assert!(stale("https://www.bilibili.com/correspond/1/ab → HTTP 404"));
        assert!(stale("https://www.bilibili.com/correspond/1/ab → HTTP 412"));
        assert!(!stale(
            "https://www.bilibili.com/correspond/1/ab → HTTP 500"
        ));
        assert!(!stale("connection reset"));
    }

    #[test]
    fn short_error_truncates_and_flattens_newlines() {
        let short = short_error("a\nb");
        assert_eq!(short, "a b");
        let long = short_error(&"x".repeat(200));
        assert_eq!(long.chars().count(), 181);
        assert!(long.ends_with('…'));
    }

    fn test_db() -> Db {
        let dir = std::env::temp_dir().join(format!("hivecast-qr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Db::open(&dir.join("qr.db")).unwrap()
    }

    #[test]
    fn rejects_unknown_platform() {
        let dir = std::env::temp_dir().join(format!("hivecast-account-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let db = Db::open(&dir.join("t.db")).unwrap();
        let err = tauri::async_runtime::block_on(save_cookie(&db, "twitch", "a=1")).unwrap_err();
        assert!(err.to_string().contains("不支持的平台"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
