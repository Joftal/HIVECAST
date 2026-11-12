//! 共享 HTTP 客户端：统一 UA、超时、gzip，并保留后续接 Cookie/代理的位置。

use std::time::Duration;

use serde::de::DeserializeOwned;

use crate::platform::PlatformError;

/// 平台接口对 UA 比较敏感，统一在这里维护。
pub const USER_AGENT: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

pub fn client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(10))
            .gzip(true)
            .build()
            .expect("构建 HTTP 客户端失败")
    })
}

/// 带响应元信息的结果。斗鱼签名要求用服务器时间（响应 Date 头）而不是本地时钟，
/// 所以这里把 Date 一并带出来。
pub struct Fetched<T> {
    pub data: T,
    /// 响应 Date 头对应的 Unix 秒
    pub date: Option<i64>,
}

const SENSITIVE_PATH_KEYS: [&str; 14] = [
    "access_token",
    "refresh_token",
    "authorization",
    "password",
    "passwd",
    "sessionid",
    "session",
    "ticket",
    "secret",
    "signature",
    "token",
    "sign",
    "sig",
    "key",
];

fn is_hex(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn looks_like_uuid(value: &str) -> bool {
    let parts: Vec<&str> = value.split('-').collect();
    parts.len() == 5
        && [8, 4, 4, 4, 12]
            .iter()
            .zip(parts.iter())
            .all(|(want, part)| part.len() == *want && is_hex(part))
}

fn looks_like_secret(value: &str) -> bool {
    let len = value.len();
    if len >= 16
        && ["tok_", "token_", "sig_", "sk_", "secret_"]
            .iter()
            .any(|prefix| value.starts_with(prefix))
    {
        return true;
    }
    if len < 24 {
        return false;
    }
    if looks_like_uuid(value) {
        return true;
    }
    if len >= 32 && is_hex(value) {
        return true;
    }
    let allowed = value.bytes().all(|b| {
        b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'%' | b'=' | b'+')
    });
    if len >= 32 && allowed {
        let digits = value.bytes().filter(|b| b.is_ascii_digit()).count();
        let letters = value.bytes().filter(|b| b.is_ascii_alphabetic()).count();
        if digits >= 8 && letters >= 8 {
            return true;
        }
    }
    false
}

fn redact_path_segment(segment: &str) -> String {
    if segment.is_empty() {
        return String::new();
    }
    let lower = segment.to_ascii_lowercase();
    let mut out = String::new();
    let mut cursor = 0;
    let mut redacted = false;
    while cursor < segment.len() {
        let mut hit: Option<(usize, &str)> = None;
        for key in SENSITIVE_PATH_KEYS {
            let needle = format!("{key}=");
            let Some(pos) = lower[cursor..].find(&needle).map(|at| cursor + at) else {
                continue;
            };
            // monkey= 里的 key= 不是参数名；要求前面不是字母或数字。
            let boundary = pos == 0 || !lower.as_bytes()[pos - 1].is_ascii_alphanumeric();
            if boundary && hit.map_or(true, |(best, _)| pos < best) {
                hit = Some((pos, key));
            }
        }
        let Some((pos, key)) = hit else {
            out.push_str(&segment[cursor..]);
            break;
        };
        let value_start = pos + key.len() + 1;
        let value_end = segment[value_start..]
            .find('&')
            .map(|at| value_start + at)
            .unwrap_or(segment.len());
        out.push_str(&segment[cursor..value_start]);
        out.push_str("***");
        cursor = value_end;
        redacted = true;
    }
    if redacted {
        return out;
    }
    if looks_like_secret(segment) {
        return "***".into();
    }
    segment.to_string()
}

fn redact_path(path: &str) -> String {
    path.split('/')
        .map(redact_path_segment)
        .collect::<Vec<_>>()
        .join("/")
}

fn redact_raw_url(raw: &str) -> String {
    // 解析失败时按 scheme://authority/path 手工切分：userinfo 只可能出现在
    // authority 里，不能拿整串的最后一个 '@' 判断，否则 path 里的 '@' 会被误当 userinfo。
    let authority_start = raw.find("://").map(|at| at + 3).unwrap_or(0);
    let authority_end = raw[authority_start..]
        .find('/')
        .map(|at| authority_start + at)
        .unwrap_or(raw.len());
    let authority = &raw[authority_start..authority_end];
    let host = match authority.rfind('@') {
        Some(at) => format!("***@{}", &authority[at + 1..]),
        None => authority.to_string(),
    };
    let path = &raw[authority_end..];
    format!("{}{}{}", &raw[..authority_start], host, redact_path(path))
}

/// 日志/错误里只保留 URL 的 scheme/host/path 与查询参数的键。
///
/// 直播平台的取流地址常把 token、sign、deadline 放在 query 里（B 站、斗鱼都是），
/// 原样写进日志等于把可重放的地址留在磁盘上。这里把值统一替换成 `***`，
/// 既保留排障需要的接口名与参数名，又不泄露凭据。
pub fn redact_url(url: &str) -> String {
    let Ok(mut parsed) = reqwest::Url::parse(url) else {
        let raw = url.split(['?', '#']).next().unwrap_or(url);
        // 解析失败的 URL 也可能带 userinfo；不能只去掉 query 就把原文返回来。
        return redact_raw_url(raw);
    };
    if !parsed.username().is_empty() {
        let _ = parsed.set_username("***");
    }
    if parsed.password().is_some() {
        let _ = parsed.set_password(Some("***"));
    }
    let redacted_path = redact_path(parsed.path());
    if redacted_path != parsed.path() {
        parsed.set_path(&redacted_path);
    }
    if parsed.query().is_some() {
        let keys: Vec<String> = parsed
            .query_pairs()
            .map(|(key, _)| format!("{key}=***"))
            .collect();
        parsed.set_query(Some(&keys.join("&")));
    }
    if parsed.fragment().is_some() {
        parsed.set_fragment(Some("***"));
    }
    parsed.to_string()
}

fn parse_date(headers: &reqwest::header::HeaderMap) -> Option<i64> {
    let value = headers.get(reqwest::header::DATE)?.to_str().ok()?;
    chrono::DateTime::parse_from_rfc2822(value)
        .ok()
        .map(|date| date.timestamp())
}

async fn send(
    request: reqwest::RequestBuilder,
    url: &str,
) -> Result<reqwest::Response, PlatformError> {
    // 统一在这里挂登录态：调用方没显式给 Cookie（扫码校验、登录接口）才用本地存的，
    // 这样既不会把同一个 Cookie 头发两遍，也避免每个适配器都去查一次凭据。
    let mut request = request
        .build()
        .map_err(|err| PlatformError::Network(format!("{} → {err}", redact_url(url))))?;
    if request.headers().get(reqwest::header::COOKIE).is_none() {
        if let Some(cookie) = crate::credential::cookie_for_url(url) {
            match reqwest::header::HeaderValue::from_str(&cookie) {
                Ok(value) => {
                    request.headers_mut().insert(reqwest::header::COOKIE, value);
                }
                Err(_) => {
                    // Cookie 里有非法字节时宁可这次不带，也不要把整个请求打挂。
                    tracing::warn!(url = %redact_url(url), "Cookie 含非法字符，已跳过注入");
                }
            }
        }
    }

    tracing::debug!(target: "hivecast_lib::platform", url = %redact_url(url), "请求平台接口");
    let response = client()
        .execute(request)
        .await
        .map_err(|err| PlatformError::Network(format!("{} → {err}", redact_url(url))))?;

    if !response.status().is_success() {
        tracing::warn!(
            target: "hivecast_lib::platform",
            url = %redact_url(url),
            status = response.status().as_u16(),
            "平台接口返回非 2xx"
        );
        return Err(PlatformError::Network(format!(
            "{} → HTTP {}",
            redact_url(url),
            response.status()
        )));
    }
    Ok(response)
}

/// 发 GET 并解析 JSON；错误统一收敛成 PlatformError，方便上层映射错误码。
pub async fn get_json<T: DeserializeOwned>(
    url: &str,
    referer: &str,
    extra_headers: &[(&str, &str)],
) -> Result<T, PlatformError> {
    get_json_with_date(url, referer, extra_headers)
        .await
        .map(|fetched| fetched.data)
}

pub async fn get_json_with_date<T: DeserializeOwned>(
    url: &str,
    referer: &str,
    extra_headers: &[(&str, &str)],
) -> Result<Fetched<T>, PlatformError> {
    let mut request = client().get(url).header("Referer", referer);
    for (name, value) in extra_headers {
        request = request.header(*name, *value);
    }

    let response = send(request, url).await?;
    let date = parse_date(response.headers());
    let data = response
        .json::<T>()
        .await
        .map_err(|err| PlatformError::Parse(format!("{} → {err}", redact_url(url))))?;
    Ok(Fetched { data, date })
}

/// 需要读响应的 Set-Cookie 时用这个（B 站扫码登录成功后，登录态只在响应头里）。
pub struct FetchedWithHeaders<T> {
    pub data: T,
    pub set_cookies: Vec<String>,
}

pub async fn get_json_with_headers<T: DeserializeOwned>(
    url: &str,
    referer: &str,
    extra_headers: &[(&str, &str)],
) -> Result<FetchedWithHeaders<T>, PlatformError> {
    let mut request = client().get(url).header("Referer", referer);
    for (name, value) in extra_headers {
        request = request.header(*name, *value);
    }

    let response = send(request, url).await?;
    let set_cookies = response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(|value| value.to_string())
        .collect();
    let data = response
        .json::<T>()
        .await
        .map_err(|err| PlatformError::Parse(format!("{} → {err}", redact_url(url))))?;
    Ok(FetchedWithHeaders { data, set_cookies })
}

/// 取文本（HTML 页面）；虎牙与斗鱼的取流参数都从页面上抠。
pub async fn get_text(
    url: &str,
    referer: &str,
    extra_headers: &[(&str, &str)],
) -> Result<(String, Option<i64>), PlatformError> {
    let mut request = client().get(url).header("Referer", referer);
    for (name, value) in extra_headers {
        request = request.header(*name, *value);
    }

    let response = send(request, url).await?;
    let date = parse_date(response.headers());
    let body = response
        .text()
        .await
        .map_err(|err| PlatformError::Parse(format!("{} → {err}", redact_url(url))))?;
    Ok((body, date))
}

/// 以表单 POST 并解析 JSON（斗鱼 getH5PlayV1 走这条路）。
pub async fn post_form_json<T: DeserializeOwned>(
    url: &str,
    referer: &str,
    form: &[(&str, String)],
    extra_headers: &[(&str, &str)],
) -> Result<Fetched<T>, PlatformError> {
    let mut request = client()
        .post(url)
        .header("Referer", referer)
        .header("Origin", "https://www.douyu.com")
        .form(form);
    for (name, value) in extra_headers {
        request = request.header(*name, *value);
    }

    let response = send(request, url).await?;
    let date = parse_date(response.headers());
    let data = response
        .json::<T>()
        .await
        .map_err(|err| PlatformError::Parse(format!("{} → {err}", redact_url(url))))?;
    Ok(Fetched { data, date })
}

/// 以 JSON body POST 并解析 JSON（B 站批量直播状态走这条路）。
pub async fn post_json<T: DeserializeOwned>(
    url: &str,
    referer: &str,
    origin: &str,
    body: &impl serde::Serialize,
    extra_headers: &[(&str, &str)],
) -> Result<Fetched<T>, PlatformError> {
    let mut request = client()
        .post(url)
        .header("Referer", referer)
        .header("Origin", origin)
        .json(body);
    for (name, value) in extra_headers {
        request = request.header(*name, *value);
    }

    let response = send(request, url).await?;
    let date = parse_date(response.headers());
    let data = response
        .json::<T>()
        .await
        .map_err(|err| PlatformError::Parse(format!("{} → {err}", redact_url(url))))?;
    Ok(Fetched { data, date })
}

/// 表单 POST，显式指定 Origin/Cookie，并读回 Set-Cookie（B 站 Cookie 续期走这条路）。
///
/// 与 post_form_json 的区别：不硬编码斗鱼的 Origin，也不依赖全局凭据注入；
/// 续期必须用调用方刚刚轮换出来的 Cookie，不能被内存里的旧值覆盖。
pub async fn post_form_json_with_headers<T: DeserializeOwned>(
    url: &str,
    referer: &str,
    origin: &str,
    cookie: &str,
    form: &[(&str, String)],
) -> Result<FetchedWithHeaders<T>, PlatformError> {
    let request = client()
        .post(url)
        .header("Referer", referer)
        .header("Origin", origin)
        .header("Cookie", cookie)
        .form(form);

    let response = send(request, url).await?;
    let set_cookies = response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(|value| value.to_string())
        .collect();
    let data = response
        .json::<T>()
        .await
        .map_err(|err| PlatformError::Parse(format!("{} → {err}", redact_url(url))))?;
    Ok(FetchedWithHeaders { data, set_cookies })
}

#[cfg(test)]
mod tests {
    use super::redact_url;

    #[test]
    fn urls_in_logs_keep_keys_but_drop_secrets() {
        let url = "https://cdn.example.com/live/a.flv?token=SECRET&sign=ABC&deadline=123";
        let redacted = redact_url(url);
        assert!(redacted.contains("token=***"));
        assert!(redacted.contains("sign=***"));
        assert!(!redacted.contains("SECRET"));
        assert!(!redacted.contains("ABC"));
        assert_eq!(
            redact_url("https://api.live.bilibili.com/room/v1/Room/get_info?room_id=6"),
            "https://api.live.bilibili.com/room/v1/Room/get_info?room_id=***"
        );
    }

    #[test]
    fn urls_with_credentials_and_fragments_are_fully_redacted() {
        let url =
            "https://alice:s3cr3t@example.com:8443/live/a.flv?token=one&token=two&room=6#frag";
        let redacted = redact_url(url);
        assert!(redacted.contains("example.com:8443/live/a.flv"));
        assert!(redacted.contains("token=***"));
        assert!(redacted.contains("room=***"));
        for secret in ["alice", "s3cr3t", "one", "two", "frag"] {
            assert!(!redacted.contains(secret), "泄露了 {secret}: {redacted}");
        }
    }

    #[test]
    fn path_tokens_and_long_hashes_are_redacted() {
        let hash = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
        let url = format!("https://cdn.example.com/live/{hash}/token=SECRET/index.m3u8?sign=ABC");
        let redacted = redact_url(&url);
        assert!(
            !redacted.contains(hash),
            "path 里的长哈希泄露了：{redacted}"
        );
        assert!(
            !redacted.contains("SECRET"),
            "path 里的 token 泄露了：{redacted}"
        );
        assert!(!redacted.contains("ABC"));
        assert!(redacted.contains("token=***"));
        assert!(redacted.contains("/live/***/"));
        assert!(redacted.contains("index.m3u8"));
    }

    #[test]
    fn multiple_path_parameters_are_all_redacted() {
        let url = "https://cdn.example.com/live/token=SECRET&sig=ABC/index.m3u8";
        let redacted = redact_url(url);
        assert!(!redacted.contains("SECRET"), "token 泄露了：{redacted}");
        assert!(!redacted.contains("ABC"), "sig 泄露了：{redacted}");
        assert!(redacted.contains("token=***&sig=***"));
    }

    #[test]
    fn path_uuids_and_prefixed_tokens_are_redacted() {
        let url = "https://cdn.example.com/v/550e8400-e29b-41d4-a716-446655440000/tok_abcdefghijklmnop/index.m3u8";
        let redacted = redact_url(url);
        assert!(!redacted.contains("550e8400"));
        assert!(!redacted.contains("tok_abcdefghijklmnop"));
        assert!(redacted.contains("index.m3u8"));
    }

    #[test]
    fn unparsable_urls_redact_path_after_an_at_sign() {
        let url = "not a url://example.com/live/a@b/token=SECRET/index.m3u8";
        let redacted = redact_url(url);
        assert!(
            !redacted.contains("SECRET"),
            "path 里的 token 泄露了：{redacted}"
        );
        assert!(redacted.contains("token=***"));
        assert!(redacted.contains("index.m3u8"));
    }

    #[test]
    fn unparsable_urls_do_not_keep_userinfo_or_query_values() {
        let hash = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
        let url = format!(
            "not a url://alice:s3cr3t@example.com/live/{hash}/index.m3u8?token=secret#frag"
        );
        let redacted = redact_url(&url);
        assert!(redacted.contains("example.com/live/***/index.m3u8"));
        assert!(
            !redacted.contains(hash),
            "path 里的长哈希泄露了：{redacted}"
        );
        for secret in ["alice", "s3cr3t", "secret", "frag"] {
            assert!(!redacted.contains(secret), "泄露了 {secret}: {redacted}");
        }
    }
}
