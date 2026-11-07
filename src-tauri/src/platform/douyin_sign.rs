//! 抖音 WebSocket 签名。
//!
//! 抖音的弹幕长连接要求 `signature` 参数，算法来自官方 webmssdk（被混淆的 JS）。
//! 与其逆向一份会随时失效的实现，不如把 extract 出来的 `sign.js` 放进嵌入式引擎执行——
//! 参考项目 bililive-go 用 goja 做的也是同一件事（MIT 许可）。
//!
//! 代价：每次建连要解析 ~485KB 脚本。实测冷启动 20–40ms，只在连接/重连时发生，
//! 所以不做缓存，避免全局持有一个非 Send 的 Runtime。

const SIGN_JS: &str = include_str!("douyin_sign.js");

/// sign.js 直接假设自己在浏览器里：它给 `document = {}` 这类未声明变量赋值。
/// 预置同名全局，既避免严格模式报错，也让它能读到 navigator.userAgent。
const PRELUDE: &str = r#"
var window = globalThis;
var document = {};
var navigator = { userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36 Edg/140.0.0.0" };
var location = {}, screen = {}, history = {}, localStorage = {}, sessionStorage = {}, crypto = {}, performance = {};
function Image() {}
function WebSocket() {}
function XMLHttpRequest() {}
function fetch() {}
function setTimeout() {}
function setInterval() {}
function clearTimeout() {}
function clearInterval() {}
"#;

fn js_err(err: rquickjs::Error) -> String {
    format!("签名脚本出错：{err}")
}

/// 用 X-MS-STUB（参数串的 md5 十六进制）换 signature。
pub fn sign(x_ms_stub: &str) -> Result<String, String> {
    if x_ms_stub.len() != 32 || !x_ms_stub.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("X-MS-STUB 不是 32 位十六进制：{x_ms_stub}"));
    }
    let runtime = rquickjs::Runtime::new().map_err(js_err)?;
    let context = rquickjs::Context::full(&runtime).map_err(js_err)?;
    context.with(|ctx| {
        ctx.eval::<(), _>(PRELUDE).map_err(js_err)?;
        ctx.eval::<(), _>(SIGN_JS).map_err(js_err)?;
        // 用 JSON 字符串字面量拼参数，避免用户可控内容注入脚本。
        let call = format!(
            "get_sign({})",
            serde_json::to_string(x_ms_stub).map_err(|e| e.to_string())?
        );
        let signature: String = ctx.eval(call).map_err(js_err)?;
        if signature.is_empty() {
            return Err("get_sign 返回空值".to_string());
        }
        Ok(signature)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_stub() {
        assert!(sign("not-hex").is_err());
        assert!(sign("00112233445566778899aabbccddeeff00").is_err());
    }

    #[test]
    fn signs_a_stub() {
        let signature = sign("00112233445566778899aabbccddeeff").expect("应能算出签名");
        assert!(signature.len() >= 8, "签名太短：{signature}");
        // X-Bogus 形如 base64：字母数字加 + / =，长度 15–40。
        assert!(signature.len() >= 12, "签名太短：{signature}");
        assert!(
            signature
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=')),
            "签名含异常字符：{signature}"
        );
    }
}
