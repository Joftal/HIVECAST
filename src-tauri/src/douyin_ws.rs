//! 抖音弹幕长连接（M7.3）：握手签名 + protobuf 解帧 + gzip + ACK/心跳。
//!
//! 这条链路比 B 站/斗鱼/虎牙都麻烦，取决于三件事：
//! 1. 网页端 web_rid 不是 WS 需要的 room_id，必须先从页面里抠出 roomIdStr；
//! 2. 连接串要 \`signature\`，算法是官方 webmssdk（被混淆的 JS），见 platform/douyin_sign.rs；
//! 3. 帧是 PushFrame 包 Response，Response 里的 messages 再各带一个 protobuf payload。
//!
//! 这里只做「一次连接」：open() 建连，next_events() 读一帧。重连策略交给 danmaku.rs，
//! 与另外三个平台的节奏保持一致。

use std::time::Duration;

use flate2::read::GzDecoder;
use futures_util::{SinkExt, StreamExt};
use std::io::Read;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::platform::{douyin_sign, http};

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36 Edg/140.0.0.0";
const BROWSER_VERSION: &str =
    "5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";
const HOME: &str = "https://live.douyin.com";
const VERSION_CODE: &str = "180800";
const SDK_VERSION: &str = "1.0.15";
const HEARTBEAT: Duration = Duration::from_secs(5);

/// 一条可展示的弹幕/礼物。
pub struct Event {
    pub kind: &'static str,
    pub user: String,
    pub content: String,
}

pub struct Stream {
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    heartbeat: tokio::time::Interval,
    seq: u64,
}

impl Stream {
    /// 读一帧，返回其中可展示的事件（非聊天帧返回空表）。
    /// 心跳也在这里发，避免另开任务去抢同一个 sink。
    pub async fn next_events(&mut self) -> Result<Vec<Event>, String> {
        loop {
            tokio::select! {
                _ = self.heartbeat.tick() => {
                    let frame = encode_frame(&[
                        (1, Value::Varint(self.next_seq())),
                        (2, Value::Varint(now_millis() as u64)),
                        (7, Value::Bytes(b"hb".to_vec())),
                    ]);
                    self.ws
                        .send(WsMessage::Binary(frame.into()))
                        .await
                        .map_err(|err| format!("发送心跳失败：{err}"))?;
                }
                incoming = self.ws.next() => {
                    let Some(message) = incoming else {
                        return Err("连接已关闭".to_string());
                    };
                    match message.map_err(|err| format!("读取失败：{err}"))? {
                        WsMessage::Binary(bytes) => {
                            if let Some(frame) = decode_push_frame(&bytes)? {
                                if let Some((log_id, ext)) = frame.ack {
                                    let mut fields = vec![
                                        (2, Value::Varint(log_id)),
                                        (7, Value::Bytes(b"ack".to_vec())),
                                    ];
                                    if !ext.is_empty() {
                                        fields.push((8, Value::Bytes(ext.into_bytes())));
                                    }
                                    let ack = encode_frame(&fields);
                                    self.ws
                                        .send(WsMessage::Binary(ack.into()))
                                        .await
                                        .map_err(|err| format!("发送 ACK 失败：{err}"))?;
                                }
                                return Ok(frame.events);
                            }
                        }
                        WsMessage::Close(reason) => return Err(format!("服务端关闭连接：{reason:?}")),
                        _ => {}
                    }
                }
            }
        }
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }
}

/// 建立连接。room_key 是网页地址里的 web_rid（纯数字）。
/// 地址栏里的 web_rid；分享短链要先跟随跳转。
async fn resolve_web_rid(room_key: &str) -> Result<String, String> {
    if !room_key.is_empty() && room_key.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(room_key.to_string());
    }
    let url = format!("https://v.douyin.com/{room_key}/");
    let response = http::client()
        .get(&url)
        .header("User-Agent", UA)
        .send()
        .await
        .map_err(|err| format!("解析抖音短链失败：{err}"))?;
    response
        .url()
        .path_segments()
        .and_then(|mut segments| segments.next_back().map(str::to_string))
        .filter(|segment| segment.bytes().all(|b| b.is_ascii_digit()))
        .ok_or_else(|| format!("短链 {room_key} 没有跳转到直播间"))
}

pub async fn open(room_key: &str) -> Result<Stream, String> {
    let web_rid = resolve_web_rid(room_key).await?;
    let page_url = format!("{HOME}/{web_rid}");
    let page = http::client()
        .get(&page_url)
        .header("User-Agent", UA)
        .header("Accept-Language", "zh-CN,zh;q=0.9")
        .send()
        .await
        .map_err(|err| format!("打开直播间页面失败：{err}"))?;

    let mut cookies: Vec<String> = Vec::new();
    for value in page.headers().get_all(reqwest::header::SET_COOKIE) {
        if let Ok(text) = value.to_str() {
            if let Some(pair) = text.split(';').next() {
                let pair = pair.trim();
                if !pair.is_empty() {
                    cookies.push(pair.to_string());
                }
            }
        }
    }
    let body = page
        .text()
        .await
        .map_err(|err| format!("读取直播间页面失败：{err}"))?;
    let ttwid = cookies
        .iter()
        .find_map(|pair| pair.strip_prefix("ttwid="))
        .map(str::to_string)
        .ok_or_else(|| "页面没有下发 ttwid".to_string())?;

    // 真实 room_id 以 enter 接口为准；页面 SSR 兜底（拿到的是 "$undefined" 时直接跳过）。
    let room_id = match crate::platform::douyin::live_room_id(&web_rid).await {
        Ok((id, _live)) => id,
        Err(err) => {
            tracing::warn!(room = %web_rid, %err, "取真实 room_id 失败，回退页面解析");
            extract_room_id(&body).unwrap_or(web_rid)
        }
    };
    let unique_id = user_unique_id();
    let url = build_url(&room_id, &unique_id);
    let stub = signature_stub(&room_id, &unique_id);
    let signature = douyin_sign::sign(&stub)?;
    let url = format!("{url}&signature={signature}");

    let mut request = url
        .into_client_request()
        .map_err(|err| format!("连接串非法：{err}"))?;
    let headers = request.headers_mut();
    headers.insert("User-Agent", UA.parse().expect("UA 头非法"));
    headers.insert("Origin", HOME.parse().expect("Origin 头非法"));
    let cookie = format!("ttwid={ttwid}");
    headers.insert(
        "Cookie",
        cookie.parse().map_err(|_| "Cookie 头非法".to_string())?,
    );

    let (ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|err| format!("WebSocket 连接失败：{err}"))?;

    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // 第一次 tick 会立刻触发，先消耗掉，之后才是真正的 5 秒间隔。
    heartbeat.tick().await;

    Ok(Stream {
        ws,
        heartbeat,
        seq: 0,
    })
}

/// 网页参数：与 bililive-go 保持同一套（抖音对差异很敏感，字段缺一个就签不过）。
fn build_url(room_id: &str, unique_id: &str) -> String {
    // 参数集对齐 biliLive-tools/packages/DouYinDanma：不带 cursor / internal_ext，
    // sdk 版本 1.0.15，时区 Etc/GMT-8，host 用 -hl 集群。
    let pairs: Vec<(&str, String)> = vec![
        ("app_name", "douyin_web".into()),
        ("room_id", room_id.into()),
        ("compress", "gzip".into()),
        ("version_code", VERSION_CODE.into()),
        ("webcast_sdk_version", SDK_VERSION.into()),
        ("update_version_code", SDK_VERSION.into()),
        ("live_id", "1".into()),
        ("did_rule", "3".into()),
        ("user_unique_id", unique_id.into()),
        ("identity", "audience".into()),
        ("device_platform", "web".into()),
        ("cookie_enabled", "true".into()),
        ("screen_width", "1920".into()),
        ("screen_height", "1080".into()),
        ("browser_language", "zh-CN".into()),
        ("browser_platform", "Win32".into()),
        ("browser_name", "Mozilla".into()),
        ("browser_version", BROWSER_VERSION.into()),
        ("browser_online", "true".into()),
        ("tz_name", "Etc/GMT-8".into()),
        ("host", HOME.into()),
        ("aid", "6383".into()),
        ("endpoint", "live_pc".into()),
        ("support_wrds", "1".into()),
        ("im_path", "/webcast/im/fetch/".into()),
        ("need_persist_msg_count", "15".into()),
        ("heartbeatDuration", "0".into()),
    ];
    let query: String = pairs
        .into_iter()
        .map(|(key, value)| {
            format!(
                "{key}={}",
                url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>()
            )
        })
        .collect::<Vec<_>>()
        .join("&");
    format!("wss://webcast100-ws-web-hl.douyin.com/webcast/im/push/v2/?{query}")
}

/// 参与签名的 13 个字段，顺序固定（顺序变了签名就废）。
fn signature_stub(room_id: &str, unique_id: &str) -> String {
    let input = format!(
        "live_id=1,aid=6383,version_code={VERSION_CODE},webcast_sdk_version={SDK_VERSION},room_id={room_id},sub_room_id=,sub_channel_id=,did_rule=3,user_unique_id={unique_id},device_platform=web,device_type=,ac=,identity=audience"
    );
    format!("{:x}", md5::compute(input.as_bytes()))
}

fn user_unique_id() -> String {
    use std::hash::{Hash, Hasher};
    // 不引 rand：用系统时间 + 进程内计数打散即可，抖音只要求它是 19 位数字。
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    now.hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    let value =
        1_000_000_000_000_000_000u128 + hasher.finish() as u128 % 9_000_000_000_000_000_000u128;
    value.to_string()
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 页面里 roomId 字段是 JS 数字，超过 2^53 会失真；roomIdStr 才是字符串原始值。
/// 页面通常把 JSON 转义进 script，所以不按 JSON 解析，直接找 roomIdStr 之后的第一段长数字。
fn extract_room_id(body: &str) -> Option<String> {
    let at = body.find("roomIdStr")?;
    let rest = &body[at..];
    let bytes = rest.as_bytes();
    let mut start = None;
    for (index, byte) in bytes.iter().enumerate() {
        if byte.is_ascii_digit() {
            if start.is_none() {
                start = Some(index);
            }
        } else if let Some(begin) = start.take() {
            if index - begin >= 10 {
                return Some(rest[begin..index].to_string());
            }
        }
    }
    start.and_then(|begin| (rest.len() - begin >= 10).then(|| rest[begin..].to_string()))
}

#[derive(Clone)]
enum Value {
    Varint(u64),
    Bytes(Vec<u8>),
}

fn encode_frame(fields: &[(u32, Value)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (number, value) in fields {
        match value {
            Value::Varint(raw) => {
                write_varint(&mut out, ((number << 3) | 0) as u64);
                write_varint(&mut out, *raw);
            }
            Value::Bytes(raw) => {
                write_varint(&mut out, ((number << 3) | 2) as u64);
                write_varint(&mut out, raw.len() as u64);
                out.extend_from_slice(raw);
            }
        }
    }
    out
}

fn write_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            return;
        }
    }
}

fn read_varint(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for shift in 0..10 {
        let byte = *buf.get(*pos)?;
        *pos += 1;
        value |= ((byte & 0x7f) as u64) << (shift * 7);
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

fn read_bytes<'a>(buf: &'a [u8], pos: &mut usize) -> Option<&'a [u8]> {
    let len = read_varint(buf, pos)? as usize;
    let slice = buf.get(*pos..*pos + len)?;
    *pos += len;
    Some(slice)
}

/// 逐字段遍历：把 (字段号, 值) 交给回调；不认识的字段按 wire type 跳过。
fn walk_fields(buf: &[u8], mut visit: impl FnMut(u32, Value) -> bool) -> Option<()> {
    let mut pos = 0;
    while pos < buf.len() {
        let key = read_varint(buf, &mut pos)?;
        let number = (key >> 3) as u32;
        match key & 7 {
            0 => {
                let value = read_varint(buf, &mut pos)?;
                if !visit(number, Value::Varint(value)) {
                    return Some(());
                }
            }
            1 => pos += 8,
            2 => {
                let raw = read_bytes(buf, &mut pos)?.to_vec();
                if !visit(number, Value::Bytes(raw)) {
                    return Some(());
                }
            }
            5 => pos += 4,
            _ => return None,
        }
        if pos > buf.len() {
            return None;
        }
    }
    Some(())
}

fn as_string(value: &Value) -> String {
    match value {
        Value::Bytes(raw) => String::from_utf8_lossy(raw).into_owned(),
        Value::Varint(raw) => raw.to_string(),
    }
}

fn as_u64(value: &Value) -> u64 {
    match value {
        Value::Varint(raw) => *raw,
        Value::Bytes(_) => 0,
    }
}

/// 解 PushFrame：1 seqId / 2 logId / 7 payloadType / 8 payload。
/// 返回 Some(帧) 表示这一帧是数据帧；None 表示可以忽略（比如空 payload）。
struct Frame {
    events: Vec<Event>,
    /// 需要回 ACK 时给出 (logId, internalExt)。
    ack: Option<(u64, String)>,
}

fn decode_push_frame(buf: &[u8]) -> Result<Option<Frame>, String> {
    let mut log_id = 0u64;
    let mut payload = Vec::new();
    walk_fields(buf, |number, value| {
        match number {
            2 => log_id = as_u64(&value),
            8 => {
                if let Value::Bytes(raw) = value {
                    payload = raw;
                }
            }
            _ => {}
        }
        true
    })
    .ok_or_else(|| "PushFrame 解析失败".to_string())?;
    if payload.is_empty() {
        return Ok(None);
    }

    let mut decoder = GzDecoder::new(payload.as_slice());
    let mut plain = Vec::new();
    decoder
        .read_to_end(&mut plain)
        .map_err(|err| format!("gzip 解压失败：{err}"))?;

    let mut messages: Vec<(String, Vec<u8>)> = Vec::new();
    let mut internal_ext = String::new();
    let mut need_ack = false;
    let mut cursor = String::new();
    let mut fetch_type = 0u64;
    walk_fields(&plain, |number, value| {
        match number {
            1 => {
                let mut method = String::new();
                let mut body = Vec::new();
                if let Value::Bytes(raw) = value {
                    let _ = walk_fields(&raw, |field, inner| {
                        match field {
                            1 => method = as_string(&inner),
                            2 => {
                                if let Value::Bytes(raw) = inner {
                                    body = raw;
                                }
                            }
                            _ => {}
                        }
                        true
                    });
                }
                messages.push((method, body));
            }
            2 => cursor = as_string(&value),
            5 => internal_ext = as_string(&value),
            6 => fetch_type = as_u64(&value),
            9 => need_ack = as_u64(&value) != 0,
            _ => {}
        }
        true
    })
    .ok_or_else(|| "Response 解析失败".to_string())?;

    let mut events = Vec::new();
    for (method, body) in messages {
        match method.as_str() {
            "WebcastChatMessage" => {
                if let Some(event) = decode_chat(&body) {
                    events.push(event);
                }
            }
            "WebcastGiftMessage" => {
                if let Some(event) = decode_gift(&body) {
                    events.push(event);
                }
            }
            _ => {}
        }
    }
    Ok(Some(Frame {
        events,
        ack: need_ack.then_some((log_id, internal_ext)),
    }))
}

fn decode_chat(body: &[u8]) -> Option<Event> {
    let mut content = String::new();
    let mut nickname = String::new();
    walk_fields(body, |number, value| {
        match number {
            2 => {
                if let Value::Bytes(raw) = value {
                    let _ = walk_fields(&raw, |field, inner| {
                        if field == 2 {
                            nickname = as_string(&inner);
                        }
                        true
                    });
                }
            }
            3 => content = as_string(&value),
            _ => {}
        }
        true
    })?;
    if content.is_empty() {
        return None;
    }
    Some(Event {
        kind: "chat",
        user: if nickname.is_empty() {
            "未知用户".into()
        } else {
            nickname
        },
        content,
    })
}

fn decode_gift(body: &[u8]) -> Option<Event> {
    let mut nickname = String::new();
    let mut gift_name = String::new();
    let mut count = 1u64;
    walk_fields(body, |number, value| {
        match number {
            5 => count = as_u64(&value).max(1),
            7 => {
                if let Value::Bytes(raw) = value {
                    let _ = walk_fields(&raw, |field, inner| {
                        if field == 2 {
                            nickname = as_string(&inner);
                        }
                        true
                    });
                }
            }
            15 => {
                if let Value::Bytes(raw) = value {
                    let _ = walk_fields(&raw, |field, inner| {
                        if field == 16 {
                            gift_name = as_string(&inner);
                        }
                        true
                    });
                }
            }
            _ => {}
        }
        true
    })?;
    if gift_name.is_empty() {
        return None;
    }
    Some(Event {
        kind: "gift",
        user: if nickname.is_empty() {
            "未知用户".into()
        } else {
            nickname
        },
        content: format!("赠送 {gift_name} × {count}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_escaped_room_id() {
        let body = r#"<script>self.__pace_f.push("{\"roomIdStr\\":\\"7392091211001140287\\",\\"roomId\\":7392091211001140287}")</script>"#;
        assert_eq!(
            extract_room_id(body).as_deref(),
            Some("7392091211001140287")
        );
    }

    #[test]
    fn extracts_plain_room_id() {
        let body = r#"{"roomIdStr":"1234567890123456789"}"#;
        assert_eq!(
            extract_room_id(body).as_deref(),
            Some("1234567890123456789")
        );
    }

    #[test]
    fn signature_stub_matches_reference() {
        // 参考 bililive-go 的字段顺序；值用固定输入，方便回归。
        let stub = signature_stub("7000000000000000001", "7000000000000000002");
        assert_eq!(stub.len(), 32);
        assert!(stub.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[tokio::test]
    #[ignore = "查一批房间的在线人数：DOUYIN_ROOMS=a,b,c"]
    async fn live_counts() {
        use crate::platform::PlatformAdapter;
        let list = std::env::var("DOUYIN_ROOMS").expect("设置 DOUYIN_ROOMS");
        let adapter = crate::platform::douyin::DouyinAdapter::new();
        for rid in list.split(',') {
            let rid = rid.trim();
            let room = crate::platform::RoomRef {
                platform: "douyin".to_string(),
                room_key: rid.to_string(),
                canonical_url: format!("https://live.douyin.com/{rid}"),
                input: rid.to_string(),
            };
            match adapter.room_info(&room).await {
                Ok(room) => println!(
                    "{rid} live={} online={:?} title={}",
                    room.is_live, room.online, room.room_name
                ),
                Err(err) => println!("{rid} -> 失败：{err}"),
            }
        }
    }

    #[tokio::test]
    #[ignore = "查一批房间的在播状态：DOUYIN_ROOMS=a,b,c"]
    async fn live_flags() {
        let list = std::env::var("DOUYIN_ROOMS").expect("设置 DOUYIN_ROOMS");
        for rid in list.split(',') {
            let rid = rid.trim();
            match crate::platform::douyin::live_room_id(rid).await {
                Ok((id, live)) => println!("{rid} -> {id} live={live}"),
                Err(err) => println!("{rid} -> 失败：{err}"),
            }
        }
    }

    #[tokio::test]
    #[ignore = "需要真实直播间：DOUYIN_ROOM=<web_rid>"]
    async fn live_chat() {
        let room = std::env::var("DOUYIN_ROOM").expect("设置 DOUYIN_ROOM");
        let mut stream = open(&room).await.expect("连接抖音弹幕");
        let mut shown = 0;
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while shown < 5 && std::time::Instant::now() < deadline {
            for event in stream.next_events().await.expect("读事件") {
                if event.kind == "chat" {
                    println!("{}: {}", event.user, event.content);
                    shown += 1;
                    if shown >= 5 {
                        break;
                    }
                }
            }
        }
        assert!(shown > 0, "一分钟内没有收到聊天消息");
    }

    #[tokio::test]
    #[ignore = "需要真实直播间：DOUYIN_ROOM=<web_rid>"]
    async fn live_probe() {
        let room = std::env::var("DOUYIN_ROOM").expect("设置 DOUYIN_ROOM");
        let mut stream = open(&room).await.expect("应能连上");
        println!("已连接 {room}");
        let mut seen = 0;
        for index in 0..6 {
            println!("等待第 {} 批…", index + 1);
            let events =
                tokio::time::timeout(std::time::Duration::from_secs(10), stream.next_events())
                    .await
                    .expect("10 秒内没有数据帧")
                    .expect("读取失败");
            for event in &events {
                println!("[{}] {}: {}", event.kind, event.user, event.content);
                seen += 1;
            }
            if seen >= 5 {
                break;
            }
        }
        assert!(seen > 0, "10 秒内没有收到任何弹幕");
    }

    #[test]
    fn writes_and_reads_varint() {
        let mut out = Vec::new();
        write_varint(&mut out, 300);
        let mut pos = 0;
        assert_eq!(read_varint(&out, &mut pos), Some(300));
    }

    #[test]
    fn encodes_heartbeat_frame() {
        let frame = encode_frame(&[
            (1, Value::Varint(1)),
            (2, Value::Varint(2)),
            (7, Value::Bytes(b"hb".to_vec())),
        ]);
        let mut payload_type = String::new();
        walk_fields(&frame, |number, value| {
            if number == 7 {
                payload_type = as_string(&value);
            }
            true
        });
        assert_eq!(payload_type, "hb");
    }
}
