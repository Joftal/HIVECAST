//! SQLite 持久层：房间列表 + 录制历史。
//!
//! 为什么用 rusqlite 而不是 async 版：本地单文件、查询都是毫秒级，
//! 加一层 spawn_blocking 或 async 包装只会让代码更难读；这里用 Mutex 串行化，
//! 并通过 WAL 让读不阻塞写。真正的长任务（ffmpeg）不在锁内。

use std::path::Path;
use std::sync::Mutex;

use hivecast_ipc::{MediaHost, MediaHostPage, MediaItem, PlatformCount, RecordSession, RoomRecord};
use rusqlite::types::Value as SqlValue;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::Value;

use crate::error::AppError;
use crate::platform::RoomInfo;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS room (\n    id TEXT PRIMARY KEY,\n    platform TEXT NOT NULL,\n    room_key TEXT NOT NULL,\n    canonical_url TEXT NOT NULL,\n    title TEXT NOT NULL DEFAULT '',\n    host_name TEXT NOT NULL DEFAULT '',\n    avatar_url TEXT,\n    cover_url TEXT,\n    area_name TEXT,\n    is_live INTEGER NOT NULL DEFAULT 0,\n    started_at INTEGER,\n    online INTEGER,\n    auto_record INTEGER NOT NULL DEFAULT 1,\n    monitor_paused INTEGER NOT NULL DEFAULT 0,\n    record_qn INTEGER NOT NULL DEFAULT 10000,\n    view_qn INTEGER NOT NULL DEFAULT 0,\n    record_line TEXT,\n    last_checked_at INTEGER,\n    created_at INTEGER NOT NULL,\n    updated_at INTEGER NOT NULL,\n    -- 软删除：移除房间只打时间戳，录像历史仍挂在它上面\n    removed_at INTEGER,\n    UNIQUE (platform, room_key)\n);\n\nCREATE TABLE IF NOT EXISTS record_session (\n    id TEXT PRIMARY KEY,\n    room_id TEXT NOT NULL REFERENCES room(id) ON DELETE CASCADE,\n    started_at INTEGER NOT NULL,\n    ended_at INTEGER,\n    status TEXT NOT NULL,\n    reason TEXT,\n    file_path TEXT,\n    file_size INTEGER NOT NULL DEFAULT 0,\n    duration_ms INTEGER NOT NULL DEFAULT 0,\n    quality TEXT,\n    line TEXT,\n    error TEXT,\n    danmaku_path TEXT,\n    danmaku_count INTEGER NOT NULL DEFAULT 0\n);\n\nCREATE INDEX IF NOT EXISTS idx_record_session_room\n    ON record_session (room_id, started_at DESC);\n\nCREATE INDEX IF NOT EXISTS idx_record_session_status\n    ON record_session (status, started_at DESC);\n";

/// 后续里程碑追加的表：与初始 SCHEMA 分开，避免每次都要改那一行超长字符串。
const EXTRA_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS app_setting (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS credential (
    platform TEXT PRIMARY KEY,
    account_id TEXT NOT NULL DEFAULT '',
    cookie TEXT NOT NULL,
    uid TEXT NOT NULL DEFAULT '',
    account_name TEXT NOT NULL DEFAULT '',
    avatar TEXT,
    status TEXT NOT NULL DEFAULT 'ok',
    updated_at INTEGER NOT NULL
);
";

/// 媒体库筛选条件：全部下推到 SQL，前端不再把 200 条拉回来自己过滤。
#[derive(Debug, Clone)]
pub struct MediaFilter {
    pub query: Option<String>,
    pub platform: Option<String>,
    /// 只看某个房间（主播页用它下钻），None 表示全库
    pub room_id: Option<String>,
    /// 只要最近 N 天（None 表示不限）
    pub days: Option<i64>,
    pub only_danmaku: bool,
    /// recent / duration / size，其余值按 recent 处理
    pub sort: String,
    pub limit: usize,
    pub offset: usize,
}

impl Default for MediaFilter {
    fn default() -> Self {
        Self {
            query: None,
            platform: None,
            room_id: None,
            days: None,
            only_danmaku: false,
            sort: "recent".into(),
            limit: 60,
            offset: 0,
        }
    }
}

/// 拼出媒体库的 WHERE 片段。
///
/// include_platform=false 用于统计各平台条数：此时忽略平台筛选本身，
/// 否则选了斗鱼之后其它平台的计数会变成 0，菜单看起来像坏了。
fn media_where(filter: &MediaFilter, include_platform: bool) -> (String, Vec<SqlValue>) {
    let mut where_sql = String::from(" WHERE s.file_path IS NOT NULL AND s.file_path <> ''");
    let mut args: Vec<SqlValue> = Vec::new();
    if let Some(keyword) = filter
        .query
        .as_deref()
        .map(str::trim)
        .filter(|one| !one.is_empty())
    {
        where_sql.push_str(r" AND (r.title LIKE ? ESCAPE '\' OR r.host_name LIKE ? ESCAPE '\')");
        let like = format!(
            "%{}%",
            keyword
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        args.push(SqlValue::Text(like.clone()));
        args.push(SqlValue::Text(like));
    }
    if include_platform {
        if let Some(platform) = filter
            .platform
            .as_deref()
            .filter(|one| !one.is_empty() && *one != "all")
        {
            where_sql.push_str(" AND r.platform = ?");
            args.push(SqlValue::Text(platform.to_string()));
        }
    }
    if let Some(room_id) = filter.room_id.as_deref().filter(|one| !one.is_empty()) {
        where_sql.push_str(" AND s.room_id = ?");
        args.push(SqlValue::Text(room_id.to_string()));
    }
    if let Some(days) = filter.days.filter(|days| *days > 0) {
        where_sql.push_str(" AND s.started_at >= ?");
        args.push(SqlValue::Integer(now_ts() - days * 86_400));
    }
    if filter.only_danmaku {
        where_sql.push_str(" AND s.danmaku_count > 0");
    }
    (where_sql, args)
}

impl MediaFilter {
    /// 只允许白名单里的三种排序，避免把用户输入拼进 SQL。
    fn order_by(&self) -> &'static str {
        match self.sort.as_str() {
            "duration" => "s.duration_ms DESC, s.started_at DESC",
            "size" => "s.file_size DESC, s.started_at DESC",
            _ => "s.started_at DESC",
        }
    }
}

pub struct Db {
    conn: Mutex<Connection>,
    /// 凭据列的加解密器；密钥文件与库文件同目录。
    secret: crate::secret::Secret,
}

/// 主播墙的筛选条件。与 MediaFilter 分开：主播维度的「条数」是录像条数之和，
/// 排序也是按主播聚合后的指标（最新/体积/条数）。
#[derive(Debug, Clone)]
pub struct MediaHostFilter {
    pub query: Option<String>,
    pub platform: Option<String>,
    /// 只要最近 N 天有录制的房间
    pub days: Option<i64>,
    /// name / latest / size / count，其余值按 name 处理
    pub sort: String,
    pub limit: usize,
    pub offset: usize,
}

impl Default for MediaHostFilter {
    fn default() -> Self {
        Self {
            query: None,
            platform: None,
            days: None,
            sort: "name".into(),
            limit: 60,
            offset: 0,
        }
    }
}

impl MediaHostFilter {
    fn order_by(&self) -> &'static str {
        match self.sort.as_str() {
            "latest" => "MAX(s.started_at) DESC",
            "size" => "SUM(s.file_size) DESC, MAX(s.started_at) DESC",
            "count" => "COUNT(s.id) DESC, MAX(s.started_at) DESC",
            // 默认按主播名：同一屏里找固定的人比按时间扫更快。
            _ => "r.host_name COLLATE NOCASE ASC, MAX(s.started_at) DESC",
        }
    }
}

/// 主播墙的 WHERE 片段：只统计有文件、已结束的会话。
fn host_where(filter: &MediaHostFilter, include_platform: bool) -> (String, Vec<SqlValue>) {
    let mut where_sql = String::from(
        " WHERE s.file_path IS NOT NULL AND s.file_path <> '' AND s.status = 'finished'",
    );
    let mut args: Vec<SqlValue> = Vec::new();
    if let Some(keyword) = filter
        .query
        .as_deref()
        .map(str::trim)
        .filter(|one| !one.is_empty())
    {
        where_sql.push_str(r" AND (r.host_name LIKE ? ESCAPE '\' OR r.title LIKE ? ESCAPE '\')");
        let like = format!(
            "%{}%",
            keyword
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        args.push(SqlValue::Text(like.clone()));
        args.push(SqlValue::Text(like));
    }
    if include_platform {
        if let Some(platform) = filter
            .platform
            .as_deref()
            .filter(|one| !one.is_empty() && *one != "all")
        {
            where_sql.push_str(" AND r.platform = ?");
            args.push(SqlValue::Text(platform.to_string()));
        }
    }
    if let Some(days) = filter.days.filter(|days| *days > 0) {
        where_sql.push_str(" AND s.started_at >= ?");
        args.push(SqlValue::Integer(now_ts() - days * 86_400));
    }
    (where_sql, args)
}

impl Db {
    pub fn open(path: &Path) -> Result<Self, AppError> {
        tracing::debug!(path = %path.display(), "打开数据库");
        let conn = Connection::open(path).map_err(db_err)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(db_err)?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(db_err)?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(db_err)?;
        conn.execute_batch(SCHEMA).map_err(db_err)?;
        conn.execute_batch(EXTRA_SCHEMA).map_err(db_err)?;
        migrate(&conn)?;
        // 密钥跟库文件放一起；内存库（单测）落到临时目录，避免在仓库里生成文件。
        let key_path = match path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir.join("secret.bin"),
            _ => std::env::temp_dir().join(format!("hivecast-memory-{}.bin", std::process::id())),
        };
        let secret = crate::secret::Secret::load_or_create(&key_path)?;
        Ok(Self {
            conn: Mutex::new(conn),
            secret,
        })
    }

    /// 只给单测用：跨模块播种数据（retention 等测试需要直接写表）。
    #[cfg(test)]
    pub(crate) fn exec_batch_for_test(&self, sql: &str) -> Result<(), AppError> {
        self.lock().execute_batch(sql).map_err(db_err)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("数据库连接被污染")
    }

    /// 启动时把上次异常退出留下的 recording 记录收尾，避免历史页永远显示「录制中」。
    ///
    /// 结束时间取这批分片最后一次写入的 mtime，而不是「这次启动的时刻」（I1）：
    /// 崩溃后隔一夜再打开应用，32 KB 的残留分片不该显示成录了十个小时。
    /// 连文件都找不到时才退回 now。
    pub fn close_orphan_sessions(&self) -> Result<usize, AppError> {
        let now = now_ts();
        let rows: Vec<(String, Option<String>, i64)> = {
            let conn = self.lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, file_path, started_at FROM record_session
                      WHERE status = 'recording'",
                )
                .map_err(db_err)?;
            let rows = stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .map_err(db_err)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(db_err)?
        };
        let mut changed = 0;
        for (id, file_path, started_at) in rows {
            let last_data = file_path.as_deref().and_then(crate::segment::last_data_at);
            let ended_at = orphan_ended_at(started_at, last_data, now);
            changed += self
                .lock()
                .execute(
                    "UPDATE record_session
                        SET status = 'aborted', reason = 'app_exit',
                            ended_at = ?2, duration_ms = (?2 - started_at) * 1000
                      WHERE id = ?1 AND status = 'recording'",
                    params![id, ended_at],
                )
                .map_err(db_err)?;
        }
        Ok(changed)
    }

    pub fn list_rooms(&self) -> Result<Vec<RoomRecord>, AppError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(&format!(
                "{ROOM_SELECT} WHERE removed_at IS NULL ORDER BY created_at ASC"
            ))
            .map_err(db_err)?;
        let rows = stmt.query_map([], map_room).map_err(db_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_err)
    }

    pub fn get_room(&self, id: &str) -> Result<Option<RoomRecord>, AppError> {
        let conn = self.lock();
        conn.query_row(
            &format!("{ROOM_SELECT} WHERE id = ?1"),
            params![id],
            map_room,
        )
        .optional()
        .map_err(db_err)
    }

    pub fn find_room(
        &self,
        platform: &str,
        room_key: &str,
    ) -> Result<Option<RoomRecord>, AppError> {
        let conn = self.lock();
        conn.query_row(
            &format!("{ROOM_SELECT} WHERE platform = ?1 AND room_key = ?2"),
            params![platform, room_key],
            map_room,
        )
        .optional()
        .map_err(db_err)
    }

    /// 新房间入库；同平台同房间号视为同一条，更新展示信息并返回。
    pub fn insert_room(&self, room: &RoomRecord, info: &RoomInfo) -> Result<RoomRecord, AppError> {
        let now = now_ts();
        {
            let conn = self.lock();
            conn.execute(
                "INSERT INTO room (
                    id, platform, room_key, canonical_url, title, host_name, avatar_url,
                    cover_url, area_name, is_live, started_at, online, auto_record,
                    monitor_paused, record_qn, view_qn, last_checked_at, created_at, updated_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
                ON CONFLICT (platform, room_key) DO UPDATE SET
                    canonical_url = excluded.canonical_url,
                    title = excluded.title,
                    host_name = excluded.host_name,
                    avatar_url = excluded.avatar_url,
                    cover_url = excluded.cover_url,
                    area_name = excluded.area_name,
                    is_live = excluded.is_live,
                    started_at = excluded.started_at,
                    online = excluded.online,
                    -- 重新添加同一个房间时，按这次弹窗里的开关覆盖。
                    auto_record = excluded.auto_record,
                    monitor_paused = excluded.monitor_paused,
                    last_checked_at = excluded.last_checked_at,
                    -- 重新添加同一房间等于恢复：把软删除标记清掉。
                    removed_at = NULL,
                    updated_at = excluded.updated_at",
                params![
                    room.id,
                    room.platform,
                    room.room_key,
                    room.canonical_url,
                    info.room_name,
                    info.host_name,
                    info.avatar_url,
                    info.cover_url,
                    info.area_name,
                    info.is_live as i64,
                    info.started_at,
                    info.online,
                    room.auto_record as i64,
                    room.monitor_paused as i64,
                    room.record_qn,
                    room.view_qn,
                    now,
                    room.created_at,
                    now,
                ],
            )
            .map_err(db_err)?;
        }

        self.find_room(&room.platform, &room.room_key)?
            .ok_or_else(|| AppError::Internal("房间写入后读取失败".into()))
    }

    pub fn update_room_info(&self, id: &str, info: &RoomInfo) -> Result<(), AppError> {
        let now = now_ts();
        self.lock()
            .execute(
                "UPDATE room SET
                    title = ?2, host_name = ?3, avatar_url = ?4, cover_url = ?5,
                    area_name = ?6, is_live = ?7, started_at = ?8, online = ?9,
                    last_checked_at = ?10, updated_at = ?10
                  WHERE id = ?1",
                params![
                    id,
                    info.room_name,
                    info.host_name,
                    info.avatar_url,
                    info.cover_url,
                    info.area_name,
                    info.is_live as i64,
                    info.started_at,
                    info.online,
                    now,
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn touch_room(&self, id: &str) -> Result<(), AppError> {
        let now = now_ts();
        self.lock()
            .execute(
                "UPDATE room SET last_checked_at = ?2, updated_at = ?2 WHERE id = ?1",
                params![id, now],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// 没有结构性变化时的写库：只更新在线人数与检测时间，且最多 min_interval 秒一次（G2）。
    ///
    /// 条件写在 UPDATE 里而不是「先查再写」：监控可能同时在检测同一个房间，
    /// 两次都读到旧时间就会都写一遍。返回是否真的落库。
    pub fn touch_room_throttled(
        &self,
        id: &str,
        online: Option<u64>,
        min_interval: i64,
    ) -> Result<bool, AppError> {
        let now = now_ts();
        let changed = self
            .lock()
            .execute(
                "UPDATE room SET online = ?2, last_checked_at = ?3, updated_at = ?3
                  WHERE id = ?1
                    AND (last_checked_at IS NULL OR last_checked_at <= ?3 - ?4)",
                params![id, online, now, min_interval.max(0)],
            )
            .map_err(db_err)?;
        Ok(changed > 0)
    }

    pub fn set_auto_record(&self, id: &str, enabled: bool) -> Result<(), AppError> {
        self.lock()
            .execute(
                "UPDATE room SET auto_record = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, enabled as i64, now_ts()],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// 暂停/恢复定时监控；暂停只停轮询，不影响已有录制。
    pub fn set_monitor_paused(&self, id: &str, paused: bool) -> Result<(), AppError> {
        self.lock()
            .execute(
                "UPDATE room SET monitor_paused = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, paused as i64, now_ts()],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn set_quality(&self, id: &str, record_qn: u32, view_qn: u32) -> Result<(), AppError> {
        self.lock()
            .execute(
                "UPDATE room SET record_qn = ?2, view_qn = ?3, updated_at = ?4 WHERE id = ?1",
                params![id, record_qn, view_qn, now_ts()],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// 录制线路偏好；None 表示自动选最优。
    pub fn set_record_line(&self, id: &str, line: Option<&str>) -> Result<(), AppError> {
        let now = now_ts();
        self.lock()
            .execute(
                "UPDATE room SET record_line = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, line, now],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// 移除房间只做软删除。
    ///
    /// record_session.room_id 是 ON DELETE CASCADE：真删会把该房间的全部录像历史
    /// 一起带走（文件还在磁盘上，媒体库却再也看不到）。这里只打 removed_at，
    /// 监控列表把它过滤掉，媒体库仍按房间聚合历史；重新添加同一房间会恢复它。
    pub fn delete_room(&self, id: &str) -> Result<(), AppError> {
        let now = now_ts();
        self.lock()
            .execute(
                "UPDATE room SET removed_at = ?2, monitor_paused = 1, updated_at = ?2
                  WHERE id = ?1",
                params![id, now],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// 这条房间是否已被移除（软删除）。
    pub fn room_removed(&self, id: &str) -> Result<bool, AppError> {
        self.lock()
            .query_row(
                "SELECT removed_at IS NOT NULL FROM room WHERE id = ?1",
                params![id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(db_err)
            .map(|found| found.unwrap_or(0) != 0)
    }

    pub fn create_session(&self, session: &RecordSession) -> Result<(), AppError> {
        self.lock()
            .execute(
                "INSERT INTO record_session (
                    id, room_id, started_at, ended_at, status, reason, file_path,
                    file_size, duration_ms, quality, line, error,
                    danmaku_path, danmaku_count
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    session.id,
                    session.room_id,
                    session.started_at,
                    session.ended_at,
                    session.status,
                    session.reason,
                    session.file_path,
                    session.file_size,
                    session.duration_ms,
                    session.quality,
                    session.line,
                    session.error,
                    session.danmaku_path,
                    session.danmaku_count,
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn finish_session(
        &self,
        id: &str,
        status: &str,
        reason: &str,
        file_size: i64,
        danmaku_path: Option<&str>,
        danmaku_count: i64,
        error: Option<&str>,
    ) -> Result<(), AppError> {
        let now = now_ts();
        self.lock()
            .execute(
                "UPDATE record_session SET
                    status = ?2, reason = ?3, ended_at = ?4, file_size = ?5,
                    duration_ms = MAX(0, (?4 - started_at) * 1000), error = ?6,
                    danmaku_path = ?7, danmaku_count = ?8
                  WHERE id = ?1",
                params![
                    id,
                    status,
                    reason,
                    now,
                    file_size,
                    error,
                    danmaku_path,
                    danmaku_count
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn get_session(&self, id: &str) -> Result<Option<RecordSession>, AppError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(&format!("{SESSION_SELECT} WHERE id = ?1"))
            .map_err(db_err)?;
        let mut rows = stmt.query_map(params![id], map_session).map_err(db_err)?;
        match rows.next() {
            Some(row) => Ok(Some(row.map_err(db_err)?)),
            None => Ok(None),
        }
    }

    pub fn list_sessions(
        &self,
        room_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<RecordSession>, AppError> {
        let conn = self.lock();
        let limit = limit.clamp(1, 500) as i64;
        let mut sessions = Vec::new();
        match room_id {
            Some(id) => {
                let mut stmt = conn
                    .prepare(&format!(
                        "{SESSION_SELECT} WHERE room_id = ?1 ORDER BY started_at DESC LIMIT ?2"
                    ))
                    .map_err(db_err)?;
                let rows = stmt
                    .query_map(params![id, limit], map_session)
                    .map_err(db_err)?;
                for row in rows {
                    sessions.push(row.map_err(db_err)?);
                }
            }
            None => {
                let mut stmt = conn
                    .prepare(&format!(
                        "{SESSION_SELECT} ORDER BY started_at DESC LIMIT ?1"
                    ))
                    .map_err(db_err)?;
                let rows = stmt
                    .query_map(params![limit], map_session)
                    .map_err(db_err)?;
                for row in rows {
                    sessions.push(row.map_err(db_err)?);
                }
            }
        }
        Ok(sessions)
    }

    // ---- 设置（v1 单一作用域，key 用点分命名，例如 record.default_qn） ----

    pub fn get_setting(&self, key: &str) -> Result<Option<String>, AppError> {
        let conn = self.lock();
        conn.query_row(
            "SELECT value FROM app_setting WHERE key = ?1",
            params![key],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(db_err)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), AppError> {
        self.lock()
            .execute(
                "INSERT INTO app_setting (key, value, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                params![key, value, now_ts()],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// 媒体库列表：会话左连房间，筛选/排序/分页全部下推到 SQL。
    ///
    /// has_file 在这里就判断好：前端逐个文件调元数据接口太慢，
    /// 每次进媒体库查一次磁盘，量级是几百条，可以接受。
    pub fn media_list(&self, filter: &MediaFilter) -> Result<(Vec<MediaItem>, i64), AppError> {
        let conn = self.lock();
        let (where_sql, args) = media_where(filter, true);

        let total: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM record_session s JOIN room r ON r.id = s.room_id{where_sql}"
                ),
                rusqlite::params_from_iter(args.iter()),
                |row| row.get(0),
            )
            .map_err(db_err)?;

        let mut page_args = args;
        page_args.push(SqlValue::Integer(filter.limit.clamp(1, 500) as i64));
        page_args.push(SqlValue::Integer(filter.offset.max(0) as i64));
        let sql = format!(
            "SELECT s.id, s.room_id, r.platform, r.title, r.host_name, s.file_path,
                    s.file_size, s.duration_ms, s.started_at, s.status, s.danmaku_count
               FROM record_session s
               JOIN room r ON r.id = s.room_id{where_sql}
              ORDER BY {}
              LIMIT ? OFFSET ?",
            filter.order_by()
        );
        let mut stmt = conn.prepare(&sql).map_err(db_err)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(page_args.iter()), |row| {
                let path: String = row.get(5)?;
                Ok(MediaItem {
                    id: row.get(0)?,
                    room_id: row.get(1)?,
                    platform: row.get(2)?,
                    title: row.get(3)?,
                    host_name: row.get(4)?,
                    path: path.clone(),
                    size: row.get(6)?,
                    duration_ms: row.get(7)?,
                    started_at: row.get(8)?,
                    status: row.get(9)?,
                    danmaku_count: row.get(10)?,
                    has_file: std::path::Path::new(&path).is_file(),
                    thumbnail_url: None,
                })
            })
            .map_err(db_err)?;
        let items = rows.collect::<Result<Vec<_>, _>>().map_err(db_err)?;
        Ok((items, total))
    }

    /// 各平台在当前筛选条件下的条数（忽略平台筛选本身）。
    pub fn media_platform_counts(
        &self,
        filter: &MediaFilter,
    ) -> Result<Vec<PlatformCount>, AppError> {
        let conn = self.lock();
        let (where_sql, args) = media_where(filter, false);
        let sql = format!(
            "SELECT r.platform, COUNT(*)
               FROM record_session s
               JOIN room r ON r.id = s.room_id{where_sql}
              GROUP BY r.platform
              ORDER BY COUNT(*) DESC, r.platform ASC"
        );
        let mut stmt = conn.prepare(&sql).map_err(db_err)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(args.iter()), |row| {
                Ok(PlatformCount {
                    platform: row.get(0)?,
                    count: row.get(1)?,
                })
            })
            .map_err(db_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_err)
    }

    /// 主播墙：按 room.id 聚合出主播卡（v1 的主播身份就是房间）。
    pub fn media_hosts(&self, filter: &MediaHostFilter) -> Result<MediaHostPage, AppError> {
        let conn = self.lock();
        let (where_sql, args) = host_where(filter, true);

        let total: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM (
                        SELECT r.id FROM record_session s JOIN room r ON r.id = s.room_id{where_sql}
                        GROUP BY r.id
                     )"
                ),
                rusqlite::params_from_iter(args.iter()),
                |row| row.get(0),
            )
            .map_err(db_err)?;

        let mut page_args = args;
        page_args.push(SqlValue::Integer(filter.limit.clamp(1, 500) as i64));
        page_args.push(SqlValue::Integer(filter.offset.max(0) as i64));
        let sql = format!(
            "SELECT r.id, r.platform, r.host_name, r.title, r.avatar_url, r.cover_url,
                    COUNT(s.id), COALESCE(SUM(s.file_size), 0), MAX(s.started_at)
               FROM record_session s
               JOIN room r ON r.id = s.room_id{where_sql}
              GROUP BY r.id
              ORDER BY {}
              LIMIT ? OFFSET ?",
            filter.order_by()
        );
        let mut stmt = conn.prepare(&sql).map_err(db_err)?;
        let items = stmt
            .query_map(rusqlite::params_from_iter(page_args.iter()), |row| {
                Ok(MediaHost {
                    room_id: row.get(0)?,
                    platform: row.get(1)?,
                    host_name: row.get(2)?,
                    room_title: row.get(3)?,
                    avatar_url: row.get(4)?,
                    cover_url: row.get(5)?,
                    count: row.get(6)?,
                    bytes: row.get(7)?,
                    latest_at: row.get(8)?,
                })
            })
            .map_err(db_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_err)?;

        let platform_counts = Self::host_counts_locked(&conn, filter)?;
        Ok(MediaHostPage {
            items,
            total,
            platform_counts,
        })
    }

    /// 各平台的主播数（忽略平台筛选本身），给平台下拉显示计数。
    fn host_counts_locked(
        conn: &Connection,
        filter: &MediaHostFilter,
    ) -> Result<Vec<PlatformCount>, AppError> {
        let (where_sql, args) = host_where(filter, false);
        let sql = format!(
            "SELECT r.platform, COUNT(DISTINCT r.id)
               FROM record_session s
               JOIN room r ON r.id = s.room_id{where_sql}
              GROUP BY r.platform
              ORDER BY COUNT(DISTINCT r.id) DESC, r.platform ASC"
        );
        let mut stmt = conn.prepare(&sql).map_err(db_err)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(args.iter()), |row| {
                Ok(PlatformCount {
                    platform: row.get(0)?,
                    count: row.get(1)?,
                })
            })
            .map_err(db_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_err)
    }

    /// 删除一条会话记录（只删数据库行，不动文件）。
    pub fn delete_session(&self, id: &str) -> Result<(), AppError> {
        self.lock()
            .execute("DELETE FROM record_session WHERE id = ?1", params![id])
            .map_err(db_err)?;
        Ok(())
    }

    // ---- 崩溃恢复与保留策略（M7.1） ----

    /// 上次没收尾的会话：启动时据此把遗留分片合成 mp4。
    ///
    /// 必须在 close_orphan_sessions 之前调用，否则状态已被改成 aborted，就找不到了。
    pub fn recording_sessions(&self) -> Result<Vec<(String, String)>, AppError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, file_path FROM record_session
                  WHERE status = 'recording' AND file_path IS NOT NULL AND file_path <> ''",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(db_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_err)
    }

    /// 已收尾但可能留下分片的会话：失败/中止的收尾可能没来得及合并（G4）。
    ///
    /// 只取最近 200 条：再老的残片早被保留策略清掉，全量扫库没有意义。
    pub fn unfinished_sessions(&self) -> Result<Vec<(String, String)>, AppError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, file_path FROM record_session
                  WHERE status IN ('failed', 'aborted')
                    AND file_path IS NOT NULL AND file_path <> ''
                  ORDER BY started_at DESC LIMIT 200",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(db_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_err)
    }

    /// 修正会话的产物路径与大小：合并失败降级成 .ts、或崩溃恢复后路径变化时用。
    pub fn update_session_file(
        &self,
        id: &str,
        path: &str,
        file_size: i64,
    ) -> Result<(), AppError> {
        self.lock()
            .execute(
                "UPDATE record_session SET file_path = ?2, file_size = ?3 WHERE id = ?1",
                params![id, path, file_size],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// 保留策略用：已收尾且有文件的会话，按开始时间倒序。
    pub fn retention_rows(&self) -> Result<Vec<(String, String, i64, i64)>, AppError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, file_path, file_size, started_at FROM record_session
                  WHERE file_path IS NOT NULL AND file_path <> '' AND status <> 'recording'
                  ORDER BY started_at DESC",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .map_err(db_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_err)
    }
}

/// 平台账号凭据（v1 单账号，account_id 预留给多账号）。
///
/// cookie 是明文：桌面端单用户，加密方案（Keychain / DPAPI）见 credential.rs 顶部说明。
#[derive(Debug, Clone)]
pub struct CredentialRow {
    pub platform: String,
    pub account_id: String,
    pub cookie: String,
    pub uid: String,
    pub account_name: String,
    pub avatar: Option<String>,
    pub status: String,
    pub updated_at: i64,
}

impl Db {
    pub fn credentials(&self) -> Result<Vec<CredentialRow>, AppError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT platform, account_id, cookie, uid, account_name, avatar, status, updated_at
                   FROM credential ORDER BY platform ASC",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(CredentialRow {
                    platform: row.get(0)?,
                    account_id: row.get(1)?,
                    cookie: row.get(2)?,
                    uid: row.get(3)?,
                    account_name: row.get(4)?,
                    avatar: row.get(5)?,
                    status: row.get(6)?,
                    updated_at: row.get(7)?,
                })
            })
            .map_err(db_err)?;
        let rows: Vec<CredentialRow> = rows.collect::<Result<Vec<_>, _>>().map_err(db_err)?;
        drop(stmt);
        let mut out = Vec::with_capacity(rows.len());
        for mut row in rows {
            match self.secret.open(&row.cookie) {
                Some(plain) => row.cookie = plain,
                None => {
                    tracing::warn!(platform = %row.platform, "凭据解密失败，已跳过（需要重新登录）");
                    continue;
                }
            }
            out.push(row);
        }
        Ok(out)
    }

    /// 把历史明文 Cookie 就地改成密文；返回改动了多少行。
    ///
    /// 只在启动时跑一次，幂等：已经带 enc:v1: 前缀的行会被跳过。
    pub fn reseal_credentials(&self) -> Result<usize, AppError> {
        let raw: Vec<(String, String)> = {
            let conn = self.lock();
            let mut stmt = conn
                .prepare("SELECT platform, cookie FROM credential")
                .map_err(db_err)?;
            let rows = stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(db_err)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(db_err)?
        };
        let mut changed = 0;
        for (platform, cookie) in raw {
            if crate::secret::Secret::is_sealed(&cookie) {
                continue;
            }
            let sealed = self.secret.seal(&cookie)?;
            self.lock()
                .execute(
                    "UPDATE credential SET cookie = ?1 WHERE platform = ?2",
                    params![sealed, platform],
                )
                .map_err(db_err)?;
            changed += 1;
        }
        if changed > 0 {
            tracing::info!(count = changed, "已把历史明文 Cookie 加密");
        }
        Ok(changed)
    }

    pub fn save_credential(&self, row: &CredentialRow) -> Result<(), AppError> {
        let sealed = self.secret.seal(&row.cookie)?;
        self.lock()
            .execute(
                "INSERT INTO credential (platform, account_id, cookie, uid, account_name, avatar, status, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT (platform) DO UPDATE SET
                    account_id = excluded.account_id,
                    cookie = excluded.cookie,
                    uid = excluded.uid,
                    account_name = excluded.account_name,
                    avatar = excluded.avatar,
                    status = excluded.status,
                    updated_at = excluded.updated_at",
                params![
                    row.platform,
                    row.account_id,
                    sealed,
                    row.uid,
                    row.account_name,
                    row.avatar,
                    row.status,
                    row.updated_at,
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn delete_credential(&self, platform: &str) -> Result<(), AppError> {
        self.lock()
            .execute(
                "DELETE FROM credential WHERE platform = ?1",
                params![platform],
            )
            .map_err(db_err)?;
        Ok(())
    }
}

/// 轻量迁移：老库缺弹幕列时补上。
///
/// 不用 migration 框架——目前只有两张表，直接按 PRAGMA 检查足够，
/// 失败时保留旧数据比重建表更安全。
fn migrate(conn: &Connection) -> Result<(), AppError> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(record_session)")
        .map_err(db_err)?;
    let columns: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(db_err)?
        .filter_map(Result::ok)
        .collect();
    if !columns.iter().any(|name| name == "danmaku_path") {
        conn.execute(
            "ALTER TABLE record_session ADD COLUMN danmaku_path TEXT",
            [],
        )
        .map_err(db_err)?;
        tracing::info!(column = "record_session.danmaku_path", "数据库迁移：新增列");
    }
    if !columns.iter().any(|name| name == "danmaku_count") {
        conn.execute(
            "ALTER TABLE record_session ADD COLUMN danmaku_count INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .map_err(db_err)?;
        tracing::info!(
            column = "record_session.danmaku_count",
            "数据库迁移：新增列"
        );
    }

    let mut stmt = conn.prepare("PRAGMA table_info(room)").map_err(db_err)?;
    let room_columns: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(db_err)?
        .filter_map(Result::ok)
        .collect();
    if !room_columns.iter().any(|name| name == "record_line") {
        conn.execute("ALTER TABLE room ADD COLUMN record_line TEXT", [])
            .map_err(db_err)?;
        tracing::info!(column = "room.record_line", "数据库迁移：新增列");
    }
    if !room_columns.iter().any(|name| name == "monitor_paused") {
        conn.execute(
            "ALTER TABLE room ADD COLUMN monitor_paused INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .map_err(db_err)?;
        tracing::info!(column = "room.monitor_paused", "数据库迁移：新增列");
    }
    if !room_columns.iter().any(|name| name == "removed_at") {
        conn.execute("ALTER TABLE room ADD COLUMN removed_at INTEGER", [])
            .map_err(db_err)?;
        tracing::info!(column = "room.removed_at", "数据库迁移：新增列");
    }
    Ok(())
}

fn db_err(err: rusqlite::Error) -> AppError {
    AppError::Db(err.to_string())
}

pub fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

const ROOM_SELECT: &str = "SELECT
    id, platform, room_key, canonical_url, title, host_name, avatar_url, cover_url,
    area_name, is_live, started_at, online, auto_record, monitor_paused,
    record_qn, view_qn, record_line, last_checked_at, created_at, updated_at
  FROM room";

const SESSION_SELECT: &str = "SELECT
    id, room_id, started_at, ended_at, status, reason, file_path, file_size,
    duration_ms, quality, line, error, danmaku_path, danmaku_count
  FROM record_session";

fn map_room(row: &Row<'_>) -> rusqlite::Result<RoomRecord> {
    Ok(RoomRecord {
        id: row.get(0)?,
        platform: row.get(1)?,
        room_key: row.get(2)?,
        canonical_url: row.get(3)?,
        title: row.get(4)?,
        host_name: row.get(5)?,
        avatar_url: row.get(6)?,
        cover_url: row.get(7)?,
        area_name: row.get(8)?,
        is_live: row.get::<_, i64>(9)? != 0,
        started_at: row.get(10)?,
        online: row
            .get::<_, Option<i64>>(11)?
            .map(|value| value.max(0) as u64),
        auto_record: row.get::<_, i64>(12)? != 0,
        monitor_paused: row.get::<_, i64>(13)? != 0,
        record_qn: row.get(14)?,
        view_qn: row.get(15)?,
        record_line: row.get(16)?,
        last_checked_at: row.get(17)?,
        created_at: row.get(18)?,
        updated_at: row.get(19)?,
    })
}

/// 崩溃会话的结束时刻（I1）：优先用磁盘上最后一次写入的时间，没有文件才用 now。
pub(crate) fn orphan_ended_at(started_at: i64, last_data: Option<i64>, now: i64) -> i64 {
    last_data.unwrap_or(now).clamp(started_at, now)
}

fn map_session(row: &Row<'_>) -> rusqlite::Result<RecordSession> {
    Ok(RecordSession {
        id: row.get(0)?,
        room_id: row.get(1)?,
        started_at: row.get(2)?,
        ended_at: row.get(3)?,
        status: row.get(4)?,
        reason: row.get(5)?,
        file_path: row.get(6)?,
        file_size: row.get(7)?,
        duration_ms: row.get(8)?,
        quality: row.get(9)?,
        line: row.get(10)?,
        error: row.get(11)?,
        danmaku_path: row.get(12)?,
        danmaku_count: row.get(13)?,
    })
}

/// 供未来扩展读取平台原始字段（当前未被调用，保留反序列化入口）。
#[allow(dead_code)]
pub fn extra_value(raw: Option<&str>) -> Value {
    raw.and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_db(tag: &str) -> (Db, PathBuf) {
        let dir = std::env::temp_dir().join(format!("hivecast-db-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录失败");
        let path = dir.join("t.db");
        let db = Db::open(&path).expect("开库失败");
        (db, path)
    }

    fn seed_media(db: &Db) {
        db.lock()
            .execute_batch(
                "INSERT INTO room (id, platform, room_key, canonical_url, host_name, title, created_at, updated_at)
                 VALUES ('rm-a', 'bilibili', '1', 'https://live.bilibili.com/1', '主播甲', '房间甲', 0, 0),
                        ('rm-b', 'douyu', '2', 'https://www.douyu.com/2', '主播乙', '房间乙', 0, 0);
                 INSERT INTO record_session (id, room_id, started_at, status, file_path, file_size, duration_ms)
                 VALUES ('s-a1', 'rm-a', 1000, 'finished', '/tmp/a1.flv', 100, 1000),
                        ('s-a2', 'rm-a', 3000, 'finished', '/tmp/a2.flv', 300, 2000),
                        ('s-b1', 'rm-b', 2000, 'finished', '/tmp/b1.flv', 50, 500),
                        ('s-a3', 'rm-a', 4000, 'recording', '/tmp/a3.flv', 999, 0);",
            )
            .expect("播种媒体库失败");
    }

    #[test]
    fn room_touch_is_throttled_but_still_records_online() {
        let (db, _path) = temp_db("touch");
        db.lock()
            .execute(
                "INSERT INTO room (id, platform, room_key, canonical_url, host_name, title, created_at, updated_at)
                 VALUES ('rm-t', 'test', 't1', 'https://example.test/t1', '主播', '房间', 0, 0)",
                [],
            )
            .expect("插入房间失败");

        // 第一次没有 last_checked_at，条件成立，必须写进去。
        assert!(db
            .touch_room_throttled("rm-t", Some(7), 10)
            .expect("首次节流写入失败"));
        let first = db
            .find_room("test", "t1")
            .expect("读房间失败")
            .expect("房间存在");
        assert_eq!(first.online, Some(7));
        let checked = first.last_checked_at.expect("首次写入要有检测时间");

        // 同一秒内第二次：节流条件不成立，返回 false 且时间不变。
        assert!(!db
            .touch_room_throttled("rm-t", Some(8), 10)
            .expect("第二次节流写入失败"));
        let second = db
            .find_room("test", "t1")
            .expect("读房间失败")
            .expect("房间存在");
        assert_eq!(
            second.last_checked_at,
            Some(checked),
            "节流时不该更新检测时间"
        );
        assert_eq!(second.online, Some(7), "节流时在线人数也不该变");

        // min_interval=0 表示不节流：立刻写。
        assert!(db
            .touch_room_throttled("rm-t", Some(9), 0)
            .expect("不节流写入失败"));
        let third = db
            .find_room("test", "t1")
            .expect("读房间失败")
            .expect("房间存在");
        assert_eq!(third.online, Some(9));
    }

    #[test]
    fn orphan_ended_at_prefers_the_last_write() {
        // 重启时刻是 10 小时后，但最后一片数据是 60 秒时写的：只该记 60 秒。
        assert_eq!(orphan_ended_at(1_000, Some(1_060), 37_000), 1_060);
        // 没有文件：只能按 now 收尾。
        assert_eq!(orphan_ended_at(1_000, None, 37_000), 37_000);
        // mtime 比开始还早（时钟回拨 / 复制过文件）：至少是 0 秒。
        assert_eq!(orphan_ended_at(1_000, Some(10), 37_000), 1_000);
        // mtime 在未来：不能超过 now。
        assert_eq!(orphan_ended_at(1_000, Some(99_999), 37_000), 37_000);
    }

    #[test]
    fn orphan_close_uses_part_mtime_for_duration() {
        use std::time::{Duration, UNIX_EPOCH};
        let (db, _path) = temp_db("orphan");
        let dir = std::env::temp_dir().join(format!("hivecast-orphan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建目录失败");
        let final_path = dir.join("崩溃场.mp4");
        let part = crate::segment::part_name(&final_path, 0);
        std::fs::write(&part, vec![0u8; 32_768]).expect("写分片失败");
        let file = std::fs::File::options()
            .write(true)
            .open(&part)
            .expect("开分片");
        file.set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_060))
            .expect("改 mtime");

        db.lock()
            .execute_batch(&format!(
                "INSERT INTO room (id, platform, room_key, canonical_url, host_name, title, created_at, updated_at)
                 VALUES ('rm-o', 'bilibili', '9', 'https://live.bilibili.com/9', '主播', '房间', 0, 0);
                 INSERT INTO record_session (id, room_id, started_at, status, file_path, file_size, duration_ms)
                 VALUES ('s-o', 'rm-o', 1700000000, 'recording', '{}', 0, 0);",
                final_path.display()
            ))
            .expect("播种失败");

        assert_eq!(db.close_orphan_sessions().expect("收尾失败"), 1);
        let (status, reason, ended_at, duration_ms): (String, Option<String>, i64, i64) = db
            .lock()
            .query_row(
                "SELECT status, reason, ended_at, duration_ms FROM record_session WHERE id = 's-o'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("读会话失败");
        assert_eq!(status, "aborted");
        assert_eq!(reason.as_deref(), Some("app_exit"));
        assert_eq!(ended_at, 1_700_000_060);
        assert_eq!(duration_ms, 60_000, "只该按最后一片的写入时间算");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unfinished_sessions_only_covers_failed_and_aborted() {
        let (db, _path) = temp_db("unfinished");
        seed_media(&db);
        db.lock()
            .execute(
                "UPDATE record_session SET status = 'failed' WHERE id = 's-a1'",
                [],
            )
            .expect("改成失败态失败");

        let rows = db.unfinished_sessions().expect("查询遗留分片失败");
        assert_eq!(rows.len(), 1, "只有 failed/aborted 算遗留：{rows:?}");
        assert_eq!(rows[0].0, "s-a1");
    }

    #[test]
    fn media_hosts_aggregates_finished_sessions_per_room() {
        let (db, _path) = temp_db("hosts");
        seed_media(&db);

        let page = db
            .media_hosts(&MediaHostFilter::default())
            .expect("主播墙查询失败");
        assert_eq!(page.total, 2);
        // 默认按主播名升序（码点序）：乙 在 甲 前面。
        assert_eq!(page.items[0].room_id, "rm-b");
        let latest = db
            .media_hosts(&MediaHostFilter {
                sort: "latest".into(),
                ..MediaHostFilter::default()
            })
            .expect("按时间排序失败");
        // 最新录制优先：rm-a 的 3000 比 rm-b 的 2000 新。
        assert_eq!(latest.items[0].room_id, "rm-a");
        assert_eq!(latest.items[0].host_name, "主播甲");
        assert_eq!(latest.items[0].count, 2);
        assert_eq!(latest.items[0].bytes, 400);
        assert_eq!(latest.items[0].latest_at, 3000);
        // 还在录的会话不进主播墙。
        assert_eq!(page.platform_counts.len(), 2);

        let by_size = db
            .media_hosts(&MediaHostFilter {
                sort: "size".into(),
                ..MediaHostFilter::default()
            })
            .expect("按体积排序失败");
        assert_eq!(by_size.items[0].room_id, "rm-a");

        let only_douyu = db
            .media_hosts(&MediaHostFilter {
                platform: Some("douyu".into()),
                ..MediaHostFilter::default()
            })
            .expect("平台筛选失败");
        assert_eq!(only_douyu.items.len(), 1);
        // 平台筛选不影响计数：两个平台都要出现。
        assert_eq!(only_douyu.platform_counts.len(), 2);
    }

    #[test]
    fn media_list_can_be_scoped_to_one_room() {
        let (db, _path) = temp_db("roomfilter");
        seed_media(&db);
        let (items, total) = db
            .media_list(&MediaFilter {
                room_id: Some("rm-a".into()),
                ..MediaFilter::default()
            })
            .expect("按房间筛选失败");
        assert_eq!(total, 3);
        assert!(items.iter().all(|one| one.room_id == "rm-a"));
    }

    #[test]
    fn removing_a_room_keeps_its_history_and_readding_revives_it() {
        let (db, _path) = temp_db("softdelete");
        seed_media(&db);

        db.delete_room("rm-a").expect("移除房间失败");
        // 软删除：监控列表看不到，但房间行、录像历史都还在。
        assert!(db.room_removed("rm-a").expect("读标记失败"));
        assert!(db.get_room("rm-a").expect("读房间失败").is_some());
        assert_eq!(db.list_rooms().expect("列表失败").len(), 1);
        let (items, total) = db.media_list(&MediaFilter::default()).expect("媒体库失败");
        assert_eq!(total, 4);
        assert!(items.iter().any(|one| one.room_id == "rm-a"));

        // 重新添加同一个房间：insert_room 的 ON CONFLICT 分支要清掉软删除标记。
        let existing = db.get_room("rm-a").expect("读失败").expect("房间还在");
        let info = crate::platform::RoomInfo {
            host_name: "主播甲".into(),
            room_name: "房间甲".into(),
            avatar_url: None,
            cover_url: None,
            area_name: None,
            is_live: true,
            started_at: Some(5000),
            online: Some(1),
            extra: serde_json::Value::Null,
        };
        db.insert_room(&existing, &info).expect("恢复房间失败");
        assert!(!db.room_removed("rm-a").expect("读标记失败"));
        assert_eq!(db.list_rooms().expect("列表失败").len(), 2);
    }

    fn row(platform: &str, cookie: &str) -> CredentialRow {
        CredentialRow {
            platform: platform.into(),
            account_id: String::new(),
            cookie: cookie.into(),
            uid: "1".into(),
            account_name: "测试".into(),
            avatar: None,
            status: "ok".into(),
            updated_at: 0,
        }
    }

    fn raw_cookie(db: &Db, platform: &str) -> String {
        db.lock()
            .query_row(
                "SELECT cookie FROM credential WHERE platform = ?1",
                params![platform],
                |r| r.get(0),
            )
            .expect("读不到 cookie")
    }

    #[test]
    fn credential_is_sealed_at_rest_and_decrypted_on_read() {
        let (db, _path) = temp_db("seal");
        db.save_credential(&row("bilibili", "SESSDATA=secret; bili_jct=abc"))
            .expect("写凭据失败");
        let stored = raw_cookie(&db, "bilibili");
        assert!(crate::secret::Secret::is_sealed(&stored));
        assert!(!stored.contains("SESSDATA"));
        let rows = db.credentials().expect("读凭据失败");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cookie, "SESSDATA=secret; bili_jct=abc");
    }

    #[test]
    fn reseals_legacy_plaintext_once() {
        let (db, _path) = temp_db("legacy");
        db.lock()
            .execute(
                "INSERT INTO credential (platform, cookie, updated_at) VALUES ('douyu', 'dy_did=1', 0)",
                [],
            )
            .expect("插明文失败");
        assert_eq!(raw_cookie(&db, "douyu"), "dy_did=1");
        assert_eq!(db.reseal_credentials().expect("迁移失败"), 1);
        let stored = raw_cookie(&db, "douyu");
        assert!(crate::secret::Secret::is_sealed(&stored));
        // 幂等：第二次跑没有可迁移的行。
        assert_eq!(db.reseal_credentials().expect("二次迁移失败"), 0);
        assert_eq!(db.credentials().expect("读失败")[0].cookie, "dy_did=1");
    }

    #[test]
    fn undecryptable_row_is_skipped_instead_of_failing() {
        let (db, _path) = temp_db("broken");
        db.lock()
            .execute(
                "INSERT INTO credential (platform, cookie, updated_at) VALUES ('huya', 'enc:v1:AAAA', 0)",
                [],
            )
            .expect("插坏密文失败");
        assert!(db.credentials().expect("读失败").is_empty());
    }
}
