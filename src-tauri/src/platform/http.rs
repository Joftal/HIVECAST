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

/// 发 GET 并解析 JSON；错误统一收敛成 PlatformError，方便上层映射错误码。
pub async fn get_json<T: DeserializeOwned>(
    url: &str,
    referer: &str,
    extra_headers: &[(&str, &str)],
) -> Result<T, PlatformError> {
    let mut request = client().get(url).header("Referer", referer);
    for (name, value) in extra_headers {
        request = request.header(*name, *value);
    }

    let response = request
        .send()
        .await
        .map_err(|err| PlatformError::Network(format!("{url} → {err}")))?;

    if !response.status().is_success() {
        return Err(PlatformError::Network(format!(
            "{url} → HTTP {}",
            response.status()
        )));
    }

    response
        .json::<T>()
        .await
        .map_err(|err| PlatformError::Parse(format!("{url} → {err}")))
}
