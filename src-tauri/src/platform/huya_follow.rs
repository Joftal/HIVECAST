//! 虎牙关注列表：WUP/JCE 二进制协议。
//!
//! 虎牙没有给关注列表提供普通 JSON 接口，页面走的是 TAF/WUP（servant
//! huyauserui、func getUserSubscribeToInfoList）。这里直接拼 JCE 请求、
//! POST 到 cdnws.api.huya.com，再解出 GetUserSubscribeToInfoListRsp。
//!
//! 关注列表一行同时带主播、房间号、昵称、头像、分区、开播状态、人气和开播时间，
//! 因此它既能给导入弹窗供数据，也能给监控做批量状态检查；取流、画质探测、
//! 线路选择、弹幕 token 仍然必须逐房独立请求。

use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use hivecast_ipc::{HuyaFollowingPage, LiveStatus};

use super::huya::HuyaAdapter;
use super::{http, PlatformAdapter, PlatformError, RoomInfo, RoomRef};

const WUP_HOST: &str = "https://cdnws.api.huya.com";
const REFERER: &str = "https://www.huya.com/myFollow";
const SERVANT: &str = "huyauserui";
const FUNC: &str = "getUserSubscribeToInfoList";
/// 服务端固定返回 120 条/页，请求结构里没有 pageSize 字段。
const PAGE_SIZE: u32 = 120;
/// 关注很多时最多翻 50 页（6000 位）；再多说明账号异常或服务端分页失控。
const MAX_PAGES: u32 = 50;

const T_INT8: u8 = 0;
const T_INT16: u8 = 1;
const T_INT32: u8 = 2;
const T_INT64: u8 = 3;
const T_FLOAT: u8 = 4;
const T_DOUBLE: u8 = 5;
const T_STRING1: u8 = 6;
const T_STRING4: u8 = 7;
const T_MAP: u8 = 8;
const T_LIST: u8 = 9;
const T_STRUCT_BEGIN: u8 = 10;
const T_STRUCT_END: u8 = 11;
const T_ZERO: u8 = 12;
const T_SIMPLE_LIST: u8 = 13;

/// 拉取一页关注。
///
/// page 从 1 开始（协议里 iPageIndex 从 0 开始，这里在内部减一）。
pub async fn fetch_follow_page(page: u32) -> Result<HuyaFollowingPage, PlatformError> {
    let mut fetched = fetch_follow_page_raw(page).await?;
    // 导入弹窗也直接读这个接口：重播在关注列表里同样标「在播」，
    // 这里先复核，避免把回放展示成直播、并在导入时误触发自动录制。
    let (items, _) = confirm_live(fetched.items).await;
    fetched.items = items;
    Ok(fetched)
}

async fn fetch_follow_page_raw(page: u32) -> Result<HuyaFollowingPage, PlatformError> {
    let page = page.max(1);
    let cookie = crate::credential::cookie_for_url("https://www.huya.com").ok_or_else(|| {
        PlatformError::Upstream {
            code: -1,
            message: "虎牙关注列表需要登录".to_string(),
        }
    })?;
    let uid = cookie_value(&cookie, "udb_uid")
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| PlatformError::Upstream {
            code: -1,
            message: "虎牙登录态缺少 udb_uid，请重新扫码登录".to_string(),
        })?;
    let guid = cookie_value(&cookie, "guid").unwrap_or_default();

    let request = build_request(uid, &guid, &cookie, page - 1);
    let url = format!("{WUP_HOST}?baseinfo=default&timestamp={}", now_millis());
    let body = http::post_octets(
        &url,
        request,
        &[
            ("Content-Type", "application/octet-stream"),
            ("Referer", REFERER),
        ],
    )
    .await?;
    let parsed = parse_response(&body)?;
    if parsed.bcode != 0 {
        let message = if parsed.message.trim().is_empty() {
            format!("虎牙关注列表请求失败（bcode={}）", parsed.bcode)
        } else {
            parsed.message
        };
        return Err(PlatformError::Upstream {
            code: parsed.bcode,
            message,
        });
    }

    let total = parsed.total.max(0) as u64;
    let page_size = if parsed.page_size > 0 {
        parsed.page_size as u32
    } else {
        PAGE_SIZE
    };
    let page_count = page_count(total, page_size);
    let items = parsed
        .items
        .into_iter()
        .filter_map(LiveItem::into_status)
        .collect();
    Ok(HuyaFollowingPage {
        total,
        page,
        page_size,
        page_count,
        items,
    })
}

/// 批量状态：按 lUid 取关注列表，只返回调用方要的房间。
///
/// 关注列表可能因为取关、隐私设置或服务端缓存而缺项；缺项交给监控回退单房检查，
/// 这里绝不把「没返回」解释成「已下播」。
pub async fn fetch_live_statuses(keys: &[String]) -> Result<Vec<LiveStatus>, PlatformError> {
    let wanted: HashSet<&str> = keys.iter().map(String::as_str).collect();
    if wanted.is_empty() {
        return Ok(Vec::new());
    }

    let mut found: HashMap<String, LiveStatus> = HashMap::new();
    let mut page = 1u32;
    loop {
        // 批量只复核调用方要的房间，不能为页内每个在播关注都打一次页面。
        let fetched = fetch_follow_page_raw(page).await?;
        let page_count = fetched.page_count.max(1);
        for status in fetched.items {
            if wanted.contains(status.uid.as_str()) {
                found.entry(status.uid.clone()).or_insert(status);
            }
        }
        if found.len() >= wanted.len() || page >= page_count || page >= MAX_PAGES {
            break;
        }
        page += 1;
        // 关注列表是登录态接口，翻页稍作停顿，避免一次性打满风控。
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    Ok(found.into_values().collect())
}

/// 复核批量结果里「在播」的虎牙房间。
///
/// 关注列表把重播也标成在播（660000 的 `is_replay=1` 在 WUP 里没有对应字段），
/// 直接用批量结果会把回放当直播显示、甚至触发自动录制。这里只对在播项
/// 逐房打开页面确认，离线项仍由一次关注列表覆盖；复核失败时保留批量结果，
/// 绝不因为一次网络错误把正在直播的房间判成离线。
/// 返回 `(复核后的状态, 复核失败数)`。
pub async fn confirm_live(statuses: Vec<LiveStatus>) -> (Vec<LiveStatus>, usize) {
    // 关注多时在播项可能几十上百个；限制 6 路并发，既不串行拖慢整批，
    // 也不让一次批量复核瞬间打出上百个页面请求。
    let outcomes: Vec<(LiveStatus, bool)> =
        futures_util::stream::iter(statuses.into_iter().map(|mut status| async move {
            if !status.is_live {
                return (status, true);
            }
            let Some(key) = status
                .room_id
                .as_deref()
                .map(str::trim)
                .filter(|key| !key.is_empty())
            else {
                // 在播但拿不到房号，无法复核；计数里如实记为一次失败。
                return (status, false);
            };
            let room = RoomRef {
                platform: "huya".to_string(),
                room_key: key.to_string(),
                canonical_url: format!("https://www.huya.com/{key}"),
                input: String::new(),
            };
            match HuyaAdapter::new().room_info(&room).await {
                Ok(info) => {
                    apply_confirmation(&mut status, &info);
                    (status, true)
                }
                Err(err) => {
                    tracing::debug!(
                        target: "hivecast_lib::platform::huya_follow",
                        room = key,
                        error = %err,
                        "虎牙批量状态复核失败，保留批量结果"
                    );
                    (status, false)
                }
            }
        }))
        .buffered(6)
        .collect()
        .await;
    let failures = outcomes.iter().filter(|(_, ok)| !ok).count();
    (
        outcomes.into_iter().map(|(status, _)| status).collect(),
        failures,
    )
}

/// 用单房页面信息纠正批量状态：重播/下播判未开播，在播则补人气与开播时间。
fn apply_confirmation(status: &mut LiveStatus, info: &RoomInfo) {
    status.is_live = info.is_live;
    if info.is_live {
        status.started_at = info.started_at.or(status.started_at);
        if info.online.is_some() {
            status.online = info.online;
        }
    } else {
        status.started_at = None;
        status.online = None;
    }
}

fn page_count(total: u64, page_size: u32) -> u32 {
    if total == 0 || page_size == 0 {
        return 1;
    }
    total.div_ceil(page_size as u64) as u32
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

/// 从 Cookie 串里取一个值；同名只取第一个，值两端的空白去掉。
fn cookie_value(cookie: &str, name: &str) -> Option<String> {
    for part in cookie.split(';') {
        let mut pair = part.trim().splitn(2, '=');
        let key = pair.next()?.trim();
        if key != name {
            continue;
        }
        let value = pair.next()?.trim();
        if value.is_empty() {
            return None;
        }
        return Some(value.to_string());
    }
    None
}

// ---------------------------------------------------------------------------
// 请求编码
// ---------------------------------------------------------------------------

fn build_request(uid: i64, guid: &str, cookie: &str, page_index: u32) -> Vec<u8> {
    // GetUserSubscribeToInfoListReq：tId tag0、iPageIndex tag1。
    let mut req = Writer::default();
    req.struct_begin(0);
    req.struct_begin(0); // UserId
    req.int64(0, uid);
    req.string(1, guid);
    req.string(2, "");
    req.string(3, http::USER_AGENT);
    req.string(4, cookie);
    req.int32(5, 0);
    req.string(6, "");
    req.string(7, "");
    req.end_struct();
    req.int32(1, page_index as i32);
    req.end_struct();

    // WUP newdata：map<string, BinBuffer>{"tReq": req}。
    let mut sbuf = Writer::default();
    sbuf.map(0, 1);
    sbuf.string(0, "tReq");
    sbuf.bytes(1, &req.out);
    encode_wup(&sbuf.out, 9000 + page_index as i32)
}

fn encode_wup(sbuffer: &[u8], request_id: i32) -> Vec<u8> {
    let mut header = Writer::default();
    header.int16(1, 3); // iVersion：v3 的 sBuffer 是 map<string, BinBuffer>
    header.int8(2, 0); // cPacketType
    header.int32(3, 0); // iMessageType
    header.int32(4, request_id);
    header.string(5, SERVANT);
    header.string(6, FUNC);
    header.bytes(7, sbuffer);
    header.int32(8, 0); // iTimeout：0 表示用服务端默认
    header.map(9, 0);
    header.map(10, 0);

    let mut out = Vec::with_capacity(header.out.len() + 4);
    out.extend_from_slice(&((header.out.len() + 4) as u32).to_be_bytes());
    out.extend_from_slice(&header.out);
    out
}

#[derive(Default)]
struct Writer {
    out: Vec<u8>,
}

impl Writer {
    fn head(&mut self, tag: u8, kind: u8) {
        if tag < 15 {
            self.out.push((tag << 4) | kind);
        } else {
            self.out.push(0xF0 | kind);
            self.out.push(tag);
        }
    }

    fn int8(&mut self, tag: u8, value: i8) {
        self.head(tag, T_INT8);
        self.out.push(value as u8);
    }

    fn int16(&mut self, tag: u8, value: i16) {
        self.head(tag, T_INT16);
        self.out.extend_from_slice(&value.to_be_bytes());
    }

    fn int32(&mut self, tag: u8, value: i32) {
        self.head(tag, T_INT32);
        self.out.extend_from_slice(&value.to_be_bytes());
    }

    fn int64(&mut self, tag: u8, value: i64) {
        self.head(tag, T_INT64);
        self.out.extend_from_slice(&value.to_be_bytes());
    }

    fn string(&mut self, tag: u8, value: &str) {
        // JCE 的字符串长度是裸 u8/u32，不带 tag/type；按长度选 STRING1/STRING4。
        let bytes = value.as_bytes();
        if bytes.len() < 256 {
            self.head(tag, T_STRING1);
            self.out.push(bytes.len() as u8);
        } else {
            self.head(tag, T_STRING4);
            self.out
                .extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        }
        self.out.extend_from_slice(bytes);
    }

    fn bytes(&mut self, tag: u8, value: &[u8]) {
        self.head(tag, T_SIMPLE_LIST);
        self.head(0, T_INT8); // 元素类型：byte
        self.head(0, T_INT32);
        self.out
            .extend_from_slice(&(value.len() as i32).to_be_bytes());
        self.out.extend_from_slice(value);
    }

    fn struct_begin(&mut self, tag: u8) {
        self.head(tag, T_STRUCT_BEGIN);
    }

    fn end_struct(&mut self) {
        self.head(0, T_STRUCT_END);
    }

    fn map(&mut self, tag: u8, count: i32) {
        self.head(tag, T_MAP);
        self.head(0, T_INT32);
        self.out.extend_from_slice(&count.to_be_bytes());
    }

    #[cfg(test)]
    fn list(&mut self, tag: u8, count: i32) {
        self.head(tag, T_LIST);
        self.head(0, T_INT32);
        self.out.extend_from_slice(&count.to_be_bytes());
    }
}

// ---------------------------------------------------------------------------
// 响应解码
// ---------------------------------------------------------------------------

#[derive(Default)]
struct ParsedResponse {
    bcode: i64,
    message: String,
    total: i64,
    page_size: i64,
    items: Vec<LiveItem>,
}

#[derive(Default)]
struct LiveItem {
    uid: i64,
    room_id: i32,
    nick: String,
    private_host: String,
    avatar: String,
    is_live: i32,
    game_name: String,
    live_desc: String,
    attendee_count: i32,
    start_time: i32,
}

impl LiveItem {
    fn into_status(self) -> Option<LiveStatus> {
        if self.uid <= 0 {
            return None;
        }
        let is_live = self.is_live == 1;
        Some(LiveStatus {
            uid: self.uid.to_string(),
            room_id: (self.room_id > 0).then(|| self.room_id.to_string()),
            short_id: non_empty(self.private_host),
            uname: self.nick.trim().to_string(),
            title: self.live_desc.trim().to_string(),
            avatar_url: non_empty(self.avatar),
            cover_url: None,
            area_name: non_empty(self.game_name),
            is_live,
            // 虎牙关注列表没有轮播标记；是否重播由单房房间页的 is_replay 判断。
            video_loop: false,
            started_at: (is_live && self.start_time > 0).then_some(self.start_time as i64),
            online: (self.attendee_count > 0).then_some(self.attendee_count as u64),
        })
    }
}

fn non_empty(value: String) -> Option<String> {
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn parse_response(body: &[u8]) -> Result<ParsedResponse, PlatformError> {
    if body.len() < 4 {
        return Err(protocol_error("响应短于 4 字节长度前缀"));
    }
    let declared = u32::from_be_bytes([body[0], body[1], body[2], body[3]]) as usize;
    if declared < 4 || declared > body.len() {
        return Err(protocol_error("响应长度前缀与 body 不一致"));
    }
    let mut reader = Reader::new(&body[4..declared]);
    let mut sbuffer: Option<&[u8]> = None;
    while reader.remaining() > 0 {
        let (tag, kind) = reader.head()?;
        if tag == 7 && kind == T_SIMPLE_LIST {
            sbuffer = Some(reader.bytes_value(kind)?);
        } else {
            reader.skip(kind)?;
        }
    }
    let sbuffer = sbuffer.ok_or_else(|| protocol_error("WUP header 缺少 sBuffer"))?;
    parse_sbuffer(sbuffer)
}

fn parse_sbuffer(sbuffer: &[u8]) -> Result<ParsedResponse, PlatformError> {
    let mut reader = Reader::new(sbuffer);
    let (tag, kind) = reader.head()?;
    if tag != 0 || kind != T_MAP {
        return Err(protocol_error("sBuffer 不是 map"));
    }
    let count = reader.read_i32_field()?;
    if !(0..=64).contains(&count) {
        return Err(protocol_error("sBuffer map 大小异常"));
    }

    let mut parsed = ParsedResponse::default();
    let mut rsp: Option<&[u8]> = None;
    for _ in 0..count {
        let (_, key_kind) = reader.head()?;
        let key = reader.string_value(key_kind)?;
        let (_, value_kind) = reader.head()?;
        let value = reader.bytes_value(value_kind)?;
        if key.is_empty() {
            parsed.bcode = read_int_blob(value)?;
        } else if key == "tRsp" || key == "tResp" {
            rsp = Some(value);
        }
    }

    if let Some(rsp) = rsp {
        let (message, total, page_size, items) = parse_rsp(rsp)?;
        parsed.message = message;
        parsed.total = total;
        parsed.page_size = page_size;
        parsed.items = items;
    }
    Ok(parsed)
}

fn read_int_blob(blob: &[u8]) -> Result<i64, PlatformError> {
    let mut reader = Reader::new(blob);
    let (_, kind) = reader.head()?;
    reader.int_value(kind)
}

fn parse_rsp(blob: &[u8]) -> Result<(String, i64, i64, Vec<LiveItem>), PlatformError> {
    let mut reader = Reader::new(blob);
    let (_, kind) = reader.head()?;
    if kind != T_STRUCT_BEGIN {
        return Err(protocol_error("tRsp 不是结构体"));
    }

    let mut message = String::new();
    let mut total = 0i64;
    let mut page_size = 0i64;
    let mut items = Vec::new();
    loop {
        let (tag, kind) = reader.head()?;
        if kind == T_STRUCT_END {
            break;
        }
        match tag {
            0 => message = reader.string_value(kind)?,
            1 => {
                let count = reader.read_i32_field()?;
                if !(0..=500).contains(&count) {
                    return Err(protocol_error("关注列表长度异常"));
                }
                for _ in 0..count {
                    let (item_tag, item_kind) = reader.head()?;
                    if item_tag == 0 && item_kind == T_STRUCT_BEGIN {
                        items.push(parse_item(&mut reader)?);
                    } else {
                        reader.skip(item_kind)?;
                    }
                }
            }
            2 => total = reader.int_value(kind)?,
            3 => page_size = reader.int_value(kind)?,
            _ => reader.skip(kind)?,
        }
    }
    Ok((message, total, page_size, items))
}

fn parse_item(reader: &mut Reader<'_>) -> Result<LiveItem, PlatformError> {
    let mut item = LiveItem::default();
    loop {
        let (tag, kind) = reader.head()?;
        if kind == T_STRUCT_END {
            break;
        }
        match tag {
            0 => item.uid = reader.int_value(kind)?,
            2 => item.nick = reader.string_value(kind)?,
            3 => item.private_host = reader.string_value(kind)?,
            4 => item.avatar = reader.string_value(kind)?,
            5 => item.room_id = reader.int_value(kind)? as i32,
            9 => item.is_live = reader.int_value(kind)? as i32,
            11 => item.game_name = reader.string_value(kind)?,
            12 => item.live_desc = reader.string_value(kind)?,
            14 => item.attendee_count = reader.int_value(kind)? as i32,
            15 => item.start_time = reader.int_value(kind)? as i32,
            _ => reader.skip(kind)?,
        }
    }
    Ok(item)
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], PlatformError> {
        if self.pos + len > self.data.len() {
            return Err(protocol_error("字段越过响应末尾"));
        }
        let slice = &self.data[self.pos..self.pos + len];
        self.pos += len;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, PlatformError> {
        Ok(self.take(1)?[0])
    }

    fn head(&mut self) -> Result<(u8, u8), PlatformError> {
        let byte = self.u8()?;
        let mut tag = byte >> 4;
        let kind = byte & 0x0F;
        if tag == 15 {
            tag = self.u8()?;
        }
        Ok((tag, kind))
    }

    fn read_i32_field(&mut self) -> Result<i32, PlatformError> {
        let (_, kind) = self.head()?;
        Ok(self.int_value(kind)? as i32)
    }

    fn int_value(&mut self, kind: u8) -> Result<i64, PlatformError> {
        Ok(match kind {
            T_ZERO => 0,
            T_INT8 => self.u8()? as i8 as i64,
            T_INT16 => {
                let bytes = self.take(2)?;
                i16::from_be_bytes([bytes[0], bytes[1]]) as i64
            }
            T_INT32 => {
                let bytes = self.take(4)?;
                i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as i64
            }
            T_INT64 => {
                let bytes = self.take(8)?;
                i64::from_be_bytes(bytes.try_into().expect("8 字节"))
            }
            _ => return Err(protocol_error("期望整数")),
        })
    }

    fn string_value(&mut self, kind: u8) -> Result<String, PlatformError> {
        let len = match kind {
            T_ZERO => 0,
            T_STRING1 => self.u8()? as usize,
            T_STRING4 => {
                let bytes = self.take(4)?;
                u32::from_be_bytes(bytes.try_into().expect("4 字节")) as usize
            }
            _ => return Err(protocol_error("期望字符串")),
        };
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| protocol_error("字符串不是 UTF-8"))
    }

    fn bytes_value(&mut self, kind: u8) -> Result<&'a [u8], PlatformError> {
        match kind {
            T_SIMPLE_LIST => {
                let (tag, inner) = self.head()?;
                if tag != 0 || inner != T_INT8 {
                    return Err(protocol_error("字节数组元素类型异常"));
                }
                let len = self.read_i32_field()?.max(0) as usize;
                self.take(len)
            }
            _ => Err(protocol_error("期望字节数组")),
        }
    }

    fn skip(&mut self, kind: u8) -> Result<(), PlatformError> {
        match kind {
            T_ZERO | T_STRUCT_END => {}
            T_INT8 => {
                self.take(1)?;
            }
            T_INT16 => {
                self.take(2)?;
            }
            T_INT32 | T_FLOAT => {
                self.take(4)?;
            }
            T_INT64 | T_DOUBLE => {
                self.take(8)?;
            }
            T_STRING1 => {
                let len = self.u8()? as usize;
                self.take(len)?;
            }
            T_STRING4 => {
                let bytes = self.take(4)?;
                let len = u32::from_be_bytes(bytes.try_into().expect("4 字节")) as usize;
                self.take(len)?;
            }
            T_MAP => {
                let count = self.read_i32_field()?.max(0);
                for _ in 0..count {
                    let (_, key_kind) = self.head()?;
                    self.skip(key_kind)?;
                    let (_, value_kind) = self.head()?;
                    self.skip(value_kind)?;
                }
            }
            T_LIST => {
                let count = self.read_i32_field()?.max(0);
                for _ in 0..count {
                    let (_, item_kind) = self.head()?;
                    self.skip(item_kind)?;
                }
            }
            T_SIMPLE_LIST => {
                let (_, inner) = self.head()?;
                if inner != T_INT8 {
                    return Err(protocol_error("简单数组元素类型异常"));
                }
                let len = self.read_i32_field()?.max(0) as usize;
                self.take(len)?;
            }
            T_STRUCT_BEGIN => loop {
                let (_, field_kind) = self.head()?;
                if field_kind == T_STRUCT_END {
                    break;
                }
                self.skip(field_kind)?;
            },
            _ => return Err(protocol_error("未知 JCE 类型")),
        }
        Ok(())
    }
}

fn protocol_error(message: &str) -> PlatformError {
    PlatformError::Parse(format!("虎牙关注列表协议解析失败：{message}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_rsp() -> Vec<u8> {
        let mut rsp = Writer::default();
        rsp.struct_begin(0);
        rsp.string(0, "");
        rsp.list(1, 1);
        rsp.struct_begin(0);
        rsp.int64(0, 1_346_609_715);
        rsp.int64(1, 1_486_578_378);
        rsp.string(2, "虎牙英雄联盟赛事");
        rsp.string(3, "lpl");
        rsp.string(4, "https://huyaimg.msstatic.com/avatar/1.jpg");
        rsp.int32(5, 660_000);
        rsp.int32(9, 1);
        rsp.string(11, "英雄联盟");
        rsp.string(12, "恭喜BLG获得2026英雄联盟全球总决赛冠军");
        rsp.int32(14, 1_015_322);
        rsp.int32(15, 1_793_092_036);
        rsp.end_struct();
        rsp.int32(2, 11);
        rsp.int32(3, 120);
        rsp.int32(4, 0);
        rsp.end_struct();
        rsp.out
    }

    fn sample_body() -> Vec<u8> {
        let mut sbuf = Writer::default();
        sbuf.map(0, 2);
        sbuf.string(0, "");
        let mut bcode = Writer::default();
        bcode.int32(0, 0);
        sbuf.bytes(1, &bcode.out);
        sbuf.string(0, "tRsp");
        sbuf.bytes(1, &sample_rsp());

        let mut header = Writer::default();
        header.int16(1, 3);
        header.int32(4, 9000);
        header.string(5, SERVANT);
        header.string(6, FUNC);
        header.bytes(7, &sbuf.out);
        header.int32(8, 0);
        header.map(9, 0);
        header.map(10, 0);

        let mut out = Vec::new();
        out.extend_from_slice(&((header.out.len() + 4) as u32).to_be_bytes());
        out.extend_from_slice(&header.out);
        out
    }

    #[test]
    fn replay_confirmation_clears_live_state() {
        let mut status = LiveStatus {
            uid: "1346609715".to_string(),
            room_id: Some("660000".to_string()),
            short_id: Some("lpl".to_string()),
            uname: "虎牙英雄联盟赛事".to_string(),
            title: "恭喜BLG".to_string(),
            avatar_url: None,
            cover_url: None,
            area_name: None,
            is_live: true,
            video_loop: false,
            started_at: Some(1_793_092_036),
            online: Some(1_015_322),
        };
        let info = RoomInfo {
            host_name: "虎牙英雄联盟赛事".to_string(),
            room_name: "S16".to_string(),
            avatar_url: None,
            cover_url: None,
            area_name: None,
            is_live: false,
            started_at: Some(0),
            online: Some(1_010_000),
            stable_id: None,
            extra: serde_json::Value::Null,
        };
        apply_confirmation(&mut status, &info);
        assert!(!status.is_live);
        assert_eq!(status.started_at, None);
        assert_eq!(status.online, None);
    }

    #[test]
    fn live_confirmation_keeps_start_time_and_refreshes_online() {
        let mut status = LiveStatus {
            uid: "1".to_string(),
            room_id: Some("2".to_string()),
            short_id: None,
            uname: "主播".to_string(),
            title: "标题".to_string(),
            avatar_url: None,
            cover_url: None,
            area_name: None,
            is_live: true,
            video_loop: false,
            started_at: None,
            online: Some(10),
        };
        let info = RoomInfo {
            host_name: "主播".to_string(),
            room_name: "标题".to_string(),
            avatar_url: None,
            cover_url: None,
            area_name: None,
            is_live: true,
            started_at: Some(1_793_000_000),
            online: Some(20),
            stable_id: None,
            extra: serde_json::Value::Null,
        };
        apply_confirmation(&mut status, &info);
        assert!(status.is_live);
        assert_eq!(status.started_at, Some(1_793_000_000));
        assert_eq!(status.online, Some(20));
    }

    #[test]
    fn parses_follow_page_payload() {
        let parsed = parse_response(&sample_body()).expect("响应应可解析");
        assert_eq!(parsed.bcode, 0);
        assert_eq!(parsed.total, 11);
        assert_eq!(parsed.page_size, 120);
        assert_eq!(parsed.items.len(), 1);
        let status = parsed
            .items
            .into_iter()
            .next()
            .unwrap()
            .into_status()
            .unwrap();
        assert_eq!(status.uid, "1346609715");
        assert_eq!(status.room_id.as_deref(), Some("660000"));
        assert_eq!(status.short_id.as_deref(), Some("lpl"));
        assert_eq!(status.uname, "虎牙英雄联盟赛事");
        assert!(status.is_live);
        assert_eq!(status.started_at, Some(1_793_092_036));
        assert_eq!(status.online, Some(1_015_322));
    }

    #[test]
    fn page_count_covers_remainder_and_empty() {
        assert_eq!(page_count(0, 120), 1);
        assert_eq!(page_count(11, 120), 1);
        assert_eq!(page_count(120, 120), 1);
        assert_eq!(page_count(121, 120), 2);
    }

    #[test]
    fn jce_string_uses_raw_length_prefix() {
        let mut short = Writer { out: Vec::new() };
        short.string(3, "abc");
        assert_eq!(short.out, vec![0x36, 0x03, b'a', b'b', b'c']);

        let long = "x".repeat(300);
        let mut wide = Writer { out: Vec::new() };
        wide.string(3, &long);
        assert_eq!(&wide.out[..7], &[0x37, 0x00, 0x00, 0x01, 0x2c, b'x', b'x']);
        assert_eq!(wide.out.len(), 5 + 300);
    }

    #[test]
    fn cookie_value_is_strict_about_names() {
        let cookie = "udb_uid=1199573670219; guid=abc; udb_uid2=1";
        assert_eq!(
            cookie_value(cookie, "udb_uid").as_deref(),
            Some("1199573670219")
        );
        assert_eq!(cookie_value(cookie, "guid").as_deref(), Some("abc"));
        assert_eq!(cookie_value(cookie, "udb_uid2").as_deref(), Some("1"));
        assert_eq!(cookie_value(cookie, "missing"), None);
    }
}
