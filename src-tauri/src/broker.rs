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

use crate::platform::{adapter_for, PlatformError, RoomInfo, RoomRef, StreamGroup};

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
    groups: HashMap<StreamKey, Cached<Vec<StreamGroup>>>,
    infos: HashMap<String, Cached<RoomInfo>>,
    group_inflight: HashMap<StreamKey, Arc<AsyncMutex<()>>>,
    info_inflight: HashMap<String, Arc<AsyncMutex<()>>>,
    gates: HashMap<String, Instant>,
    failures: HashMap<String, Failure>,
}

#[derive(Default)]
pub struct StreamBroker {
    inner: Mutex<Inner>,
}

impl StreamBroker {
    pub fn new() -> Self {
        Self::default()
    }

    /// 取某房间某档位的候选线路。命中缓存时不会触网。
    pub async fn groups(
        &self,
        room: &RoomRef,
        qn: u32,
    ) -> Result<Arc<Vec<StreamGroup>>, PlatformError> {
        let key = StreamKey::new(room, qn);
        if let Some(groups) = self.cached_groups(&key) {
            return Ok(groups);
        }

        let gate = self.group_gate(&key);
        let _guard = gate.lock().await;

        if let Some(groups) = self.cached_groups(&key) {
            return Ok(groups);
        }
        if let Some(err) = self.backoff_error(&key.room_key) {
            return Err(err);
        }
        self.wait_turn(&key.room_key).await;

        let adapter = adapter_for(&room.platform)?;
        let started = Instant::now();
        let result = adapter.stream_groups(room, qn).await;
        tracing::debug!(
            platform = room.platform,
            room = %room.room_key,
            qn,
            elapsed_ms = started.elapsed().as_millis() as u64,
            ok = result.is_ok(),
            "取流请求完成"
        );

        let mut inner = self.inner.lock().expect("broker 状态被污染");
        inner.group_inflight.remove(&key);
        match result {
            Ok(groups) => {
                inner.failures.remove(&key.room_key);
                let groups = Arc::new(groups);
                inner.groups.insert(
                    key,
                    Cached {
                        at: Instant::now(),
                        value: groups.clone(),
                    },
                );
                Ok(groups)
            }
            Err(err) => {
                self.note_failure(&mut inner, &key.room_key, &err);
                Err(err)
            }
        }
    }

    /// 取房间信息（开播状态 / 标题 / 人气）。监控轮询走这里。
    pub async fn room_info(&self, room: &RoomRef) -> Result<Arc<RoomInfo>, PlatformError> {
        let gate_key = room_gate(room);
        if let Some(info) = self.cached_info(&gate_key) {
            return Ok(info);
        }

        let gate = self.info_gate(&gate_key);
        let _guard = gate.lock().await;

        if let Some(info) = self.cached_info(&gate_key) {
            return Ok(info);
        }
        if let Some(err) = self.backoff_error(&gate_key) {
            return Err(err);
        }
        self.wait_turn(&gate_key).await;

        let adapter = adapter_for(&room.platform)?;
        let result = adapter.room_info(room).await;

        let mut inner = self.inner.lock().expect("broker 状态被污染");
        inner.info_inflight.remove(&gate_key);
        match result {
            Ok(info) => {
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

    /// 房间状态变化（下播 / 手动刷新）时主动失效缓存。
    pub fn invalidate(&self, room: &RoomRef) {
        let mut inner = self.inner.lock().expect("broker 状态被污染");
        inner
            .groups
            .retain(|key, _| !(key.platform == room.platform && key.room_key == room.room_key));
        inner.infos.remove(&room_gate(room));
        inner.failures.remove(&room_gate(room));
    }

    fn cached_groups(&self, key: &StreamKey) -> Option<Arc<Vec<StreamGroup>>> {
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
        let failure = inner.failures.get(gate_key)?;
        let now = Instant::now();
        if now >= failure.until {
            return None;
        }
        let wait = failure.until.saturating_duration_since(now);
        Some(PlatformError::Network(format!(
            "平台请求过快，请 {} 秒后重试",
            wait.as_secs().max(1)
        )))
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
        inner.failures.insert(
            gate_key.to_string(),
            Failure {
                attempts,
                until: Instant::now() + backoff(attempts),
            },
        );
    }
}

fn should_backoff(err: &PlatformError) -> bool {
    match err {
        PlatformError::Network(_) => true,
        PlatformError::Upstream { code, .. } => matches!(*code, -352 | -429 | 429),
        _ => false,
    }
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
}
