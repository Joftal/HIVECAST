//! 平台账号：B 站扫码登录、Cookie 校验与落库。
//!
//! 扫码流程（对齐 passport 的 web 端）：
//! 1. generate 拿 qrcode_key + 二维码内容；
//! 2. 轮询 poll，data.code 为 0 时登录成功，登录态只出现在响应的 Set-Cookie 里；
//! 3. 把 Set-Cookie 并进已有 Cookie，再打一次 nav 接口确认身份，最后落库。
//!
//! 其余平台没有公开的扫码接口（斗鱼/虎牙/抖音都依赖风控），v1 走粘贴 Cookie。

use std::time::Duration;

use hivecast_ipc::{AppNotice, CredentialInfo, QrPoll, QrTicket};
use serde::Deserialize;
use tauri::{Emitter, Manager};

use crate::credential;
use crate::db::{now_ts, CredentialRow, Db};
use crate::error::AppError;
use crate::platform::http;

const PASSPORT: &str = "https://passport.bilibili.com";
const API: &str = "https://api.bilibili.com";
/// 运行中定期复验登录态：B 站 Cookie 可能在长时间录制中失效，只在启动时查一次不够。
pub const REVALIDATE_INTERVAL: Duration = Duration::from_secs(30 * 60);
const REFERER: &str = "https://www.bilibili.com/";

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
    /// 距离二维码失效还剩多少秒；B 站轮询接口每次都带。
    #[serde(default)]
    expires_in: i64,
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
    CredentialInfo {
        platform: row.platform.clone(),
        account_id: row.account_id.clone(),
        account_name: row.account_name.clone(),
        uid: row.uid.clone(),
        avatar: row.avatar.clone(),
        cookie_names: credential::names(&row.cookie),
        status: row.status.clone(),
        updated_at: row.updated_at,
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

fn render_qr(content: &str) -> Result<String, AppError> {
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

/// 轮询扫码状态；已确认时顺带把登录态落库。
pub async fn bilibili_qr_poll(db: &Db, qrcode_key: &str) -> Result<QrPoll, AppError> {
    let key = qrcode_key.trim();
    if key.is_empty() {
        tracing::warn!(platform = "bilibili", "扫码轮询缺少 qrcode_key");
        return Err(AppError::User("缺少 qrcode_key".into()));
    }
    let url = format!(
        "{PASSPORT}/x/passport-login/web/qrcode/poll?qrcode_key={key}&source=main-fe-header"
    );
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
        let extra = cookies_from_set_cookie(&fetched.set_cookies);
        let base = credential::get("bilibili").unwrap_or_default();
        let merged = credential::merge(&base, &extra);
        let account = bilibili_nav(&merged).await?;
        let row = store_account(db, "bilibili", &merged, &account)?;
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

fn store_account(
    db: &Db,
    platform: &str,
    cookie: &str,
    account: &Account,
) -> Result<CredentialRow, AppError> {
    tracing::debug!(platform, account = %account.name, uid = %account.uid, "保存平台凭据");
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
    } else {
        Account {
            uid: String::new(),
            name: format!("{} Cookie", platform_name(platform)),
            avatar: None,
        }
    };

    let row = store_account(db, platform, &cookie, &account)?;
    tracing::info!(platform, account = %row.account_name, "Cookie 已保存");
    Ok(info_from_row(&row))
}

pub fn delete(db: &Db, platform: &str) -> Result<(), AppError> {
    db.delete_credential(platform).map_err(|err| {
        tracing::warn!(platform, error = %err, "删除平台凭据失败");
        err
    })?;
    credential::remove(platform);
    tracing::info!(platform, "已删除平台凭据");
    Ok(())
}

pub fn list(db: &Db) -> Result<Vec<CredentialInfo>, AppError> {
    Ok(db.credentials()?.iter().map(info_from_row).collect())
}

/// 启动时与运行中定期复验 B 站登录态：过期就标 expired，设置页会提示重新扫码。
/// 登录态变化后清空取流缓存，避免继续使用旧画质/旧 Cookie 换来的候选地址。
fn clear_stream_cache(app: &tauri::AppHandle) {
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
                        tracing::info!(
                            platform = "bilibili",
                            account = %account.name,
                            "B 站登录态复验通过，已更新账号信息"
                        );
                        clear_stream_cache(app);
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
                    Ok(true) => clear_stream_cache(app),
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
