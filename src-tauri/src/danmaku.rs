//! 弹幕子系统（M3）：每个房间一条 B 站 WS 连接，扇出给「录制落盘」与「观看实时渲染」。
//!
//! 设计要点（docs/09、docs/19 §3）：
//! - 一房间一条连接：录制和最多 4 个观看格共享同一条 WS，避免被平台限流；
//! - 录制侧写 JSONL（与视频同名），时间基准是「写入时的本地单调时钟 - 本文件打开时刻」，
//!   不用平台时间戳（跨时区/休眠必然错位）；
//! - 观看侧每 200ms 合并成一批走 Tauri 事件，前端 Canvas 按批渲染；
//! - 单房间每秒最多处理 200 条，超出丢弃并计数，宁丢不卡。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use hivecast_ipc::{DanmakuBatch, DanmakuMessage, RoomRecord};
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::platform::http;

/// 实时弹幕事件名：前端按 roomId 分发给对应格子。
pub const EVENT_DANMAKU_BATCH: &str = "danmaku://batch";

const API: &str = "https://api.live.bilibili.com";
const REFERER: &str = "https://live.bilibili.com/";
const NAV_URL: &str = "https://api.bilibili.com/x/web-interface/nav";
const WWW_REFERER: &str = "https://www.bilibili.com/";
const FALLBACK_HOST: &str = "broadcastlv.chat.bilibili.com";
const HEARTBEAT: Duration = Duration::from_secs(30);
const BATCH_WINDOW: Duration = Duration::from_millis(200);
const MAX_PER_SECOND: u32 = 200;
const RETRY_MAX: Duration = Duration::from_secs(30);
/// 连续失败这么多次就丢弃 token，重新向平台申请。
const TOKEN_FAILURES: u32 = 5;
/// 解压递归的深度上限，防止坏包把栈打爆。
const MAX_DEPTH: usize = 3;

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// 房间级弹幕扇出：refs 归零即断开连接。
pub struct DanmakuHub {
    app: AppHandle,
    rooms: Mutex<HashMap<String, Arc<RoomConn>>>,
}

struct RoomConn {
    room: RoomRecord,
    refs: AtomicUsize,
    closed: AtomicBool,
    writers: Mutex<Vec<Arc<RecordWriter>>>,
    dropped: AtomicU32,
    last_error: Mutex<Option<String>>,
    started: Instant,
}

impl RoomConn {
    fn new(room: RoomRecord) -> Self {
        Self {
            room,
            refs: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
            writers: Mutex::new(Vec::new()),
            dropped: AtomicU32::new(0),
            last_error: Mutex::new(None),
            started: Instant::now(),
        }
    }

    fn set_error(&self, message: impl Into<String>) {
        *self.last_error.lock().expect("弹幕错误状态被污染") = Some(message.into());
    }

    fn fan_out(&self, message: &DanmakuMessage) {
        let writers = self.writers.lock().expect("弹幕写入器被污染");
        for writer in writers.iter() {
            writer.push(message);
        }
    }
}

impl DanmakuHub {
    pub fn new(app: AppHandle) -> Arc<Self> {
        Arc::new(Self {
            app,
            rooms: Mutex::new(HashMap::new()),
        })
    }

    /// 增加一个消费者（录制或观看）；第一个消费者负责拉起连接。
    pub fn retain(&self, room: &RoomRecord) {
        if !supports(room) {
            return;
        }
        let mut rooms = self.rooms.lock().expect("弹幕房间表被污染");
        if let Some(conn) = rooms.get(&room.id) {
            conn.refs.fetch_add(1, Ordering::SeqCst);
            return;
        }
        let conn = Arc::new(RoomConn::new(room.clone()));
        conn.refs.store(1, Ordering::SeqCst);
        rooms.insert(room.id.clone(), conn.clone());
        drop(rooms);

        let app = self.app.clone();
        let handle = conn.clone();
        tauri::async_runtime::spawn(async move {
            run_room(app, handle).await;
        });
    }

    /// 释放一个消费者；归零时让连接任务自行退出。
    pub fn release(&self, room_id: &str) {
        let mut rooms = self.rooms.lock().expect("弹幕房间表被污染");
        let Some(conn) = rooms.get(room_id) else {
            return;
        };
        let previous = conn.refs.fetch_sub(1, Ordering::SeqCst);
        if previous <= 1 {
            conn.closed.store(true, Ordering::SeqCst);
            rooms.remove(room_id);
        }
    }

    /// 录制开始时挂一个 JSONL 写入器；同时自动 retain。
    pub fn attach_record(&self, room: &RoomRecord, path: &Path) -> Option<Arc<RecordWriter>> {
        if !supports(room) {
            return None;
        }
        self.retain(room);
        let writer = RecordWriter::new(path).ok()?;
        let rooms = self.rooms.lock().expect("弹幕房间表被污染");
        let conn = rooms.get(&room.id)?;
        conn.writers
            .lock()
            .expect("弹幕写入器被污染")
            .push(writer.clone());
        Some(writer)
    }

    /// 录制结束：flush + 关闭文件，返回 (路径, 条数)。
    pub fn detach_record(
        &self,
        room_id: &str,
        writer: &Arc<RecordWriter>,
    ) -> Option<(PathBuf, u64)> {
        {
            let rooms = self.rooms.lock().expect("弹幕房间表被污染");
            if let Some(conn) = rooms.get(room_id) {
                conn.writers
                    .lock()
                    .expect("弹幕写入器被污染")
                    .retain(|item| !Arc::ptr_eq(item, writer));
            }
        }
        let result = writer.finish();
        self.release(room_id);
        Some(result)
    }

    /// 诊断用：连接错误与丢弃计数暂时只在日志/调试里看。
    #[allow(dead_code)]
    pub fn last_error(&self, room_id: &str) -> Option<String> {
        self.rooms
            .lock()
            .expect("弹幕房间表被污染")
            .get(room_id)
            .and_then(|conn| conn.last_error.lock().expect("弹幕错误状态被污染").clone())
    }

    #[allow(dead_code)]
    pub fn dropped(&self, room_id: &str) -> u32 {
        self.rooms
            .lock()
            .expect("弹幕房间表被污染")
            .get(room_id)
            .map(|conn| conn.dropped.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// 退出应用：断开所有连接并保证 JSONL 落盘。
    pub fn shutdown_all(&self) {
        let conns: Vec<Arc<RoomConn>> = {
            let mut rooms = self.rooms.lock().expect("弹幕房间表被污染");
            rooms.drain().map(|(_, conn)| conn).collect()
        };
        for conn in conns {
            conn.closed.store(true, Ordering::SeqCst);
            let writers = conn.writers.lock().expect("弹幕写入器被污染");
            for writer in writers.iter() {
                writer.finish();
            }
        }
    }
}

/// 是否已有该平台的弹幕接入（M3 只做了 B 站；斗鱼/虎牙在 M4 不接弹幕）。
pub fn supports(room: &RoomRecord) -> bool {
    room.platform == "bilibili"
}

/// 单个录制会话的 JSONL 写入器。
///
/// 时间基准取「写入时刻 - 文件打开时刻」，与 ffmpeg 启动基本同刻，
/// 因此偏移可直接当视频时间轴使用。
pub struct RecordWriter {
    inner: Mutex<WriterInner>,
}

struct WriterInner {
    file: Option<std::io::BufWriter<std::fs::File>>,
    path: PathBuf,
    started: Instant,
    count: u64,
    /// 上次落盘时刻：慢速聊天室可能几分钟都攒不满 50 条，不能只靠条数触发。
    flushed_at: Instant,
}

impl RecordWriter {
    fn new(path: &Path) -> std::io::Result<Arc<Self>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::File::create(path)?;
        Ok(Arc::new(Self {
            inner: Mutex::new(WriterInner {
                file: Some(std::io::BufWriter::new(file)),
                path: path.to_path_buf(),
                started: Instant::now(),
                count: 0,
                flushed_at: Instant::now(),
            }),
        }))
    }

    fn push(&self, message: &DanmakuMessage) {
        let mut inner = self.inner.lock().expect("弹幕文件被污染");
        let offset = inner.started.elapsed().as_millis() as i64;
        // 先把文件取出再写：BufWriter 借用整个 inner 时无法同时改 count。
        let Some(mut file) = inner.file.take() else {
            return;
        };
        let line = json!({
            "t": offset,
            "k": message.kind,
            "u": message.user,
            "c": message.content,
            "col": message.color,
        });
        if writeln!(file, "{line}").is_ok() {
            inner.count += 1;
            if inner.count % 50 == 0 || inner.flushed_at.elapsed() >= Duration::from_secs(1) {
                let _ = file.flush();
                inner.flushed_at = Instant::now();
            }
        }
        inner.file = Some(file);
    }

    pub fn finish(&self) -> (PathBuf, u64) {
        let mut inner = self.inner.lock().expect("弹幕文件被污染");
        if let Some(mut file) = inner.file.take() {
            let _ = file.flush();
        }
        (inner.path.clone(), inner.count)
    }
}

impl Drop for RecordWriter {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.inner.lock() {
            if let Some(mut file) = inner.file.take() {
                let _ = file.flush();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 连接循环
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ConfInfo {
    #[serde(default)]
    token: String,
    #[serde(default)]
    host_server_list: Vec<HostEntry>,
}

#[derive(Clone, Deserialize)]
struct HostEntry {
    #[serde(default)]
    host: String,
    #[serde(default)]
    wss_port: u16,
}

#[derive(Clone, Deserialize)]
struct DanmuInfo {
    #[serde(default)]
    token: String,
    #[serde(default)]
    host_list: Vec<HostEntry>,
}

#[derive(Clone)]
struct DanmuSession {
    token: String,
    hosts: Vec<HostEntry>,
    room_id: i64,
}

#[derive(Deserialize)]
struct Envelope<T> {
    code: i64,
    #[serde(default)]
    message: Option<String>,
    data: Option<T>,
}

#[derive(Deserialize)]
struct RoomInitData {
    #[serde(default)]
    room_id: i64,
}

// ---------------------------------------------------------------------------
// WBI 签名
//
// getDanmuInfo 从 2023 年起要求 WBI 签名，裸请求会返回 -352（风控校验失败）。
// 算法：取 nav 接口下发的 img_key/sub_key，按固定 64 元素混淆表取前 32 位得到
// mixin key；把 query 按 key 排序（值里去掉 !'()* ），拼上 mixin key 求 md5 即 w_rid。
// ---------------------------------------------------------------------------

const MIXIN_KEY_ENC_TAB: [usize; 64] = [
    46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49, 33, 9, 42, 19, 29,
    28, 14, 39, 12, 38, 41, 13, 37, 48, 7, 16, 24, 55, 40, 61, 26, 17, 0, 1, 60, 51, 30, 4, 22, 25,
    54, 21, 56, 59, 6, 63, 57, 62, 11, 36, 20, 34, 44, 52,
];

/// mixin key 一小时有效，缓存一份即可。
static WBI_KEY: tokio::sync::Mutex<Option<(String, Instant)>> =
    tokio::sync::Mutex::const_new(None);

#[derive(Deserialize)]
struct NavData {
    #[serde(default)]
    wbi_img: Option<WbiImg>,
}

#[derive(Deserialize)]
struct WbiImg {
    #[serde(default)]
    img_url: String,
    #[serde(default)]
    sub_url: String,
}

/// 从 ".../7cd084941338484aae1ad9425b84077c.png" 里取文件名主干。
fn key_from_url(raw: &str) -> String {
    let name = raw.rsplit('/').next().unwrap_or_default();
    match name.rsplit_once('.') {
        Some((stem, _)) => stem.to_string(),
        None => name.to_string(),
    }
}

async fn wbi_mixin_key() -> Result<String, String> {
    let mut cached = WBI_KEY.lock().await;
    if let Some((key, at)) = cached.as_ref() {
        if at.elapsed() < Duration::from_secs(3600) {
            return Ok(key.clone());
        }
    }

    let envelope: Envelope<NavData> = http::get_json(NAV_URL, WWW_REFERER, &[])
        .await
        .map_err(|err| format!("取 WBI 密钥失败：{err}"))?;
    let img = envelope
        .data
        .and_then(|data| data.wbi_img)
        .ok_or_else(|| format!("nav 未下发 WBI 密钥（code={}）", envelope.code))?;
    let raw = format!("{}{}", key_from_url(&img.img_url), key_from_url(&img.sub_url));
    if raw.len() < 64 {
        return Err("WBI 密钥长度异常".into());
    }
    let mixin: String = MIXIN_KEY_ENC_TAB
        .iter()
        .map(|idx| raw.as_bytes()[*idx] as char)
        .collect();
    *cached = Some((mixin.clone(), Instant::now()));
    Ok(mixin)
}

/// 给 URL 追加 wts 与 w_rid，参数顺序按 key 升序（与 B 站前端一致）。
async fn sign_url(raw: &str) -> Result<String, String> {
    let mixin = wbi_mixin_key().await?;
    let parsed = url::Url::parse(raw).map_err(|err| format!("URL 非法：{err}"))?;

    let mut pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(key, _)| key != "wts" && key != "w_rid")
        .map(|(key, value)| {
            let cleaned = value
                .chars()
                .filter(|ch| !matches!(ch, '!' | '\'' | '(' | ')' | '*'))
                .collect::<String>();
            (key.into_owned(), cleaned)
        })
        .collect();
    pairs.push((
        "wts".into(),
        chrono::Utc::now().timestamp().to_string(),
    ));
    pairs.sort_by(|a, b| a.0.cmp(&b.0));

    let query = pairs
        .iter()
        .fold(url::form_urlencoded::Serializer::new(String::new()), |acc, (key, value)| {
            let mut acc = acc;
            acc.append_pair(key, value);
            acc
        })
        .finish();
    let signature = format!("{:x}", md5::compute(format!("{query}{mixin}")));

    let mut signed = parsed.clone();
    signed.set_query(None);
    Ok(format!("{signed}?{query}&w_rid={signature}"))
}

async fn fetch_session(room_key: &str) -> Result<DanmuSession, String> {
    let init_url = format!("{API}/room/v1/Room/room_init?id={room_key}");
    let init: Envelope<RoomInitData> = http::get_json(&init_url, REFERER, &[])
        .await
        .map_err(|err| format!("解析真实房间号失败：{err}"))?;
    let real_id = init
        .data
        .map(|data| data.room_id)
        .filter(|id| *id > 0)
        .ok_or_else(|| {
            let detail = init.message.unwrap_or_default();
            format!("房间不存在或已失效（code={} {detail}）", init.code)
        })?;

    // 主接口：网页版 getDanmuInfo（需要 WBI 签名）。风控命中时会返回 -352，
    // 此时退到老的 getConf（无需签名，字段名不同但内容等价）。
    let primary = fetch_danmu_info(real_id).await;
    match primary {
        Ok(session) => Ok(session),
        Err(primary_err) => match fetch_conf(real_id).await {
            Ok(session) => {
                tracing::debug!(%primary_err, "getDanmuInfo 不可用，已退回 getConf");
                Ok(session)
            }
            Err(conf_err) => Err(format!("{primary_err}；备用接口：{conf_err}")),
        },
    }
}

async fn fetch_danmu_info(real_id: i64) -> Result<DanmuSession, String> {
    let info_url = format!("{API}/xlive/web-room/v1/index/getDanmuInfo?id={real_id}&type=0");
    let signed = sign_url(&info_url).await?;
    let envelope: Envelope<DanmuInfo> = http::get_json(&signed, REFERER, &[])
        .await
        .map_err(|err| format!("取弹幕服务器失败：{err}"))?;
    let info = envelope.data.ok_or_else(|| {
        let detail = envelope.message.unwrap_or_default();
        format!("getDanmuInfo 返回空数据（code={} {detail}）", envelope.code)
    })?;
    if info.token.is_empty() {
        return Err("getDanmuInfo 下发的 token 为空".into());
    }
    Ok(DanmuSession {
        token: info.token,
        hosts: info.host_list,
        room_id: real_id,
    })
}

/// 老接口 room/v1/Danmu/getConf：不需要 WBI 签名，仍能拿到 token 与服务器列表。
async fn fetch_conf(real_id: i64) -> Result<DanmuSession, String> {
    let url = format!("{API}/room/v1/Danmu/getConf?room_id={real_id}&platform=pc&player=web");
    let envelope: Envelope<ConfInfo> = http::get_json(&url, REFERER, &[])
        .await
        .map_err(|err| format!("取弹幕配置失败：{err}"))?;
    let info = envelope.data.ok_or_else(|| {
        let detail = envelope.message.unwrap_or_default();
        format!("getConf 返回空数据（code={} {detail}）", envelope.code)
    })?;
    if info.token.is_empty() {
        return Err("getConf 下发的 token 为空".into());
    }
    Ok(DanmuSession {
        token: info.token,
        hosts: info.host_server_list,
        room_id: real_id,
    })
}

async fn run_room(app: AppHandle, conn: Arc<RoomConn>) {
    let mut session: Option<DanmuSession> = None;
    let mut failures = 0u32;
    let mut backoff = Duration::from_secs(1);

    loop {
        if conn.closed.load(Ordering::SeqCst) {
            break;
        }
        if session.is_none() {
            match fetch_session(&conn.room.room_key).await {
                Ok(found) => session = Some(found),
                Err(err) => {
                    tracing::warn!(room = %conn.room.room_key, %err, "弹幕服务器解析失败");
                    conn.set_error(err);
                    sleep_interruptible(&conn, backoff).await;
                    backoff = (backoff * 2).min(RETRY_MAX);
                    continue;
                }
            }
        }

        let current = session.as_ref().expect("刚赋值过").clone();
        match run_once(&app, &conn, &current, &mut backoff).await {
            Ok(()) => break,
            Err(err) => {
                failures += 1;
                tracing::warn!(room = %conn.room.room_key, %err, failures, "弹幕连接中断");
                conn.set_error(err);
                if failures >= TOKEN_FAILURES {
                    session = None;
                    failures = 0;
                }
                sleep_interruptible(&conn, backoff).await;
                backoff = (backoff * 2).min(RETRY_MAX);
            }
        }
    }

    let writers = conn.writers.lock().expect("弹幕写入器被污染");
    for writer in writers.iter() {
        writer.finish();
    }
}

async fn sleep_interruptible(conn: &RoomConn, duration: Duration) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        if conn.closed.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn run_once(
    app: &AppHandle,
    conn: &Arc<RoomConn>,
    session: &DanmuSession,
    backoff: &mut Duration,
) -> Result<(), String> {
    let mut ws = connect(session).await?;
    send_packet(&mut ws, 7, &auth_body(session)).await?;

    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut window_start = Instant::now();
    let mut count = 0u32;
    let mut batch: Vec<DanmakuMessage> = Vec::new();

    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                send_packet(&mut ws, 2, b"[object Object]").await?;
            }
            incoming = ws.next() => {
                match incoming {
                    Some(Ok(WsMessage::Binary(data))) => {
                        let outcome = parse_frame(&data);
                        if let Some(code) = outcome.auth {
                            tracing::debug!(code, "弹幕认证回复");
                            if code != 0 {
                                return Err(format!("弹幕认证被拒（code={code}），token 可能已过期"));
                            }
                            *backoff = Duration::from_secs(1);
                        }
                        for mut message in outcome.messages {
                            if window_start.elapsed() >= Duration::from_secs(1) {
                                window_start = Instant::now();
                                count = 0;
                            }
                            if count >= MAX_PER_SECOND {
                                conn.dropped.fetch_add(1, Ordering::Relaxed);
                                continue;
                            }
                            count += 1;
                            message.offset_ms = conn.started.elapsed().as_millis() as i64;
                            conn.fan_out(&message);
                            batch.push(message);
                        }
                    }
                    Some(Ok(WsMessage::Close(_))) => return Err("弹幕连接被服务端关闭".into()),
                    Some(Ok(_)) => {}
                    Some(Err(err)) => return Err(format!("弹幕连接异常：{err}")),
                    None => return Err("弹幕连接结束".into()),
                }
            }
            _ = tokio::time::sleep(BATCH_WINDOW) => {
                if !batch.is_empty() {
                    tracing::debug!(room = %conn.room.room_key, n = batch.len(), "弹幕批量下发");
                    let payload = DanmakuBatch {
                        room_id: conn.room.id.clone(),
                        messages: std::mem::take(&mut batch),
                    };
                    let _ = app.emit(EVENT_DANMAKU_BATCH, payload);
                }
            }
        }

        if conn.closed.load(Ordering::SeqCst) {
            return Ok(());
        }
    }
}

async fn connect(session: &DanmuSession) -> Result<WsStream, String> {
    let mut candidates: Vec<String> = session
        .hosts
        .iter()
        .filter(|entry| !entry.host.is_empty())
        .map(|entry| {
            let port = if entry.wss_port == 0 { 443 } else { entry.wss_port };
            format!("wss://{}:{}/sub", entry.host, port)
        })
        .collect();
    candidates.push(format!("wss://{FALLBACK_HOST}/sub"));

    let mut last = String::new();
    for url in candidates {
        match try_connect(&url).await {
            Ok(ws) => return Ok(ws),
            Err(err) => last = err,
        }
    }
    Err(format!("弹幕服务器全部连接失败：{last}"))
}

async fn try_connect(url: &str) -> Result<WsStream, String> {
    let mut request = url
        .into_client_request()
        .map_err(|err| format!("构造 WS 请求失败：{err}"))?;
    {
        let headers = request.headers_mut();
        headers.insert("User-Agent", http::USER_AGENT.parse().map_err(|_| "UA 非法".to_string())?);
        headers.insert("Origin", "https://live.bilibili.com".parse().map_err(|_| "Origin 非法".to_string())?);
        headers.insert("Referer", REFERER.parse().map_err(|_| "Referer 非法".to_string())?);
    }
    let (ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|err| format!("{url} → {err}"))?;
    tracing::debug!(%url, "弹幕 WS 已连接");
    Ok(ws)
}

fn auth_body(session: &DanmuSession) -> Vec<u8> {
    json!({
        "uid": 0,
        "buvid": "",
        "roomid": session.room_id,
        "protover": 3,
        "platform": "web",
        "type": 2,
        "key": session.token,
    })
    .to_string()
    .into_bytes()
}

async fn send_packet(ws: &mut WsStream, op: u32, body: &[u8]) -> Result<(), String> {
    ws.send(WsMessage::Binary(packet(op, body)))
        .await
        .map_err(|err| format!("发送弹幕包失败：{err}"))
}

// ---------------------------------------------------------------------------
// 协议：16 字节头 + 可压缩的多个子包
// ---------------------------------------------------------------------------

fn packet(op: u32, body: &[u8]) -> Vec<u8> {
    let total = (16 + body.len()) as u32;
    let mut out = Vec::with_capacity(total as usize);
    out.extend_from_slice(&total.to_be_bytes());
    out.extend_from_slice(&16u16.to_be_bytes());
    // 包头里的 protover 固定 1（证书包），压缩协商写在 JSON 体的 protover 里。
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&op.to_be_bytes());
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(body);
    out
}

#[derive(Default)]
struct FrameOutcome {
    messages: Vec<DanmakuMessage>,
    auth: Option<i64>,
}

fn parse_frame(data: &[u8]) -> FrameOutcome {
    let mut out = FrameOutcome::default();
    walk_packets(data, 0, &mut out);
    out
}

fn walk_packets(data: &[u8], depth: usize, out: &mut FrameOutcome) {
    if depth > MAX_DEPTH {
        return;
    }
    let mut offset = 0usize;
    while offset + 16 <= data.len() {
        let length = u32::from_be_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
        let header_len =
            u16::from_be_bytes(data[offset + 4..offset + 6].try_into().unwrap()) as usize;
        let protover = u16::from_be_bytes(data[offset + 6..offset + 8].try_into().unwrap());
        let operation = u32::from_be_bytes(data[offset + 8..offset + 12].try_into().unwrap());
        if length < 16 || offset + length > data.len() || header_len > length {
            break;
        }
        let body = &data[offset + header_len..offset + length];

        match operation {
            5 => match protover {
                2 => {
                    if let Ok(raw) = inflate(body) {
                        walk_packets(&raw, depth + 1, out);
                    }
                }
                3 => {
                    if let Ok(raw) = brotli_decode(body) {
                        walk_packets(&raw, depth + 1, out);
                    }
                }
                _ => {
                    if let Some(message) = parse_message(body) {
                        out.messages.push(message);
                    }
                }
            },
            8 => {
                if let Ok(value) = serde_json::from_slice::<Value>(body) {
                    out.auth = Some(value.get("code").and_then(Value::as_i64).unwrap_or(-1));
                }
            }
            _ => {}
        }

        offset += length;
    }
}

fn inflate(body: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    let mut decoder = flate2::read::ZlibDecoder::new(body);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out)?;
    Ok(out)
}

fn brotli_decode(body: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    let mut decoder = brotli::Decompressor::new(body, 4096);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out)?;
    Ok(out)
}

fn parse_message(body: &[u8]) -> Option<DanmakuMessage> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let cmd = value.get("cmd")?.as_str()?;
    let cmd = cmd.split(':').next().unwrap_or(cmd);
    let data = value.get("data").cloned().unwrap_or(Value::Null);

    match cmd {
        "DANMU_MSG" => {
            let info = value.get("info")?.as_array()?;
            let content = info.get(1)?.as_str()?.to_string();
            if content.trim().is_empty() {
                return None;
            }
            let user = info
                .get(2)
                .and_then(|item| item.get(1))
                .and_then(Value::as_str)
                .map(str::to_string);
            let color = info
                .get(0)
                .and_then(|item| item.get(3))
                .and_then(Value::as_u64)
                .unwrap_or(0xFFFFFF) as u32;
            Some(DanmakuMessage {
                offset_ms: 0,
                kind: "chat".into(),
                user,
                content,
                color,
            })
        }
        "SEND_GIFT" => {
            let gift = data.get("giftName").and_then(Value::as_str)?;
            let num = data.get("num").and_then(Value::as_u64).unwrap_or(1);
            let user = data
                .get("uname")
                .and_then(Value::as_str)
                .map(str::to_string);
            Some(DanmakuMessage {
                offset_ms: 0,
                kind: "gift".into(),
                user,
                content: format!("赠送 {gift} × {num}"),
                color: 0xFFB027,
            })
        }
        "SUPER_CHAT_MESSAGE" => {
            let text = data.get("message").and_then(Value::as_str)?;
            let price = data.get("price").and_then(Value::as_u64).unwrap_or(0);
            let user = data
                .pointer("/user_info/uname")
                .and_then(Value::as_str)
                .map(str::to_string);
            Some(DanmakuMessage {
                offset_ms: 0,
                kind: "super_chat".into(),
                user,
                content: format!("[{price} 元] {text}"),
                color: 0x2EA7E0,
            })
        }
        "GUARD_BUY" => {
            let gift = data.get("gift_name").and_then(Value::as_str)?;
            let num = data.get("num").and_then(Value::as_u64).unwrap_or(1);
            let user = data
                .get("username")
                .and_then(Value::as_str)
                .map(str::to_string);
            Some(DanmakuMessage {
                offset_ms: 0,
                kind: "guard".into(),
                user,
                content: format!("开通了 {gift} × {num}"),
                color: 0xA06BFF,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn danmu_packet(json_body: &str) -> Vec<u8> {
        packet(5, json_body.as_bytes())
    }

    #[test]
    fn packet_header_is_big_endian() {
        let bytes = packet(7, b"{}");
        assert_eq!(u32::from_be_bytes(bytes[0..4].try_into().unwrap()), 18);
        assert_eq!(u16::from_be_bytes(bytes[4..6].try_into().unwrap()), 16);
        assert_eq!(u16::from_be_bytes(bytes[6..8].try_into().unwrap()), 1);
        assert_eq!(u32::from_be_bytes(bytes[8..12].try_into().unwrap()), 7);
    }

    #[test]
    fn parses_chat_message() {
        // JSON 不支持 0x 字面量，这里的颜色是 0x123456 的十进制。
        let json = r#"{"cmd":"DANMU_MSG","info":[[0,1,25,1193046,0,0,0,""],"哈哈哈",[42,"小明",0,0,0,10000,1,""]]}"#;
        let outcome = parse_frame(&danmu_packet(json));
        assert_eq!(outcome.messages.len(), 1);
        let message = &outcome.messages[0];
        assert_eq!(message.kind, "chat");
        assert_eq!(message.content, "哈哈哈");
        assert_eq!(message.user.as_deref(), Some("小明"));
        assert_eq!(message.color, 0x123456);
    }

    #[test]
    fn parses_gift_and_super_chat() {
        let gift = r#"{"cmd":"SEND_GIFT","data":{"giftName":"辣条","num":3,"uname":"甲"}}"#;
        let outcome = parse_frame(&danmu_packet(gift));
        assert_eq!(outcome.messages[0].kind, "gift");
        assert_eq!(outcome.messages[0].content, "赠送 辣条 × 3");

        let sc = r#"{"cmd":"SUPER_CHAT_MESSAGE","data":{"message":"加油","price":30,"user_info":{"uname":"乙"}}}"#;
        let outcome = parse_frame(&danmu_packet(sc));
        assert_eq!(outcome.messages[0].kind, "super_chat");
        assert_eq!(outcome.messages[0].content, "[30 元] 加油");
    }

    #[test]
    fn parses_zlib_compressed_packet() {
        use std::io::Write as _;
        let json = r#"{"cmd":"DANMU_MSG","info":[[0,1,25,16777215],"压缩",[7,"丙"]]}"#;
        let inner = danmu_packet(json);
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&inner).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut outer = packet(5, &compressed);
        outer[6..8].copy_from_slice(&2u16.to_be_bytes());

        let outcome = parse_frame(&outer);
        assert_eq!(outcome.messages.len(), 1);
        assert_eq!(outcome.messages[0].content, "压缩");
    }

    #[test]
    fn ignores_unknown_cmd() {
        let outcome = parse_frame(&danmu_packet(r#"{"cmd":"ONLINE_RANK_COUNT","data":{}}"#));
        assert!(outcome.messages.is_empty());
    }

    /// 联网探测：拿真实房间的 token 并读若干帧，用于定位 -352/认证失败。
    /// 运行：cargo test --lib -- --ignored --nocapture probe_live_danmaku
    #[tokio::test]
    #[ignore]
    async fn probe_live_danmaku() {
        let session = match fetch_session("545068").await {
            Ok(session) => session,
            Err(err) => panic!("fetch_session 失败：{err}"),
        };
        println!(
            "token_len={} room_id={} hosts={:?}",
            session.token.len(),
            session.room_id,
            session.hosts.iter().map(|h| (&h.host, h.wss_port)).collect::<Vec<_>>()
        );

        let mut ws = connect(&session).await.expect("连接失败");
        send_packet(&mut ws, 7, &auth_body(&session)).await.expect("认证包发送失败");

        let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        let mut frames = 0usize;
        let mut messages = 0usize;
        let mut auth = None;
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => break,
                incoming = ws.next() => {
                    match incoming {
                        Some(Ok(WsMessage::Binary(data))) => {
                            frames += 1;
                            let outcome = parse_frame(&data);
                            messages += outcome.messages.len();
                            if outcome.auth.is_some() {
                                auth = outcome.auth;
                            }
                            if frames <= 3 {
                                println!(
                                    "frame#{frames} len={} msgs={} auth={:?}",
                                    data.len(),
                                    outcome.messages.len(),
                                    outcome.auth
                                );
                            }
                        }
                        Some(Ok(_)) => {}
                        Some(Err(err)) => {
                            println!("读包错误：{err}");
                            break;
                        }
                        None => {
                            println!("连接被关闭");
                            break;
                        }
                    }
                }
            }
        }
        println!("frames={frames} messages={messages} auth={auth:?}");
        assert!(messages > 0, "8 秒内没有解析出任何弹幕");
    }
}
