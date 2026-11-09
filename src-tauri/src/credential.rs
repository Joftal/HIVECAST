//! 账号凭据：Cookie 的标准化、按平台存取，以及注入 HTTP 请求的入口。
//!
//! 落库时由 db 层用 AES-256-GCM 加密（enc:v1: 前缀，密钥见 secret.rs），
//! 这一层拿到的 cookie 始终是明文，只管解析、合并、按域名分发。

use std::collections::BTreeMap;
use std::sync::{OnceLock, RwLock};

static STORE: OnceLock<RwLock<BTreeMap<String, String>>> = OnceLock::new();

fn store() -> &'static RwLock<BTreeMap<String, String>> {
    STORE.get_or_init(|| RwLock::new(BTreeMap::new()))
}

/// 启动时把库里的凭据灌进内存，之后 HTTP 层直接按 URL 取用。
pub fn load_from_db(db: &crate::db::Db) {
    let rows = match db.credentials() {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!(error = %err, "读取账号凭据失败");
            return;
        }
    };
    let mut guard = store().write().expect("凭据表被污染");
    guard.clear();
    for row in rows {
        guard.insert(row.platform, row.cookie);
    }
    tracing::info!(count = guard.len(), "已加载账号凭据");
}

pub fn set(platform: &str, cookie: &str) {
    store()
        .write()
        .expect("凭据表被污染")
        .insert(platform.to_string(), cookie.to_string());
}

pub fn remove(platform: &str) {
    store().write().expect("凭据表被污染").remove(platform);
}

pub fn get(platform: &str) -> Option<String> {
    store().read().expect("凭据表被污染").get(platform).cloned()
}

/// 按请求 URL 的域名找对应平台的 Cookie。
///
/// 只认平台自己的接口域名：流地址落在 CDN（bilivideo.com、douyucdn.cn 等）上，
/// 不该把登录态发给第三方节点。
pub fn cookie_for_url(url: &str) -> Option<String> {
    let host = url::Url::parse(url).ok()?.host_str()?.to_lowercase();
    let platform = platform_for_host(&host)?;
    get(platform)
}

pub fn platform_for_host(host: &str) -> Option<&'static str> {
    let host = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
    let matches = |suffix: &str| host == suffix || host.ends_with(&format!(".{suffix}"));
    if matches("bilibili.com") || matches("b23.tv") {
        Some("bilibili")
    } else if matches("douyu.com") || matches("douyu.tv") {
        Some("douyu")
    } else if matches("huya.com") {
        Some("huya")
    } else if matches("douyin.com") || matches("iesdouyin.com") || matches("douyincdn.com") {
        Some("douyin")
    } else {
        None
    }
}

/// 兼容用户从整理文件里直接复制的「哔哩哔哩：SESSDATA=…」格式。
fn strip_platform_prefix(text: &str) -> &str {
    for label in [
        "哔哩哔哩",
        "斗鱼",
        "虎牙",
        "抖音",
        "bilibili",
        "douyu",
        "huya",
        "douyin",
    ] {
        if let Some(rest) = text.strip_prefix(label) {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix(':').or_else(|| rest.strip_prefix('：')) {
                let rest = rest.trim_start();
                if !rest.is_empty() {
                    return rest;
                }
            }
        }
    }
    text
}

/// 把用户粘贴的内容收敛成 a=1; b=2 形态。
///
/// 兼容：浏览器 DevTools 复制的一整行、只复制了 Cookie: 后面的值、
/// 以及从别处拷来的多行（有的工具按行输出 name=value;）。
pub fn normalize(raw: &str) -> Result<String, String> {
    let text = raw.trim();
    if text.is_empty() {
        return Err("Cookie 为空".into());
    }
    if text.len() > 16_384 {
        return Err("Cookie 过长（超过 16 KB）".into());
    }
    let text = text
        .strip_prefix("Cookie:")
        .or_else(|| text.strip_prefix("cookie:"))
        .unwrap_or(text);
    let text = strip_platform_prefix(text);
    let mut pairs: Vec<(String, String)> = Vec::new();
    for chunk in text.split(';') {
        for line in chunk.split('\n') {
            let piece = line.trim().trim_end_matches(',');
            if piece.is_empty() {
                continue;
            }
            let Some((name, value)) = piece.split_once('=') else {
                return Err(format!("无法识别的 Cookie 片段：{piece}"));
            };
            let name = name.trim();
            if name.is_empty() || name.contains(' ') {
                return Err(format!("Cookie 名不合法：{name}"));
            }
            pairs.push((name.to_string(), value.trim().to_string()));
        }
    }
    if pairs.is_empty() {
        return Err("Cookie 中没有 name=value".into());
    }
    let mut merged: Vec<(String, String)> = Vec::new();
    for (name, value) in pairs {
        match merged.iter_mut().find(|(existing, _)| existing == &name) {
            Some(slot) => slot.1 = value,
            None => merged.push((name, value)),
        }
    }
    Ok(merged
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; "))
}

/// 合并两段 Cookie，后来者覆盖同名项（扫码拿到的 Set-Cookie 要并进旧 Cookie）。
pub fn merge(base: &str, extra: &str) -> String {
    let mut map: Vec<(String, String)> = Vec::new();
    for part in [base, extra] {
        for chunk in part.split(';') {
            let piece = chunk.trim();
            if piece.is_empty() {
                continue;
            }
            if let Some((name, value)) = piece.split_once('=') {
                let name = name.trim().to_string();
                match map.iter_mut().find(|(existing, _)| existing == &name) {
                    Some(slot) => slot.1 = value.trim().to_string(),
                    None => map.push((name, value.trim().to_string())),
                }
            }
        }
    }
    map.into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// 给界面展示的条目名（不含值，避免把登录态渲染到屏幕上）。
pub fn names(cookie: &str) -> Vec<String> {
    let mut names: Vec<String> = cookie
        .split(';')
        .filter_map(|chunk| chunk.split_once('='))
        .map(|(name, _)| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect();
    names.sort();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_devtools_line() {
        let raw = "SESSDATA=abc%2Cdef; bili_jct=token; DedeUserID=42";
        assert_eq!(
            normalize(raw).unwrap(),
            "SESSDATA=abc%2Cdef; bili_jct=token; DedeUserID=42"
        );
    }

    #[test]
    fn strips_prefix_and_multiline() {
        let raw = "Cookie: dy_did=1\n; acf_uid=2;";
        assert_eq!(normalize(raw).unwrap(), "dy_did=1; acf_uid=2");
    }

    #[test]
    fn strips_platform_label_prefix() {
        assert_eq!(
            normalize("哔哩哔哩：SESSDATA=a; b=2").unwrap(),
            "SESSDATA=a; b=2"
        );
        assert_eq!(
            normalize("抖音: ttwid=x; uid_tt=y").unwrap(),
            "ttwid=x; uid_tt=y"
        );
        // 普通 Cookie 名里带平台字样不能被误删。
        assert_eq!(normalize("huya_uid=1; b=2").unwrap(), "huya_uid=1; b=2");
    }

    #[test]
    fn keeps_last_duplicate() {
        assert_eq!(normalize("a=1; b=2; a=3").unwrap(), "a=3; b=2");
    }

    #[test]
    fn rejects_garbage() {
        assert!(normalize("   ").is_err());
        assert!(normalize("SESSDATA").is_err());
        assert!(normalize("has space=1").is_err());
    }

    #[test]
    fn merge_overrides_by_name() {
        let base = "SESSDATA=old; bili_jct=j1; DedeUserID=42";
        let extra = "SESSDATA=new; sid=s9";
        assert_eq!(
            merge(base, extra),
            "SESSDATA=new; bili_jct=j1; DedeUserID=42; sid=s9"
        );
    }

    #[test]
    fn host_mapping_ignores_cdn() {
        assert_eq!(platform_for_host("api.live.bilibili.com"), Some("bilibili"));
        assert_eq!(platform_for_host("www.douyu.com:443"), Some("douyu"));
        assert_eq!(platform_for_host("d1--cn-gotcha09.bilivideo.com"), None);
        assert_eq!(platform_for_host("live.douyin.com"), Some("douyin"));
        assert_eq!(platform_for_host("example.com"), None);
    }

    #[test]
    fn cookie_only_goes_to_platform_hosts() {
        set("douyu", "dy_did=abc");
        assert_eq!(
            cookie_for_url("https://www.douyu.com/9999").as_deref(),
            Some("dy_did=abc")
        );
        // CDN 域名不匹配平台，避免把登录态发给第三方节点。
        assert_eq!(
            cookie_for_url("https://hw-tct.douyucdn.cn/live/x.flv"),
            None
        );
        remove("douyu");
        assert_eq!(cookie_for_url("https://www.douyu.com/9999"), None);
    }

    #[test]
    fn names_are_sorted_and_unique() {
        assert_eq!(
            names("bili_jct=x; SESSDATA=y; bili_jct=z"),
            vec!["SESSDATA".to_string(), "bili_jct".to_string()]
        );
    }
}
