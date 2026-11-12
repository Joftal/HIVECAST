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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use hivecast_ipc::{DanmakuBatch, DanmakuMessage, RoomRecord};
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::jce;
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
/// 弹幕批次日志的汇总周期。每 200ms 一批，逐批打印会轻易写满当日日志额度。
const BATCH_LOG_INTERVAL: Duration = Duration::from_secs(60);
const MAX_PER_SECOND: u32 = 200;
/// 读空闲超过 90 秒即判定连接假死：TCP 半开时 ws.next() 可能永远不返回。
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);
const IDLE_CHECK: Duration = Duration::from_secs(15);
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
    batch_log: Mutex<BatchMeter>,
    started: Instant,
}

/// 弹幕批次日志的计数器：按周期汇总，而不是每批打一行。
struct BatchMeter {
    last: Instant,
    batches: u64,
    messages: u64,
}

impl BatchMeter {
    fn new(now: Instant) -> Self {
        Self {
            last: now,
            batches: 0,
            messages: 0,
        }
    }

    /// 记一批；到达汇总周期时返回 (批次数, 消息数) 并清零窗口。
    fn record(&mut self, messages: usize, now: Instant) -> Option<(u64, u64)> {
        self.batches += 1;
        self.messages += messages as u64;
        if now.saturating_duration_since(self.last) < BATCH_LOG_INTERVAL {
            return None;
        }
        self.last = now;
        Some((
            std::mem::take(&mut self.batches),
            std::mem::take(&mut self.messages),
        ))
    }
}

impl RoomConn {
    fn new(room: RoomRecord) -> Self {
        let now = Instant::now();
        Self {
            room,
            refs: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
            writers: Mutex::new(Vec::new()),
            dropped: AtomicU32::new(0),
            last_error: Mutex::new(None),
            batch_log: Mutex::new(BatchMeter::new(now)),
            started: now,
        }
    }

    /// 记录一批弹幕；每 60 秒最多输出一行汇总。
    fn note_batch(&self, messages: usize) {
        let window = {
            let mut meter = self.batch_log.lock().expect("弹幕批次统计被污染");
            meter.record(messages, Instant::now())
        };
        if let Some((batches, messages)) = window {
            tracing::debug!(
                room = %self.room.room_key,
                batches,
                messages,
                dropped_total = self.dropped.load(Ordering::Relaxed),
                "弹幕批量下发汇总（60 秒）"
            );
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

    fn flush_writers(&self) {
        let writers = self.writers.lock().expect("弹幕写入器被污染");
        for writer in writers.iter() {
            writer.flush_pending();
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
    pub fn attach_record(
        &self,
        room: &RoomRecord,
        path: &Path,
        rules: RecordRules,
    ) -> Option<Arc<RecordWriter>> {
        if !supports(room) {
            return None;
        }
        self.retain(room);
        let writer = match RecordWriter::new(path, rules) {
            Ok(writer) => writer,
            Err(err) => {
                tracing::warn!(
                    room = %room.room_key,
                    path = %path.display(),
                    %err,
                    "弹幕文件创建失败，本场不记录弹幕"
                );
                // retain 已经加过引用，创建失败必须释放，否则这个房间的 WS 永远不关。
                self.release(&room.id);
                return None;
            }
        };
        let rooms = self.rooms.lock().expect("弹幕房间表被污染");
        let Some(conn) = rooms.get(&room.id) else {
            drop(rooms);
            self.release(&room.id);
            return None;
        };
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
        // 整场没有留下任何一条事件时 finish 已删掉空文件，这里返回 None，
        // 调用方就不会把一条不存在的路径写进数据库。
        if result.1 == 0 {
            None
        } else {
            Some(result)
        }
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

/// 是否已有该平台的弹幕接入（M3 B 站；M7.3 起加斗鱼、虎牙）。
pub fn supports(room: &RoomRecord) -> bool {
    matches!(
        room.platform.as_str(),
        "bilibili" | "douyu" | "huya" | "douyin"
    )
}

/// 录像侧的事件过滤规则；聊天弹幕与未知类型永远保留。
#[derive(Debug, Clone, Copy)]
pub struct RecordRules {
    pub gift: bool,
    pub super_chat: bool,
    pub guard: bool,
}

impl RecordRules {
    pub fn new(gift: bool, super_chat: bool, guard: bool) -> Self {
        Self {
            gift,
            super_chat,
            guard,
        }
    }

    fn allows(&self, kind: &str) -> bool {
        match kind {
            "gift" => self.gift,
            "super_chat" => self.super_chat,
            "guard" => self.guard,
            // chat 以及后续新增的事件类型默认保留，避免升级后静默丢数据。
            _ => true,
        }
    }
}

impl Default for RecordRules {
    fn default() -> Self {
        Self::new(true, true, true)
    }
}

/// 单个录制会话的 JSONL 写入器。
///
/// 时间基准取「写入时刻 - 文件打开时刻」，与 ffmpeg 启动基本同刻，
/// 因此偏移可直接当视频时间轴使用。
pub struct RecordWriter {
    rules: RecordRules,
    inner: Mutex<WriterInner>,
}

struct WriterInner {
    file: Option<std::io::BufWriter<std::fs::File>>,
    path: PathBuf,
    started: Instant,
    /// 与本文件第一条时间轴对应的 Unix 毫秒；用于把整场 JSONL 按片段切分。
    started_wall_ms: i64,
    count: u64,
    /// 写失败次数。磁盘满/权限异常时不能静默少行，至少要在日志里留下证据。
    write_errors: u64,
    /// 上次落盘时刻：慢速聊天室可能几分钟都攒不满 50 条，不能只靠条数触发。
    flushed_at: Instant,
}

impl RecordWriter {
    fn new(path: &Path, rules: RecordRules) -> std::io::Result<Arc<Self>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::File::create(path)?;
        let started_wall_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or(0);
        Ok(Arc::new(Self {
            rules,
            inner: Mutex::new(WriterInner {
                file: Some(std::io::BufWriter::new(file)),
                path: path.to_path_buf(),
                started: Instant::now(),
                started_wall_ms,
                count: 0,
                write_errors: 0,
                flushed_at: Instant::now(),
            }),
        }))
    }

    fn push(&self, message: &DanmakuMessage) {
        if !self.rules.allows(&message.kind) {
            return;
        }
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
        match writeln!(file, "{line}") {
            Ok(()) => {
                inner.count += 1;
                if inner.count % 50 == 0 || inner.flushed_at.elapsed() >= Duration::from_secs(1) {
                    if let Err(err) = file.flush() {
                        inner.write_errors += 1;
                        tracing::warn!(
                            file = %inner.path.display(),
                            %err,
                            errors = inner.write_errors,
                            "弹幕文件落盘失败，可能有弹幕未写入"
                        );
                    }
                    inner.flushed_at = Instant::now();
                }
            }
            Err(err) => {
                let first = inner.write_errors == 0;
                inner.write_errors += 1;
                if first || inner.write_errors % 100 == 0 {
                    tracing::warn!(
                        file = %inner.path.display(),
                        %err,
                        errors = inner.write_errors,
                        "弹幕写入失败，可能有弹幕未写入"
                    );
                }
            }
        }
        inner.file = Some(file);
    }

    /// 本文件 t=0 对应的 Unix 毫秒，供分割录制按时间切片。
    pub fn started_epoch_ms(&self) -> i64 {
        self.inner
            .lock()
            .map(|inner| inner.started_wall_ms)
            .unwrap_or(0)
    }

    /// 把 BufWriter 里的残余行立刻落盘；批次 flush 每 200ms 调一次。
    pub fn flush_pending(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            if let Some(file) = inner.file.as_mut() {
                if let Err(err) = file.flush() {
                    inner.write_errors += 1;
                    tracing::warn!(
                        file = %inner.path.display(),
                        %err,
                        errors = inner.write_errors,
                        "弹幕文件 flush 失败"
                    );
                }
            }
        }
    }

    pub fn finish(&self) -> (PathBuf, u64) {
        let mut inner = self.inner.lock().expect("弹幕文件被污染");
        if let Some(mut file) = inner.file.take() {
            if let Err(err) = file.flush() {
                inner.write_errors += 1;
                tracing::warn!(
                    file = %inner.path.display(),
                    %err,
                    errors = inner.write_errors,
                    "弹幕文件收尾 flush 失败"
                );
            }
        }
        if inner.write_errors > 0 {
            tracing::warn!(
                file = %inner.path.display(),
                errors = inner.write_errors,
                lines = inner.count,
                "本场弹幕存在写入失败，文件可能不完整"
            );
        }
        // 没有事件就不要在媒体目录里留一个 0 字节的 .jsonl。
        if inner.count == 0 {
            let _ = std::fs::remove_file(&inner.path);
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
    /// 登录态弹幕必须同时带 uid 与 buvid；匿名连接两者为空。
    uid: u64,
    buvid: String,
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
static WBI_KEY: tokio::sync::Mutex<Option<(String, Instant)>> = tokio::sync::Mutex::const_new(None);

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

    let envelope: Envelope<NavData> = http::get_json_anon(NAV_URL, WWW_REFERER, &[])
        .await
        .map_err(|err| format!("取 WBI 密钥失败：{err}"))?;
    let img = envelope
        .data
        .and_then(|data| data.wbi_img)
        .ok_or_else(|| format!("nav 未下发 WBI 密钥（code={}）", envelope.code))?;
    let raw = format!(
        "{}{}",
        key_from_url(&img.img_url),
        key_from_url(&img.sub_url)
    );
    if raw.len() < 64 {
        return Err("WBI 密钥长度异常".into());
    }
    // 混入密钥只取前 32 位；取满 64 位会算出错误的 w_rid，接口固定返回 -352。
    let mixin: String = MIXIN_KEY_ENC_TAB
        .iter()
        .map(|idx| raw.as_bytes()[*idx] as char)
        .take(32)
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
    pairs.push(("wts".into(), chrono::Utc::now().timestamp().to_string()));
    pairs.sort_by(|a, b| a.0.cmp(&b.0));

    let query = pairs
        .iter()
        .fold(
            url::form_urlencoded::Serializer::new(String::new()),
            |acc, (key, value)| {
                let mut acc = acc;
                acc.append_pair(key, value);
                acc
            },
        )
        .finish();
    let signature = format!("{:x}", md5::compute(format!("{query}{mixin}")));

    let mut signed = parsed.clone();
    signed.set_query(None);
    Ok(format!("{signed}?{query}&w_rid={signature}"))
}

async fn fetch_session(room_key: &str) -> Result<DanmuSession, String> {
    let init_url = format!("{API}/room/v1/Room/room_init?id={room_key}");
    let init: Envelope<RoomInitData> = http::get_json_anon(&init_url, REFERER, &[])
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

    // B 站对匿名 WS 做明显降采样：同一个高人气房间，登录态 60 秒能收到
    // 60+ 条聊天，匿名只有个位数。token 必须用带 Cookie 的请求换取，认证包
    // 再带上同一个 uid/buvid；WS 握手本身不需要 Cookie。
    if let Some(cookie) = crate::credential::get("bilibili") {
        if let Some(uid) = uid_from_cookie(&cookie) {
            // 扫码登录写回的 Cookie 不一定带 buvid3；没有就向 B 站申请一个
            // 设备指纹。token 由 Cookie 绑定 uid，buvid 只用于 auth 包。
            let buvid = match crate::credential::value(&cookie, "buvid3")
                .or_else(|| crate::credential::value(&cookie, "buvid4"))
                .filter(|value| !value.is_empty())
                .map(str::to_string)
            {
                Some(value) => Some(value),
                None => fetch_buvid().await,
            };
            if let Some(buvid) = buvid {
                match fetch_danmu_info_authed(real_id, &buvid).await {
                    Ok(mut session) => {
                        session.uid = uid;
                        session.buvid = buvid;
                        tracing::debug!(room = real_id, uid, "使用登录态获取弹幕 token");
                        return Ok(session);
                    }
                    Err(err) => {
                        tracing::warn!(room = real_id, uid, %err, "登录态弹幕 token 获取失败，回退匿名");
                    }
                }
            } else {
                tracing::warn!(room = real_id, uid, "无法取得 buvid，弹幕回退匿名");
            }
        }
    }

    // 匿名主接口：网页版 getDanmuInfo（需要 WBI 签名）。风控命中时会返回 -352，
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

/// 从 B 站 Cookie 里取登录态弹幕需要的 uid。
fn uid_from_cookie(cookie: &str) -> Option<u64> {
    crate::credential::value(cookie, "DedeUserID")?
        .parse::<u64>()
        .ok()
        .filter(|uid| *uid > 0)
}

#[derive(Deserialize)]
struct FingerData {
    #[serde(default)]
    b_3: String,
    #[serde(default)]
    b_4: String,
}

/// 申请一个匿名设备指纹；只在 Cookie 里没有 buvid3/buvid4 时调用。
async fn fetch_buvid() -> Option<String> {
    static BUVID: tokio::sync::Mutex<Option<String>> = tokio::sync::Mutex::const_new(None);
    let mut cached = BUVID.lock().await;
    if let Some(value) = cached.as_ref() {
        return Some(value.clone());
    }
    let url = "https://api.bilibili.com/x/frontend/finger/spi";
    let envelope: Envelope<FingerData> = http::get_json_anon(url, WWW_REFERER, &[]).await.ok()?;
    let data = envelope.data?;
    let value = if !data.b_3.is_empty() {
        data.b_3
    } else if !data.b_4.is_empty() {
        data.b_4
    } else {
        return None;
    };
    *cached = Some(value.clone());
    Some(value)
}

async fn fetch_danmu_info(real_id: i64) -> Result<DanmuSession, String> {
    let info_url = format!("{API}/xlive/web-room/v1/index/getDanmuInfo?id={real_id}&type=0");
    let signed = sign_url(&info_url).await?;
    let envelope: Envelope<DanmuInfo> = http::get_json_anon(&signed, REFERER, &[])
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
        uid: 0,
        buvid: String::new(),
    })
}

/// 登录态 getDanmuInfo：WBI 签名不变，但请求要带 Cookie，服务端才会
/// 下发与 uid 绑定的 token。token 拿到后 WS 握手不需要再带 Cookie。
async fn fetch_danmu_info_authed(real_id: i64, buvid: &str) -> Result<DanmuSession, String> {
    let info_url = format!("{API}/xlive/web-room/v1/index/getDanmuInfo?id={real_id}&type=0");
    let signed = sign_url(&info_url).await?;
    // 只带 SESSDATA 没有 buvid3 时 B 站风控仍会判成匿名甚至 -352，所以把
    // 设备指纹一起塞进本次请求的 Cookie（不写回凭据）。
    let mut cookie = crate::credential::get("bilibili").unwrap_or_default();
    if crate::credential::value(&cookie, "buvid3").is_none()
        && crate::credential::value(&cookie, "buvid4").is_none()
    {
        cookie.push_str("; buvid3=");
        cookie.push_str(buvid);
    }
    let headers = [("Cookie", cookie.as_str())];
    let envelope: Envelope<DanmuInfo> = http::get_json(&signed, REFERER, &headers)
        .await
        .map_err(|err| format!("取登录态弹幕服务器失败：{err}"))?;
    let info = envelope.data.ok_or_else(|| {
        let detail = envelope.message.unwrap_or_default();
        format!(
            "登录态 getDanmuInfo 返回空数据（code={} {detail}）",
            envelope.code
        )
    })?;
    if info.token.is_empty() {
        return Err("登录态 getDanmuInfo 下发的 token 为空".into());
    }
    Ok(DanmuSession {
        token: info.token,
        hosts: info.host_list,
        room_id: real_id,
        uid: 0,
        buvid: String::new(),
    })
}

/// 老接口 room/v1/Danmu/getConf：不需要 WBI 签名，仍能拿到 token 与服务器列表。
async fn fetch_conf(real_id: i64) -> Result<DanmuSession, String> {
    let url = format!("{API}/room/v1/Danmu/getConf?room_id={real_id}&platform=pc&player=web");
    let envelope: Envelope<ConfInfo> = http::get_json_anon(&url, REFERER, &[])
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
        uid: 0,
        buvid: String::new(),
    })
}

/// 单房间限速 + 打时间戳 + 写盘 + 攒批。
fn accept(
    conn: &RoomConn,
    window_start: &mut Instant,
    count: &mut u32,
    batch: &mut Vec<DanmakuMessage>,
    mut message: DanmakuMessage,
) {
    if window_start.elapsed() >= Duration::from_secs(1) {
        *window_start = Instant::now();
        *count = 0;
    }
    message.offset_ms = conn.started.elapsed().as_millis() as i64;
    // 录制落盘先于限流：爆发房间也要留下完整弹幕文件。
    conn.fan_out(&message);
    if *count >= MAX_PER_SECOND {
        conn.dropped.fetch_add(1, Ordering::Relaxed);
        return;
    }
    *count += 1;
    batch.push(message);
}

fn flush(app: &AppHandle, conn: &RoomConn, batch: &mut Vec<DanmakuMessage>) {
    // 即使这一批没有新消息，也把上一条残留在 BufWriter 里的行落盘。
    conn.flush_writers();
    if batch.is_empty() {
        return;
    }
    conn.note_batch(batch.len());
    let payload = DanmakuBatch {
        room_id: conn.room.id.clone(),
        messages: std::mem::take(batch),
    };
    let _ = app.emit(EVENT_DANMAKU_BATCH, payload);
}

async fn run_room(app: AppHandle, conn: Arc<RoomConn>) {
    if conn.room.platform == "douyu" {
        run_douyu_room(app, conn).await;
        return;
    }
    if conn.room.platform == "huya" {
        run_huya_room(app, conn).await;
        return;
    }
    if conn.room.platform == "douyin" {
        run_douyin_room(app, conn).await;
        return;
    }

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
                    sleep_interruptible(&conn, retry_delay(backoff, &conn.room.room_key)).await;
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
                sleep_interruptible(&conn, retry_delay(backoff, &conn.room.room_key)).await;
                backoff = (backoff * 2).min(RETRY_MAX);
            }
        }
    }

    let writers = conn.writers.lock().expect("弹幕写入器被污染");
    for writer in writers.iter() {
        writer.finish();
    }
}

/// 退避加 0–400ms jitter，避免同一平台所有房间在同一秒重连。
fn retry_delay(base: Duration, room_key: &str) -> Duration {
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos() as u64)
        .unwrap_or(0)
        ^ room_key
            .bytes()
            .fold(0u64, |acc, byte| acc.wrapping_mul(31) + byte as u64);
    base + Duration::from_millis(seed % 400)
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
    let mut batch: Vec<DanmakuMessage> = Vec::new();
    let result = run_once_inner(app, conn, session, backoff, &mut batch).await;
    // 任何退出路径（认证失败、服务端关闭、假死、主动 release）都先把
    // 最后一批已经解析出来的消息发给前端，避免 UI 丢掉最后 200ms 的弹幕。
    flush(app, conn, &mut batch);
    result
}

async fn run_once_inner(
    app: &AppHandle,
    conn: &Arc<RoomConn>,
    session: &DanmuSession,
    backoff: &mut Duration,
    batch: &mut Vec<DanmakuMessage>,
) -> Result<(), String> {
    let mut ws = connect(session).await?;
    send_packet(&mut ws, 7, &auth_body(session)).await?;

    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut idle_check = tokio::time::interval(IDLE_CHECK);
    idle_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_rx = Instant::now();
    let mut window_start = Instant::now();
    let mut count = 0u32;
    // 固定节拍，不能用 select! 里每轮新建 sleep：消息连续不断时定时器会被
    // 反复取消，flush 永远轮不到，batch 会一直涨到断线才丢。
    let mut batch_tick = tokio::time::interval(BATCH_WINDOW);
    batch_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                send_packet(&mut ws, 2, b"[object Object]").await?;
            }
            _ = idle_check.tick() => {
                if last_rx.elapsed() >= IDLE_TIMEOUT {
                    return Err(format!(
                        "弹幕连接 {} 秒无任何数据，判定假死",
                        last_rx.elapsed().as_secs()
                    ));
                }
            }
            incoming = ws.next() => {
                last_rx = Instant::now();
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
                        for message in outcome.messages {
                            accept(conn, &mut window_start, &mut count, batch, message);
                        }
                    }
                    Some(Ok(WsMessage::Close(_))) => return Err("弹幕连接被服务端关闭".into()),
                    Some(Ok(_)) => {}
                    Some(Err(err)) => return Err(format!("弹幕连接异常：{err}")),
                    None => return Err("弹幕连接结束".into()),
                }
            }
            _ = batch_tick.tick() => {
                flush(app, conn, batch);
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
            let port = if entry.wss_port == 0 {
                443
            } else {
                entry.wss_port
            };
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
        headers.insert(
            "User-Agent",
            http::USER_AGENT
                .parse()
                .map_err(|_| "UA 非法".to_string())?,
        );
        headers.insert(
            "Origin",
            "https://live.bilibili.com"
                .parse()
                .map_err(|_| "Origin 非法".to_string())?,
        );
        headers.insert(
            "Referer",
            REFERER.parse().map_err(|_| "Referer 非法".to_string())?,
        );
    }
    let (ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|err| format!("{} → {err}", crate::platform::http::redact_url(&url)))?;
    tracing::debug!(
        url = %crate::platform::http::redact_url(&url),
        "弹幕 WS 已连接"
    );
    Ok(ws)
}

fn auth_body(session: &DanmuSession) -> Vec<u8> {
    json!({
        "uid": session.uid,
        "buvid": session.buvid,
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

/// protobuf 的最小读取器：只处理 varint 与 length-delimited，足够解礼物 pb。
enum PbValue<'a> {
    Num(u64),
    Data(&'a [u8]),
}

fn pb_varint(buf: &[u8], offset: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    let mut shift = 0u32;
    while *offset < buf.len() {
        let byte = buf[*offset];
        *offset += 1;
        value |= ((byte & 0x7F) as u64) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
    None
}

fn pb_fields(buf: &[u8]) -> Option<Vec<(u64, PbValue<'_>)>> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset < buf.len() {
        let key = pb_varint(buf, &mut offset)?;
        let field = key >> 3;
        let wire = key & 7;
        let value = match wire {
            0 => PbValue::Num(pb_varint(buf, &mut offset)?),
            2 => {
                let len = pb_varint(buf, &mut offset)? as usize;
                let end = offset.checked_add(len)?;
                if end > buf.len() {
                    return None;
                }
                let data = &buf[offset..end];
                offset = end;
                PbValue::Data(data)
            }
            1 => {
                let end = offset.checked_add(8)?;
                if end > buf.len() {
                    return None;
                }
                offset = end;
                continue;
            }
            5 => {
                let end = offset.checked_add(4)?;
                if end > buf.len() {
                    return None;
                }
                offset = end;
                continue;
            }
            _ => return None,
        };
        out.push((field, value));
    }
    Some(out)
}

fn pb_string(fields: &[(u64, PbValue<'_>)], field: u64) -> Option<String> {
    fields.iter().find_map(|(id, value)| {
        if *id != field {
            return None;
        }
        match value {
            PbValue::Data(data) => String::from_utf8(data.to_vec()).ok(),
            PbValue::Num(_) => None,
        }
    })
}

fn pb_number(fields: &[(u64, PbValue<'_>)], field: u64) -> Option<u64> {
    fields.iter().find_map(|(id, value)| {
        if *id != field {
            return None;
        }
        match value {
            PbValue::Num(number) => Some(*number),
            PbValue::Data(_) => None,
        }
    })
}

fn parse_gift_v2(data: &Value) -> Option<DanmakuMessage> {
    let mut user = None;
    let mut gift = None;
    let mut num = 1u64;
    if let Some(pb) = data.get("pb").and_then(Value::as_str) {
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(pb) {
            if let Some(fields) = pb_fields(&bytes) {
                user = pb_string(&fields, 2);
                if let Some(inner) = fields.iter().find_map(|(id, value)| {
                    if *id != 10 {
                        return None;
                    }
                    match value {
                        PbValue::Data(data) => Some(*data),
                        PbValue::Num(_) => None,
                    }
                }) {
                    if let Some(gift_fields) = pb_fields(inner) {
                        gift = pb_string(&gift_fields, 2);
                        num = pb_number(&gift_fields, 3).unwrap_or(1).max(1);
                    }
                }
            }
        }
    }
    // 兼容未来 pb 缺失但 JSON 仍带旧字段的包。
    let user = user.or_else(|| {
        data.get("uname")
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    let gift = gift.or_else(|| {
        data.get("giftName")
            .or_else(|| data.get("gift_name"))
            .and_then(Value::as_str)
            .map(str::to_string)
    })?;
    let num = data
        .get("num")
        .and_then(Value::as_u64)
        .unwrap_or(num)
        .max(1);
    Some(DanmakuMessage {
        offset_ms: 0,
        kind: "gift".into(),
        user,
        content: format!("赠送 {gift} × {num}"),
        color: 0xFFB027,
    })
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
        "SEND_GIFT_V2" => parse_gift_v2(&data),
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

    #[test]
    fn uid_from_cookie_requires_a_positive_numeric_id() {
        assert_eq!(
            uid_from_cookie("SESSDATA=x; DedeUserID=352130563; buvid3=8E1A-test"),
            Some(352130563)
        );
        assert!(uid_from_cookie("DedeUserID=0; buvid3=A").is_none());
        assert!(uid_from_cookie("DedeUserID=abc; buvid3=A").is_none());
        assert!(uid_from_cookie("buvid3=A").is_none());
    }

    #[test]
    fn auth_body_carries_login_identity_or_zero() {
        let authed = DanmuSession {
            token: "t".into(),
            hosts: Vec::new(),
            room_id: 732,
            uid: 352130563,
            buvid: "buvid".into(),
        };
        let body: Value = serde_json::from_slice(&auth_body(&authed)).expect("json");
        assert_eq!(body["uid"], 352130563);
        assert_eq!(body["buvid"], "buvid");
        assert_eq!(body["roomid"], 732);

        let anon = DanmuSession {
            token: "t".into(),
            hosts: Vec::new(),
            room_id: 732,
            uid: 0,
            buvid: String::new(),
        };
        let body: Value = serde_json::from_slice(&auth_body(&anon)).expect("json");
        assert_eq!(body["uid"], 0);
        assert_eq!(body["buvid"], "");
    }

    fn record_message(kind: &str) -> DanmakuMessage {
        DanmakuMessage {
            offset_ms: 0,
            kind: kind.into(),
            user: Some("观众".into()),
            content: format!("{kind} 内容"),
            color: 0x00_FF_FF,
        }
    }

    #[test]
    fn record_rules_keep_chat_and_unknown_kinds() {
        let dir = std::env::temp_dir().join(format!("hivecast-danmaku-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("record.jsonl");
        let writer =
            RecordWriter::new(&path, RecordRules::new(true, false, false)).expect("writer");
        for kind in ["chat", "gift", "super_chat", "guard", "system"] {
            writer.push(&record_message(kind));
        }
        let (finished, count) = writer.finish();
        assert_eq!(finished, path);
        assert_eq!(count, 3, "chat、gift、未知事件应保留，SC/舰长应过滤");
        let text = std::fs::read_to_string(&path).expect("jsonl");
        assert!(text.contains("\"k\":\"chat\""));
        assert!(text.contains("\"k\":\"gift\""));
        assert!(text.contains("\"k\":\"system\""));
        assert!(!text.contains("\"k\":\"super_chat\""));
        assert!(!text.contains("\"k\":\"guard\""));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_record_file_is_removed() {
        let dir = std::env::temp_dir().join(format!("hivecast-danmaku-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("empty.jsonl");
        let writer =
            RecordWriter::new(&path, RecordRules::new(false, false, false)).expect("writer");
        writer.push(&record_message("gift"));
        let (_, count) = writer.finish();
        assert_eq!(count, 0);
        assert!(!path.exists(), "0 条事件时不能留下空 JSONL");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn danmu_packet(json_body: &str) -> Vec<u8> {
        packet(5, json_body.as_bytes())
    }

    #[test]
    fn batch_meter_aggregates_for_a_minute() {
        let start = Instant::now();
        let mut meter = BatchMeter::new(start);
        assert_eq!(meter.record(3, start + Duration::from_secs(10)), None);
        assert_eq!(meter.record(5, start + Duration::from_secs(59)), None);
        assert_eq!(
            meter.record(2, start + Duration::from_secs(60)),
            Some((3, 10))
        );
        // 汇总后窗口清零，下一分钟重新累计。
        assert_eq!(meter.record(1, start + Duration::from_secs(61)), None);
        assert_eq!(
            meter.record(4, start + Duration::from_secs(120)),
            Some((2, 5))
        );
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
    fn parses_gift_v2_protobuf() {
        // 2026-11-12 从房间 545068 抓到的真实 SEND_GIFT_V2：橘子入雨 / 人气票 / 1 个。
        let json = format!(
            r#"{{"cmd":"SEND_GIFT_V2","data":{{"pb":"{}"}}}}"#,
            "CMDhjwMSDOapmOWtkOWFpembqBpJaHR0cDovL2kwLmhkc2xiLmNvbS9iZnMvZmFjZS8xYzFmNWVjZDBiYjcwOTBkYTI2NTE1MmIxZGVkZDk0NmM0ZjJlYzRmLmpwZ0IAUtsFCMSJAhIJ5Lq65rCU56WoGAEgAShkMGQ4ZEIEZ29sZEoTNDgzODI2MjY2NjU2NzAzNTM5MlDtkNbXBlgBYjliYXRjaDpnaWZ0OmNvbWJvX2lkOjY1NDk2OTY6ODczOTQ3NzozMzk4ODoxNzk0NDc2MTQxLjMyOThoCnBkeAWFAQAAgD+IAQGSAQbmipXlloLAAcbltK8B6gEcChXogIHlrp7mhqjljprnmoTnrJHnrJEQlbWVBIoCswIIlbWVBBKrAgoV6ICB5a6e5oao5Y6a55qE56yR56yREkpodHRwczovL2kwLmhkc2xiLmNvbS9iZnMvZmFjZS81OWRhMzI5YTUzNmIxNDhiMGU4ZDE5MTQzYjY4NmQyZGEwNmRhZDkzLmpwZzJjChXogIHlrp7mhqjljprnmoTnrJHnrJESSmh0dHBzOi8vaTAuaGRzbGIuY29tL2Jmcy9mYWNlLzU5ZGEzMjlhNTM2YjE0OGIwZThkMTkxNDNiNjg2ZDJkYTA2ZGFkOTMuanBnOmEIAhJdYmlsaWJpbGkgMjAyMueZvuWkp1VQ5Li744CBMjAyMuebtOaSreW5tOW6puW8ueW5leS6uuawlOWlllVQ5Li744CB6IGM5Lia5ri45oiP6Kej6K+05a2Z5Lqa6b6ZkgIAmgLlAQpKaHR0cHM6Ly9zMS5oZHNsYi5jb20vYmZzL2xpdmUvN2Q3ODY0NmVlNGVjYzkwMGNlYmJlNDZjYjk2M2Y0MjEwNzk1NDhmMC5wbmcSS2h0dHBzOi8vaTAuaGRzbGIuY29tL2Jmcy9saXZlLzRkNTIxNWRhNDdkMjViM2RiNzg3ZDJiYTU0NDkwMTZlZTE0MmRjODAud2VicCpKaHR0cHM6Ly9pMC5oZHNsYi5jb20vYmZzL2xpdmUvNzBkOTJhZjBhODM3YWU2NmUyNWYyMzQ5M2E1NmUxNjkwZjc2ZWJiYS5naWaqAgBYAWoCCBZ6sAIIwOGPAxLBAQoM5qmY5a2Q5YWl6ZuoEklodHRwOi8vaTAuaGRzbGIuY29tL2Jmcy9mYWNlLzFjMWY1ZWNkMGJiNzA5MGRhMjY1MTUyYjFkZWRkOTQ2YzRmMmVjNGYuanBnMlkKDOapmOWtkOWFpembqBJJaHR0cDovL2kwLmhkc2xiLmNvbS9iZnMvZmFjZS8xYzFmNWVjZDBiYjcwOTBkYTI2NTE1MmIxZGVkZDk0NmM0ZjJlYzRmLmpwZzoLIP///////////wEaZQoJ5b635LqR6ImyEB8Y1ZC0ASD/t/YEKNWQtAEw1ZC0AUgBUJW1lQRg1Z8CegkjNEM3REZGOTmCAQkjNEM3REZGOTmKAQkjNEM3REZGOTmSAQcjRkZGRkZGmgEJIzRDN0RGRkU2"
        );
        let outcome = parse_frame(&danmu_packet(&json));
        assert_eq!(outcome.messages.len(), 1);
        let message = &outcome.messages[0];
        assert_eq!(message.kind, "gift");
        assert_eq!(message.user.as_deref(), Some("橘子入雨"));
        assert_eq!(message.content, "赠送 人气票 × 1");
    }

    #[test]
    fn parses_guard_buy() {
        let json = r#"{"cmd":"GUARD_BUY","data":{"gift_name":"舰长","num":1,"username":"丙"}}"#;
        let outcome = parse_frame(&danmu_packet(json));
        assert_eq!(outcome.messages[0].kind, "guard");
        assert_eq!(outcome.messages[0].content, "开通了 舰长 × 1");
        assert_eq!(outcome.messages[0].user.as_deref(), Some("丙"));
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
            session
                .hosts
                .iter()
                .map(|h| (&h.host, h.wss_port))
                .collect::<Vec<_>>()
        );

        let mut ws = connect(&session).await.expect("连接失败");
        send_packet(&mut ws, 7, &auth_body(&session))
            .await
            .expect("认证包发送失败");

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

// ---------------------------------------------------------------------------
// 斗鱼弹幕（M7.3）
//
// 协议是「自定义二进制帧 + STT 文本」：
//   [len: u32 LE][len: u32 LE][0x02B1: u16 LE][0x00][0x00][payload][0x00]
// len 从帧首算起（含自身 4 字节），payload 是 key@=value/ 串，值里的 "/" 与 "@"
// 分别转义成 @S / @A。登录两步：loginreq 认房间，joingroup 进 -9999 分组
// （-9999 = 不分组，能收到全房间弹幕）；心跳 45 秒发一次 mrkl。
// ---------------------------------------------------------------------------

const DOUYU_HEARTBEAT: Duration = Duration::from_secs(45);
const DOUYU_PORTS: std::ops::Range<u16> = 8501..8507;

fn douyu_frame(payload: &str) -> Vec<u8> {
    // 帧尾补一个 0，payload 之间用 NUL 分隔。
    let body = payload.as_bytes();
    let len = 8 + body.len() + 1;
    let mut out = Vec::with_capacity(len + 4);
    out.extend_from_slice(&(len as u32).to_le_bytes());
    out.extend_from_slice(&(len as u32).to_le_bytes());
    out.extend_from_slice(&0x02B1u16.to_le_bytes());
    out.push(0);
    out.push(0);
    out.extend_from_slice(body);
    out.push(0);
    out
}

/// 从累积缓冲里取出所有完整帧的文本部分。
fn douyu_drain(buffer: &mut Vec<u8>) -> Vec<String> {
    let mut out = Vec::new();
    loop {
        if buffer.len() < 4 {
            break;
        }
        let len = u32::from_le_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) as usize;
        if len < 9 || len > 1024 * 1024 {
            // 帧头不可信说明流已经错位，交给上层断开重连。
            buffer.clear();
            break;
        }
        if buffer.len() < len + 4 {
            break;
        }
        // 帧头 12 字节之后是 payload，末尾那个 NUL 不计入文本。
        let text = String::from_utf8_lossy(&buffer[12..len + 3]).to_string();
        buffer.drain(0..len + 4);
        out.push(text);
    }
    out
}

fn douyu_unescape(raw: &str) -> String {
    raw.replace("@S", "/").replace("@A", "@")
}

fn douyu_fields(raw: &str) -> HashMap<&str, &str> {
    let mut fields = HashMap::new();
    for part in raw.split('/') {
        if let Some((key, value)) = part.split_once("@=") {
            fields.insert(key, value);
        }
    }
    fields
}

fn douyu_color(raw: &str) -> u32 {
    match raw.parse::<u32>() {
        Ok(0) | Err(_) => 0xFF_FF_FF,
        Ok(value) => value & 0xFF_FF_FF,
    }
}

fn douyu_message(raw: &str) -> Option<DanmakuMessage> {
    let fields = douyu_fields(raw);
    match fields.get("type").copied() {
        Some("chatmsg") => {
            let content = douyu_unescape(fields.get("txt")?);
            if content.trim().is_empty() {
                return None;
            }
            Some(DanmakuMessage {
                offset_ms: 0,
                kind: "chat".into(),
                user: fields.get("nn").map(|name| douyu_unescape(name)),
                content,
                color: douyu_color(fields.get("col").copied().unwrap_or("0")),
            })
        }
        Some("dgb") => {
            // 礼物：gfid 是礼物 id，gfcnt 是数量，hits 是连击。
            let name = fields.get("gfn").map(|name| douyu_unescape(name))?;
            let count = fields.get("gfcnt").copied().unwrap_or("1");
            Some(DanmakuMessage {
                offset_ms: 0,
                kind: "gift".into(),
                user: fields.get("nn").map(|name| douyu_unescape(name)),
                content: format!("{name} x{count}"),
                color: 0xFF_C8_5C,
            })
        }
        _ => None,
    }
}

async fn run_douyu_room(app: AppHandle, conn: Arc<RoomConn>) {
    let mut backoff = Duration::from_secs(1);
    loop {
        if conn.closed.load(Ordering::SeqCst) {
            break;
        }
        match douyu_once(&app, &conn, &mut backoff).await {
            Ok(()) => break,
            Err(err) => {
                tracing::warn!(room = %conn.room.room_key, %err, "斗鱼弹幕连接中断");
                conn.set_error(err);
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

async fn douyu_once(
    app: &AppHandle,
    conn: &Arc<RoomConn>,
    backoff: &mut Duration,
) -> Result<(), String> {
    let room_id: i64 = conn
        .room
        .room_key
        .parse()
        .map_err(|_| format!("斗鱼房间号不是数字：{}", conn.room.room_key))?;

    let mut ws = douyu_connect().await?;
    {
        use futures_util::SinkExt;
        ws.send(WsMessage::Binary(douyu_frame(&format!(
            "type@=loginreq/roomid@={room_id}/"
        ))))
        .await
        .map_err(|err| format!("发送登录包失败：{err}"))?;
        ws.send(WsMessage::Binary(douyu_frame(&format!(
            "type@=joingroup/rid@={room_id}/gid@=-9999/"
        ))))
        .await
        .map_err(|err| format!("发送入组包失败：{err}"))?;
    }
    *backoff = Duration::from_secs(1);

    let mut heartbeat = tokio::time::interval(DOUYU_HEARTBEAT);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut window_start = Instant::now();
    let mut count = 0u32;
    let mut batch: Vec<DanmakuMessage> = Vec::new();
    let mut buffer: Vec<u8> = Vec::new();

    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                ws.send(WsMessage::Binary(douyu_frame("type@=mrkl/")))
                    .await
                    .map_err(|err| format!("弹幕心跳失败：{err}"))?;
            }
            incoming = ws.next() => {
                match incoming {
                    Some(Ok(WsMessage::Binary(data))) => {
                        buffer.extend_from_slice(&data);
                        for raw in douyu_drain(&mut buffer) {
                            if let Some(message) = douyu_message(&raw) {
                                accept(conn, &mut window_start, &mut count, &mut batch, message);
                            }
                        }
                    }
                    Some(Ok(WsMessage::Close(_))) => return Err("斗鱼弹幕连接被服务端关闭".into()),
                    Some(Ok(_)) => {}
                    Some(Err(err)) => return Err(format!("斗鱼弹幕连接异常：{err}")),
                    None => return Err("斗鱼弹幕连接结束".into()),
                }
            }
            _ = tokio::time::sleep(BATCH_WINDOW) => {
                flush(app, conn, &mut batch);
            }
        }

        if conn.closed.load(Ordering::SeqCst) {
            return Ok(());
        }
    }
}

async fn douyu_connect() -> Result<WsStream, String> {
    // 斗鱼弹幕前置网关对 rustls 的 ClientHello 直接回 HandshakeFailure，
    // 用系统 TLS（macOS Security / Windows SChannel）才握得上，其它平台仍走 rustls。
    let native = native_tls::TlsConnector::builder()
        .build()
        .map_err(|err| format!("初始化系统 TLS 失败：{err}"))?;
    let connector = tokio_tungstenite::Connector::NativeTls(native);

    let mut last = String::new();
    for port in DOUYU_PORTS {
        let url = format!("wss://danmuproxy.douyu.com:{port}/");
        match tokio_tungstenite::connect_async_tls_with_config(
            &url,
            None,
            false,
            Some(connector.clone()),
        )
        .await
        {
            Ok((ws, _)) => return Ok(ws),
            Err(err) => last = format!("{url} → {err}"),
        }
    }
    Err(format!("斗鱼弹幕服务器全部连接失败：{last}"))
}

#[cfg(test)]
mod douyu_tests {
    use super::*;

    #[test]
    fn frame_round_trips_through_buffer() {
        let mut buffer = douyu_frame("type@=loginreq/roomid@=9999/");
        let frames = douyu_drain(&mut buffer);
        assert_eq!(frames, vec!["type@=loginreq/roomid@=9999/".to_string()]);
        assert!(buffer.is_empty());
    }

    #[test]
    fn drain_keeps_partial_frame() {
        let mut buffer = douyu_frame("type@=mrkl/");
        let cut = buffer.len() - 3;
        let tail = buffer.split_off(cut);
        assert!(douyu_drain(&mut buffer).is_empty());
        buffer.extend_from_slice(&tail);
        assert_eq!(douyu_drain(&mut buffer), vec!["type@=mrkl/".to_string()]);
    }

    #[test]
    fn drain_handles_two_frames_in_one_read() {
        let mut buffer = douyu_frame("type@=mrkl/");
        buffer.extend_from_slice(&douyu_frame("type@=mrkl/"));
        assert_eq!(douyu_drain(&mut buffer).len(), 2);
    }

    #[test]
    fn parses_chat_and_unescapes() {
        let raw = "type@=chatmsg/rid@=9999/nn@=测试@A用户/txt@=哈哈@S真棒/col@=16777215/";
        let message = douyu_message(raw).expect("应该解析出弹幕");
        assert_eq!(message.user.as_deref(), Some("测试@用户"));
        assert_eq!(message.content, "哈哈/真棒");
        assert_eq!(message.color, 0xFF_FF_FF);
        assert_eq!(message.kind, "chat");
    }

    #[test]
    fn ignores_unknown_types_and_blank_text() {
        assert!(douyu_message("type@=uenter/nn@=x/").is_none());
        assert!(douyu_message("type@=chatmsg/nn@=x/txt@=/").is_none());
    }

    #[tokio::test]
    #[ignore = "需要联网与活跃直播间（9999）"]
    async fn live_probe() {
        use futures_util::SinkExt;
        let room: i64 = 9999;
        let mut ws = douyu_connect().await.expect("连不上斗鱼弹幕");
        ws.send(WsMessage::Binary(douyu_frame(&format!(
            "type@=loginreq/roomid@={room}/"
        ))))
        .await
        .expect("登录包发送失败");
        ws.send(WsMessage::Binary(douyu_frame(&format!(
            "type@=joingroup/rid@={room}/gid@=-9999/"
        ))))
        .await
        .expect("入组包发送失败");

        let deadline = tokio::time::Instant::now() + Duration::from_secs(25);
        let mut buffer: Vec<u8> = Vec::new();
        let mut chats = 0usize;
        let mut samples: Vec<String> = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                break;
            }
            match tokio::time::timeout(left, ws.next()).await {
                Ok(Some(Ok(WsMessage::Binary(data)))) => {
                    buffer.extend_from_slice(&data);
                    for raw in douyu_drain(&mut buffer) {
                        if let Some(message) = douyu_message(&raw) {
                            chats += 1;
                            if samples.len() < 5 {
                                samples.push(format!(
                                    "{}: {}",
                                    message.user.unwrap_or_default(),
                                    message.content
                                ));
                            }
                        }
                    }
                }
                Ok(Some(Ok(_))) => {}
                Ok(Some(Err(err))) => panic!("连接异常：{err}"),
                Ok(None) => break,
                Err(_) => break,
            }
            if chats >= 20 && samples.len() >= 5 {
                break;
            }
        }
        println!("收到弹幕 {chats} 条，样例：{samples:#?}");
        assert!(chats > 0, "25 秒内没有收到任何斗鱼弹幕");
    }

    #[test]
    fn parses_gift() {
        let raw = "type@=dgb/nn@=老板/gfn@=火箭/gfcnt@=2/";
        let message = douyu_message(raw).expect("应该解析出礼物");
        assert_eq!(message.kind, "gift");
        assert_eq!(message.content, "火箭 x2");
    }
}

// ---------------------------------------------------------------------------
// 虎牙弹幕（M7.3）
//
// 协议是 JCE/TARS：WS 上每条消息都是一个 WebSocketCommand{iCmdType, vData}。
// - 注册：命令 1，vData = WSUserInfo（房间三元组 + 分组类型 3）
// - 心跳：命令 3，vData = Wup 信封（onlineui / OnUserHeartBeat）
// - 弹幕：命令 7，vData = WSPushMessage{iUri, sMsg}：iUri 1400 聊天、6501 礼物。
// 房间三元组（lUid / lChannelId / lSubChannelId）只能从移动版页面里抠。
// ---------------------------------------------------------------------------

const HUYA_WS: &str = "ws://ws.api.huya.com";
const HUYA_HEARTBEAT: Duration = Duration::from_secs(60);
const HUYA_UA: &str = "Mozilla/5.0 (Linux; Android 5.1.1; Nexus 6 Build/LYZ28E) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/63.0.3239.84 Mobile Safari/537.36";
const HUYA_CMD_REGISTER: i64 = 1;
const HUYA_CMD_WUP_REQ: i64 = 3;
const HUYA_CMD_MSG_PUSH: i64 = 7;
const HUYA_URI_CHAT: i64 = 1400;
const HUYA_URI_GIFT: i64 = 6501;

#[derive(Debug, Clone, PartialEq, Eq)]
struct HuyaIds {
    uid: i64,
    tid: i64,
    sid: i64,
}

async fn huya_ids(room_key: &str) -> Result<HuyaIds, String> {
    let url = format!("https://m.huya.com/{room_key}");
    let (html, _) = http::get_text(&url, "https://m.huya.com/", &[("User-Agent", HUYA_UA)])
        .await
        .map_err(|err| format!("虎牙房间页请求失败：{err}"))?;
    huya_ids_from_html(&html)
        .ok_or_else(|| "虎牙房间页里没有 lUid / lChannelId / lSubChannelId".to_string())
}

fn huya_ids_from_html(html: &str) -> Option<HuyaIds> {
    let uid = huya_number(html, "lUid")?;
    let tid = huya_number(html, "lChannelId").unwrap_or(uid);
    let sid = huya_number(html, "lSubChannelId").unwrap_or(uid);
    Some(HuyaIds {
        uid,
        tid: if tid > 0 { tid } else { uid },
        sid: if sid > 0 { sid } else { uid },
    })
}

/// 抠 \"key\": 123 里的数字，兼容值是空串、或裸数字结尾的情况。
fn huya_number(html: &str, key: &str) -> Option<i64> {
    let needle = format!("\"{key}\"");
    let start = html.find(&needle)? + needle.len();
    let rest = html[start..].trim_start().strip_prefix(':')?.trim_start();
    if let Some(after) = rest.strip_prefix('"') {
        let end = after.find('"')?;
        return after[..end].trim().parse().ok();
    }
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '-'))
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn huya_command(cmd: i64, data: Vec<u8>) -> Vec<u8> {
    let mut enc = jce::Enc::new();
    enc.int(0, cmd);
    enc.bytes(1, &data);
    enc.into_vec()
}

fn huya_register_packet(ids: &HuyaIds) -> Vec<u8> {
    let mut info = jce::Enc::new();
    info.int(0, ids.uid);
    info.boolean(1, ids.uid == 0);
    info.string(2, "");
    info.string(3, "");
    info.int(4, ids.tid);
    info.int(5, ids.sid);
    info.int(6, ids.uid);
    info.int(7, 3);
    huya_command(HUYA_CMD_REGISTER, info.into_vec())
}

fn huya_heartbeat_packet(ids: &HuyaIds) -> Vec<u8> {
    // UserId 的 sHuYaUA 必须带上，否则服务端会把连接当成未知客户端。
    let mut user = jce::Enc::new();
    user.int(0, 0);
    user.string(1, "");
    user.string(2, "");
    user.string(3, "webh5&1.0.0&websocket");
    user.string(4, "");

    let mut req = jce::Enc::new();
    // 外层 UserHeartBeatReq 用 writeStruct(0, ..)，tId 自身也是一个写全的结构体。
    req.begin_struct(0);
    req.begin_struct(0);
    req.raw(user.as_slice());
    req.end_struct();
    req.int(1, ids.tid);
    req.int(2, ids.sid);
    req.int(3, 0);
    req.int(4, ids.uid);
    req.boolean(5, false);
    req.int(6, 1);
    req.int(7, 0);
    req.int(8, 0);
    req.int(9, 0);
    req.int(10, 0);
    req.end_struct();

    let mut data = jce::Enc::new();
    data.map_bytes(0, &[("tReq", req.into_vec())]);
    let newdata = data.into_vec();

    let mut header = jce::Enc::new();
    header.int(1, 3);
    header.int(2, 0);
    header.int(3, 0);
    header.int(4, 0);
    header.string(5, "onlineui");
    header.string(6, "OnUserHeartBeat");
    header.bytes(7, &newdata);
    header.int(8, 0);
    header.empty_map(9);
    header.empty_map(10);
    let header = header.into_vec();

    // 前缀 4 字节是整个 Wup（前缀 + header）的总长度，不是 header 自身的长度。
    let mut wup = Vec::with_capacity(header.len() + 4);
    wup.extend_from_slice(&((header.len() as u32) + 4).to_be_bytes());
    wup.extend_from_slice(&header);
    huya_command(HUYA_CMD_WUP_REQ, wup)
}

/// 虎牙的字体颜色是 0xBBGGRR，转成前端用的 0xRRGGBB；<= 0 表示默认白色。
fn huya_color(raw: i64) -> u32 {
    if raw <= 0 {
        return 0xFF_FF_FF;
    }
    let value = raw as u32;
    let red = value & 0xFF;
    let green = (value >> 8) & 0xFF;
    let blue = (value >> 16) & 0xFF;
    (red << 16) | (green << 8) | blue
}

fn huya_messages(payload: &[u8]) -> Vec<DanmakuMessage> {
    let mut out = Vec::new();
    let Ok(fields) = jce::decode(payload) else {
        return out;
    };
    if jce::get_int(&fields, 0) != Some(HUYA_CMD_MSG_PUSH) {
        return out;
    }
    let Some(body) = jce::get_bytes(&fields, 1) else {
        return out;
    };
    let Ok(push) = jce::decode(body) else {
        return out;
    };
    let uri = jce::get_int(&push, 1).unwrap_or(0);
    let Some(data) = jce::get_bytes(&push, 2) else {
        return out;
    };
    let Ok(notice) = jce::decode(data) else {
        return out;
    };

    match uri {
        HUYA_URI_CHAT => {
            let user = jce::get_struct(&notice, 0)
                .and_then(|fields| jce::get_str(fields, 2))
                .unwrap_or("")
                .to_string();
            let content = jce::get_str(&notice, 3).unwrap_or("").trim().to_string();
            if content.is_empty() {
                return out;
            }
            let color = jce::get_struct(&notice, 6)
                .and_then(|fields| jce::get_int(fields, 0))
                .unwrap_or(-1);
            out.push(DanmakuMessage {
                offset_ms: 0,
                kind: "chat".into(),
                user: if user.is_empty() { None } else { Some(user) },
                content,
                color: huya_color(color),
            });
        }
        HUYA_URI_GIFT => {
            let user = jce::get_str(&notice, 6).unwrap_or("").to_string();
            let name = jce::get_str(&notice, 7).unwrap_or("").trim().to_string();
            if name.is_empty() {
                return out;
            }
            let count = jce::get_int(&notice, 2).unwrap_or(1).max(1);
            out.push(DanmakuMessage {
                offset_ms: 0,
                kind: "gift".into(),
                user: if user.is_empty() { None } else { Some(user) },
                content: format!("{name} x{count}"),
                color: 0xFF_C8_5C,
            });
        }
        _ => {}
    }
    out
}

async fn run_huya_room(app: AppHandle, conn: Arc<RoomConn>) {
    let mut backoff = Duration::from_secs(1);
    loop {
        if conn.closed.load(Ordering::SeqCst) {
            break;
        }
        match huya_once(&app, &conn, &mut backoff).await {
            Ok(()) => break,
            Err(err) => {
                tracing::warn!(room = %conn.room.room_key, %err, "虎牙弹幕连接中断");
                conn.set_error(err);
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

async fn huya_once(
    app: &AppHandle,
    conn: &Arc<RoomConn>,
    backoff: &mut Duration,
) -> Result<(), String> {
    let ids = huya_ids(&conn.room.room_key).await?;
    let (mut ws, _) = tokio_tungstenite::connect_async(HUYA_WS)
        .await
        .map_err(|err| format!("虎牙弹幕连接失败：{err}"))?;
    ws.send(WsMessage::Binary(huya_register_packet(&ids)))
        .await
        .map_err(|err| format!("发送注册包失败：{err}"))?;
    ws.send(WsMessage::Binary(huya_heartbeat_packet(&ids)))
        .await
        .map_err(|err| format!("发送心跳失败：{err}"))?;
    *backoff = Duration::from_secs(1);

    let mut heartbeat = tokio::time::interval(HUYA_HEARTBEAT);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // 建连时已经手动发过一次，跳过 interval 的立即触发。
    heartbeat.tick().await;

    let mut window_start = Instant::now();
    let mut count = 0u32;
    let mut batch: Vec<DanmakuMessage> = Vec::new();

    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                ws.send(WsMessage::Binary(huya_heartbeat_packet(&ids)))
                    .await
                    .map_err(|err| format!("虎牙弹幕心跳失败：{err}"))?;
            }
            incoming = ws.next() => {
                match incoming {
                    Some(Ok(WsMessage::Binary(data))) => {
                        for message in huya_messages(&data) {
                            accept(conn, &mut window_start, &mut count, &mut batch, message);
                        }
                    }
                    Some(Ok(WsMessage::Close(_))) => return Err("虎牙弹幕连接被服务端关闭".into()),
                    Some(Ok(_)) => {}
                    Some(Err(err)) => return Err(format!("虎牙弹幕连接异常：{err}")),
                    None => return Err("虎牙弹幕连接结束".into()),
                }
            }
            _ = tokio::time::sleep(BATCH_WINDOW) => {
                flush(app, conn, &mut batch);
            }
        }

        if conn.closed.load(Ordering::SeqCst) {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod huya_tests {
    use super::*;

    fn ids() -> HuyaIds {
        HuyaIds {
            uid: 1_346_609_715,
            tid: 1_346_609_715,
            sid: 1_346_609_715,
        }
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn register_packet_matches_reference() {
        // 参考值由 biliLive-tools/packages/huya-danmu/lib.js 生成。
        assert_eq!(
            hex(&huya_register_packet(&ids())),
            "00011d00001b025043a2331c26003600425043a233525043a233625043a2337003"
        );
    }

    #[test]
    fn heartbeat_packet_matches_reference() {
        assert_eq!(
            hex(&huya_heartbeat_packet(&ids())),
            "00031d0000730000007310032c3c4c56086f6e6c696e657569660f4f6e557365724865617274426561747d0000460800010604745265711d0000390a0a0c160026003615776562683526312e302e3026776562736f636b657446000b125043a233225043a2333c425043a2335c60017c8c9cac0b8c980ca80c"
        );
    }

    #[test]
    fn reads_ids_from_mobile_page() {
        let html = r#"<script>var cfg = {"lChannelId": 1346609715, "lSubChannelId": 1346609715, "lUid": 1346609715}</script>"#;
        assert_eq!(
            huya_ids_from_html(html),
            Some(HuyaIds {
                uid: 1_346_609_715,
                tid: 1_346_609_715,
                sid: 1_346_609_715
            })
        );
    }

    #[test]
    fn falls_back_to_uid_when_channel_ids_are_blank() {
        let html = r#"{"lChannelId":"","lSubChannelId":"","lUid":42}"#;
        assert_eq!(
            huya_ids_from_html(html),
            Some(HuyaIds {
                uid: 42,
                tid: 42,
                sid: 42
            })
        );
    }

    fn push_packet(uri: i64, body: Vec<u8>) -> Vec<u8> {
        let mut push = jce::Enc::new();
        push.int(0, 0);
        push.int(1, uri);
        push.bytes(2, &body);
        push.int(3, 0);
        huya_command(HUYA_CMD_MSG_PUSH, push.into_vec())
    }

    #[test]
    fn parses_chat_push() {
        let mut sender = jce::Enc::new();
        sender.int(0, 123);
        sender.string(2, "测试用户");
        let mut notice = jce::Enc::new();
        notice.begin_struct(0);
        notice.raw(sender.as_slice());
        notice.end_struct();
        notice.int(1, 1_346_609_715);
        notice.int(2, 1_346_609_715);
        notice.string(3, "这波很强");
        notice.int(4, 0);
        notice.begin_struct(5);
        notice.end_struct();
        let mut bullet = jce::Enc::new();
        bullet.int(0, 0x00_00_FF);
        notice.begin_struct(6);
        notice.raw(bullet.as_slice());
        notice.end_struct();

        let messages = huya_messages(&push_packet(HUYA_URI_CHAT, notice.into_vec()));
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].kind, "chat");
        assert_eq!(messages[0].user.as_deref(), Some("测试用户"));
        assert_eq!(messages[0].content, "这波很强");
        assert_eq!(messages[0].color, 0x00_FF_00_00);
    }

    #[test]
    fn parses_gift_push() {
        let mut notice = jce::Enc::new();
        notice.int(0, 520);
        notice.string(1, "pay-id");
        notice.int(2, 3);
        notice.int(3, 1_346_609_715);
        notice.int(4, 777);
        notice.string(5, "主播");
        notice.string(6, "老板");
        notice.string(7, "荧光棒");
        let messages = huya_messages(&push_packet(HUYA_URI_GIFT, notice.into_vec()));
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].kind, "gift");
        assert_eq!(messages[0].user.as_deref(), Some("老板"));
        assert_eq!(messages[0].content, "荧光棒 x3");
    }

    #[test]
    fn ignores_non_push_commands() {
        let mut command = jce::Enc::new();
        command.int(0, 2);
        command.bytes(1, &[]);
        assert!(huya_messages(command.as_slice()).is_empty());
    }

    #[tokio::test]
    #[ignore = "需要联网与活跃直播间（HUYA_PROBE_ROOM，默认 660000）"]
    async fn live_probe() {
        use futures_util::SinkExt;
        let room = std::env::var("HUYA_PROBE_ROOM").unwrap_or_else(|_| "660000".to_string());
        let ids = huya_ids(&room).await.expect("抓不到房间三元组");
        let (mut ws, _) = tokio_tungstenite::connect_async(HUYA_WS)
            .await
            .expect("连不上虎牙弹幕");
        ws.send(WsMessage::Binary(huya_register_packet(&ids)))
            .await
            .expect("注册包发送失败");
        ws.send(WsMessage::Binary(huya_heartbeat_packet(&ids)))
            .await
            .expect("心跳发送失败");

        // 赛事房间会有系统公告，平时房间则是观众发言，两者都走 1400。
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        let mut chats = 0usize;
        let mut samples: Vec<String> = Vec::new();
        loop {
            if chats > 0 && !samples.is_empty() {
                break;
            }
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                break;
            }
            match tokio::time::timeout(left, ws.next()).await {
                Ok(Some(Ok(WsMessage::Binary(data)))) => {
                    for message in huya_messages(&data) {
                        chats += 1;
                        if samples.len() < 5 {
                            samples.push(format!(
                                "{}: {}",
                                message.user.unwrap_or_default(),
                                message.content
                            ));
                        }
                    }
                }
                Ok(Some(Ok(_))) => {}
                Ok(Some(Err(err))) => panic!("连接异常：{err}"),
                Ok(None) => break,
                Err(_) => break,
            }
        }
        println!("收到虎牙弹幕 {chats} 条，样例：{samples:#?}");
        assert!(chats > 0, "30 秒内没有收到任何虎牙弹幕");
    }
}

/// 抖音：一条 WS，帧里可能一次带回几十条消息；批量窗口与其它平台保持一致。
async fn run_douyin_room(app: AppHandle, conn: Arc<RoomConn>) {
    let mut backoff = Duration::from_secs(3);
    loop {
        if conn.closed.load(Ordering::SeqCst) {
            break;
        }
        match douyin_session(&app, &conn).await {
            Ok(()) => {
                if conn.closed.load(Ordering::SeqCst) {
                    break;
                }
                conn.set_error("抖音弹幕连接被关闭");
            }
            Err(err) => {
                tracing::warn!(room = %conn.room.room_key, %err, "抖音弹幕断开");
                conn.set_error(err);
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = wait_closed(&conn) => break,
        }
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
}

async fn wait_closed(conn: &RoomConn) {
    while !conn.closed.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn douyin_session(app: &AppHandle, conn: &RoomConn) -> Result<(), String> {
    let mut stream = crate::douyin_ws::open(&conn.room.room_key).await?;
    let mut window = Instant::now();
    let mut count = 0u32;
    let mut batch: Vec<DanmakuMessage> = Vec::new();
    let mut last_flush = Instant::now();

    loop {
        if conn.closed.load(Ordering::SeqCst) {
            flush(app, conn, &mut batch);
            return Ok(());
        }
        let events =
            match tokio::time::timeout(Duration::from_millis(200), stream.next_events()).await {
                Err(_) => Vec::new(),
                Ok(Err(err)) => return Err(err),
                Ok(Ok(events)) => events,
            };
        for event in events {
            let message = DanmakuMessage {
                offset_ms: 0,
                kind: event.kind.into(),
                user: Some(event.user),
                content: event.content,
                color: if event.kind == "gift" {
                    0xFFB027
                } else {
                    0xFFFFFF
                },
            };
            accept(conn, &mut window, &mut count, &mut batch, message);
        }
        if last_flush.elapsed() >= BATCH_WINDOW {
            flush(app, conn, &mut batch);
            last_flush = Instant::now();
        }
    }
}
