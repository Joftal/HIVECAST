//! 抖音账号：官方登录窗口、Cookie 校验与定期保活。
//!
//! 抖音的扫码轮询接口带设备风控：生成二维码不需要签名，但 check_qrconnect 要
//! a_bogus 与 x-tt-session-dtrait（由两份混淆 JS + 原生 RSA/AES 生成）。复刻这套
//! 签名既重又易随前端发版失效，所以这里不复刻协议，而是：
//! 1. 打开一个加载抖音官方登录页的 Tauri 子窗口，让用户在真实浏览器环境完成
//!    扫码、短信二次验证等流程；
//! 2. 登录成功后用 Tauri 原生 CookieStore 读取 HttpOnly 的 sessionid；
//! 3. 用 self profile 接口校验身份并落库，身份信息用 uid/nickname/avatar；
//! 4. 运行中定期用当前 Cookie 复验，并访问一次主页吸收服务端轮换的 Set-Cookie。
//!
//! 抖音没有公开的 refresh_token 端点（不像 B 站的 ac_time_value 或斗鱼的 LTP0），
//! 因此这里不承诺「永久自动续期」：只能保持会话活跃、同步轮换 Cookie，失效后
//! 明确标记过期并让用户重新扫码。

use std::collections::BTreeMap;
use std::time::Duration;

use hivecast_ipc::{AppNotice, QrPoll, QrTicket, RefreshStatus};
use reqwest::header::{HeaderValue, ACCEPT, COOKIE, REFERER, SET_COOKIE};
use serde::Deserialize;
use serde_json::Value;
use tauri::webview::WebviewWindowBuilder;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl};

use crate::account::Account;
use crate::credential;
use crate::db::{now_ts, CredentialRow, Db};
use crate::error::AppError;
use crate::platform::PlatformError;

/// 登录窗口的固定 label：同一时间只允许一个抖音登录会话。
const LOGIN_WINDOW: &str = "douyin-login";
const LOGIN_URL: &str = "https://www.douyin.com/login_page?next=https%3A%2F%2Fwww.douyin.com";
const HOME: &str = "https://www.douyin.com/";
const PROFILE: &str = "https://www.douyin.com/aweme/v1/web/user/profile/self/?device_platform=webapp&aid=6383&channel=channel_pc_web&publish_video_strategy_type=2&source=channel_pc_web";
const LOGIN_TTL: i64 = 600;
const REFRESH_EVERY: i64 = 6 * 60 * 60;
const RETRY_FAILED: i64 = 30 * 60;
/// 维护循环的检查间隔；真正是否请求抖音由 refresh_due 决定。
pub const REVALIDATE_INTERVAL: Duration = Duration::from_secs(30 * 60);

static LOGIN_SESSION: std::sync::Mutex<LoginSession> =
    std::sync::Mutex::new(LoginSession { started_at: 0 });
/// 确认登录只允许一个请求做「校验 + 落库」，避免用户连点或轮询并发写入两份凭据。
static CONFIRM_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, Clone, Copy)]
struct LoginSession {
    started_at: i64,
}

#[derive(Debug, Default, Deserialize)]
struct ProfileEnvelope {
    #[serde(default)]
    status_code: i64,
    #[serde(default)]
    status_msg: String,
    #[serde(default)]
    user: Option<Value>,
}

#[derive(Debug, PartialEq, Eq)]
enum RefreshOutcome {
    NoCredential,
    Refreshed,
    Fresh,
    Invalid,
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

fn set_started_at(value: i64) {
    if let Ok(mut session) = LOGIN_SESSION.lock() {
        session.started_at = value;
    }
}

fn started_at() -> i64 {
    LOGIN_SESSION
        .lock()
        .map(|session| session.started_at)
        .unwrap_or(0)
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
            "抖音 Cookie 缺少 sessionid，请通过扫码重新登录".into(),
        ));
    }
    Ok(cookie)
}

/// 读取子窗口里属于 douyin.com 的 Cookie（包含 HttpOnly）。
fn window_cookie(window: &tauri::WebviewWindow) -> Result<Option<String>, AppError> {
    let url = tauri::Url::parse(HOME)
        .map_err(|err| AppError::Internal(format!("抖音地址无效：{err}")))?;
    let cookies = window
        .cookies_for_url(url)
        .map_err(|err| AppError::Internal(format!("读取抖音 Cookie 失败：{err}")))?;
    let mut jar: BTreeMap<String, String> = BTreeMap::new();
    for cookie in cookies {
        let domain = cookie.domain().unwrap_or_default();
        let domain_ok =
            domain.is_empty() || domain == "douyin.com" || domain.ends_with(".douyin.com");
        if !domain_ok || cookie.name().is_empty() || cookie.value().is_empty() {
            continue;
        }
        jar.insert(cookie.name().to_string(), cookie.value().to_string());
    }
    if jar.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        jar.iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; "),
    ))
}

fn open_window(app: &AppHandle) -> Result<tauri::WebviewWindow, AppError> {
    let url = tauri::Url::parse(LOGIN_URL)
        .map_err(|err| AppError::Internal(format!("抖音登录地址无效：{err}")))?;
    if let Some(window) = app.get_webview_window(LOGIN_WINDOW) {
        // 抖音登录页是桌面双栏布局，窗口太窄会把二维码和密码登录裁掉。
        let _ = window.set_size(tauri::LogicalSize::new(980.0, 760.0));
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        // 用户可能停在上一次的页面；重新打开时回到官方登录页。
        let _ = window.navigate(url);
        return Ok(window);
    }
    WebviewWindowBuilder::new(app, LOGIN_WINDOW, WebviewUrl::External(url))
        .title("抖音登录")
        .inner_size(980.0, 760.0)
        .min_inner_size(820.0, 600.0)
        .center()
        .build()
        .map_err(|err| AppError::User(format!("打开抖音登录窗口失败：{err}")))
}

/// 打开官方登录页。返回给前端的 svg 为空：二维码由真实网页渲染，前端只显示说明。
pub fn open_login(app: &AppHandle) -> Result<QrTicket, AppError> {
    let _window = open_window(app)?;
    set_started_at(now_ts());
    tracing::info!("打开抖音官方登录窗口");
    Ok(QrTicket {
        qrcode_key: LOGIN_WINDOW.to_string(),
        url: LOGIN_URL.to_string(),
        svg: String::new(),
        expires_in: LOGIN_TTL,
    })
}

pub fn cancel_login(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(LOGIN_WINDOW) {
        let _ = window.close();
    }
    set_started_at(0);
}

fn pending(message: &str, expires_in: i64) -> QrPoll {
    QrPoll {
        state: "pending".into(),
        message: message.to_string(),
        expires_in,
        credential: None,
    }
}

fn expired(message: &str) -> QrPoll {
    QrPoll {
        state: "expired".into(),
        message: message.to_string(),
        expires_in: 0,
        credential: None,
    }
}

/// 轮询一次登录窗口：检测到 sessionid 后校验身份并落库。
pub async fn poll_login(db: &Db, app: &AppHandle, qrcode_key: &str) -> Result<QrPoll, AppError> {
    if qrcode_key != LOGIN_WINDOW {
        return Err(AppError::User("抖音登录会话已失效，请重新打开".into()));
    }
    let Some(window) = app.get_webview_window(LOGIN_WINDOW) else {
        return Ok(expired("登录窗口已关闭；如需继续请重新点击扫码登录"));
    };
    let started = started_at();
    let elapsed = now_ts() - started;
    let left = (LOGIN_TTL - elapsed).max(0);
    if started > 0 && left == 0 {
        let _ = window.close();
        set_started_at(0);
        return Ok(expired("登录已超时，请重新点击扫码登录"));
    }

    let cookie = match window_cookie(&window) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(error = %err, "读取抖音登录窗口 Cookie 失败");
            return Ok(pending("正在等待抖音登录窗口就绪…", left));
        }
    };
    let Some(cookie) = cookie else {
        return Ok(pending("请在抖音登录窗口完成扫码或验证码登录…", left));
    };
    if !has_session_cookie(&cookie) {
        return Ok(pending("请在抖音登录窗口完成扫码或验证码登录…", left));
    }

    // 拿到 sessionid 后还要过 profile 校验，避免页面刚写入半套 Cookie 就落库。
    let _guard = CONFIRM_LOCK.lock().await;
    // 等锁期间用户可能已经关闭窗口或换号，重新读一次。
    let cookie = match window_cookie(&window) {
        Ok(Some(cookie)) if has_session_cookie(&cookie) => cookie,
        _ => return Ok(pending("正在确认登录状态…", left)),
    };
    match validate_cookie(&cookie).await {
        Ok(account) => {
            let row = crate::account::store_account(db, "douyin", &cookie, &account)?;
            mark_fresh(db, app, "登录态有效");
            let _ = window.close();
            set_started_at(0);
            tracing::info!(account = %account.name, uid = %account.uid, "抖音扫码登录成功");
            Ok(QrPoll {
                state: "confirmed".into(),
                message: format!("已登录：{}", account.name),
                expires_in: 0,
                credential: Some(crate::account::info_for_row(db, &row)),
            })
        }
        Err(AppError::User(message)) => {
            // sessionid 可能先于登录资料写入；在 TTL 内继续等，不把半登录态落库。
            tracing::debug!(error = %message, "抖音登录态尚未确认，继续等待");
            Ok(pending("已检测到登录，正在确认账号信息…", left))
        }
        Err(err) => {
            tracing::warn!(error = %err, "抖音登录态确认暂时失败");
            Ok(pending("抖音接口暂时不可用，正在重试…", left))
        }
    }
}

fn map_profile_error(err: PlatformError) -> AppError {
    match err {
        PlatformError::Throttled { retry_after } => {
            AppError::User(format!("抖音接口请求过快，请 {retry_after} 秒后重试"))
        }
        _ => AppError::Platform {
            code: "douyin_upstream",
            message: "抖音接口暂时不可用，请稍后重试".into(),
        },
    }
}

async fn fetch_profile(cookie: &str) -> Result<ProfileEnvelope, AppError> {
    let headers = [
        ("Cookie", cookie),
        ("Accept", "application/json, text/plain, */*"),
    ];
    crate::platform::http::get_json(PROFILE, HOME, &headers)
        .await
        .map_err(map_profile_error)
}

fn value_string(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        _ => String::new(),
    }
}

fn first_avatar(user: &Value) -> Option<String> {
    ["avatar_thumb", "avatar_larger", "avatar_medium"]
        .iter()
        .find_map(|key| {
            user.get(*key)
                .and_then(|avatar| avatar.get("url_list"))
                .and_then(|list| list.as_array())
                .and_then(|list| list.first())
                .and_then(|url| url.as_str())
                .filter(|url| !url.is_empty())
                .map(str::to_string)
        })
}

fn account_from_user(user: &Value) -> Result<Account, AppError> {
    let uid = value_string(user, "uid");
    if uid.is_empty() {
        return Err(AppError::User("抖音登录信息不完整，请重新扫码".into()));
    }
    let nickname = value_string(user, "nickname");
    Ok(Account {
        uid: uid.clone(),
        name: if nickname.is_empty() {
            format!("抖音用户 {uid}")
        } else {
            nickname
        },
        avatar: first_avatar(user),
    })
}

/// 用 Cookie 查 self profile；未登录返回用户可读的失效原因。
pub async fn validate_cookie(raw: &str) -> Result<Account, AppError> {
    let cookie = normalize_cookie(raw)?;
    let envelope = fetch_profile(&cookie).await?;
    if envelope.status_code == 8 {
        return Err(AppError::User("抖音 Cookie 已失效或未登录".into()));
    }
    if envelope.status_code != 0 {
        return Err(AppError::Platform {
            code: "douyin_upstream",
            message: format!(
                "抖音接口返回错误（code {}）：{}",
                envelope.status_code, envelope.status_msg
            ),
        });
    }
    let Some(user) = envelope.user else {
        return Err(AppError::User("抖音 Cookie 已失效或未登录".into()));
    };
    account_from_user(&user)
}

/// 合并 Set-Cookie：只取 name=value，空值不覆盖（避免平台清理 Cookie 时误删登录态）。
fn merge_set_cookies(base: &str, set_cookies: &[String]) -> (String, usize) {
    let mut jar: BTreeMap<String, String> = BTreeMap::new();
    for chunk in base.split(';') {
        if let Some((name, value)) = chunk.trim().split_once('=') {
            let name = name.trim();
            let value = value.trim();
            if !name.is_empty() && !value.is_empty() {
                jar.insert(name.to_string(), value.to_string());
            }
        }
    }
    let mut changed = 0usize;
    for raw in set_cookies {
        let Some(first) = raw.split(';').next() else {
            continue;
        };
        let Some((name, value)) = first.trim().split_once('=') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim().trim_matches('"');
        if name.is_empty() || value.is_empty() {
            continue;
        }
        match jar.get(name) {
            Some(old) if old == value => {}
            Some(_) => {
                jar.insert(name.to_string(), value.to_string());
                changed += 1;
            }
            None => {
                jar.insert(name.to_string(), value.to_string());
                changed += 1;
            }
        }
    }
    let merged = jar
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; ");
    (merged, changed)
}

/// 访问一次主页，吸收服务端可能轮换的 Cookie。失败只是少一次轮换，不影响校验结果。
async fn rotate_from_home(cookie: &str) -> Result<(String, usize), PlatformError> {
    let header = HeaderValue::from_str(cookie)
        .map_err(|_| PlatformError::Network("抖音 Cookie 含非法字符".into()))?;
    let response = dy_client()
        .get(HOME)
        .header(REFERER, HOME)
        .header(
            ACCEPT,
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header(COOKIE, header)
        .send()
        .await
        .map_err(|err| PlatformError::Network(format!("抖音主页请求失败：{err}")))?;
    if !response.status().is_success() {
        return Err(PlatformError::Network(format!(
            "抖音主页返回 HTTP {}",
            response.status()
        )));
    }
    let set_cookies = response
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(str::to_string)
        .collect::<Vec<_>>();
    Ok(merge_set_cookies(cookie, &set_cookies))
}

fn find_row(db: &Db) -> Option<CredentialRow> {
    db.credentials()
        .ok()?
        .into_iter()
        .find(|row| row.platform == "douyin")
}

fn write_status(db: &Db, status: &RefreshStatus) {
    crate::account::save_refresh_status(db, "douyin", status);
}

/// 复验通过：更新「上次检查」，保留上一次真正轮换的时间。
fn mark_fresh(db: &Db, app: &AppHandle, message: &str) {
    let before = crate::account::load_refresh_status(db, "douyin");
    write_status(
        db,
        &RefreshStatus {
            checked_at: now_ts(),
            outcome: "fresh".into(),
            message: Some(message.to_string()),
            key_stale: false,
            last_refreshed_at: before.last_refreshed_at,
        },
    );
    crate::account::emit_account_refresh(app);
}

fn refresh_due(row: &CredentialRow, status: &RefreshStatus, force: bool) -> bool {
    if force {
        return true;
    }
    if status.outcome == "failed" {
        return now_ts() >= status.checked_at + RETRY_FAILED;
    }
    now_ts() >= row.updated_at.max(status.last_refreshed_at) + REFRESH_EVERY
}

fn mark_expired_locked(db: &Db, app: &AppHandle, row: &CredentialRow) -> bool {
    if row.status == "expired" {
        return false;
    }
    if credential::get("douyin").as_deref() != Some(row.cookie.as_str()) {
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
            let notice = AppNotice {
                kind: "credential_expired".into(),
                room_id: String::new(),
                host_name: row.account_name.clone(),
                detail: Some("douyin".into()),
                suppress: false,
            };
            if let Err(err) = app.emit(crate::recorder::EVENT_APP_NOTICE, notice) {
                tracing::warn!(error = %err, "抖音登录失效通知发送失败");
            }
            crate::account::emit_account_refresh(app);
            true
        }
        Ok(false) => false,
        Err(err) => {
            tracing::warn!(error = %err, "标记抖音账号过期失败");
            false
        }
    }
}

fn set_invalid_status(db: &Db, message: &str) {
    write_status(
        db,
        &RefreshStatus {
            checked_at: now_ts(),
            outcome: "failed".into(),
            message: Some(message.to_string()),
            key_stale: true,
            last_refreshed_at: crate::account::load_refresh_status(db, "douyin").last_refreshed_at,
        },
    );
}

fn record_success(
    db: &Db,
    app: &AppHandle,
    row: &CredentialRow,
    account: &Account,
    cookie: String,
    changed: usize,
) -> bool {
    let _guard = crate::account::credential_write_guard();
    if credential::get("douyin").as_deref() != Some(row.cookie.as_str()) {
        tracing::info!("抖音保活期间 Cookie 已更新，丢弃本次结果");
        return false;
    }
    if changed > 0 {
        let updated = CredentialRow {
            cookie: cookie.clone(),
            status: "ok".to_string(),
            account_name: account.name.clone(),
            avatar: account.avatar.clone(),
            updated_at: now_ts(),
            ..row.clone()
        };
        if let Err(err) = db.save_credential(&updated) {
            tracing::warn!(error = %err, "保存抖音轮换后的 Cookie 失败");
            return false;
        }
        credential::set("douyin", &cookie);
        tracing::info!(changed, "抖音 Cookie 已同步并轮换");
    }
    let last_refreshed_at = if changed > 0 {
        now_ts()
    } else {
        crate::account::load_refresh_status(db, "douyin").last_refreshed_at
    };
    write_status(
        db,
        &RefreshStatus {
            checked_at: now_ts(),
            outcome: if changed > 0 { "refreshed" } else { "fresh" }.into(),
            message: Some(if changed > 0 {
                format!("登录态有效，已同步 {changed} 个 Cookie")
            } else {
                "登录态有效，暂不需要更新 Cookie".into()
            }),
            key_stale: false,
            last_refreshed_at,
        },
    );
    crate::account::emit_account_refresh(app);
    true
}

async fn refresh_once(db: &Db, app: &AppHandle, force: bool) -> RefreshOutcome {
    let Some(row) = find_row(db) else {
        return RefreshOutcome::NoCredential;
    };
    if !has_session_cookie(&row.cookie) {
        return RefreshOutcome::Fresh;
    }
    let before = crate::account::load_refresh_status(db, "douyin");
    if !refresh_due(&row, &before, force) {
        return RefreshOutcome::Fresh;
    }
    let account = match validate_cookie(&row.cookie).await {
        Ok(account) => account,
        Err(AppError::User(message)) => {
            tracing::warn!(error = %message, "抖音登录态已失效");
            {
                let _guard = crate::account::credential_write_guard();
                mark_expired_locked(db, app, &row);
            }
            set_invalid_status(db, &message);
            crate::account::emit_account_refresh(app);
            return RefreshOutcome::Invalid;
        }
        Err(err) => {
            tracing::warn!(error = %err, "抖音登录态检查暂时失败");
            write_status(
                db,
                &RefreshStatus {
                    checked_at: now_ts(),
                    outcome: "failed".into(),
                    message: Some(crate::account::short_error(&err.to_string())),
                    key_stale: false,
                    last_refreshed_at: before.last_refreshed_at,
                },
            );
            crate::account::emit_account_refresh(app);
            return RefreshOutcome::Fresh;
        }
    };
    let (cookie, changed) = match rotate_from_home(&row.cookie).await {
        Ok(result) => result,
        Err(err) => {
            tracing::debug!(error = %err, "抖音 Cookie 轮换请求失败，保留现有 Cookie");
            (row.cookie.clone(), 0)
        }
    };
    if !record_success(db, app, &row, &account, cookie, changed) {
        return RefreshOutcome::Discarded;
    }
    if changed > 0 {
        RefreshOutcome::Refreshed
    } else {
        RefreshOutcome::Fresh
    }
}

/// 设置页「立即检查」：强制复验一次，并尝试同步 Cookie。
pub async fn refresh_now(db: &Db, app: &AppHandle) -> Result<RefreshStatus, AppError> {
    if find_row(db).is_none() {
        return Err(AppError::User("尚未登录抖音".into()));
    }
    let outcome = refresh_once(db, app, true).await;
    if outcome == RefreshOutcome::NoCredential {
        return Err(AppError::User("尚未登录抖音".into()));
    }
    Ok(crate::account::load_refresh_status(db, "douyin"))
}

/// 定期复验：只读 profile，不主动轮换；失效时标记过期并通知。
pub async fn revalidate(db: &Db, app: &AppHandle) {
    let Some(row) = find_row(db) else {
        return;
    };
    // 老版本可能存过不含 sessionid 的抖音 Cookie（只用于取流预热）。这类凭据
    // 不参与账号登录，不能因为「缺少 sessionid」就标成过期。
    if !has_session_cookie(&row.cookie) {
        return;
    }
    match validate_cookie(&row.cookie).await {
        Ok(account) => {
            {
                let _guard = crate::account::credential_write_guard();
                if credential::get("douyin").as_deref() != Some(row.cookie.as_str()) {
                    return;
                }
                let changed = row.status != "ok"
                    || row.account_name != account.name
                    || row.avatar != account.avatar;
                if changed {
                    match db.update_credential_profile(
                        "douyin",
                        "ok",
                        &account.name,
                        account.avatar.as_deref(),
                        now_ts(),
                    ) {
                        Ok(true) => {
                            credential::set_status("douyin", "ok");
                            tracing::info!(account = %account.name, "抖音登录态复验通过");
                        }
                        Ok(false) => {}
                        Err(err) => tracing::warn!(error = %err, "更新抖音账号信息失败"),
                    }
                }
            }
            mark_fresh(db, app, "登录态有效");
        }
        Err(AppError::User(message)) => {
            let _guard = crate::account::credential_write_guard();
            if mark_expired_locked(db, app, &row) {
                tracing::warn!(error = %message, "抖音登录态已失效，已标记过期");
            }
            set_invalid_status(db, &message);
        }
        Err(err) => {
            tracing::debug!(error = %err, "抖音登录态复验暂时失败");
        }
    }
}

/// 独立维护循环：先复验，再按 6 小时节奏尝试同步 Cookie；不触碰 B 站/斗鱼循环。
pub async fn maintenance_loop(db: std::sync::Arc<Db>, app: AppHandle) {
    tokio::time::sleep(Duration::from_secs(20)).await;
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
    fn merge_cookies_replaces_changed_values_and_keeps_others() {
        let (merged, changed) = merge_set_cookies(
            "sessionid=old; ttwid=t1; odin_tt=o1",
            &[
                "sessionid=new; Path=/; HttpOnly".into(),
                "ttwid=t1; Path=/".into(),
                "passport_csrf_token=p1; Path=/".into(),
                "empty=; Max-Age=0".into(),
            ],
        );
        assert_eq!(changed, 2);
        assert!(merged.contains("sessionid=new"));
        assert!(merged.contains("odin_tt=o1"));
        assert!(merged.contains("passport_csrf_token=p1"));
        assert!(!merged.contains("empty="));
    }

    #[test]
    fn session_cookie_requires_sessionid_or_ss() {
        assert!(!has_session_cookie("ttwid=x; odin_tt=y"));
        assert!(has_session_cookie("ttwid=x; sessionid=s1"));
        assert!(has_session_cookie("sessionid_ss=s2; ttwid=x"));
    }

    #[test]
    fn account_reads_uid_nickname_and_nested_avatar() {
        let user: Value = serde_json::from_str(
            r#"{"uid":"123","nickname":"阿抖","avatar_thumb":{"url_list":["https://a/b.jpg"]}}"#,
        )
        .unwrap();
        let account = account_from_user(&user).unwrap();
        assert_eq!(account.uid, "123");
        assert_eq!(account.name, "阿抖");
        assert_eq!(account.avatar.as_deref(), Some("https://a/b.jpg"));
    }

    #[test]
    fn account_without_uid_is_rejected() {
        let user: Value = serde_json::from_str(r#"{"nickname":"阿抖"}"#).unwrap();
        assert!(account_from_user(&user).is_err());
    }

    #[test]
    fn refresh_due_uses_failed_backoff_and_regular_interval() {
        let row = CredentialRow {
            platform: "douyin".into(),
            account_id: String::new(),
            cookie: "sessionid=x".into(),
            uid: "1".into(),
            account_name: "a".into(),
            avatar: None,
            status: "ok".into(),
            updated_at: now_ts(),
        };
        let failed = RefreshStatus {
            checked_at: now_ts(),
            outcome: "failed".into(),
            key_stale: false,
            last_refreshed_at: 0,
            message: None,
        };
        assert!(!refresh_due(&row, &failed, false));
        assert!(refresh_due(&row, &failed, true));
        let fresh = RefreshStatus {
            checked_at: 0,
            outcome: "fresh".into(),
            ..failed
        };
        assert!(!refresh_due(&row, &fresh, false));
    }
}
