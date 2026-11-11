//! StreamBroker：应用内所有「取流 / 取房间信息」调用的唯一入口。
//!
//! 它不持有流会话，只负责把「房间 + 清晰度」翻译成候选线路，并做四件事：
//! 1. 缓存（流候选 60s，房间信息 15s）；
//! 2. 并发去重（同 key 同一时刻只有一个请求真的出门）；
//! 3. 限速（同一房间至少间隔 1.5 秒，房间信息与取流共用闸门）；
//! 4. 退避（-352 / 429 / 网络错误指数退避，退避期内有旧数据就先给旧数据）。
//!
//! 录制端和观看端各自持有不可变的 StreamLease，互不影响（见 docs/19 §3）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Mutex as AsyncMutex;

use crate::platform::{adapter_for, PlatformError, RoomInfo, RoomRef, StreamPlan};

const GROUPS_TTL: Duration = Duration::from_secs(60);
const INFO_TTL: Duration = Duration::from_secs(15);
const MIN_INTERVAL: Duration = Duration::from_millis(1500);
const BACKOFF_BASE: Duration = Duration::from_secs(5);
const BACKOFF_MAX: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct StreamKey {
    platform: String,
    room_key: String,
    qn: u32,
}

impl StreamKey {
    fn new(room: &RoomRef, qn: u32) -> Self {
        Self {
            platform: room.platform.to_string(),
            room_key: room.room_key.clone(),
            qn,
        }
    }
}

fn room_gate(room: &RoomRef) -> String {
    format!("{}:{}", room.platform, room.room_key)
}

/// 丢掉这个房间的播放地址与房间信息缓存，但保留失败退避。
/// 录制续录、看播侧重连都只该丢地址，不该替监控清冷却（A5/B3）。
fn drop_address_cache(inner: &mut Inner, room: &RoomRef) {
    inner
        .groups
        .retain(|key, _| !(key.platform == room.platform && key.room_key == room.room_key));
    inner.infos.remove(&room_gate(room));
}

struct Cached<T> {
    at: Instant,
    value: Arc<T>,
}

struct Failure {
    attempts: u32,
    until: Instant,
}

#[derive(Default)]
struct Inner {
    groups: HashMap<StreamKey, Cached<StreamPlan>>,
    infos: HashMap<String, Cached<RoomInfo>>,
    group_inflight: HashMap<StreamKey, Arc<AsyncMutex<()>>>,
    info_inflight: HashMap<String, Arc<AsyncMutex<()>>>,
    gates: HashMap<String, Instant>,
    failures: HashMap<String, Failure>,
    /// 凭据变更代次：clear_groups 递增，用来丢弃在途请求的旧结果
    epoch: u64,
}

#[derive(Default)]
pub struct StreamBroker {
    inner: Mutex<Inner>,
}

impl StreamBroker {
    pub fn new() -> Self {
        Self::default()
    }

    /// 取某房间某档位的线路组与真实画质清单。命中缓存时不会触网。
    pub async fn plan(&self, room: &RoomRef, qn: u32) -> Result<Arc<StreamPlan>, PlatformError> {
        self.plan_inner(room, qn, false, false).await
    }

    /// 断流续录专用：丢掉旧地址重新解析，但保留共享的失败冷却（B3）。
    ///
    /// 录制重启走 invalidate 会把监控侧的限流退避一起清掉：限流风暴时每续录
    /// 一次就等于放行一轮请求。这里只丢地址缓存；本次解析允许在冷却期内出门
    /// （用户已经在等这一路恢复），失败则照常累计退避。
    pub async fn plan_for_respawn(
        &self,
        room: &RoomRef,
        qn: u32,
    ) -> Result<Arc<StreamPlan>, PlatformError> {
        {
            let mut inner = self.inner.lock().expect("broker 状态被污染");
            drop_address_cache(&mut inner, room);
        }
        self.plan_inner(room, qn, true, false).await
    }

    /// 画质升级探测：丢地址重新解析，但失败不累计监控退避、成功也不清退避。
    /// 升级是锦上添花，探测失败不能把正常的监控节奏打乱。
    pub async fn probe(&self, room: &RoomRef, qn: u32) -> Result<Arc<StreamPlan>, PlatformError> {
        {
            let mut inner = self.inner.lock().expect("broker 状态被污染");
            drop_address_cache(&mut inner, room);
        }
        self.plan_inner(room, qn, true, true).await
    }

    async fn plan_inner(
        &self,
        room: &RoomRef,
        qn: u32,
        bypass_cooldown: bool,
        probe: bool,
    ) -> Result<Arc<StreamPlan>, PlatformError> {
        let key = StreamKey::new(room, qn);
        // 冷却与限速按「房间」算，而不是按「房间+档位」：以前 info 用房间键、
        // groups 用裸 room_key，两套键互不相干，invalidate 也就清不干净（C3）。
        let gate_key = room_gate(room);
        if let Some(plan) = self.cached_plan(&key) {
            tracing::debug!(platform = room.platform, room = %room.room_key, qn, "取流命中缓存");
            return Ok(plan);
        }

        let gate = self.group_gate(&key);
        let _guard = gate.lock().await;

        if let Some(plan) = self.cached_plan(&key) {
            return Ok(plan);
        }
        if !bypass_cooldown {
            if let Some(err) = self.backoff_error(&gate_key) {
                tracing::debug!(
                    platform = room.platform,
                    room = %room.room_key,
                    retry_after = err.retry_after(),
                    "取流仍在失败冷却中，直接返回"
                );
                return Err(err);
            }
        }
        self.wait_turn(&gate_key).await;

        let epoch = self.inner.lock().expect("broker 状态被污染").epoch;
        let adapter = adapter_for(&room.platform)?;
        let started = Instant::now();
        let result = adapter.stream_plan(room, qn).await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        match &result {
            Ok(plan) => tracing::debug!(
                platform = room.platform,
                room = %room.room_key,
                qn,
                groups = plan.groups.len(),
                qualities = plan.qualities.len(),
                elapsed_ms,
                "取流请求完成"
            ),
            Err(err) => tracing::warn!(
                platform = room.platform,
                room = %room.room_key,
                qn,
                elapsed_ms,
                error = %err,
                "取流请求失败"
            ),
        }

        let mut inner = self.inner.lock().expect("broker 状态被污染");
        inner.group_inflight.remove(&key);
        match result {
            Ok(plan) => {
                if inner.epoch != epoch {
                    tracing::debug!(
                        platform = room.platform,
                        room = %room.room_key,
                        qn,
                        "解析期间凭据已变更，丢弃本次取流缓存"
                    );
                    return Ok(Arc::new(plan));
                }
                if !probe {
                    inner.failures.remove(&gate_key);
                }
                let plan = Arc::new(plan);
                inner.groups.insert(
                    key,
                    Cached {
                        at: Instant::now(),
                        value: plan.clone(),
                    },
                );
                Ok(plan)
            }
            Err(err) => {
                if inner.epoch != epoch {
                    tracing::debug!(
                        platform = room.platform,
                        room = %room.room_key,
                        qn,
                        "解析期间凭据已变更，丢弃本次取流失败"
                    );
                    return Err(err);
                }
                if !probe {
                    self.note_failure(&mut inner, &gate_key, &err);
                }
                Err(err)
            }
        }
    }

    /// 取房间信息（开播状态 / 标题 / 人气）。手动刷新、开录前检查走这里。
    pub async fn room_info(&self, room: &RoomRef) -> Result<Arc<RoomInfo>, PlatformError> {
        self.room_info_inner(room, false).await
    }

    /// 监控轮询专用：跳过 15 秒信息缓存，但仍走房间闸门与失败退避。
    /// 间隔设得比缓存短时（例如 5 秒），用它可以真的按间隔取到新状态。
    pub async fn room_info_fresh(&self, room: &RoomRef) -> Result<Arc<RoomInfo>, PlatformError> {
        self.room_info_inner(room, true).await
    }

    async fn room_info_inner(
        &self,
        room: &RoomRef,
        fresh: bool,
    ) -> Result<Arc<RoomInfo>, PlatformError> {
        let gate_key = room_gate(room);
        if !fresh {
            if let Some(info) = self.cached_info(&gate_key) {
                tracing::debug!(platform = room.platform, room = %room.room_key, "房间信息命中缓存");
                return Ok(info);
            }
        }

        let gate = self.info_gate(&gate_key);
        let _guard = gate.lock().await;

        if !fresh {
            if let Some(info) = self.cached_info(&gate_key) {
                return Ok(info);
            }
        }
        if let Some(err) = self.backoff_error(&gate_key) {
            tracing::debug!(
                platform = room.platform,
                room = %room.room_key,
                retry_after = err.retry_after(),
                "房间信息仍在失败冷却中，直接返回"
            );
            return Err(err);
        }
        self.wait_turn(&gate_key).await;

        let epoch = self.inner.lock().expect("broker 状态被污染").epoch;
        let adapter = adapter_for(&room.platform)?;
        let started = Instant::now();
        let result = adapter.room_info(room).await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        match &result {
            Ok(info) => tracing::debug!(
                platform = room.platform,
                room = %room.room_key,
                live = info.is_live,
                elapsed_ms,
                "房间信息请求完成"
            ),
            Err(err) => tracing::warn!(
                platform = room.platform,
                room = %room.room_key,
                elapsed_ms,
                error = %err,
                "房间信息请求失败"
            ),
        }

        let mut inner = self.inner.lock().expect("broker 状态被污染");
        inner.info_inflight.remove(&gate_key);
        match result {
            Ok(info) => {
                if inner.epoch != epoch {
                    tracing::debug!(
                        platform = room.platform,
                        room = %room.room_key,
                        "解析期间凭据已变更，丢弃本次房间信息缓存"
                    );
                    return Ok(Arc::new(info));
                }
                inner.failures.remove(&gate_key);
                let info = Arc::new(info);
                inner.infos.insert(
                    gate_key,
                    Cached {
                        at: Instant::now(),
                        value: info.clone(),
                    },
                );
                Ok(info)
            }
            Err(err) => {
                if inner.epoch != epoch {
                    tracing::debug!(
                        platform = room.platform,
                        room = %room.room_key,
                        "解析期间凭据已变更，丢弃本次房间信息失败"
                    );
                    return Err(err);
                }
                self.note_failure(&mut inner, &gate_key, &err);
                Err(err)
            }
        }
    }

    /// 录制侧反馈线路质量，供后续选线参考（M3 起在 UI 上展示）。
    #[allow(dead_code)] // M2 线路质量统计接入口
    pub fn report_line(&self, room: &RoomRef, line: &str, ttfb: Option<Duration>) {
        tracing::debug!(
            platform = room.platform,
            room = %room.room_key,
            line,
            ttfb = ?ttfb,
            "线路反馈"
        );
    }

    /// 只看播侧重连时用：只丢播放地址与房间信息，保留失败计数。
    ///
    /// 用 invalidate 会把共享的失败退避也清掉：看视频断一次线，录制侧的
    /// 限流保护就没了，监控会立刻再去打接口（A5）。
    pub fn invalidate_address(&self, room: &RoomRef) {
        let mut inner = self.inner.lock().expect("broker 状态被污染");
        drop_address_cache(&mut inner, room);
        tracing::debug!(platform = room.platform, room = %room.room_key, "已失效地址缓存（保留失败冷却）");
    }

    /// 房间状态变化（下播 / 手动刷新）时主动失效缓存。
    pub fn invalidate(&self, room: &RoomRef) {
        let mut inner = self.inner.lock().expect("broker 状态被污染");
        drop_address_cache(&mut inner, room);
        inner.failures.remove(&room_gate(room));
        tracing::debug!(platform = room.platform, room = %room.room_key, "已失效地址缓存与失败冷却");
    }

    /// 账号凭据变更（登录 / 退出 / 换号）后清空所有取流缓存。
    ///
    /// 候选地址里带着按登录态算出的画质，缓存 60 秒；不清的话，用户刚登录
    /// 仍可能按游客档位开录，直到下一次监控刷新。失败的冷却保留，避免把
    /// 一次凭据变更当成放行限流的理由。
    pub fn clear_groups(&self) {
        let mut inner = self.inner.lock().expect("broker 状态被污染");
        inner.groups.clear();
        inner.infos.clear();
        inner.epoch = inner.epoch.wrapping_add(1);
        tracing::debug!("账号凭据变更，已清空取流与房间信息缓存");
    }

    fn cached_plan(&self, key: &StreamKey) -> Option<Arc<StreamPlan>> {
        let inner = self.inner.lock().expect("broker 状态被污染");
        let cached = inner.groups.get(key)?;
        (cached.at.elapsed() <= GROUPS_TTL).then(|| cached.value.clone())
    }

    fn cached_info(&self, gate_key: &str) -> Option<Arc<RoomInfo>> {
        let inner = self.inner.lock().expect("broker 状态被污染");
        let cached = inner.infos.get(gate_key)?;
        (cached.at.elapsed() <= INFO_TTL).then(|| cached.value.clone())
    }

    fn group_gate(&self, key: &StreamKey) -> Arc<AsyncMutex<()>> {
        let mut inner = self.inner.lock().expect("broker 状态被污染");
        inner.group_inflight.entry(key.clone()).or_default().clone()
    }

    fn info_gate(&self, gate_key: &str) -> Arc<AsyncMutex<()>> {
        let mut inner = self.inner.lock().expect("broker 状态被污染");
        inner
            .info_inflight
            .entry(gate_key.to_string())
            .or_default()
            .clone()
    }

    fn backoff_error(&self, gate_key: &str) -> Option<PlatformError> {
        let inner = self.inner.lock().expect("broker 状态被污染");
        cooldown_error(inner.failures.get(gate_key)?, Instant::now())
    }

    async fn wait_turn(&self, gate_key: &str) {
        let wait = {
            let mut inner = self.inner.lock().expect("broker 状态被污染");
            let now = Instant::now();
            let ready_at = inner
                .gates
                .get(gate_key)
                .map(|last| *last + MIN_INTERVAL)
                .unwrap_or(now);
            let wait = ready_at.saturating_duration_since(now);
            if wait.is_zero() {
                inner.gates.insert(gate_key.to_string(), now);
            }
            wait
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
            let mut inner = self.inner.lock().expect("broker 状态被污染");
            inner.gates.insert(gate_key.to_string(), Instant::now());
        }
    }

    fn note_failure(&self, inner: &mut Inner, gate_key: &str, err: &PlatformError) {
        if !should_backoff(err) {
            return;
        }
        let attempts = inner
            .failures
            .get(gate_key)
            .map(|failure| failure.attempts + 1)
            .unwrap_or(1);
        let delay = backoff(attempts);
        inner.failures.insert(
            gate_key.to_string(),
            Failure {
                attempts,
                until: Instant::now() + delay,
            },
        );
        tracing::warn!(room = %gate_key, attempts, delay_secs = delay.as_secs(), error = %err, "平台请求失败，进入冷却");
    }
}

/// 冷却期内的合成错误：调用方据此重排，但不会把它当成一次真失败。
fn cooldown_error(failure: &Failure, now: Instant) -> Option<PlatformError> {
    if now >= failure.until {
        return None;
    }
    let wait = failure.until.saturating_duration_since(now);
    Some(PlatformError::Throttled {
        retry_after: wait.as_secs().max(1),
    })
}

/// 与 PlatformError::is_throttle 共用一套判定，避免 broker 与监控对同一个
/// 错误码一个退避、一个不退避。
fn should_backoff(err: &PlatformError) -> bool {
    err.is_throttle()
}

fn backoff(attempts: u32) -> Duration {
    let factor = 2u32.saturating_pow(attempts.saturating_sub(1).min(4));
    (BACKOFF_BASE * factor).min(BACKOFF_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff(1), Duration::from_secs(5));
        assert_eq!(backoff(2), Duration::from_secs(10));
        assert_eq!(backoff(3), Duration::from_secs(20));
        assert_eq!(backoff(9), BACKOFF_MAX);
    }

    #[test]
    fn only_transient_errors_backoff() {
        assert!(should_backoff(&PlatformError::Network("x".into())));
        assert!(should_backoff(&PlatformError::Upstream {
            code: -352,
            message: String::new()
        }));
        assert!(!should_backoff(&PlatformError::NotLive));
        assert!(!should_backoff(&PlatformError::RoomNotFound("1".into())));
    }

    #[test]
    fn cooldown_reports_retry_after_instead_of_a_fake_network_error() {
        let now = Instant::now();
        let failure = Failure {
            attempts: 2,
            until: now + Duration::from_secs(7),
        };
        let err = cooldown_error(&failure, now).expect("冷却期内应返回错误");
        assert_eq!(err.retry_after(), Some(7));
        assert_eq!(err.code(), "platform_throttled");
        // 冷却结束后不再拦截。
        assert!(cooldown_error(&failure, now + Duration::from_secs(8)).is_none());
    }

    #[test]
    fn room_gate_ignores_quality() {
        let room = RoomRef {
            platform: "bilibili".to_string(),
            room_key: "545068".into(),
            canonical_url: String::new(),
            input: String::new(),
        };
        assert_eq!(room_gate(&room), "bilibili:545068");
        assert_eq!(StreamKey::new(&room, 10000).room_key, "545068");
    }

    #[test]
    fn invalidate_clears_the_shared_room_cooldown() {
        let broker = StreamBroker::new();
        let room = RoomRef {
            platform: "douyu".to_string(),
            room_key: "9999".into(),
            canonical_url: String::new(),
            input: String::new(),
        };
        // 取流失败与房间信息共用同一把房间键：一处进冷却，另一处也要看得到。
        {
            let mut inner = broker.inner.lock().expect("broker 状态被污染");
            broker.note_failure(
                &mut inner,
                &room_gate(&room),
                &PlatformError::Network("x".into()),
            );
        }
        assert!(broker.backoff_error(&room_gate(&room)).is_some());
        broker.invalidate(&room);
        assert!(broker.backoff_error(&room_gate(&room)).is_none());
    }

    #[test]
    fn dropping_address_cache_keeps_the_shared_cooldown() {
        let broker = StreamBroker::new();
        let room = RoomRef {
            platform: "douyu".to_string(),
            room_key: "9999".into(),
            canonical_url: String::new(),
            input: String::new(),
        };
        {
            let mut inner = broker.inner.lock().expect("broker 状态被污染");
            broker.note_failure(
                &mut inner,
                &room_gate(&room),
                &PlatformError::Network("x".into()),
            );
            inner.infos.insert(
                room_gate(&room),
                Cached {
                    at: Instant::now(),
                    value: Arc::new(RoomInfo {
                        host_name: "主播".into(),
                        room_name: "房间".into(),
                        avatar_url: None,
                        cover_url: None,
                        area_name: None,
                        is_live: true,
                        started_at: Some(1),
                        online: None,
                        stable_id: None,
                        extra: serde_json::Value::Null,
                    }),
                },
            );
        }
        broker.invalidate_address(&room);
        // 地址与信息缓存清掉了，失败冷却还在：续录不能替监控放行限流（B3）。
        assert!(broker.cached_info(&room_gate(&room)).is_none());
        assert!(broker.backoff_error(&room_gate(&room)).is_some());
    }

    #[test]
    fn credential_change_advances_epoch_and_keeps_cooldown() {
        let broker = StreamBroker::new();
        let room = RoomRef {
            platform: "bilibili".to_string(),
            room_key: "1".into(),
            canonical_url: String::new(),
            input: String::new(),
        };
        {
            let mut inner = broker.inner.lock().expect("broker 状态被污染");
            broker.note_failure(
                &mut inner,
                &room_gate(&room),
                &PlatformError::Network("x".into()),
            );
            assert_eq!(inner.epoch, 0);
        }
        broker.clear_groups();
        let inner = broker.inner.lock().expect("broker 状态被污染");
        // 代次递增让在途旧请求无法回填；冷却保留，避免登录/轮换被当成放行限流。
        assert_eq!(inner.epoch, 1);
        assert!(inner.groups.is_empty());
        assert!(inner.infos.is_empty());
        assert!(inner.failures.contains_key(&room_gate(&room)));
    }
}
