//! SQLite 持久层：房间列表 + 录制历史。
//!
//! 为什么用 rusqlite 而不是 async 版：本地单文件、查询都是毫秒级，
//! 加一层 spawn_blocking 或 async 包装只会让代码更难读；这里用 Mutex 串行化，
//! 并通过 WAL 让读不阻塞写。真正的长任务（ffmpeg）不在锁内。

use std::path::Path;
use std::sync::Mutex;

use hivecast_ipc::{RecordSession, RoomRecord};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::Value;

use crate::error::AppError;
use crate::platform::RoomInfo;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS room (\n    id TEXT PRIMARY KEY,\n    platform TEXT NOT NULL,\n    room_key TEXT NOT NULL,\n    canonical_url TEXT NOT NULL,\n    title TEXT NOT NULL DEFAULT '',\n    host_name TEXT NOT NULL DEFAULT '',\n    avatar_url TEXT,\n    cover_url TEXT,\n    area_name TEXT,\n    is_live INTEGER NOT NULL DEFAULT 0,\n    started_at INTEGER,\n    online INTEGER,\n    auto_record INTEGER NOT NULL DEFAULT 1,\n    record_qn INTEGER NOT NULL DEFAULT 10000,\n    view_qn INTEGER NOT NULL DEFAULT 0,\n    last_checked_at INTEGER,\n    created_at INTEGER NOT NULL,\n    updated_at INTEGER NOT NULL,\n    UNIQUE (platform, room_key)\n);\n\nCREATE TABLE IF NOT EXISTS record_session (\n    id TEXT PRIMARY KEY,\n    room_id TEXT NOT NULL REFERENCES room(id) ON DELETE CASCADE,\n    started_at INTEGER NOT NULL,\n    ended_at INTEGER,\n    status TEXT NOT NULL,\n    reason TEXT,\n    file_path TEXT,\n    file_size INTEGER NOT NULL DEFAULT 0,\n    duration_ms INTEGER NOT NULL DEFAULT 0,\n    quality TEXT,\n    line TEXT,\n    error TEXT,\n    danmaku_path TEXT,\n    danmaku_count INTEGER NOT NULL DEFAULT 0\n);\n\nCREATE INDEX IF NOT EXISTS idx_record_session_room\n    ON record_session (room_id, started_at DESC);\n\nCREATE INDEX IF NOT EXISTS idx_record_session_status\n    ON record_session (status, started_at DESC);\n";

pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self, AppError> {
        let conn = Connection::open(path).map_err(db_err)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(db_err)?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(db_err)?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(db_err)?;
        conn.execute_batch(SCHEMA).map_err(db_err)?;
        migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("数据库连接被污染")
    }

    /// 启动时把上次异常退出留下的 recording 记录收尾，避免历史页永远显示「录制中」。
    pub fn close_orphan_sessions(&self) -> Result<usize, AppError> {
        let now = now_ts();
        let changed = self
            .lock()
            .execute(
                "UPDATE record_session
                    SET status = 'aborted', reason = 'app_exit',
                        ended_at = ?, duration_ms = (? - started_at) * 1000
                  WHERE status = 'recording'",
                params![now, now],
            )
            .map_err(db_err)?;
        Ok(changed)
    }

    pub fn list_rooms(&self) -> Result<Vec<RoomRecord>, AppError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(&format!("{ROOM_SELECT} ORDER BY created_at ASC"))
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

    pub fn find_room(&self, platform: &str, room_key: &str) -> Result<Option<RoomRecord>, AppError> {
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
                    record_qn, view_qn, last_checked_at, created_at, updated_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
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
                    last_checked_at = excluded.last_checked_at,
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

    pub fn set_auto_record(&self, id: &str, enabled: bool) -> Result<(), AppError> {
        self.lock()
            .execute(
                "UPDATE room SET auto_record = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, enabled as i64, now_ts()],
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

    pub fn delete_room(&self, id: &str) -> Result<(), AppError> {
        self.lock()
            .execute("DELETE FROM room WHERE id = ?1", params![id])
            .map_err(db_err)?;
        Ok(())
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
                params![id, status, reason, now, file_size, error, danmaku_path, danmaku_count],
            )
            .map_err(db_err)?;
        Ok(())
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
                    .prepare(&format!("{SESSION_SELECT} WHERE room_id = ?1 ORDER BY started_at DESC LIMIT ?2"))
                    .map_err(db_err)?;
                let rows = stmt.query_map(params![id, limit], map_session).map_err(db_err)?;
                for row in rows {
                    sessions.push(row.map_err(db_err)?);
                }
            }
            None => {
                let mut stmt = conn
                    .prepare(&format!("{SESSION_SELECT} ORDER BY started_at DESC LIMIT ?1"))
                    .map_err(db_err)?;
                let rows = stmt.query_map(params![limit], map_session).map_err(db_err)?;
                for row in rows {
                    sessions.push(row.map_err(db_err)?);
                }
            }
        }
        Ok(sessions)
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
        conn.execute("ALTER TABLE record_session ADD COLUMN danmaku_path TEXT", [])
            .map_err(db_err)?;
    }
    if !columns.iter().any(|name| name == "danmaku_count") {
        conn.execute(
            "ALTER TABLE record_session ADD COLUMN danmaku_count INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .map_err(db_err)?;
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
    area_name, is_live, started_at, online, auto_record, record_qn, view_qn,
    last_checked_at, created_at, updated_at
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
        online: row.get::<_, Option<i64>>(11)?.map(|value| value.max(0) as u64),
        auto_record: row.get::<_, i64>(12)? != 0,
        record_qn: row.get(13)?,
        view_qn: row.get(14)?,
        last_checked_at: row.get(15)?,
        created_at: row.get(16)?,
        updated_at: row.get(17)?,
    })
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
