//! 平台账号：B 站扫码登录、Cookie 校验与落库。
//!
//! 扫码流程（对齐 passport 的 web 端）：
//! 1. generate 拿 qrcode_key + 二维码内容；
//! 2. 轮询 poll，data.code 为 0 时登录成功，登录态只出现在响应的 Set-Cookie 里；
//! 3. 把 Set-Cookie 并进已有 Cookie，再打一次 nav 接口确认身份，最后落库。
//!
//! 其余平台没有公开的扫码接口（斗鱼/虎牙/抖音都依赖风控），v1 走粘贴 Cookie。

use hivecast_ipc::{CredentialInfo, QrPoll, QrTicket};
use serde::Deserialize;

use crate::credential;
use crate::db::{now_ts, CredentialRow, Db};
use crate::error::AppError;
use crate::platform::http;

const PASSPORT: &str = "https://passport.bilibili.com";
const API: &str = "https://api.bilibili.com";
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

/// 开始扫码：返回二维码内容、key 与后端渲染好的 SVG。
pub async fn bilibili_qr_start() -> Result<QrTicket, AppError> {
    let url = format!("{PASSPORT}/x/passport-login/web/qrcode/generate?source=main-fe-header");
    let envelope: Envelope<QrData> = http::get_json(&url, REFERER, &[])
        .await
        .map_err(AppError::from)?;
    let data = match (envelope.code, envelope.data) {
        (0, Some(data)) => data,
        (code, _) => {
            return Err(AppError::User(format!(
                "生成二维码失败（code {code}）{}",
                envelope.message.unwrap_or_default()
            )))
        }
    };
    let svg = render_qr(&data.url)?;
    Ok(QrTicket {
        qrcode_key: data.qrcode_key,
        url: data.url,
        svg,
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
        return Err(AppError::User("缺少 qrcode_key".into()));
    }
    let url = format!(
        "{PASSPORT}/x/passport-login/web/qrcode/poll?qrcode_key={key}&source=main-fe-header"
    );
    let fetched: http::FetchedWithHeaders<Envelope<PollData>> =
        http::get_json_with_headers(&url, REFERER, &[])
            .await
            .map_err(AppError::from)?;

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

    if state == "confirmed" {
        let extra = cookies_from_set_cookie(&fetched.set_cookies);
        let base = credential::get("bilibili").unwrap_or_default();
        let merged = credential::merge(&base, &extra);
        let account = bilibili_nav(&merged).await?;
        let row = store_account(db, "bilibili", &merged, &account)?;
        return Ok(QrPoll {
            state: state.to_string(),
            message: format!("已登录：{}", row.account_name),
            credential: Some(info_from_row(&row)),
        });
    }

    Ok(QrPoll {
        state: state.to_string(),
        message,
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
            if name.is_empty() || name.eq_ignore_ascii_case("path") || name.eq_ignore_ascii_case("expires") {
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
            let hint = if code == -101 {
                "Cookie 已失效或未登录".to_string()
            } else {
                format!(
                    "Cookie 无效（code {code}）{}",
                    envelope.message.unwrap_or_default()
                )
            };
            return Err(AppError::User(hint));
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

fn store_account(db: &Db, platform: &str, cookie: &str, account: &Account) -> Result<CredentialRow, AppError> {
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
    db.save_credential(&row)?;
    credential::set(platform, cookie);
    Ok(row)
}

/// 保存用户粘贴的 Cookie：先规范化，B 站再验一次身份，其余平台直接存。
pub async fn save_cookie(db: &Db, platform: &str, raw: &str) -> Result<CredentialInfo, AppError> {
    let platform = platform.trim();
    if !matches!(platform, "bilibili" | "douyu" | "huya" | "douyin") {
        return Err(AppError::User(format!("不支持的平台：{platform}")));
    }
    let cookie = credential::normalize(raw).map_err(AppError::User)?;

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
    Ok(info_from_row(&row))
}

pub fn delete(db: &Db, platform: &str) -> Result<(), AppError> {
    db.delete_credential(platform)?;
    credential::remove(platform);
    Ok(())
}

pub fn list(db: &Db) -> Result<Vec<CredentialInfo>, AppError> {
    Ok(db.credentials()?.iter().map(info_from_row).collect())
}

/// 启动后异步复验 B 站登录态：过期就标 expired，设置页会提示重新扫码。
pub async fn revalidate(db: &Db) {
    let rows = match db.credentials() {
        Ok(rows) => rows,
        Err(_) => return,
    };
    for row in rows {
        if row.platform != "bilibili" {
            continue;
        }
        match bilibili_nav(&row.cookie).await {
            Ok(account) => {
                if account.name != row.account_name || row.status != "ok" {
                    let updated = CredentialRow {
                        account_name: account.name,
                        avatar: account.avatar,
                        status: "ok".to_string(),
                        ..row
                    };
                    let _ = db.save_credential(&updated);
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "B 站登录态复验失败");
                let updated = CredentialRow {
                    status: "expired".to_string(),
                    ..row
                };
                let _ = db.save_credential(&updated);
            }
        }
    }
    credential::load_from_db(db);
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
        assert_eq!(
            credential::merge(base, &extra),
            "SESSDATA=new; dy_did=keep"
        );
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
