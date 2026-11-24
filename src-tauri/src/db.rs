//! SQLite 持久层：房间列表 + 录制历史。
//!
//! 为什么用 rusqlite 而不是 async 版：本地单文件、查询都是毫秒级，
//! 加一层 spawn_blocking 或 async 包装只会让代码更难读；这里用 Mutex 串行化，
//! 并通过 WAL 让读不阻塞写。真正的长任务（ffmpeg）不在锁内。

use std::path::Path;
use std::sync::Mutex;

use hivecast_ipc::{
    DouyinFollow, MediaFile, MediaItem, PlatformCount, QualitySegment, RecordSession, RemovedRoom,
    RoomRecord, SessionSummary, TaskRecord,
};
use rusqlite::types::Value as SqlValue;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::Value;

use crate::error::AppError;
use crate::platform::RoomInfo;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS room (\n    id TEXT PRIMARY KEY,\n    platform TEXT NOT NULL,\n    room_key TEXT NOT NULL,\n    platform_uid TEXT,\n    -- 关注导入标记：抖音关注房可用 follow/feed 批量状态，手动房仍走单房\n    batch_follow INTEGER NOT NULL DEFAULT 0,\n    -- 平台内部房间号：抖音批量状态用 19 位 room_id，room_key 是 web_rid\n    internal_room_id TEXT,\n    canonical_url TEXT NOT NULL,\n    title TEXT NOT NULL DEFAULT '',\n    host_name TEXT NOT NULL DEFAULT '',\n    avatar_url TEXT,\n    cover_url TEXT,\n    area_name TEXT,\n    is_live INTEGER NOT NULL DEFAULT 0,\n    started_at INTEGER,\n    online INTEGER,\n    auto_record INTEGER NOT NULL DEFAULT 1,\n    monitor_paused INTEGER NOT NULL DEFAULT 0,\n    record_qn INTEGER NOT NULL DEFAULT 10000,\n    view_qn INTEGER NOT NULL DEFAULT 0,\n    record_line TEXT,\n    last_checked_at INTEGER,\n    created_at INTEGER NOT NULL,\n    updated_at INTEGER NOT NULL,\n    -- 软删除：移除房间只打时间戳，录像历史仍挂在它上面\n    removed_at INTEGER,\n    UNIQUE (platform, room_key)\n);\n\n-- 抖音关注池：离线关注没有 web_rid，先留在这里等开播；开播拿到 web_rid 后
-- 转成正式 room，`promoted_room_id` 指回房间。
CREATE TABLE IF NOT EXISTS douyin_follow (
    uid TEXT PRIMARY KEY,
    sec_uid TEXT NOT NULL DEFAULT '',
    uname TEXT NOT NULL DEFAULT '',
    avatar_url TEXT,
    web_rid TEXT,
    internal_room_id TEXT,
    is_live INTEGER NOT NULL DEFAULT 0,
    title TEXT NOT NULL DEFAULT '',
    cover_url TEXT,
    area_name TEXT,
    online INTEGER,
    started_at INTEGER,
    monitor INTEGER NOT NULL DEFAULT 1,
    auto_record INTEGER NOT NULL DEFAULT 0,
    promoted_room_id TEXT,
    last_checked_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_douyin_follow_pending
    ON douyin_follow (promoted_room_id, last_checked_at);

CREATE TABLE IF NOT EXISTS record_session (\n    id TEXT PRIMARY KEY,\n    room_id TEXT NOT NULL REFERENCES room(id) ON DELETE CASCADE,\n    started_at INTEGER NOT NULL,\n    ended_at INTEGER,\n    status TEXT NOT NULL,\n    reason TEXT,\n    file_path TEXT,\n    file_size INTEGER NOT NULL DEFAULT 0,\n    duration_ms INTEGER NOT NULL DEFAULT 0,\n    quality TEXT,\n    line TEXT,\n    error TEXT,\n    danmaku_path TEXT,\n    danmaku_count INTEGER NOT NULL DEFAULT 0,\n    -- 分割录制：同一场直播的片段共享 group_id，clip_index 从 0 递增\n    group_id TEXT,\n    clip_index INTEGER NOT NULL DEFAULT 0,\n    split_reason TEXT,\n    -- 后处理：源封装母带 + 派生文件。source_deleted 只表示源已进回收站。\n    source_path TEXT,\n    post_mode TEXT NOT NULL DEFAULT 'source',\n    post_state TEXT NOT NULL DEFAULT 'done',\n    source_deleted INTEGER NOT NULL DEFAULT 0,\n    post_error TEXT,\n    quality_history TEXT\n);\n\nCREATE INDEX IF NOT EXISTS idx_record_session_room\n    ON record_session (room_id, started_at DESC);\n\nCREATE INDEX IF NOT EXISTS idx_record_session_status\n    ON record_session (status, started_at DESC);\n\n-- 任务中心按开始时间倒序扫全表；没有这个索引时每次轮询都会临时排序。\nCREATE INDEX IF NOT EXISTS idx_record_session_started\n    ON record_session (started_at DESC);\n";

/// 后续里程碑追加的表：与初始 SCHEMA 分开，避免每次都要改那一行超长字符串。
const EXTRA_SCHEMA: &str = "
-- 同一秒内的多条场次必须有稳定次序，否则 OFFSET 分页可能重复或漏行。
CREATE INDEX IF NOT EXISTS idx_record_session_started_id
    ON record_session (started_at DESC, id DESC);

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

-- 任务中心历史：运行中的任务也会先落一行，进程异常退出后启动时改成 interrupted。
CREATE TABLE IF NOT EXISTS task_history (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    title TEXT NOT NULL,
    detail TEXT,
    state TEXT NOT NULL,
    percent INTEGER NOT NULL DEFAULT 0,
    stage TEXT,
    message TEXT,
    session_id TEXT,
    room_id TEXT,
    started_at INTEGER NOT NULL,
    -- 入队与真正开始执行分开记：排队时长不能被任务创建时间糊掉
    queued_at INTEGER,
    started_running_at INTEGER,
    finished_at INTEGER
);

CREATE INDEX IF NOT EXISTS idx_task_history_started
    ON task_history (started_at DESC);

-- 阶段事件：只记转换与 25/50/75/100 里程碑，不按秒膨胀。
CREATE TABLE IF NOT EXISTS task_event (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id TEXT NOT NULL,
    run_id TEXT,
    kind TEXT NOT NULL,
    stage TEXT NOT NULL,
    state TEXT NOT NULL,
    percent INTEGER NOT NULL DEFAULT 0,
    message TEXT,
    at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_task_event_run
    ON task_event (run_id, at);

CREATE INDEX IF NOT EXISTS idx_task_event_task
    ON task_event (task_id, at);

-- 跨 Run 检索的全文索引：trigram 支持 `LIKE '%foo%'` 的任意子串，
-- 但至少 3 个字符才进索引；1–2 字符由调用方回退到阶段/状态枚举与 LIKE。
-- rowid 直接复用 task_event.id，触发器保证增删改同步，避免检索到已删除事件。
CREATE VIRTUAL TABLE IF NOT EXISTS task_event_fts USING fts5(
    body,
    tokenize = 'trigram'
);

CREATE TRIGGER IF NOT EXISTS task_event_fts_ai AFTER INSERT ON task_event BEGIN
    INSERT INTO task_event_fts(rowid, body)
    VALUES (new.id,
        COALESCE(new.message, '') || ' ' || new.stage || ' ' || new.state || ' ' ||
        new.kind || ' ' || new.task_id);
END;

CREATE TRIGGER IF NOT EXISTS task_event_fts_ad AFTER DELETE ON task_event BEGIN
    DELETE FROM task_event_fts WHERE rowid = old.id;
END;

CREATE TRIGGER IF NOT EXISTS task_event_fts_au
AFTER UPDATE OF message, stage, state, kind, task_id ON task_event BEGIN
    DELETE FROM task_event_fts WHERE rowid = old.id;
    INSERT INTO task_event_fts(rowid, body)
    VALUES (new.id,
        COALESCE(new.message, '') || ' ' || new.stage || ' ' || new.state || ' ' ||
        new.kind || ' ' || new.task_id);
END;

-- 一条录像登记在册的全部文件：主文件、源封装母带、后处理派生文件。
-- record_session 的 file_path/source_path 仍是播放与兼容的主索引，这张表负责完整清单。
CREATE TABLE IF NOT EXISTS media_file (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES record_session(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    role TEXT NOT NULL DEFAULT 'derived',
    size INTEGER NOT NULL DEFAULT 0,
    format TEXT,
    state TEXT NOT NULL DEFAULT 'present',
    created_at INTEGER NOT NULL,
    deleted_at INTEGER,
    UNIQUE (session_id, path)
);

CREATE INDEX IF NOT EXISTS idx_media_file_session
    ON media_file (session_id, role);

-- 平台长期凭证（目前只有斗鱼用）：LTP0 与配套设备号单独加密存放，
-- 不混进主站 Cookie，避免被带到所有 douyu.com 请求与界面字段列表里。
CREATE TABLE IF NOT EXISTS credential_secret (
    platform TEXT PRIMARY KEY,
    refresh_token TEXT NOT NULL DEFAULT '',
    device_id TEXT NOT NULL DEFAULT '',
    updated_at INTEGER NOT NULL
);

-- Cookie 自动续期的最近一次检查结果：只存状态，不重复存 Cookie。
CREATE TABLE IF NOT EXISTS credential_refresh (
    platform TEXT PRIMARY KEY,
    checked_at INTEGER NOT NULL DEFAULT 0,
    outcome TEXT NOT NULL DEFAULT 'never',
    message TEXT,
    key_stale INTEGER NOT NULL DEFAULT 0,
    last_refreshed_at INTEGER NOT NULL DEFAULT 0
);
";

/// 任务中心保留策略的删除结果，供日志与测试使用。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PrunedTaskData {
    pub history: usize,
    pub events: usize,
}

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
        // 「今天」是本地零点，不是滚动 24 小时；其余天数仍按滚动窗口。
        let since = if days == 1 {
            crate::clock::today_start_secs()
        } else {
            now_ts() - days * 86_400
        };
        args.push(SqlValue::Integer(since));
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
    /// FTS 回填是否成功；失败时检索退回 LIKE，不能让全文索引缺失变成 0 条结果。
    fts_ready: std::sync::atomic::AtomicBool,
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
        let db = Self {
            conn: Mutex::new(conn),
            secret,
            fts_ready: std::sync::atomic::AtomicBool::new(false),
        };
        // 老库没有 media_file 表：启动时按 file_path/source_path 回填一次。
        match db.backfill_media_files() {
            Ok(count) if count > 0 => {
                tracing::info!(count, "数据库迁移：回填媒体文件登记")
            }
            Ok(_) => {}
            Err(err) => tracing::warn!(error = %err, "回填媒体文件登记失败"),
        }
        // 早期版本只给后续片段写 group_id，首片段为空；COALESCE 已经能兼容，
        // 这里把首片段也指向自己，让所有旧场次都有稳定的分组键。
        match db.backfill_session_groups() {
            Ok(count) if count > 0 => {
                tracing::info!(count, "数据库迁移：补全旧录制场次分组")
            }
            Ok(_) => {}
            Err(err) => tracing::warn!(error = %err, "补全旧录制场次分组失败"),
        }
        // 跨 Run 检索的 FTS 索引：老库第一次启动时回填一次。失败不阻塞启动，
        // 检索会自动退回 LIKE（见 task_event_search），最多慢一点，不会功能缺失。
        let fts_ready = match db.ensure_task_event_fts() {
            Ok(count) => {
                if count > 0 {
                    tracing::info!(count, "数据库迁移：回填任务事件全文索引");
                }
                true
            }
            Err(err) => {
                tracing::warn!(error = %err, "回填任务事件全文索引失败，检索将回退 LIKE");
                false
            }
        };
        db.fts_ready
            .store(fts_ready, std::sync::atomic::Ordering::Relaxed);
        Ok(db)
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
        let rows: Vec<(String, Option<String>, i64, String)> = {
            let conn = self.lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, file_path, started_at, room_id FROM record_session
                      WHERE status = 'recording'",
                )
                .map_err(db_err)?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })
                .map_err(db_err)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(db_err)?
        };
        let mut changed = 0;
        for (id, file_path, started_at, room_id) in rows {
            let last_data = file_path.as_deref().and_then(crate::segment::last_data_at);
            let ended_at = orphan_ended_at(started_at, last_data, now);
            let conn = self.lock();
            changed += conn
                .execute(
                    "UPDATE record_session
                        SET status = 'aborted', reason = 'app_exit',
                            ended_at = ?2, duration_ms = (?2 - started_at) * 1000
                      WHERE id = ?1 AND status = 'recording'",
                    params![id, ended_at],
                )
                .map_err(db_err)?;
            refresh_room_last_session(&conn, &room_id).map_err(db_err)?;
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

    /// 已移除的直播间：软删除后房间不再出现在监控列表，但录像仍要能找回。
    pub fn list_removed_rooms(&self) -> Result<Vec<RemovedRoom>, AppError> {
        let conn = self.lock();
        let rooms = {
            let mut stmt = conn
                .prepare(&format!(
                    "{ROOM_SELECT} WHERE removed_at IS NOT NULL ORDER BY removed_at DESC"
                ))
                .map_err(db_err)?;
            let rows = stmt.query_map([], map_room).map_err(db_err)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(db_err)?
        };
        rooms
            .into_iter()
            .map(|room| {
                let (session_count, total_bytes) = conn
                    .query_row(
                        "SELECT COUNT(DISTINCT COALESCE(group_id, id)),
                                COALESCE(SUM(file_size), 0)
                           FROM record_session WHERE room_id = ?1",
                        params![room.id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .map_err(db_err)?;
                Ok(RemovedRoom {
                    room,
                    session_count,
                    total_bytes,
                })
            })
            .collect()
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

    pub fn find_room_by_uid(
        &self,
        platform: &str,
        platform_uid: &str,
    ) -> Result<Option<RoomRecord>, AppError> {
        let conn = self.lock();
        conn.query_row(
            &format!(
                "{ROOM_SELECT} WHERE platform = ?1 AND platform_uid = ?2 \
                 ORDER BY (removed_at IS NULL) DESC, created_at ASC LIMIT 1"
            ),
            params![platform, platform_uid],
            map_room,
        )
        .optional()
        .map_err(db_err)
    }

    /// 把旧短号行改挂到平台稳定 ID 上；保留主键，历史 session 不受影响。
    pub fn rekey_room(
        &self,
        id: &str,
        room_key: &str,
        canonical_url: &str,
    ) -> Result<(), AppError> {
        self.lock()
            .execute(
                "UPDATE room SET room_key = ?2, canonical_url = ?3 WHERE id = ?1",
                params![id, room_key, canonical_url],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// 合并两条指向同一直播间的旧记录：保留 keep，把 drop 的 session/history 迁过去。
    pub fn merge_room(&self, keep_id: &str, drop_id: &str) -> Result<(), AppError> {
        if keep_id == drop_id {
            return Ok(());
        }
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(db_err)?;
        tx.execute(
            "UPDATE room SET
                note = CASE WHEN COALESCE(note, '') = '' THEN
                    COALESCE((SELECT note FROM room WHERE id = ?2), '') ELSE note END,
                tags = CASE WHEN COALESCE(tags, '') = '' THEN
                    COALESCE((SELECT tags FROM room WHERE id = ?2), '') ELSE tags END,
                auto_record = MAX(auto_record, COALESCE((SELECT auto_record FROM room WHERE id = ?2), 0)),
                monitor_paused = MIN(monitor_paused, COALESCE((SELECT monitor_paused FROM room WHERE id = ?2), 0)),
                platform_uid = COALESCE(platform_uid, (SELECT platform_uid FROM room WHERE id = ?2)),
                batch_follow = MAX(batch_follow, COALESCE((SELECT batch_follow FROM room WHERE id = ?2), 0)),
                internal_room_id = COALESCE(internal_room_id, (SELECT internal_room_id FROM room WHERE id = ?2)),
                record_qn = CASE WHEN COALESCE(record_qn, 0) = 0 THEN
                    COALESCE((SELECT record_qn FROM room WHERE id = ?2), 0) ELSE record_qn END,
                view_qn = CASE WHEN COALESCE(view_qn, 0) = 0 THEN
                    COALESCE((SELECT view_qn FROM room WHERE id = ?2), 0) ELSE view_qn END,
                record_line = COALESCE(record_line, (SELECT record_line FROM room WHERE id = ?2)),
                title_include = COALESCE(title_include, (SELECT title_include FROM room WHERE id = ?2)),
                title_exclude = COALESCE(title_exclude, (SELECT title_exclude FROM room WHERE id = ?2)),
                split_max_duration_sec = COALESCE(split_max_duration_sec, (SELECT split_max_duration_sec FROM room WHERE id = ?2)),
                split_max_size_mb = COALESCE(split_max_size_mb, (SELECT split_max_size_mb FROM room WHERE id = ?2)),
                notify_live = COALESCE(notify_live, (SELECT notify_live FROM room WHERE id = ?2)),
                notify_error = COALESCE(notify_error, (SELECT notify_error FROM room WHERE id = ?2))
              WHERE id = ?1",
            params![keep_id, drop_id],
        )
        .map_err(db_err)?;
        tx.execute(
            "UPDATE record_session SET room_id = ?1 WHERE room_id = ?2",
            params![keep_id, drop_id],
        )
        .map_err(db_err)?;
        tx.execute(
            "UPDATE task_history SET room_id = ?1 WHERE room_id = ?2",
            params![keep_id, drop_id],
        )
        .map_err(db_err)?;
        tx.execute("DELETE FROM room WHERE id = ?1", params![drop_id])
            .map_err(db_err)?;
        tx.commit().map_err(db_err)?;
        refresh_room_last_session(&conn, keep_id).map_err(db_err)?;
        Ok(())
    }

    /// 新房间入库；同平台同房间号视为同一条，更新展示信息并返回。
    pub fn insert_room(&self, room: &RoomRecord, info: &RoomInfo) -> Result<RoomRecord, AppError> {
        let now = now_ts();
        {
            let conn = self.lock();
            conn.execute(
                "INSERT INTO room (
                    id, platform, room_key, platform_uid, canonical_url, title, host_name,
                    avatar_url, cover_url, area_name, is_live, started_at, online, auto_record,
                    monitor_paused, record_qn, view_qn, last_checked_at, created_at, updated_at,
                    batch_follow, internal_room_id
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)
                ON CONFLICT (platform, room_key) DO UPDATE SET
                    platform_uid = COALESCE(excluded.platform_uid, platform_uid),
                    -- 关注来源是粘性的：手动重新添加同一直播间也不该丢掉批量资格。
                    batch_follow = MAX(batch_follow, excluded.batch_follow),
                    internal_room_id = COALESCE(excluded.internal_room_id, internal_room_id),
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
                    platform_uid_of(info).or_else(|| room.platform_uid.clone()),
                    room.canonical_url,
                    info.room_name,
                    info.host_name,
                    info.avatar_url,
                    info.cover_url,
                    info.area_name,
                    info.is_live as i64,
                    info.started_at,
                    info.online.map(|value| value as i64),
                    room.auto_record as i64,
                    room.monitor_paused as i64,
                    room.record_qn,
                    room.view_qn,
                    now,
                    room.created_at,
                    now,
                    room.batch_follow as i64,
                    internal_room_id_of(info).or_else(|| room.internal_room_id.clone()),
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
                    platform_uid = COALESCE(?11, platform_uid),
                    internal_room_id = COALESCE(?12, internal_room_id),
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
                    info.online.map(|value| value as i64),
                    now,
                    platform_uid_of(info),
                    internal_room_id_of(info),
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

    /// 关注池：新增或刷新一条关注项。已转正式房间的行不会被重新覆盖成待开播。
    pub fn upsert_douyin_follow(
        &self,
        item: &DouyinFollow,
        monitor: bool,
        auto_record: bool,
    ) -> Result<(), AppError> {
        let now = now_ts();
        self.lock()
            .execute(
                "INSERT INTO douyin_follow (
                    uid, sec_uid, uname, avatar_url, web_rid, internal_room_id, is_live,
                    title, cover_url, area_name, online, started_at, monitor, auto_record,
                    last_checked_at, created_at, updated_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?15, ?15)
                ON CONFLICT (uid) DO UPDATE SET
                    sec_uid = CASE WHEN excluded.sec_uid <> '' THEN excluded.sec_uid ELSE sec_uid END,
                    uname = CASE WHEN excluded.uname <> '' THEN excluded.uname ELSE uname END,
                    avatar_url = COALESCE(excluded.avatar_url, avatar_url),
                    web_rid = COALESCE(excluded.web_rid, web_rid),
                    internal_room_id = COALESCE(excluded.internal_room_id, internal_room_id),
                    is_live = excluded.is_live,
                    title = CASE WHEN excluded.title <> '' THEN excluded.title ELSE title END,
                    cover_url = COALESCE(excluded.cover_url, cover_url),
                    area_name = COALESCE(excluded.area_name, area_name),
                    online = excluded.online,
                    started_at = excluded.started_at,
                    monitor = excluded.monitor,
                    auto_record = excluded.auto_record,
                    last_checked_at = excluded.last_checked_at,
                    updated_at = excluded.updated_at",
                params![
                    item.uid,
                    item.sec_uid,
                    item.uname,
                    item.avatar_url,
                    item.web_rid,
                    item.internal_room_id,
                    item.is_live as i64,
                    item.title,
                    item.cover_url,
                    item.area_name,
                    item.online.map(|value| value as i64),
                    item.started_at,
                    monitor as i64,
                    auto_record as i64,
                    now,
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// 关注池列表：在播的排前面，其次按最近检查时间。
    pub fn list_douyin_follows(&self) -> Result<Vec<DouyinFollow>, AppError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT uid, sec_uid, uname, avatar_url, web_rid, internal_room_id, is_live,
                        title, cover_url, area_name, online, started_at, monitor, auto_record, promoted_room_id
                   FROM douyin_follow
                  ORDER BY is_live DESC, last_checked_at DESC, uname ASC",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], map_douyin_follow)
            .map_err(db_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_err)?;
        Ok(rows)
    }

    /// 还没转成正式房间的关注项；后台只扫描这些，已转正的交给 follow/feed。
    pub fn pending_douyin_follows(&self) -> Result<Vec<DouyinFollow>, AppError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT uid, sec_uid, uname, avatar_url, web_rid, internal_room_id, is_live,
                        title, cover_url, area_name, online, started_at, monitor, auto_record, promoted_room_id
                   FROM douyin_follow
                  WHERE promoted_room_id IS NULL
                  ORDER BY last_checked_at ASC, uid ASC",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], map_douyin_follow)
            .map_err(db_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_err)?;
        Ok(rows)
    }

    /// 扫描关注列表后的状态写回；不动 monitor / auto_record / promoted_room_id。
    pub fn update_douyin_follow_status(&self, item: &DouyinFollow) -> Result<(), AppError> {
        let now = now_ts();
        self.lock()
            .execute(
                "UPDATE douyin_follow SET
                    sec_uid = CASE WHEN ?2 <> '' THEN ?2 ELSE sec_uid END,
                    uname = CASE WHEN ?3 <> '' THEN ?3 ELSE uname END,
                    avatar_url = COALESCE(?4, avatar_url),
                    web_rid = COALESCE(?5, web_rid),
                    internal_room_id = COALESCE(?6, internal_room_id),
                    is_live = ?7,
                    title = CASE WHEN ?8 <> '' THEN ?8 ELSE title END,
                    cover_url = COALESCE(?9, cover_url),
                    area_name = COALESCE(?10, area_name),
                    online = ?11,
                    started_at = ?12,
                    last_checked_at = ?13,
                    updated_at = ?13
                  WHERE uid = ?1",
                params![
                    item.uid,
                    item.sec_uid,
                    item.uname,
                    item.avatar_url,
                    item.web_rid,
                    item.internal_room_id,
                    item.is_live as i64,
                    item.title,
                    item.cover_url,
                    item.area_name,
                    item.online.map(|value| value as i64),
                    item.started_at,
                    now,
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// 关注池项已转成正式房间，后续不再扫描它。
    pub fn mark_douyin_follow_promoted(
        &self,
        uid: &str,
        promoted_room_id: &str,
    ) -> Result<(), AppError> {
        let now = now_ts();
        self.lock()
            .execute(
                "UPDATE douyin_follow
                    SET promoted_room_id = ?2, is_live = 1, last_checked_at = ?3, updated_at = ?3
                  WHERE uid = ?1",
                params![uid, promoted_room_id, now],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// 把已有抖音房间补成「关注批量」房间。
    ///
    /// 用户可能先手动添加、之后才导入关注；导入接口命中已有行时不会重建，
    /// 但必须补上 batch_follow 与关注 uid，否则这个房间永远走单房轮询。
    /// 关注池转正命中已有房间也走这里。
    pub fn mark_douyin_batch_room(
        &self,
        room_id: &str,
        uid: Option<&str>,
        internal_room_id: Option<&str>,
    ) -> Result<(), AppError> {
        let now = now_ts();
        self.lock()
            .execute(
                "UPDATE room SET
                    batch_follow = 1,
                    platform_uid = COALESCE(?2, platform_uid),
                    internal_room_id = COALESCE(?3, internal_room_id),
                    updated_at = ?4
                  WHERE id = ?1 AND platform = 'douyin'",
                params![room_id, uid, internal_room_id, now],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// 给已有房间补批量标记（虎牙先手动添加、后导入关注时用）。
    ///
    /// 只改平台匹配的行，避免把别的平台房间误标成批量；uid 为 None 时保留
    /// 原值，重复导入不会把已经解析出的 UID 清空。
    pub fn mark_platform_batch_room(
        &self,
        room_id: &str,
        platform: &str,
        uid: Option<&str>,
    ) -> Result<(), AppError> {
        let now = now_ts();
        self.lock()
            .execute(
                "UPDATE room SET
                    batch_follow = 1,
                    platform_uid = COALESCE(?3, platform_uid),
                    updated_at = ?4
                  WHERE id = ?1 AND platform = ?2",
                params![room_id, platform, uid, now],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// 连续多次在关注列表里找不到这个房间：退回单房检查。
    ///
    /// 不删平台 UID，也不删房间；只是清掉 `batch_follow`，下一拍开始走
    /// 单房接口。用户重新导入关注时会再次补上标记。
    pub fn clear_batch_follow(&self, room_id: &str) -> Result<(), AppError> {
        let now = now_ts();
        self.lock()
            .execute(
                "UPDATE room SET batch_follow = 0, updated_at = ?2 WHERE id = ?1",
                params![room_id, now],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// 房间被删除时一起删掉对应的关注池项。
    ///
    /// 不能只把 promoted_room_id 置空：关注仍在池里，下一轮扫描会把刚删掉的
    /// 房间重新转正，用户会看到「删了又回来」。要恢复关注批量，重新导入即可。
    pub fn delete_douyin_follow_for_room(&self, room_id: &str) -> Result<usize, AppError> {
        let removed = self
            .lock()
            .execute(
                "DELETE FROM douyin_follow WHERE promoted_room_id = ?1",
                params![room_id],
            )
            .map_err(db_err)?;
        Ok(removed)
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
                params![
                    id,
                    online.map(|value| value as i64),
                    now,
                    min_interval.max(0)
                ],
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
    /// 监控列表把它过滤掉，历史录像仍能从该房间的「录制文件」里找到；
    /// 重新添加同一房间会清掉标记并恢复它。
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

    /// 恢复一个软删除的房间：重新进入监控列表，并马上安排一次检查。
    pub fn restore_room(&self, id: &str) -> Result<(), AppError> {
        let now = now_ts();
        self.lock()
            .execute(
                "UPDATE room
                    SET removed_at = NULL, monitor_paused = 0,
                        last_checked_at = NULL, updated_at = ?2
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
        let conn = self.lock();
        conn.execute(
            "INSERT INTO record_session (
                    id, room_id, started_at, ended_at, status, reason, file_path,
                    file_size, duration_ms, quality, line, error,
                    danmaku_path, danmaku_count, group_id, clip_index, source_path,
                    post_mode, post_state, source_deleted, post_error, quality_history
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                          ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
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
                session.id,
                0_i64,
                session.source_path,
                session.post_mode,
                session.post_state,
                session.source_deleted as i64,
                session.post_error,
                serde_json::to_string(&session.quality_history).unwrap_or_else(|_| "[]".into()),
            ],
        )
        .map_err(db_err)?;
        sync_media_files_on(
            &conn,
            &session.id,
            session.file_path.as_deref(),
            session.source_path.as_deref(),
            session.source_deleted,
        );
        refresh_room_last_session(&conn, &session.room_id).map_err(db_err)?;
        Ok(())
    }

    /// 创建一个分割片段：group_id 指向首片段，clip_index 从 0 递增。
    pub fn create_clip(
        &self,
        session: &RecordSession,
        group_id: &str,
        clip_index: i64,
        split_reason: Option<&str>,
    ) -> Result<(), AppError> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO record_session (
                    id, room_id, started_at, ended_at, status, reason, file_path,
                    file_size, duration_ms, quality, line, error,
                    danmaku_path, danmaku_count, group_id, clip_index, split_reason,
                    source_path, post_mode, post_state, source_deleted, post_error,
                    quality_history
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                          ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)",
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
                group_id,
                clip_index,
                split_reason,
                session.source_path,
                session.post_mode,
                session.post_state,
                session.source_deleted as i64,
                session.post_error,
                serde_json::to_string(&session.quality_history).unwrap_or_else(|_| "[]".into()),
            ],
        )
        .map_err(db_err)?;
        sync_media_files_on(
            &conn,
            &session.id,
            session.file_path.as_deref(),
            session.source_path.as_deref(),
            session.source_deleted,
        );
        refresh_room_last_session(&conn, &session.room_id).map_err(db_err)?;
        Ok(())
    }

    /// 收尾并提交后处理结果：路径可能从源封装变成派生文件，所以一并更新。
    #[allow(clippy::too_many_arguments)]
    pub fn finish_session_with_post(
        &self,
        id: &str,
        status: &str,
        reason: &str,
        ended_at: i64,
        file_path: &str,
        file_size: i64,
        source_path: Option<&str>,
        post_mode: &str,
        post_state: &str,
        source_deleted: bool,
        post_error: Option<&str>,
        danmaku_path: Option<&str>,
        danmaku_count: i64,
        error: Option<&str>,
    ) -> Result<(), AppError> {
        let conn = self.lock();
        conn.execute(
            "UPDATE record_session SET
                    status = ?2, reason = ?3, ended_at = ?4, file_path = ?5,
                    file_size = ?6, duration_ms = MAX(0, (?4 - started_at) * 1000),
                    source_path = ?7, post_mode = ?8, post_state = ?9,
                    source_deleted = ?10, post_error = ?11,
                    danmaku_path = ?12, danmaku_count = ?13, error = ?14
                  WHERE id = ?1",
            params![
                id,
                status,
                reason,
                ended_at,
                file_path,
                file_size,
                source_path,
                post_mode,
                post_state,
                source_deleted as i64,
                post_error,
                danmaku_path,
                danmaku_count,
                error
            ],
        )
        .map_err(db_err)?;
        sync_media_files_on(&conn, id, Some(file_path), source_path, source_deleted);
        let room_id: String = conn
            .query_row(
                "SELECT room_id FROM record_session WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .map_err(db_err)?;
        refresh_room_last_session(&conn, &room_id).map_err(db_err)?;
        Ok(())
    }

    /// 源已经移进回收站后单独打标；失败只影响展示，不影响播放。
    pub fn set_source_deleted(&self, id: &str, deleted: bool) -> Result<(), AppError> {
        let conn = self.lock();
        conn.execute(
            "UPDATE record_session SET source_deleted = ?2 WHERE id = ?1",
            params![id, deleted as i64],
        )
        .map_err(db_err)?;
        let source: Option<Option<String>> = conn
            .query_row(
                "SELECT source_path FROM record_session WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?;
        if let Some(Some(source)) = source {
            if deleted {
                conn.execute(
                    "UPDATE media_file SET state = 'deleted', deleted_at = ?3
                      WHERE session_id = ?1 AND path = ?2",
                    params![id, source, now_ts()],
                )
                .map_err(db_err)?;
            } else {
                conn.execute(
                    "UPDATE media_file SET state = 'present', deleted_at = NULL
                      WHERE session_id = ?1 AND path = ?2",
                    params![id, source],
                )
                .map_err(db_err)?;
            }
        }
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
        offset: usize,
    ) -> Result<Vec<RecordSession>, AppError> {
        let conn = self.lock();
        let limit = limit.clamp(1, 500) as i64;
        let offset = offset as i64;
        let mut sessions = Vec::new();
        match room_id {
            Some(id) => {
                let mut stmt = conn
                    .prepare(&format!(
                        "{SESSION_SELECT} WHERE room_id = ?1 ORDER BY started_at DESC, id DESC LIMIT ?2 OFFSET ?3"
                    ))
                    .map_err(db_err)?;
                let rows = stmt
                    .query_map(params![id, limit, offset], map_session)
                    .map_err(db_err)?;
                for row in rows {
                    sessions.push(row.map_err(db_err)?);
                }
            }
            None => {
                let mut stmt = conn
                    .prepare(&format!(
                        "{SESSION_SELECT} ORDER BY started_at DESC, id DESC LIMIT ?1 OFFSET ?2"
                    ))
                    .map_err(db_err)?;
                let rows = stmt
                    .query_map(params![limit, offset], map_session)
                    .map_err(db_err)?;
                for row in rows {
                    sessions.push(row.map_err(db_err)?);
                }
            }
        }
        Ok(sessions)
    }

    /// 单房间录制历史汇总：不随分页变化，详情抽屉的合计和可合并数用它。
    pub fn session_summary(&self, room_id: &str) -> Result<SessionSummary, AppError> {
        let conn = self.lock();
        let since = now_ts() - 7 * 86_400;
        conn.query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(file_size), 0),
                    COALESCE(SUM(CASE WHEN status != 'recording' AND file_path IS NOT NULL THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(duration_ms), 0),
                    COALESCE(SUM(CASE WHEN started_at >= ?2 THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN started_at >= ?2 THEN duration_ms ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN started_at >= ?2 THEN file_size ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN status IN ('failed', 'aborted') THEN 1 ELSE 0 END), 0)
               FROM record_session
              WHERE room_id = ?1",
            params![room_id, since],
            |row| {
                Ok(SessionSummary {
                    total: row.get(0)?,
                    total_bytes: row.get(1)?,
                    mergeable: row.get(2)?,
                    total_duration_ms: row.get(3)?,
                    recent_count: row.get(4)?,
                    recent_duration_ms: row.get(5)?,
                    recent_bytes: row.get(6)?,
                    failed_count: row.get(7)?,
                })
            },
        )
        .map_err(db_err)
    }

    /// 单房间规则/备注：一次写回，避免每个字段拆成单独命令。
    // 逐列写回房间规则；拆结构体只会多一层搬运。
    #[allow(clippy::too_many_arguments)]
    pub fn set_room_rules(
        &self,
        id: &str,
        note: &str,
        tags: &str,
        title_include: Option<&str>,
        title_exclude: Option<&str>,
        split_max_duration_sec: Option<i64>,
        split_max_size_mb: Option<i64>,
        notify_live: Option<bool>,
        notify_error: Option<bool>,
    ) -> Result<(), AppError> {
        self.lock()
            .execute(
                "UPDATE room SET
                    note = ?2, tags = ?3,
                    title_include = ?4, title_exclude = ?5,
                    split_max_duration_sec = ?6, split_max_size_mb = ?7,
                    notify_live = ?8, notify_error = ?9,
                    updated_at = ?10
                  WHERE id = ?1",
                params![
                    id,
                    note,
                    tags,
                    title_include,
                    title_exclude,
                    split_max_duration_sec,
                    split_max_size_mb,
                    notify_live.map(|value| value as i64),
                    notify_error.map(|value| value as i64),
                    now_ts(),
                ],
            )
            .map_err(db_err)?;
        Ok(())
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

    /// 一次事务写入一批设置：保存设置要么整套生效，要么整套保持原样。
    pub fn set_settings(&self, values: &[(&str, String)]) -> Result<(), AppError> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(db_err)?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO app_setting (key, value, updated_at) VALUES (?1, ?2, ?3)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                )
                .map_err(db_err)?;
            let now = now_ts();
            for (key, value) in values {
                stmt.execute(params![key, value, now]).map_err(db_err)?;
            }
        }
        tx.commit().map_err(db_err)
    }

    /// 媒体库单行映射：列表和整组补齐共用，避免两处 SELECT 漂移。
    fn media_item_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MediaItem> {
        let path: String = row.get(5)?;
        let source_path: Option<String> = row.get(16)?;
        let source_deleted = row.get::<_, i64>(19)? != 0;
        let has_source = !source_deleted
            && source_path
                .as_deref()
                .is_some_and(|source| source != path && std::path::Path::new(source).is_file());
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
            danmaku_path: row.get(11)?,
            has_file: std::path::Path::new(&path).is_file(),
            thumbnail_url: None,
            group_id: row.get(12)?,
            clip_index: row.get(13)?,
            split_reason: row.get(14)?,
            clip_count: row.get(15)?,
            source_path,
            post_mode: row.get(17)?,
            post_state: row.get(18)?,
            source_deleted,
            post_error: row.get(20)?,
            has_source,
        })
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
        page_args.push(SqlValue::Integer(filter.offset as i64));
        let sql = format!(
            "SELECT s.id, s.room_id, r.platform, r.title, r.host_name, s.file_path,
                    s.file_size, s.duration_ms, s.started_at, s.status, s.danmaku_count,
                    s.danmaku_path,
                    COALESCE(s.group_id, s.id), s.clip_index, s.split_reason,
                    (SELECT COUNT(*) FROM record_session x
                      WHERE COALESCE(x.group_id, x.id) = COALESCE(s.group_id, s.id)),
                    s.source_path, s.post_mode, s.post_state, s.source_deleted, s.post_error
               FROM record_session s
               JOIN room r ON r.id = s.room_id{where_sql}
              ORDER BY {}
              LIMIT ? OFFSET ?",
            filter.order_by()
        );
        let mut stmt = conn.prepare(&sql).map_err(db_err)?;
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(page_args.iter()),
                Self::media_item_from_row,
            )
            .map_err(db_err)?;
        let items = rows.collect::<Result<Vec<_>, _>>().map_err(db_err)?;
        Ok((items, total))
    }

    /// 取一整场直播的全部片段：分页只按片段返回时，前端可能拿不到整组，
    /// 单独用这个接口补齐，避免“+N”点了没反应。
    pub fn media_group_items(&self, group_id: &str) -> Result<Vec<MediaItem>, AppError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT s.id, s.room_id, r.platform, r.title, r.host_name, s.file_path,
                        s.file_size, s.duration_ms, s.started_at, s.status, s.danmaku_count,
                        s.danmaku_path,
                        COALESCE(s.group_id, s.id), s.clip_index, s.split_reason,
                        (SELECT COUNT(*) FROM record_session x
                          WHERE COALESCE(x.group_id, x.id) = COALESCE(s.group_id, s.id)),
                        s.source_path, s.post_mode, s.post_state, s.source_deleted, s.post_error
                   FROM record_session s
                   JOIN room r ON r.id = s.room_id
                  WHERE COALESCE(s.group_id, s.id) = ?1
                  ORDER BY s.clip_index ASC, s.started_at ASC",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([group_id], Self::media_item_from_row)
            .map_err(db_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_err)
    }

    /// 各平台在当前筛选条件下的条数（忽略平台筛选本身）。
    pub fn media_platform_counts(
        &self,
        filter: &MediaFilter,
    ) -> Result<Vec<PlatformCount>, AppError> {
        let conn = self.lock();
        let (where_sql, args) = media_where(filter, false);
        let sql = format!(
            "SELECT r.platform, COUNT(DISTINCT COALESCE(s.group_id, s.id))
               FROM record_session s
               JOIN room r ON r.id = s.room_id{where_sql}
              GROUP BY r.platform
              ORDER BY COUNT(DISTINCT COALESCE(s.group_id, s.id)) DESC, r.platform ASC"
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
        let conn = self.lock();
        let room_id: Option<String> = conn
            .query_row(
                "SELECT room_id FROM record_session WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?;
        conn.execute("DELETE FROM record_session WHERE id = ?1", params![id])
            .map_err(db_err)?;
        if let Some(room_id) = room_id {
            refresh_room_last_session(&conn, &room_id).map_err(db_err)?;
        }
        Ok(())
    }

    /// 这条录像所属场次一共有几个片段。老数据没有 group_id 时算 1。
    pub fn session_group_size(&self, id: &str) -> Result<i64, AppError> {
        let conn = self.lock();
        conn.query_row(
            "SELECT COUNT(*) FROM record_session
              WHERE COALESCE(group_id, id) = COALESCE(
                  (SELECT COALESCE(group_id, id) FROM record_session WHERE id = ?1), ?1)",
            params![id],
            |row| row.get(0),
        )
        .map_err(db_err)
    }

    /// 同一场直播的全部片段。老数据 group_id 为空时按自己的 id 成组。
    pub fn group_sessions(&self, group_id: &str) -> Result<Vec<RecordSession>, AppError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(&format!(
                "{SESSION_SELECT} WHERE COALESCE(group_id, id) = ?1 ORDER BY clip_index, started_at"
            ))
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![group_id], map_session)
            .map_err(db_err)?;
        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row.map_err(db_err)?);
        }
        Ok(sessions)
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

    /// 记录 / 清除分割收尾清单。写清单后、收尾完成前崩溃，启动时按清单恢复。
    pub fn set_split_manifest(&self, id: &str, manifest: Option<&str>) -> Result<(), AppError> {
        self.lock()
            .execute(
                "UPDATE record_session SET split_manifest = ?2 WHERE id = ?1",
                params![id, manifest],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// 有分割清单的会话：不论状态都要先按清单恢复，不能走普通整段合并。
    pub fn manifest_sessions(&self) -> Result<Vec<(String, String)>, AppError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, split_manifest FROM record_session
                  WHERE split_manifest IS NOT NULL AND split_manifest <> ''",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(db_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_err)
    }

    /// 修正会话的产物路径与大小：合并失败降级成 .ts、或崩溃恢复后路径变化时用。
    /// 给已经写入的会话补上片段归属（首片段收尾后才知道自己是 group 的起点）。
    pub fn set_session_group(
        &self,
        id: &str,
        group_id: &str,
        clip_index: i64,
        split_reason: Option<&str>,
    ) -> Result<(), AppError> {
        self.lock()
            .execute(
                "UPDATE record_session
                    SET group_id = ?2, clip_index = ?3, split_reason = ?4
                  WHERE id = ?1",
                params![id, group_id, clip_index, split_reason],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn update_session_file(
        &self,
        id: &str,
        path: &str,
        file_size: i64,
    ) -> Result<(), AppError> {
        let conn = self.lock();
        conn.execute(
            "UPDATE record_session SET file_path = ?2, file_size = ?3 WHERE id = ?1",
            params![id, path, file_size],
        )
        .map_err(db_err)?;
        let source: Option<Option<String>> = conn
            .query_row(
                "SELECT source_path FROM record_session WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?;
        sync_media_files_on(&conn, id, Some(path), source.flatten().as_deref(), false);
        Ok(())
    }

    /// 断流续录拿到不同实际档位时更新会话元数据。
    ///
    /// 只改画质单值与 history，不碰状态、时长、路径：录制仍在同一个会话里继续。
    pub fn update_session_quality(
        &self,
        id: &str,
        quality: &str,
        line: &str,
        history: &[QualitySegment],
    ) -> Result<(), AppError> {
        self.lock()
            .execute(
                "UPDATE record_session
                    SET quality = ?2, line = ?3, quality_history = ?4
                  WHERE id = ?1",
                params![
                    id,
                    quality,
                    line,
                    serde_json::to_string(history).unwrap_or_else(|_| "[]".into())
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// 手动转封装成功后把主文件切到派生文件，同时保留源封装母带。
    ///
    /// 只改后处理相关列，不动状态、时长和弹幕：转封装不改录制本身。
    // 后处理字段逐列更新；保持与 SQL 列一一对应。
    #[allow(clippy::too_many_arguments)]
    pub fn update_session_post_file(
        &self,
        id: &str,
        file_path: &str,
        file_size: i64,
        source_path: Option<&str>,
        post_mode: &str,
        post_state: &str,
        source_deleted: bool,
        post_error: Option<&str>,
    ) -> Result<(), AppError> {
        let conn = self.lock();
        conn.execute(
            "UPDATE record_session SET
                    file_path = ?2, file_size = ?3, source_path = ?4,
                    post_mode = ?5, post_state = ?6, source_deleted = ?7, post_error = ?8
                  WHERE id = ?1",
            params![
                id,
                file_path,
                file_size,
                source_path,
                post_mode,
                post_state,
                source_deleted as i64,
                post_error
            ],
        )
        .map_err(db_err)?;
        sync_media_files_on(&conn, id, Some(file_path), source_path, source_deleted);
        Ok(())
    }

    /// 保留策略用：已收尾且有文件的会话，按开始时间倒序。
    pub fn retention_rows(&self) -> Result<Vec<(String, String, i64, i64)>, AppError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                // 容量按 media_file 汇总：源封装母带、派生文件都要算，不能只看主文件。
                "SELECT id, file_path,
                        COALESCE((SELECT SUM(size) FROM media_file
                                   WHERE session_id = record_session.id AND state <> 'deleted'),
                                 file_size),
                        started_at
                   FROM record_session
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

/// 任务中心与文件登记：与录制历史同库，但独立成表，删除录像不会连带删任务历史。
impl Db {
    pub fn task_start(&self, task: &TaskRecord) -> Result<(), AppError> {
        self.lock()
            .execute(
                "INSERT INTO task_history (
                    id, kind, title, detail, state, percent, stage, message,
                    session_id, room_id, started_at, queued_at, started_running_at, finished_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11, NULL, NULL)
                 ON CONFLICT(id) DO UPDATE SET
                    kind = excluded.kind, title = excluded.title, detail = excluded.detail,
                    state = excluded.state, percent = excluded.percent, stage = excluded.stage,
                    message = excluded.message, session_id = excluded.session_id,
                    room_id = excluded.room_id, started_at = excluded.started_at,
                    queued_at = excluded.queued_at, started_running_at = NULL,
                    finished_at = NULL",
                params![
                    task.id,
                    task.kind,
                    task.title,
                    task.detail,
                    task.state,
                    task.percent,
                    task.stage,
                    task.message,
                    task.session_id,
                    task.room_id,
                    task.started_at,
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// 进度落库；queue→running 的边沿同时补 started_running_at（只写一次）。
    pub fn task_progress(
        &self,
        id: &str,
        percent: i64,
        stage: Option<&str>,
        state: &str,
        message: Option<&str>,
        at: i64,
    ) -> Result<(), AppError> {
        self.lock()
            .execute(
                "UPDATE task_history SET state = ?2, percent = ?3, stage = ?4, message = ?5,
                    started_running_at = CASE
                        WHEN started_running_at IS NULL AND ?2 = 'running' AND ?4 <> 'queued'
                        THEN ?6 ELSE started_running_at END
                  WHERE id = ?1 AND state IN ('running', 'queued')",
                params![id, state, percent.clamp(0, 100), stage, message, at],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// 老库第一次引入 FTS 时回填；用 app_setting 标记，避免每次启动重建。
    /// 触发器已经接管新事件，因此只需把历史行按 id 写进索引。
    fn ensure_task_event_fts(&self) -> Result<usize, AppError> {
        let mut conn = self.lock();
        let ready: Option<String> = conn
            .query_row(
                "SELECT value FROM app_setting WHERE key = 'db.task_event_fts_v1'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?;
        if ready.as_deref() == Some("1") {
            return Ok(0);
        }
        let tx = conn.transaction().map_err(db_err)?;
        tx.execute("DELETE FROM task_event_fts", [])
            .map_err(db_err)?;
        let count = tx
            .execute(
                "INSERT INTO task_event_fts(rowid, body)
                 SELECT id, COALESCE(message, '') || ' ' || stage || ' ' || state || ' ' ||
                        kind || ' ' || task_id
                   FROM task_event",
                [],
            )
            .map_err(db_err)?;
        tx.execute(
            "INSERT INTO app_setting (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params!["db.task_event_fts_v1", "1", now_ts()],
        )
        .map_err(db_err)?;
        tx.commit().map_err(db_err)?;
        Ok(count)
    }

    /// 追加一条阶段事件。id 由 SQLite 自增，传入值会被忽略。
    pub fn task_event(&self, event: &hivecast_ipc::TaskEvent) -> Result<(), AppError> {
        self.lock()
            .execute(
                "INSERT INTO task_event (task_id, run_id, kind, stage, state, percent, message, at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    event.task_id,
                    event.run_id,
                    event.kind,
                    event.stage,
                    event.state,
                    event.percent.clamp(0, 100),
                    event.message,
                    event.at,
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// 把 session id 或 group id 解析成同一场录制的所有片段 id。
    /// 任务中心懒加载时间线时用它把 session:group 展开成每个片段的
    /// session:id，否则分割录制的详情只能看到首片段事件。
    pub fn session_group_ids(&self, session_or_group_id: &str) -> Result<Vec<String>, AppError> {
        let conn = self.lock();
        // 调用方可能传 group id，也可能传组内某个片段 id；后者要先找到它
        // 所属的 group，否则只会返回该片段自己。
        let group_id: Option<String> = conn
            .query_row(
                "SELECT COALESCE(group_id, id) FROM record_session WHERE id = ?1 LIMIT 1",
                [session_or_group_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?;
        let group_id = group_id.as_deref().unwrap_or(session_or_group_id);
        let mut stmt = conn
            .prepare(
                "SELECT id FROM record_session
                  WHERE id = ?1 OR group_id = ?1
                  ORDER BY started_at ASC, id ASC",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([group_id], |row| row.get::<_, String>(0))
            .map_err(db_err)?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row.map_err(db_err)?);
        }
        Ok(ids)
    }

    /// 跨 Run 检索阶段事件。直接查 task_event，不依赖任务页当前加载的历史窗口。
    /// 通过 `session:<id>` 关联录像与房间，任务级事件没有房间信息时留空。
    pub fn task_event_search(
        &self,
        query: &str,
        only_failed: bool,
        platform: Option<&str>,
        limit: usize,
    ) -> Result<hivecast_ipc::TaskEventSearch, AppError> {
        let query = query.trim();
        let pattern = like_pattern(query);
        let has_query = i64::from(!query.is_empty());
        let only_failed = i64::from(only_failed);
        let platform = platform.unwrap_or("").trim();
        let limit = limit.clamp(1, 50_000);
        let from = "FROM task_event e
            LEFT JOIN record_session s ON e.run_id = 'session:' || s.id
            LEFT JOIN task_history th ON th.id = e.task_id
            LEFT JOIN room r ON r.id = COALESCE(s.room_id, th.room_id)";
        // 界面显示的是「录制 / 封面 / Running」，数据库存的是 record/thumbnail/running。
        // 阶段/状态先按中英文标签映射回内部枚举，用 IN 走索引；消息等自由文本才进 FTS。
        let stage_values = crate::labels::matching_stages(query);
        let state_values = crate::labels::matching_states(query);
        // trigram 索引至少 3 个字符；1–2 字符仍走 message LIKE，但阶段/状态
        // 已经由 IN 命中，不会每次都对整张表做无索引扫描。
        let use_fts = self.fts_ready.load(std::sync::atomic::Ordering::Relaxed)
            && query.chars().count() >= 3
            && query.chars().any(|ch| ch.is_alphanumeric());
        let mut clauses = vec!["?2 = 0".to_string()];
        if use_fts {
            clauses.push(
                "e.id IN (SELECT rowid FROM task_event_fts WHERE task_event_fts MATCH ?5)"
                    .to_string(),
            );
        } else {
            clauses.push(r"e.message LIKE ?1 ESCAPE '\'".to_string());
        }
        if let Some(clause) = enum_in_clause("e.stage", &stage_values) {
            clauses.push(clause);
        }
        if let Some(clause) = enum_in_clause("e.state", &state_values) {
            clauses.push(clause);
        }
        // 房间名改走 room 小表 + run_id/task_id 索引，避免 JOIN 后对事件表全表 LIKE。
        clauses.push(
            r"e.run_id IN (
                SELECT 'session:' || s2.id FROM record_session s2
                  JOIN room r2 ON r2.id = s2.room_id
                 WHERE r2.host_name LIKE ?1 ESCAPE '\' OR r2.title LIKE ?1 ESCAPE '\'
              )"
            .to_string(),
        );
        clauses.push(
            r"e.task_id IN (
                SELECT th2.id FROM task_history th2
                  JOIN room r3 ON r3.id = th2.room_id
                 WHERE r3.host_name LIKE ?1 ESCAPE '\' OR r3.title LIKE ?1 ESCAPE '\'
              )"
            .to_string(),
        );
        let filter = format!(
            "WHERE ({})
              AND (?3 = 0 OR e.state IN ('failed', 'interrupted'))
              AND (?4 = '' OR r.platform = ?4 OR r.platform IS NULL)",
            clauses.join(" OR ")
        );
        let mut args = vec![
            SqlValue::Text(pattern),
            SqlValue::Integer(has_query),
            SqlValue::Integer(only_failed),
            SqlValue::Text(platform.to_string()),
        ];
        if use_fts {
            // 整体加引号做短语匹配；内部双引号按 FTS5 规则翻倍。
            args.push(SqlValue::Text(format!(
                "\"{}\"",
                query.replace('"', "\"\"")
            )));
        }
        let conn = self.lock();
        let total: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) {from} {filter}"),
                rusqlite::params_from_iter(args.iter()),
                |row| row.get(0),
            )
            .map_err(db_err)?;
        let sql = format!(
            "SELECT e.id, e.task_id, e.run_id, e.kind, e.stage, e.state, e.percent, e.message, e.at,
                    s.id, s.group_id, COALESCE(s.room_id, th.room_id),
                    r.room_key, r.platform, r.host_name, r.title
               {from} {filter}
              ORDER BY e.at DESC, e.id DESC LIMIT ?{}",
            args.len() + 1
        );
        let mut hit_args = args;
        hit_args.push(SqlValue::Integer(limit as i64));
        let mut stmt = conn.prepare(&sql).map_err(db_err)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(hit_args.iter()), |row| {
                Ok(hivecast_ipc::TaskEventHit {
                    id: row.get(0)?,
                    task_id: row.get(1)?,
                    run_id: row.get(2)?,
                    kind: row.get(3)?,
                    stage: row.get(4)?,
                    state: row.get(5)?,
                    percent: row.get(6)?,
                    message: row.get(7)?,
                    at: row.get(8)?,
                    session_id: row.get(9)?,
                    group_id: row.get(10)?,
                    room_id: row.get(11)?,
                    room_key: row.get(12)?,
                    platform: row.get(13)?,
                    host_name: row.get(14)?,
                    room_title: row.get(15)?,
                })
            })
            .map_err(db_err)?;
        let mut hits = Vec::new();
        for row in rows {
            hits.push(row.map_err(db_err)?);
        }
        Ok(hivecast_ipc::TaskEventSearch { total, hits })
    }

    /// 一页事件：before 为 (at, id) 游标，只取更早的事件；返回是否还有更早页。
    pub fn task_event_page(
        &self,
        run_ids: &[String],
        before: Option<(i64, i64)>,
        limit: usize,
    ) -> Result<hivecast_ipc::TaskEventPage, AppError> {
        if run_ids.is_empty() {
            return Ok(hivecast_ipc::TaskEventPage {
                events: Vec::new(),
                has_more: false,
            });
        }
        let limit = limit.clamp(1, 1_000);
        let conn = self.lock();
        // SQLite 默认只有 999 个绑定变量；任务中心一次可能拿到上千条 Run，
        // 按 400 个一组查，避免 IN (...) 直接报 "too many SQL variables"。
        let mut events = Vec::new();
        for chunk in run_ids.chunks(400) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let mut where_sql = format!("run_id IN ({placeholders})");
            let mut values: Vec<SqlValue> =
                chunk.iter().map(|id| SqlValue::Text(id.clone())).collect();
            if let Some((at, id)) = before {
                where_sql.push_str(" AND (at < ? OR (at = ? AND id < ?))");
                values.push(SqlValue::Integer(at));
                values.push(SqlValue::Integer(at));
                values.push(SqlValue::Integer(id));
            }
            let sql = format!(
                "SELECT id, task_id, run_id, kind, stage, state, percent, message, at
                   FROM task_event WHERE {where_sql}
                  ORDER BY at DESC, id DESC LIMIT ?"
            );
            values.push(SqlValue::Integer(limit as i64 + 1));
            let mut stmt = conn.prepare(&sql).map_err(db_err)?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(values), |row| {
                    Ok(hivecast_ipc::TaskEvent {
                        id: row.get(0)?,
                        task_id: row.get(1)?,
                        run_id: row.get(2)?,
                        kind: row.get(3)?,
                        stage: row.get(4)?,
                        state: row.get(5)?,
                        percent: row.get(6)?,
                        message: row.get(7)?,
                        at: row.get(8)?,
                    })
                })
                .map_err(db_err)?;
            for row in rows {
                events.push(row.map_err(db_err)?);
            }
        }
        // 跨分块合并后仍只保留最新的一页；多取 1 条判断是否还有更早。
        events.sort_by_key(|event| std::cmp::Reverse((event.at, event.id)));
        let has_more = events.len() > limit;
        events.truncate(limit);
        events.reverse();
        Ok(hivecast_ipc::TaskEventPage { events, has_more })
    }

    pub fn task_finish(
        &self,
        id: &str,
        state: &str,
        message: Option<&str>,
        finished_at: i64,
    ) -> Result<(), AppError> {
        self.lock()
            .execute(
                "UPDATE task_history SET
                    state = ?2,
                    percent = CASE WHEN ?2 = 'done' THEN 100 ELSE percent END,
                    stage = CASE WHEN ?2 = 'done' THEN 'done' ELSE stage END,
                    message = ?3, finished_at = ?4
                  WHERE id = ?1",
                params![id, state, message, finished_at],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// 清理任务中心的历史数据：先按时间淘汰，再压到条数上限。
    ///
    /// 运行中/排队中的任务永远保留；`keep_recent` 保证长期不录制时最近
    /// 几百条记录不会因为一刀切的时间线而全部消失。事件不设独立时间策略，
    /// 只跟随已保留的任务，另外再加一道总条数上限，防止单任务事件异常膨胀。
    pub fn prune_task_data(
        &self,
        cutoff: i64,
        keep_recent: usize,
        max_history: usize,
        max_events: usize,
    ) -> Result<PrunedTaskData, AppError> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(db_err)?;
        let keep_recent = keep_recent.max(1) as i64;
        let max_history = max_history.max(1) as i64;
        let max_events = max_events.max(1) as i64;
        let mut pruned = PrunedTaskData::default();

        pruned.history += tx
            .execute(
                "DELETE FROM task_history
                  WHERE state NOT IN ('running', 'queued')
                    AND COALESCE(finished_at, started_at) < ?1
                    AND id NOT IN (
                        SELECT id FROM task_history
                         WHERE state NOT IN ('running', 'queued')
                         ORDER BY COALESCE(finished_at, started_at) DESC, id DESC
                         LIMIT ?2
                    )",
                params![cutoff, keep_recent],
            )
            .map_err(db_err)?;
        pruned.history += tx
            .execute(
                "DELETE FROM task_history
                  WHERE state NOT IN ('running', 'queued')
                    AND id NOT IN (
                        SELECT id FROM task_history
                         WHERE state NOT IN ('running', 'queued')
                         ORDER BY COALESCE(finished_at, started_at) DESC, id DESC
                         LIMIT ?1
                    )",
                params![max_history],
            )
            .map_err(db_err)?;
        pruned.events += tx
            .execute(
                // phase:* 是 Run 的阶段事件（缩略图、收尾等），按设计只挂在
                // run_id 上、不写 task_history；不能当成孤儿事件删掉，否则每次
                // 收尾都会把刚写入的时间线清空。真正孤儿的 phase 事件由下面的
                // 时间窗口兜底。
                "DELETE FROM task_event
                  WHERE task_id NOT IN (SELECT id FROM task_history)
                    AND task_id NOT LIKE 'phase:%'",
                [],
            )
            .map_err(db_err)?;
        pruned.events += tx
            .execute("DELETE FROM task_event WHERE at < ?1", params![cutoff])
            .map_err(db_err)?;
        pruned.events += tx
            .execute(
                "DELETE FROM task_event
                  WHERE id NOT IN (
                      SELECT id FROM task_event ORDER BY at DESC, id DESC LIMIT ?1
                  )",
                params![max_events],
            )
            .map_err(db_err)?;
        tx.commit().map_err(db_err)?;
        Ok(pruned)
    }

    pub fn task_history(&self, limit: usize) -> Result<Vec<TaskRecord>, AppError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, kind, title, detail, state, percent, stage, message,
                        session_id, room_id, started_at, queued_at, started_running_at,
                        finished_at
                   FROM task_history WHERE state NOT IN ('running', 'queued')
                  ORDER BY started_at DESC LIMIT ?1",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![limit.clamp(1, 2_000) as i64], map_task)
            .map_err(db_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_err)
    }

    /// 启动时把上次没来得及收尾的任务标成 interrupted；它们不会再继续跑。
    ///
    /// 排队中的任务也必须一起处理：它们已经不可能被本轮进程执行，但留在
    /// queued 会被 `task_history` 的过滤条件排除，Run 会被误判为「完成」。
    pub fn mark_tasks_interrupted(&self) -> Result<usize, AppError> {
        let changed = self
            .lock()
            .execute(
                "UPDATE task_history SET state = 'interrupted',
                    message = COALESCE(message, '应用上次退出时任务未完成'),
                    finished_at = ?1
                  WHERE state IN ('running', 'queued')",
                params![now_ts()],
            )
            .map_err(db_err)?;
        Ok(changed)
    }

    pub fn media_files(&self, session_id: &str) -> Result<Vec<MediaFile>, AppError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT session_id, path, role, size, format, state, created_at, deleted_at
                   FROM media_file WHERE session_id = ?1
                  ORDER BY CASE role WHEN 'main' THEN 0 WHEN 'source' THEN 1 ELSE 2 END,
                           created_at ASC, path ASC",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![session_id], |row| {
                let path: String = row.get(1)?;
                let stored: String = row.get(5)?;
                let exists = std::path::Path::new(&path).is_file();
                let state = if stored == "deleted" {
                    "deleted".to_string()
                } else if exists {
                    "present".to_string()
                } else {
                    "missing".to_string()
                };
                Ok(MediaFile {
                    session_id: row.get(0)?,
                    size: row.get(3)?,
                    format: row.get(4)?,
                    path,
                    role: row.get(2)?,
                    state,
                    exists,
                    created_at: row.get(6)?,
                    deleted_at: row.get(7)?,
                })
            })
            .map_err(db_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_err)
    }

    /// 老库回填：之前只记了 file_path/source_path，启动时补进登记表。
    pub fn backfill_media_files(&self) -> Result<usize, AppError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT id, file_path, source_path, source_deleted FROM record_session
                  WHERE (file_path IS NOT NULL AND file_path <> '')
                     OR (source_path IS NOT NULL AND source_path <> '')",
            )
            .map_err(db_err)?;
        let rows: Vec<(String, Option<String>, Option<String>, i64)> = stmt
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .map_err(db_err)?
            .filter_map(Result::ok)
            .collect();
        drop(stmt);
        let mut count = 0;
        for (id, main, source, deleted) in rows {
            let before: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM media_file WHERE session_id = ?1",
                    params![id],
                    |row| row.get(0),
                )
                .map_err(db_err)?;
            sync_media_files_on(&conn, &id, main.as_deref(), source.as_deref(), deleted != 0);
            if before == 0 {
                count += 1;
            }
        }
        Ok(count)
    }

    /// 早期分割录制只在后续片段写 group_id，首片段为空。任务中心用
    /// COALESCE(group_id, id) 已经能正确分组，但补上首片段自身 id 后，
    /// 所有按 group_id 精确查询的路径也能覆盖旧数据。
    ///
    /// 只处理「已有片段明确指向它」的行，不猜测哪些 session 属于同一场。
    pub fn backfill_session_groups(&self) -> Result<usize, AppError> {
        self.lock()
            .execute(
                "UPDATE record_session
                    SET group_id = id
                  WHERE group_id IS NULL
                    AND EXISTS (
                        SELECT 1 FROM record_session AS clip
                         WHERE clip.group_id = record_session.id
                           AND clip.id <> record_session.id
                    )",
                [],
            )
            .map_err(db_err)
    }

    /// 某场次待恢复的分割清单：片段 id 或同组任意片段上的清单都算。
    pub fn split_manifest_for(&self, session_id: &str) -> Result<Option<String>, AppError> {
        let conn = self.lock();
        conn.query_row(
            "SELECT split_manifest FROM record_session
              WHERE split_manifest IS NOT NULL AND split_manifest <> ''
                AND (id = ?1 OR COALESCE(group_id, id) = COALESCE(
                    (SELECT COALESCE(group_id, id) FROM record_session WHERE id = ?1), ?1))
              ORDER BY started_at ASC LIMIT 1",
            params![session_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_err)
    }
}

fn map_task(row: &Row<'_>) -> rusqlite::Result<TaskRecord> {
    Ok(TaskRecord {
        id: row.get(0)?,
        kind: row.get(1)?,
        title: row.get(2)?,
        detail: row.get(3)?,
        state: row.get(4)?,
        percent: row.get(5)?,
        stage: row.get(6)?,
        message: row.get(7)?,
        session_id: row.get(8)?,
        room_id: row.get(9)?,
        started_at: row.get(10)?,
        queued_at: row.get(11)?,
        started_running_at: row.get(12)?,
        finished_at: row.get(13)?,
    })
}

fn file_format(path: &str) -> Option<String> {
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
}

fn upsert_media_file(
    conn: &Connection,
    session_id: &str,
    path: &str,
    role: &str,
    state: &str,
    now: i64,
) {
    let size = std::fs::metadata(path)
        .map(|meta| meta.len() as i64)
        .unwrap_or(0);
    let format = file_format(path);
    let deleted_at = (state == "deleted").then_some(now);
    if let Err(err) = conn.execute(
        "INSERT INTO media_file (session_id, path, role, size, format, state, created_at, deleted_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(session_id, path) DO UPDATE SET
            role = excluded.role,
            size = CASE WHEN excluded.size > 0 THEN excluded.size ELSE media_file.size END,
            format = COALESCE(excluded.format, media_file.format),
            state = CASE
                WHEN excluded.state = 'deleted' OR media_file.state = 'deleted' THEN 'deleted'
                ELSE 'present'
            END,
            deleted_at = CASE
                WHEN excluded.state = 'deleted' THEN excluded.deleted_at
                WHEN media_file.state = 'deleted' THEN media_file.deleted_at
                ELSE NULL
            END",
        params![session_id, path, role, size, format, state, now, deleted_at],
    ) {
        tracing::warn!(session_id, path, error = %err, "登记媒体文件失败");
    }
}

fn sync_media_files_on(
    conn: &Connection,
    session_id: &str,
    main: Option<&str>,
    source: Option<&str>,
    source_deleted: bool,
) {
    let now = now_ts();
    // 旧主文件可能已经变成源封装：先降级，随后 source upsert 会把它提回 source。
    if let Some(main) = main.filter(|path| !path.is_empty()) {
        if let Err(err) = conn.execute(
            "UPDATE media_file SET role = 'derived'
              WHERE session_id = ?1 AND role = 'main' AND path <> ?2",
            params![session_id, main],
        ) {
            tracing::warn!(session_id, error = %err, "降级旧主文件失败");
        }
        upsert_media_file(conn, session_id, main, "main", "present", now);
    }
    if let Some(source) = source.filter(|path| !path.is_empty() && Some(*path) != main) {
        let state = if source_deleted { "deleted" } else { "present" };
        upsert_media_file(conn, session_id, source, "source", state, now);
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

    /// 只更新账号展示信息与状态，不碰 cookie。
    ///
    /// 复验结果可能来自旧 cookie：用户在这期间重新登录/替换了 cookie 时，
    /// 不能再把整行写回去，否则新 cookie 会被旧值覆盖。
    pub fn update_credential_profile(
        &self,
        platform: &str,
        status: &str,
        account_name: &str,
        avatar: Option<&str>,
        updated_at: i64,
    ) -> Result<bool, AppError> {
        let changed = self
            .lock()
            .execute(
                "UPDATE credential SET status = ?2, account_name = ?3, avatar = ?4, updated_at = ?5
                  WHERE platform = ?1",
                params![platform, status, account_name, avatar, updated_at],
            )
            .map_err(db_err)?;
        Ok(changed > 0)
    }

    pub fn delete_credential(&self, platform: &str) -> Result<(), AppError> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM credential WHERE platform = ?1",
            params![platform],
        )
        .map_err(db_err)?;
        conn.execute(
            "DELETE FROM credential_refresh WHERE platform = ?1",
            params![platform],
        )
        .map_err(db_err)?;
        conn.execute(
            "DELETE FROM credential_secret WHERE platform = ?1",
            params![platform],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// 读取平台长期凭证（LTP0 / 设备号）；解密失败时当作没有，避免拿坏值去换票。
    pub fn credential_secret(
        &self,
        platform: &str,
    ) -> Result<Option<CredentialSecretRow>, AppError> {
        let raw = self
            .lock()
            .query_row(
                "SELECT platform, refresh_token, device_id, updated_at
                   FROM credential_secret WHERE platform = ?1",
                params![platform],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(db_err)?;
        let Some((platform, refresh_token, device_id, updated_at)) = raw else {
            return Ok(None);
        };
        let refresh_token = match self.secret.open(&refresh_token) {
            Some(plain) => plain,
            None => {
                tracing::warn!(platform = %platform, "长期凭证解密失败，已跳过");
                return Ok(None);
            }
        };
        let device_id = if device_id.is_empty() {
            String::new()
        } else {
            match self.secret.open(&device_id) {
                Some(plain) => plain,
                None => {
                    tracing::warn!(platform = %platform, "设备号解密失败，已忽略");
                    String::new()
                }
            }
        };
        Ok(Some(CredentialSecretRow {
            platform,
            refresh_token,
            device_id,
            updated_at,
        }))
    }

    pub fn save_credential_secret(&self, row: &CredentialSecretRow) -> Result<(), AppError> {
        let seal = |value: &str| -> Result<String, AppError> {
            if value.is_empty() {
                Ok(String::new())
            } else {
                self.secret.seal(value)
            }
        };
        let refresh_token = seal(&row.refresh_token)?;
        let device_id = seal(&row.device_id)?;
        self.lock()
            .execute(
                "INSERT INTO credential_secret (platform, refresh_token, device_id, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (platform) DO UPDATE SET
                    refresh_token = excluded.refresh_token,
                    device_id = excluded.device_id,
                    updated_at = excluded.updated_at",
                params![row.platform, refresh_token, device_id, row.updated_at],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn clear_credential_secret(&self, platform: &str) -> Result<(), AppError> {
        self.lock()
            .execute(
                "DELETE FROM credential_secret WHERE platform = ?1",
                params![platform],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// 最近一次续期检查结果；没有记录时返回 None（前端按 never 展示）。
    pub fn credential_refresh(
        &self,
        platform: &str,
    ) -> Result<Option<CredentialRefreshRow>, AppError> {
        self.lock()
            .query_row(
                "SELECT platform, checked_at, outcome, message, key_stale, last_refreshed_at
                   FROM credential_refresh WHERE platform = ?1",
                params![platform],
                |row| {
                    Ok(CredentialRefreshRow {
                        platform: row.get(0)?,
                        checked_at: row.get(1)?,
                        outcome: row.get(2)?,
                        message: row.get(3)?,
                        key_stale: row.get(4)?,
                        last_refreshed_at: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(db_err)
    }

    /// 写入检查结果。last_refreshed_at 只在真正轮换时前进，普通检查不会把它清零。
    pub fn save_credential_refresh(&self, row: &CredentialRefreshRow) -> Result<(), AppError> {
        self.lock()
            .execute(
                "INSERT INTO credential_refresh
                    (platform, checked_at, outcome, message, key_stale, last_refreshed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (platform) DO UPDATE SET
                    checked_at = excluded.checked_at,
                    outcome = excluded.outcome,
                    message = excluded.message,
                    key_stale = excluded.key_stale,
                    last_refreshed_at = CASE
                        WHEN excluded.last_refreshed_at > 0 THEN excluded.last_refreshed_at
                        ELSE credential_refresh.last_refreshed_at
                    END",
                params![
                    row.platform,
                    row.checked_at,
                    row.outcome,
                    row.message,
                    row.key_stale as i64,
                    row.last_refreshed_at,
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn clear_credential_refresh(&self, platform: &str) -> Result<(), AppError> {
        self.lock()
            .execute(
                "DELETE FROM credential_refresh WHERE platform = ?1",
                params![platform],
            )
            .map_err(db_err)?;
        Ok(())
    }
}

/// 平台长期凭证（目前只有斗鱼的 LTP0 与设备号）；字段在库里都是密文。
#[derive(Debug, Clone)]
pub struct CredentialSecretRow {
    pub platform: String,
    pub refresh_token: String,
    pub device_id: String,
    pub updated_at: i64,
}

/// Cookie 续期的检查结果（只用于持久化，不直接进 IPC）。
#[derive(Debug, Clone)]
pub struct CredentialRefreshRow {
    pub platform: String,
    pub checked_at: i64,
    pub outcome: String,
    pub message: Option<String>,
    pub key_stale: bool,
    pub last_refreshed_at: i64,
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
    for (name, ddl) in [
        (
            "group_id",
            "ALTER TABLE record_session ADD COLUMN group_id TEXT",
        ),
        (
            "clip_index",
            "ALTER TABLE record_session ADD COLUMN clip_index INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "split_reason",
            "ALTER TABLE record_session ADD COLUMN split_reason TEXT",
        ),
        (
            "split_manifest",
            "ALTER TABLE record_session ADD COLUMN split_manifest TEXT",
        ),
        (
            "source_path",
            "ALTER TABLE record_session ADD COLUMN source_path TEXT",
        ),
        (
            "post_mode",
            "ALTER TABLE record_session ADD COLUMN post_mode TEXT NOT NULL DEFAULT 'source'",
        ),
        (
            "post_state",
            "ALTER TABLE record_session ADD COLUMN post_state TEXT NOT NULL DEFAULT 'done'",
        ),
        (
            "source_deleted",
            "ALTER TABLE record_session ADD COLUMN source_deleted INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "post_error",
            "ALTER TABLE record_session ADD COLUMN post_error TEXT",
        ),
        (
            "quality_history",
            "ALTER TABLE record_session ADD COLUMN quality_history TEXT",
        ),
    ] {
        if !columns.iter().any(|column| column == name) {
            conn.execute(ddl, []).map_err(db_err)?;
            tracing::info!(column = name, "数据库迁移：新增列");
        }
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

    let mut stmt = conn
        .prepare("PRAGMA table_info(task_history)")
        .map_err(db_err)?;
    let task_columns: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(db_err)?
        .filter_map(Result::ok)
        .collect();
    for (name, ddl) in [
        (
            "queued_at",
            "ALTER TABLE task_history ADD COLUMN queued_at INTEGER",
        ),
        (
            "started_running_at",
            "ALTER TABLE task_history ADD COLUMN started_running_at INTEGER",
        ),
    ] {
        if !task_columns.iter().any(|column| column == name) {
            conn.execute(ddl, []).map_err(db_err)?;
            tracing::info!(column = name, "数据库迁移：新增列");
        }
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
    if !room_columns.iter().any(|name| name == "platform_uid") {
        conn.execute("ALTER TABLE room ADD COLUMN platform_uid TEXT", [])
            .map_err(db_err)?;
        tracing::info!(column = "room.platform_uid", "数据库迁移：新增列");
    }
    if !room_columns.iter().any(|name| name == "batch_follow") {
        conn.execute(
            "ALTER TABLE room ADD COLUMN batch_follow INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .map_err(db_err)?;
        tracing::info!(column = "room.batch_follow", "数据库迁移：新增列");
    }
    if !room_columns.iter().any(|name| name == "internal_room_id") {
        conn.execute("ALTER TABLE room ADD COLUMN internal_room_id TEXT", [])
            .map_err(db_err)?;
        tracing::info!(column = "room.internal_room_id", "数据库迁移：新增列");
    }
    let mut backfill_last_session = false;
    for (name, ddl) in [
        (
            "last_session_id",
            "ALTER TABLE room ADD COLUMN last_session_id TEXT",
        ),
        (
            "last_session_started_at",
            "ALTER TABLE room ADD COLUMN last_session_started_at INTEGER",
        ),
        (
            "last_session_duration_ms",
            "ALTER TABLE room ADD COLUMN last_session_duration_ms INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "last_session_status",
            "ALTER TABLE room ADD COLUMN last_session_status TEXT",
        ),
        (
            "last_session_reason",
            "ALTER TABLE room ADD COLUMN last_session_reason TEXT",
        ),
        (
            "last_session_file_size",
            "ALTER TABLE room ADD COLUMN last_session_file_size INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "last_session_quality",
            "ALTER TABLE room ADD COLUMN last_session_quality TEXT",
        ),
        (
            "note",
            "ALTER TABLE room ADD COLUMN note TEXT NOT NULL DEFAULT ''",
        ),
        (
            "tags",
            "ALTER TABLE room ADD COLUMN tags TEXT NOT NULL DEFAULT ''",
        ),
        (
            "title_include",
            "ALTER TABLE room ADD COLUMN title_include TEXT",
        ),
        (
            "title_exclude",
            "ALTER TABLE room ADD COLUMN title_exclude TEXT",
        ),
        (
            "split_max_duration_sec",
            "ALTER TABLE room ADD COLUMN split_max_duration_sec INTEGER",
        ),
        (
            "split_max_size_mb",
            "ALTER TABLE room ADD COLUMN split_max_size_mb INTEGER",
        ),
        (
            "notify_live",
            "ALTER TABLE room ADD COLUMN notify_live INTEGER",
        ),
        (
            "notify_error",
            "ALTER TABLE room ADD COLUMN notify_error INTEGER",
        ),
    ] {
        if !room_columns.iter().any(|column| column == name) {
            conn.execute(ddl, []).map_err(db_err)?;
            backfill_last_session = true;
            tracing::info!(column = name, "数据库迁移：新增列");
        }
    }
    // 老库补列后必须回填一次，否则升级后监控列表的「最近录制」会全是空，
    // 直到该房间再次产生 session。批量 UPDATE 走 idx_record_session_room。
    if backfill_last_session {
        conn.execute(
            "UPDATE room SET
                last_session_id = (
                    SELECT id FROM record_session
                    WHERE room_id = room.id
                    ORDER BY started_at DESC, id DESC LIMIT 1
                ),
                last_session_started_at = (
                    SELECT started_at FROM record_session
                    WHERE room_id = room.id
                    ORDER BY started_at DESC, id DESC LIMIT 1
                ),
                last_session_duration_ms = COALESCE((
                    SELECT duration_ms FROM record_session
                    WHERE room_id = room.id
                    ORDER BY started_at DESC, id DESC LIMIT 1
                ), 0),
                last_session_status = (
                    SELECT status FROM record_session
                    WHERE room_id = room.id
                    ORDER BY started_at DESC, id DESC LIMIT 1
                ),
                last_session_reason = (
                    SELECT reason FROM record_session
                    WHERE room_id = room.id
                    ORDER BY started_at DESC, id DESC LIMIT 1
                ),
                last_session_file_size = COALESCE((
                    SELECT file_size FROM record_session
                    WHERE room_id = room.id
                    ORDER BY started_at DESC, id DESC LIMIT 1
                ), 0),
                last_session_quality = (
                    SELECT quality FROM record_session
                    WHERE room_id = room.id
                    ORDER BY started_at DESC, id DESC LIMIT 1
                )",
            [],
        )
        .map_err(db_err)?;
        tracing::info!("数据库迁移：回填 room 最近录制摘要");
    }
    Ok(())
}

/// 把用户输入转成 LIKE 模式，转义 `%`、`_` 和反斜杠本身。
fn like_pattern(query: &str) -> String {
    let mut out = String::with_capacity(query.len() + 2);
    out.push('%');
    for ch in query.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '%' => out.push_str("\\%"),
            '_' => out.push_str("\\_"),
            other => out.push(other),
        }
    }
    out.push('%');
    out
}

/// 生成 `column IN ('a','b')` 片段；阶段/状态来自静态表，值里只需处理单引号。
fn enum_in_clause(column: &str, values: &[&str]) -> Option<String> {
    if values.is_empty() {
        return None;
    }
    let list = values
        .iter()
        .map(|value| format!("'{}'", value.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");
    Some(format!("{column} IN ({list})"))
}

fn db_err(err: rusqlite::Error) -> AppError {
    AppError::Db(err.to_string())
}

/// 房间表的最近一场录制摘要：session 写入/收尾/删除后调用，监控列表只读 room。
fn refresh_room_last_session(conn: &Connection, room_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE room SET
            last_session_id = (
                SELECT id FROM record_session
                WHERE room_id = ?1
                ORDER BY started_at DESC, id DESC LIMIT 1
            ),
            last_session_started_at = (
                SELECT started_at FROM record_session
                WHERE room_id = ?1
                ORDER BY started_at DESC, id DESC LIMIT 1
            ),
            last_session_duration_ms = COALESCE((
                SELECT duration_ms FROM record_session
                WHERE room_id = ?1
                ORDER BY started_at DESC, id DESC LIMIT 1
            ), 0),
            last_session_status = (
                SELECT status FROM record_session
                WHERE room_id = ?1
                ORDER BY started_at DESC, id DESC LIMIT 1
            ),
            last_session_reason = (
                SELECT reason FROM record_session
                WHERE room_id = ?1
                ORDER BY started_at DESC, id DESC LIMIT 1
            ),
            last_session_file_size = COALESCE((
                SELECT file_size FROM record_session
                WHERE room_id = ?1
                ORDER BY started_at DESC, id DESC LIMIT 1
            ), 0),
            last_session_quality = (
                SELECT quality FROM record_session
                WHERE room_id = ?1
                ORDER BY started_at DESC, id DESC LIMIT 1
            )
          WHERE id = ?1",
        params![room_id],
    )?;
    Ok(())
}

pub fn now_ts() -> i64 {
    crate::clock::now_secs()
}

const ROOM_SELECT: &str = "SELECT
    id, platform, room_key, canonical_url, title, host_name, avatar_url, cover_url,
    area_name, is_live, started_at, online, auto_record, monitor_paused,
    record_qn, view_qn, record_line, last_checked_at,
    last_session_id, last_session_started_at, last_session_duration_ms,
    last_session_status, last_session_reason, last_session_file_size,
    note, tags, title_include, title_exclude,
    split_max_duration_sec, split_max_size_mb, notify_live, notify_error,
    created_at, updated_at, last_session_quality, platform_uid, batch_follow, internal_room_id,
    removed_at
  FROM room";

const SESSION_SELECT: &str = "SELECT
    id, room_id, started_at, ended_at, status, reason, file_path, file_size,
    duration_ms, quality, line, error, danmaku_path, danmaku_count,
    source_path, post_mode, post_state, source_deleted, post_error,
    quality_history, group_id, clip_index
  FROM record_session";

/// 平台内部房间号：抖音 follow/feed 只认 19 位 room_id，不认页面上的 web_rid。
/// 其它平台的 extra.roomId 与 room_key 等价，存下来也不会被批量逻辑使用。
pub(crate) fn internal_room_id_of(info: &RoomInfo) -> Option<String> {
    let value = info.extra.get("roomId")?;
    let text = match value {
        serde_json::Value::String(text) => text.trim().to_string(),
        serde_json::Value::Number(number) => number.to_string(),
        _ => return None,
    };
    (!text.is_empty()).then_some(text)
}

/// 从平台 extra 里取账号 UID；只有 B 站会填，其它平台为 None。
pub(crate) fn platform_uid_of(info: &RoomInfo) -> Option<String> {
    let value = info.extra.get("uid")?;
    if let Some(uid) = value.as_i64() {
        return (uid > 0).then(|| uid.to_string());
    }
    // 批量导入把 uid 作为字符串放进 extra；如果只认数字，
    // 导入的房间会没有 platform_uid，首轮进不了批量状态接口。
    let text = value.as_str()?.trim();
    let uid = text.parse::<i64>().ok()?;
    (uid > 0).then(|| text.to_string())
}

fn map_douyin_follow(row: &Row<'_>) -> rusqlite::Result<DouyinFollow> {
    Ok(DouyinFollow {
        uid: row.get(0)?,
        sec_uid: row.get(1)?,
        uname: row.get(2)?,
        avatar_url: row.get(3)?,
        web_rid: row.get(4)?,
        internal_room_id: row.get(5)?,
        is_live: row.get::<_, i64>(6)? != 0,
        title: row.get(7)?,
        cover_url: row.get(8)?,
        area_name: row.get(9)?,
        online: row
            .get::<_, Option<i64>>(10)?
            .map(|value| value.max(0) as u64),
        started_at: row.get(11)?,
        in_pool: true,
        monitor: row.get::<_, i64>(12)? != 0,
        auto_record: row.get::<_, i64>(13)? != 0,
        promoted_room_id: row.get(14)?,
    })
}

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
        last_session_id: row.get(18)?,
        last_session_started_at: row.get(19)?,
        last_session_duration_ms: row.get(20)?,
        last_session_status: row.get(21)?,
        last_session_reason: row.get(22)?,
        last_session_file_size: row.get(23)?,
        note: row.get(24)?,
        tags: row.get(25)?,
        title_include: row.get(26)?,
        title_exclude: row.get(27)?,
        split_max_duration_sec: row.get(28)?,
        split_max_size_mb: row.get(29)?,
        notify_live: row.get::<_, Option<i64>>(30)?.map(|value| value != 0),
        notify_error: row.get::<_, Option<i64>>(31)?.map(|value| value != 0),
        created_at: row.get(32)?,
        updated_at: row.get(33)?,
        last_session_quality: row.get(34)?,
        platform_uid: row.get(35)?,
        batch_follow: row.get::<_, i64>(36)? != 0,
        internal_room_id: row.get(37)?,
        removed_at: row.get(38)?,
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
        source_path: row.get(14)?,
        post_mode: row.get(15)?,
        post_state: row.get(16)?,
        source_deleted: row.get::<_, i64>(17)? != 0,
        post_error: row.get(18)?,
        quality_history: row
            .get::<_, Option<String>>(19)?
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default(),
        group_id: row.get(20)?,
        clip_index: row.get(21)?,
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
                        ('s-a3', 'rm-a', 4000, 'recording', '/tmp/a3.flv', 999, 0);
                 UPDATE record_session SET quality = '2K · FLV · H.264' WHERE id = 's-a3';",
            )
            .expect("播种媒体库失败");
    }

    #[test]
    fn room_last_session_summary_refreshes() {
        let (db, _path) = temp_db("room-last-session");
        seed_media(&db);
        {
            let conn = db.lock();
            refresh_room_last_session(&conn, "rm-a").expect("刷新 rm-a 失败");
            refresh_room_last_session(&conn, "rm-b").expect("刷新 rm-b 失败");
        }
        let rooms = db.list_rooms().expect("读取房间失败");
        let a = rooms
            .iter()
            .find(|room| room.id == "rm-a")
            .expect("rm-a 缺失");
        assert_eq!(a.last_session_id.as_deref(), Some("s-a3"));
        assert_eq!(a.last_session_started_at, Some(4000));
        assert_eq!(a.last_session_duration_ms, 0);
        assert_eq!(a.last_session_quality.as_deref(), Some("2K · FLV · H.264"));
        let b = rooms
            .iter()
            .find(|room| room.id == "rm-b")
            .expect("rm-b 缺失");
        assert_eq!(b.last_session_id.as_deref(), Some("s-b1"));
        assert_eq!(b.last_session_started_at, Some(2000));
        assert_eq!(b.last_session_duration_ms, 500);
    }

    #[test]
    fn merging_alias_room_keeps_history_and_preferred_flags() {
        let (db, _path) = temp_db("merge-room");
        db.lock()
            .execute_batch(
                "INSERT INTO room
                    (id, platform, room_key, platform_uid, batch_follow, internal_room_id, canonical_url, host_name, title, note, tags, auto_record, monitor_paused,
                     record_qn, view_qn, record_line, title_include, title_exclude, split_max_duration_sec, split_max_size_mb, notify_live, notify_error,
                     created_at, updated_at)
                 VALUES
                    ('keep', 'bilibili', '6', NULL, 0, NULL, 'https://live.bilibili.com/6', '主播', '短号', '', 'LOL', 0, 1, 0, 0, NULL, NULL, NULL, NULL, NULL, NULL, NULL, 0, 0),
                    ('drop', 'bilibili', '7734200', '42', 1, '7708331906513767187', 'https://live.bilibili.com/7734200', '主播', '真号', '备注', '', 1, 0,
                     15000, 250, 'line-b', '仅标题', '排除', 3600, 2048, 1, 0, 0, 0);
                 INSERT INTO record_session (id, room_id, started_at, status, file_path, file_size, duration_ms, quality)
                 VALUES ('s-drop', 'drop', 100, 'finished', '/tmp/drop.flv', 10, 1000, '原画 · FLV · H.264');",
            )
            .expect("播种房间失败");

        db.merge_room("keep", "drop").expect("合并失败");
        db.rekey_room("keep", "7734200", "https://live.bilibili.com/7734200")
            .expect("改键失败");

        let room = db
            .get_room("keep")
            .expect("读房间失败")
            .expect("保留行存在");
        assert_eq!(room.room_key, "7734200");
        assert_eq!(room.note, "备注", "保留行备注为空时继承被合并行");
        assert_eq!(room.tags, "LOL", "保留行已有标签不被覆盖");
        assert!(room.auto_record, "两边任一开启自动录制就保持开启");
        assert!(!room.monitor_paused, "两边任一在监控就保持监控");
        assert_eq!(room.last_session_id.as_deref(), Some("s-drop"));
        assert_eq!(
            room.last_session_quality.as_deref(),
            Some("原画 · FLV · H.264")
        );
        assert_eq!(room.platform_uid.as_deref(), Some("42"));
        assert!(room.batch_follow, "关注来源在合并后仍保留");
        assert_eq!(
            room.internal_room_id.as_deref(),
            Some("7708331906513767187"),
            "合并不能丢掉批量状态所需的内部 room_id"
        );
        assert_eq!(room.record_qn, 15000, "保留行未设置画质时继承被合并行");
        assert_eq!(room.view_qn, 250);
        assert_eq!(room.record_line.as_deref(), Some("line-b"));
        assert_eq!(room.title_include.as_deref(), Some("仅标题"));
        assert_eq!(room.title_exclude.as_deref(), Some("排除"));
        assert_eq!(room.split_max_duration_sec, Some(3600));
        assert_eq!(room.split_max_size_mb, Some(2048));
        assert_eq!(room.notify_live, Some(true));
        assert_eq!(room.notify_error, Some(false));
        assert!(db.get_room("drop").expect("查询失败").is_none());
    }

    #[test]
    fn find_room_by_uid_prefers_active_row() {
        let (db, _path) = temp_db("find-room-by-uid");
        db.lock()
            .execute_batch(
                "INSERT INTO room
                    (id, platform, room_key, platform_uid, canonical_url, host_name, title, removed_at, created_at, updated_at)
                 VALUES
                    ('removed', 'bilibili', '6', '42', 'https://live.bilibili.com/6', '主播', '已删', 100, 0, 0),
                    ('active', 'bilibili', '7734200', '42', 'https://live.bilibili.com/7734200', '主播', '在库', NULL, 1, 1);",
            )
            .expect("播种房间失败");
        let room = db
            .find_room_by_uid("bilibili", "42")
            .expect("查询失败")
            .expect("应命中");
        assert_eq!(room.id, "active", "同 UID 多条时优先仍在库的房间");
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
    fn split_clips_form_one_session_in_library() {
        let (db, _path) = temp_db("clips");
        seed_media(&db);
        // 把 a2 挂到 a1 下，模拟一场直播切成两段。
        db.lock()
            .execute(
                "UPDATE record_session
                    SET group_id = 's-a1', clip_index = 1, split_reason = 'duration'
                  WHERE id = 's-a2'",
                [],
            )
            .expect("标记片段失败");

        let (items, total) = db.media_list(&MediaFilter::default()).expect("媒体库失败");
        assert_eq!(total, 4, "分页仍按片段计数");
        assert_eq!(items.len(), 4);

        let clips = db.media_group_items("s-a1").expect("整组查询失败");
        assert_eq!(clips.len(), 2);
        assert_eq!(clips[0].id, "s-a1");
        assert_eq!(clips[1].split_reason.as_deref(), Some("duration"));
        assert_eq!(clips.iter().map(|one| one.clip_count).max(), Some(2));

        let counts = db
            .media_platform_counts(&MediaFilter::default())
            .expect("平台计数失败");
        let bilibili = counts
            .iter()
            .find(|one| one.platform == "bilibili")
            .expect("B 站计数");
        // 平台筛选条按“场”计数，和媒体库卡片一致：a1+a2 只算一场。
        assert_eq!(bilibili.count, 2);

        // create_clip 写入的片段同样能被整组查询与去重计数看到。
        let mut clip = db
            .get_session("s-b1")
            .expect("读会话失败")
            .expect("会话存在");
        clip.id = "s-b2".into();
        clip.started_at = 2500;
        clip.ended_at = Some(2600);
        clip.duration_ms = 100;
        clip.file_size = 50;
        db.create_clip(&clip, "s-b1", 1, Some("size"))
            .expect("写入片段失败");
        assert_eq!(db.session_group_size("s-b1").expect("场次大小"), 2);
        assert_eq!(db.session_group_size("s-b2").expect("片段大小"), 2);
        assert_eq!(db.media_group_items("s-b1").expect("整组查询失败").len(), 2);
    }

    #[test]
    fn session_quality_history_roundtrips() {
        let (db, _path) = temp_db("quality-history");
        seed_media(&db);
        let history = vec![
            QualitySegment {
                at: 1000,
                qn: 250,
                quality: "超清".into(),
                line: "线路1".into(),
            },
            QualitySegment {
                at: 1600,
                qn: 150,
                quality: "高清".into(),
                line: "线路2".into(),
            },
        ];
        db.update_session_quality("s-a1", "高清", "线路2", &history)
            .expect("写画质历史失败");
        let session = db
            .get_session("s-a1")
            .expect("读会话失败")
            .expect("会话存在");
        assert_eq!(session.quality.as_deref(), Some("高清"));
        assert_eq!(session.line.as_deref(), Some("线路2"));
        assert_eq!(session.quality_history, history);
    }

    #[test]
    fn post_fields_roundtrip_and_source_flag_reaches_media_library() {
        let (db, db_path) = temp_db("post");
        seed_media(&db);
        let dir = db_path.parent().expect("库目录").join("media");
        std::fs::create_dir_all(&dir).expect("建媒体目录失败");
        let source = dir.join("live.flv");
        let derived = dir.join("live.mp4");
        std::fs::write(&source, b"source-bytes").expect("写源文件失败");
        std::fs::write(&derived, b"derived-bytes").expect("写派生文件失败");

        let mut session = db
            .get_session("s-a1")
            .expect("读会话失败")
            .expect("会话存在");
        session.id = "s-post".into();
        session.file_path = Some(derived.display().to_string());
        session.file_size = 13;
        session.source_path = Some(source.display().to_string());
        session.post_mode = "convert".into();
        session.post_state = "done".into();
        session.source_deleted = false;
        db.create_session(&session).expect("写入后处理会话失败");

        let loaded = db
            .get_session("s-post")
            .expect("读后处理会话失败")
            .expect("后处理会话存在");
        assert_eq!(
            loaded.source_path.as_deref(),
            Some(source.to_str().unwrap())
        );
        assert_eq!(loaded.post_mode, "convert");
        assert_eq!(loaded.post_state, "done");
        assert!(!loaded.source_deleted);

        // 手动转封装：主文件切到派生文件，源封装保留且不标记删除。
        db.update_session_post_file(
            "s-post",
            &derived.display().to_string(),
            13,
            Some(&source.display().to_string()),
            "convert",
            "done",
            false,
            None,
        )
        .expect("写入手动转封装结果失败");
        let switched = db
            .get_session("s-post")
            .expect("读手动转封装结果失败")
            .expect("手动转封装结果存在");
        assert_eq!(
            switched.file_path.as_deref(),
            Some(derived.to_str().unwrap())
        );
        assert_eq!(
            switched.source_path.as_deref(),
            Some(source.to_str().unwrap())
        );
        assert_eq!(switched.file_size, 13);
        assert_eq!(switched.post_mode, "convert");
        assert_eq!(switched.post_state, "done");
        assert!(!switched.source_deleted);
        assert_eq!(switched.post_error, None);

        let items = db.media_group_items("s-post").expect("整组查询失败");
        assert_eq!(items.len(), 1);
        assert!(items[0].has_source, "源文件存在且未标记删除时应可见");
        assert_eq!(items[0].post_mode, "convert");

        db.finish_session_with_post(
            "s-post",
            "finished",
            "stream_ended",
            1_100,
            &derived.display().to_string(),
            13,
            Some(&source.display().to_string()),
            "convert",
            "done",
            true,
            None,
            None,
            0,
            None,
        )
        .expect("提交后处理结果失败");
        let committed = db
            .get_session("s-post")
            .expect("读提交结果失败")
            .expect("提交结果存在");
        assert!(committed.source_deleted, "移入回收站后应标记源已删");
        assert_eq!(committed.status, "finished");
        assert_eq!(committed.duration_ms, 100_000);
        let items = db.media_group_items("s-post").expect("整组查询失败");
        assert!(!items[0].has_source, "源已删标记要同步到媒体库");
    }

    #[test]
    fn source_equal_to_main_is_not_reported_as_a_second_file() {
        let (db, db_path) = temp_db("sourcedup");
        seed_media(&db);
        let dir = db_path.parent().expect("库目录").join("media-dup");
        std::fs::create_dir_all(&dir).expect("建媒体目录失败");
        let main = dir.join("live.flv");
        std::fs::write(&main, b"one-file").expect("写源文件失败");
        let mut session = db
            .get_session("s-a1")
            .expect("读会话失败")
            .expect("会话存在");
        session.id = "s-dup".into();
        session.file_path = Some(main.display().to_string());
        session.source_path = Some(main.display().to_string());
        session.post_mode = "source".into();
        session.post_state = "done".into();
        session.source_deleted = false;
        db.create_session(&session).expect("写入会话失败");
        let items = db.media_group_items("s-dup").expect("整组查询失败");
        assert!(!items[0].has_source, "主文件与源相同时不应显示第二个文件");
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
        let removed = db.list_removed_rooms().expect("已移除列表失败");
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].room.id, "rm-a");
        assert!(removed[0].session_count > 0);
        assert!(removed[0].total_bytes > 0);

        db.restore_room("rm-a").expect("恢复房间失败");
        assert!(!db.room_removed("rm-a").expect("读标记失败"));
        assert_eq!(db.list_rooms().expect("恢复后列表失败").len(), 2);
        assert!(db
            .list_removed_rooms()
            .expect("恢复后已移除列表失败")
            .is_empty());
        db.delete_room("rm-a").expect("再次移除失败");

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
            stable_id: None,
            extra: serde_json::Value::Null,
        };
        db.insert_room(&existing, &info).expect("恢复房间失败");
        assert!(!db.room_removed("rm-a").expect("读标记失败"));
        assert_eq!(db.list_rooms().expect("列表失败").len(), 2);
    }

    #[test]
    fn platform_uid_accepts_numeric_and_string_forms() {
        let mut info = crate::platform::RoomInfo {
            host_name: String::new(),
            room_name: String::new(),
            avatar_url: None,
            cover_url: None,
            area_name: None,
            is_live: false,
            started_at: None,
            online: None,
            stable_id: None,
            extra: serde_json::json!({ "uid": "42" }),
        };
        assert_eq!(platform_uid_of(&info).as_deref(), Some("42"));
        info.extra = serde_json::json!({ "uid": 42 });
        assert_eq!(platform_uid_of(&info).as_deref(), Some("42"));
        info.extra = serde_json::json!({ "uid": " 43 " });
        assert_eq!(platform_uid_of(&info).as_deref(), Some("43"));
        info.extra = serde_json::json!({ "uid": "0" });
        assert_eq!(platform_uid_of(&info), None);
        info.extra = serde_json::json!({ "uid": "" });
        assert_eq!(platform_uid_of(&info), None);
    }

    #[test]
    fn insert_room_uses_string_uid_then_draft_fallback() {
        let (db, _path) = temp_db("insert-uid");
        seed_media(&db);
        let room = db.get_room("rm-a").expect("读失败").expect("房间缺失");
        let mut info = crate::platform::RoomInfo {
            host_name: "主播甲".into(),
            room_name: "房间甲".into(),
            avatar_url: None,
            cover_url: None,
            area_name: None,
            is_live: true,
            started_at: Some(5000),
            online: Some(1),
            stable_id: None,
            extra: serde_json::json!({ "uid": "123" }),
        };
        db.insert_room(&room, &info).expect("写入失败");
        let saved = db.get_room("rm-a").expect("读失败").expect("房间缺失");
        assert_eq!(saved.platform_uid.as_deref(), Some("123"));

        // 平台信息里没有 uid 时，至少要用调用方已经解析好的 draft uid。
        info.extra = serde_json::Value::Null;
        let mut fallback = saved;
        fallback.platform_uid = Some("456".into());
        db.insert_room(&fallback, &info).expect("写入失败");
        let saved = db.get_room("rm-a").expect("读失败").expect("房间缺失");
        assert_eq!(saved.platform_uid.as_deref(), Some("456"));
    }

    #[test]
    fn update_room_info_persists_uid_for_a_legacy_row() {
        let (db, _path) = temp_db("update-uid");
        seed_media(&db);
        db.exec_batch_for_test("UPDATE room SET platform_uid = NULL WHERE id = 'rm-a'")
            .expect("清空 UID 失败");
        assert_eq!(
            db.get_room("rm-a")
                .expect("读失败")
                .expect("房间缺失")
                .platform_uid,
            None
        );

        let info = crate::platform::RoomInfo {
            host_name: "主播甲".into(),
            room_name: "房间甲".into(),
            avatar_url: None,
            cover_url: None,
            area_name: None,
            is_live: true,
            started_at: Some(5000),
            online: Some(1),
            stable_id: None,
            extra: serde_json::json!({ "uid": 50329118 }),
        };
        db.update_room_info("rm-a", &info).expect("更新失败");
        assert_eq!(
            db.get_room("rm-a")
                .expect("读失败")
                .expect("房间缺失")
                .platform_uid
                .as_deref(),
            Some("50329118")
        );

        let mut no_uid = info;
        no_uid.extra = serde_json::json!({});
        db.update_room_info("rm-a", &no_uid)
            .expect("无 UID 更新失败");
        assert_eq!(
            db.get_room("rm-a")
                .expect("读失败")
                .expect("房间缺失")
                .platform_uid
                .as_deref(),
            Some("50329118"),
            "平台这次没给 UID 时不能把已有值清空"
        );
    }

    #[test]
    fn internal_room_id_is_written_from_platform_info_and_never_cleared() {
        let (db, _path) = temp_db("internal-room-id");
        seed_media(&db);
        let mut info = crate::platform::RoomInfo {
            host_name: "主播甲".into(),
            room_name: "房间甲".into(),
            avatar_url: None,
            cover_url: None,
            area_name: None,
            is_live: false,
            started_at: None,
            online: None,
            stable_id: None,
            extra: serde_json::json!({ "roomId": 7708331906513767187i64 }),
        };
        db.update_room_info("rm-a", &info).expect("更新失败");
        assert_eq!(
            db.get_room("rm-a")
                .expect("读失败")
                .expect("房间缺失")
                .internal_room_id
                .as_deref(),
            Some("7708331906513767187")
        );

        // 平台后续只返回普通房间字段时，不能把已经拿到的内部 room_id 清空。
        info.extra = serde_json::json!({});
        db.update_room_info("rm-a", &info)
            .expect("无内部 ID 更新失败");
        assert_eq!(
            db.get_room("rm-a")
                .expect("读失败")
                .expect("房间缺失")
                .internal_room_id
                .as_deref(),
            Some("7708331906513767187")
        );
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
    fn credential_secret_is_sealed_and_deleted_with_the_credential() {
        let (db, _path) = temp_db("secret");
        db.save_credential(&row("douyu", "acf_uid=1"))
            .expect("写凭据失败");
        db.save_credential_secret(&crate::db::CredentialSecretRow {
            platform: "douyu".into(),
            refresh_token: "LTP0-plain".into(),
            device_id: "dy-did-plain".into(),
            updated_at: 7,
        })
        .expect("写长期凭证失败");
        let raw: String = db
            .lock()
            .query_row(
                "SELECT refresh_token || '|' || device_id FROM credential_secret WHERE platform = 'douyu'",
                [],
                |r| r.get(0),
            )
            .expect("读原始长期凭证失败");
        assert!(crate::secret::Secret::is_sealed(
            raw.split('|').next().unwrap()
        ));
        assert!(!raw.contains("plain"));
        let secret = db
            .credential_secret("douyu")
            .expect("读长期凭证失败")
            .expect("长期凭证不存在");
        assert_eq!(secret.refresh_token, "LTP0-plain");
        assert_eq!(secret.device_id, "dy-did-plain");
        db.delete_credential("douyu").expect("删凭据失败");
        assert!(db.credential_secret("douyu").expect("读失败").is_none());
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
    fn profile_update_keeps_the_existing_cookie() {
        let (db, _path) = temp_db("profile");
        db.save_credential(&row("bilibili", "SESSDATA=old"))
            .expect("写凭据失败");
        let raw_before = raw_cookie(&db, "bilibili");
        assert!(db
            .update_credential_profile("bilibili", "expired", "新名字", Some("http://a"), 99)
            .expect("更新资料失败"));
        // 复验只能改状态与展示信息，不能把旧 cookie 写回去覆盖新登录。
        assert_eq!(raw_cookie(&db, "bilibili"), raw_before);
        let rows = db.credentials().expect("读凭据失败");
        assert_eq!(rows[0].cookie, "SESSDATA=old");
        assert_eq!(rows[0].status, "expired");
        assert_eq!(rows[0].account_name, "新名字");
        assert_eq!(rows[0].avatar.as_deref(), Some("http://a"));
        assert_eq!(rows[0].updated_at, 99);
        assert!(!db
            .update_credential_profile("douyu", "ok", "x", None, 1)
            .expect("更新不存在行失败"));
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

    #[test]
    fn douyin_follow_upsert_keeps_promotion_and_flags() {
        let (db, _path) = temp_db("douyin-follow");
        let mut item = DouyinFollow {
            uid: "u-1".into(),
            sec_uid: "MS4wLjAB".into(),
            uname: "离线主播".into(),
            avatar_url: None,
            is_live: false,
            web_rid: None,
            internal_room_id: None,
            title: String::new(),
            cover_url: None,
            area_name: None,
            online: None,
            started_at: None,
            in_pool: false,
            monitor: true,
            auto_record: false,
            promoted_room_id: None,
        };
        db.upsert_douyin_follow(&item, true, false)
            .expect("写入关注池失败");
        let rows = db.list_douyin_follows().expect("读取关注池失败");
        assert_eq!(rows.len(), 1);
        assert!(rows[0].in_pool);
        assert!(rows[0].monitor);
        assert!(!rows[0].auto_record);
        assert!(rows[0].promoted_room_id.is_none());

        db.mark_douyin_follow_promoted("u-1", "room-1")
            .expect("标记转正失败");
        assert!(db
            .pending_douyin_follows()
            .expect("读取待开播失败")
            .is_empty());

        // 再次导入同一 uid：展示字段刷新，但 monitor/auto_record 采用新选择，
        // promoted_room_id 不被覆盖。
        item.is_live = true;
        item.web_rid = Some("938611933111".into());
        item.internal_room_id = Some("7708331906513767187".into());
        db.upsert_douyin_follow(&item, false, true)
            .expect("更新关注池失败");
        let rows = db.list_douyin_follows().expect("读取关注池失败");
        let saved = &rows[0];
        assert_eq!(saved.web_rid.as_deref(), Some("938611933111"));
        assert_eq!(
            saved.internal_room_id.as_deref(),
            Some("7708331906513767187")
        );
        assert!(!saved.monitor);
        assert!(saved.auto_record);
        assert_eq!(saved.promoted_room_id.as_deref(), Some("room-1"));

        // 用户移除房间时关注池项一并删除：只解绑 promoted_room_id 会让池扫描
        // 在下一轮把刚删掉的房间重新转正。
        assert_eq!(
            db.delete_douyin_follow_for_room("room-1")
                .expect("删除关注池项失败"),
            1
        );
        assert!(db.list_douyin_follows().expect("读取关注池失败").is_empty());
        assert!(db.pending_douyin_follows().expect("读取失败").is_empty());
        assert_eq!(
            db.delete_douyin_follow_for_room("room-1")
                .expect("重复删除失败"),
            0
        );
    }

    #[test]
    fn manual_douyin_room_can_join_batch_and_removal_drops_follow() {
        let (db, _path) = temp_db("douyin-batch-upgrade");
        seed_media(&db);
        db.lock()
            .execute("UPDATE room SET platform = 'douyin' WHERE id = 'rm-a'", [])
            .expect("改平台失败");
        db.lock()
            .execute(
                "INSERT INTO douyin_follow (uid, promoted_room_id, created_at, updated_at)
                 VALUES ('u-9', 'rm-a', 0, 0)",
                [],
            )
            .expect("写入关注池失败");

        db.mark_douyin_batch_room("rm-a", Some("u-9"), Some("7708331906513767187"))
            .expect("标记批量失败");
        let rooms = db.list_rooms().expect("读取房间失败");
        let room = rooms
            .iter()
            .find(|room| room.id == "rm-a")
            .expect("rm-a 缺失");
        assert!(room.batch_follow);
        assert_eq!(room.platform_uid.as_deref(), Some("u-9"));
        assert_eq!(
            room.internal_room_id.as_deref(),
            Some("7708331906513767187")
        );

        assert_eq!(
            db.delete_douyin_follow_for_room("rm-a")
                .expect("删除关注池项失败"),
            1
        );
        assert!(db.list_douyin_follows().expect("读取关注池失败").is_empty());
    }

    #[test]
    fn task_events_and_running_start_are_persisted_in_order() {
        let (db, _path) = temp_db("task-events");
        let record = hivecast_ipc::TaskRecord {
            id: "finalize:s1".into(),
            kind: "finalize".into(),
            title: "a.flv".into(),
            detail: None,
            state: "running".into(),
            percent: 0,
            stage: Some("queued".into()),
            message: None,
            session_id: Some("s1".into()),
            room_id: None,
            started_at: 100,
            queued_at: Some(100),
            started_running_at: None,
            finished_at: None,
        };
        db.task_start(&record).expect("写任务失败");
        // 仍在 queued 阶段：不能把入队时间当成真正开始执行。
        db.task_progress("finalize:s1", 0, Some("queued"), "running", None, 150)
            .expect("更新队列进度失败");
        db.task_progress(
            "finalize:s1",
            5,
            Some("merging"),
            "running",
            Some("合并分片"),
            160,
        )
        .expect("更新运行进度失败");
        for (at, stage, state, percent) in [
            (100, "queued", "queued", 0),
            (160, "merging", "running", 5),
            (200, "done", "done", 100),
        ] {
            db.task_event(&hivecast_ipc::TaskEvent {
                id: 0,
                task_id: "finalize:s1".into(),
                run_id: Some("session:s1".into()),
                kind: "finalize".into(),
                stage: stage.into(),
                state: state.into(),
                percent,
                message: None,
                at,
            })
            .expect("写事件失败");
        }
        db.task_finish("finalize:s1", "done", None, 200)
            .expect("写结束失败");

        let events = db
            .task_event_page(&["session:s1".into()], None, 10)
            .expect("读事件失败")
            .events;
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].stage, "queued");
        assert_eq!(events[1].stage, "merging");
        assert_eq!(events[2].state, "done");
        assert!(db
            .task_event_page(&["session:other".into()], None, 10)
            .expect("读事件失败")
            .events
            .is_empty());

        let history = db.task_history(10).expect("读历史失败");
        assert_eq!(history[0].queued_at, Some(100));
        assert_eq!(history[0].started_running_at, Some(160));
        assert_eq!(history[0].finished_at, Some(200));
    }

    #[test]
    fn session_group_ids_expands_every_clip_in_the_group() {
        let (db, _path) = temp_db("session-group-ids");
        seed_media(&db);
        db.lock()
            .execute(
                "UPDATE record_session SET group_id = 's-a1' WHERE id IN ('s-a1', 's-a2')",
                [],
            )
            .expect("写分组失败");

        assert_eq!(
            db.session_group_ids("s-a1").expect("展开分组失败"),
            vec!["s-a1".to_string(), "s-a2".to_string()]
        );
        assert_eq!(
            db.session_group_ids("s-a2").expect("按片段展开失败"),
            vec!["s-a1".to_string(), "s-a2".to_string()]
        );
        assert_eq!(
            db.session_group_ids("s-b1").expect("单场展开失败"),
            vec!["s-b1".to_string()]
        );
        assert!(db
            .session_group_ids("missing")
            .expect("未知场次应返回空")
            .is_empty());
    }

    #[test]
    fn task_event_search_joins_rooms_and_escapes_like_wildcards() {
        let (db, _path) = temp_db("task-event-search");
        seed_media(&db);
        db.lock()
            .execute(
                "UPDATE record_session SET group_id = 's-a1' WHERE id IN ('s-a1', 's-a2')",
                [],
            )
            .expect("写分组失败");
        for (task, run, stage, state, message, at) in [
            (
                "post:s-a1",
                "session:s-a1",
                "postprocessing",
                "failed",
                "磁盘写入失败 100%",
                100,
            ),
            ("merge:x", "task:x", "merging", "failed", "合并失败", 150),
            (
                "record:s-b1",
                "session:s-b1",
                "recording",
                "done",
                "1000 完成",
                200,
            ),
            (
                "post:s-a2",
                "session:s-a2",
                "postprocessing",
                "done",
                "普通完成",
                300,
            ),
        ] {
            db.task_event(&hivecast_ipc::TaskEvent {
                id: 0,
                task_id: task.into(),
                run_id: Some(run.into()),
                kind: "post".into(),
                stage: stage.into(),
                state: state.into(),
                percent: 100,
                message: Some(message.into()),
                at,
            })
            .expect("写事件失败");
        }

        let failed = db
            .task_event_search("失败", false, None, 100)
            .expect("检索失败");
        assert_eq!(failed.total, 2);
        assert_eq!(failed.hits[0].message.as_deref(), Some("合并失败"));
        assert_eq!(failed.hits[1].room_title.as_deref(), Some("房间甲"));
        assert_eq!(failed.hits[1].group_id.as_deref(), Some("s-a1"));
        assert_eq!(failed.hits[1].room_key.as_deref(), Some("1"));
        assert_eq!(failed.hits[0].room_key, None, "任务级事件没有房间");

        // 界面显示的是「后处理 / 录制中」，检索输入中文标签也必须命中。
        let localized = db
            .task_event_search("后处理", false, None, 100)
            .expect("检索失败");
        assert_eq!(localized.total, 2);
        let recording = db
            .task_event_search("录制中", false, None, 100)
            .expect("检索失败");
        assert_eq!(recording.total, 1);
        assert_eq!(recording.hits[0].stage, "recording");

        // `%` 是 LIKE 的通配符，必须按字面量匹配，不能把 1000 也命中。
        let literal = db
            .task_event_search("100%", false, None, 100)
            .expect("检索失败");
        assert_eq!(literal.total, 1);
        assert_eq!(literal.hits[0].run_id.as_deref(), Some("session:s-a1"));

        let only = db.task_event_search("", true, None, 100).expect("检索失败");
        assert_eq!(only.total, 2);
        let bilibili = db
            .task_event_search("", false, Some("bilibili"), 100)
            .expect("检索失败");
        // 没有房间的合并/导出独立 Run 在平台筛选下仍然可见：它不属于任何平台，
        // 隐藏它会让用户选完平台后以为那场合并失败消失了。
        assert_eq!(bilibili.total, 3);
        assert!(
            bilibili.hits.iter().any(|hit| hit.task_id == "merge:x"),
            "无房间的独立 Run 不应被平台筛选隐藏"
        );
        let limited = db.task_event_search("", false, None, 1).expect("检索失败");
        assert_eq!(limited.total, 4);
        assert_eq!(limited.hits.len(), 1);
        assert_eq!(limited.hits[0].at, 300);
    }

    #[test]
    fn task_event_search_matches_english_labels_and_short_chinese() {
        let (db, _path) = temp_db("task-event-search-labels");
        for (stage, state, message, at) in [
            ("thumbnail", "done", "封面生成完成", 10),
            ("postprocessing", "failed", "transcode failed", 20),
            ("recording", "done", "录制开始", 30),
        ] {
            db.task_event(&hivecast_ipc::TaskEvent {
                id: 0,
                task_id: format!("task:{at}"),
                run_id: Some(format!("task:{at}")),
                kind: "post".into(),
                stage: stage.into(),
                state: state.into(),
                percent: 100,
                message: Some(message.into()),
                at,
            })
            .expect("写事件失败");
        }

        let cover = db
            .task_event_search("Cover", false, None, 100)
            .expect("检索失败");
        assert_eq!(cover.total, 1);
        assert_eq!(cover.hits[0].stage, "thumbnail");

        let post = db
            .task_event_search("Post-processing", false, None, 100)
            .expect("检索失败");
        assert_eq!(post.total, 1);
        assert_eq!(post.hits[0].stage, "postprocessing");

        // 中文两个字低于 trigram 下限，必须靠阶段/状态枚举命中。
        let short = db
            .task_event_search("录制", false, None, 100)
            .expect("检索失败");
        assert_eq!(short.total, 1);
        assert_eq!(short.hits[0].stage, "recording");
    }

    #[test]
    fn task_event_fts_backfills_old_events_once() {
        let (db, _path) = temp_db("task-event-fts-backfill");
        db.task_event(&hivecast_ipc::TaskEvent {
            id: 0,
            task_id: "post:s-a1".into(),
            run_id: Some("session:s-a1".into()),
            kind: "post".into(),
            stage: "thumbnail".into(),
            state: "done".into(),
            percent: 100,
            message: Some("封面回填".into()),
            at: 10,
        })
        .expect("写事件失败");
        // 模拟升级前的老库：事件已经在，但索引为空且没有回填标记。
        db.lock()
            .execute(
                "DELETE FROM app_setting WHERE key = 'db.task_event_fts_v1'",
                [],
            )
            .expect("清标记失败");
        db.lock()
            .execute("DELETE FROM task_event_fts", [])
            .expect("清索引失败");
        // 索引不可用时退回 LIKE，不能因为 FTS 缺失就返回 0 条。
        db.fts_ready
            .store(false, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            db.task_event_search("封面回填", false, None, 100)
                .expect("检索失败")
                .total,
            1
        );
        db.fts_ready
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            db.task_event_search("封面回填", false, None, 100)
                .expect("检索失败")
                .total,
            0
        );

        assert_eq!(db.ensure_task_event_fts().expect("回填失败"), 1);
        assert_eq!(
            db.task_event_search("封面回填", false, None, 100)
                .expect("检索失败")
                .total,
            1
        );
        // 标记写入后再次打开不再重复扫全表。
        assert_eq!(db.ensure_task_event_fts().expect("回填失败"), 0);
    }

    #[test]
    fn task_events_keeps_the_newest_events_when_capped() {
        let (db, _path) = temp_db("task-event-newest");
        for at in 1..=5 {
            db.task_event(&hivecast_ipc::TaskEvent {
                id: 0,
                task_id: "post:s1".into(),
                run_id: Some("session:s1".into()),
                kind: "post".into(),
                stage: "postprocessing".into(),
                state: "running".into(),
                percent: at * 10,
                message: None,
                at,
            })
            .expect("写事件失败");
        }
        let events = db
            .task_event_page(&["session:s1".into()], None, 3)
            .expect("读事件失败")
            .events;
        assert_eq!(events.len(), 3);
        assert_eq!(events.first().map(|event| event.at), Some(3));
        assert_eq!(events.last().map(|event| event.at), Some(5));
    }

    #[test]
    fn task_event_page_walks_older_with_a_stable_cursor() {
        let (db, _path) = temp_db("task-event-page");
        for at in 1..=5 {
            db.task_event(&hivecast_ipc::TaskEvent {
                id: 0,
                task_id: "post:s1".into(),
                run_id: Some("session:s1".into()),
                kind: "post".into(),
                stage: "postprocessing".into(),
                state: "running".into(),
                percent: at * 10,
                message: None,
                at,
            })
            .expect("写事件失败");
        }
        let first = db
            .task_event_page(&["session:s1".into()], None, 2)
            .expect("读第一页失败");
        assert!(first.has_more);
        assert_eq!(
            first
                .events
                .iter()
                .map(|event| event.at)
                .collect::<Vec<_>>(),
            vec![4, 5]
        );

        let oldest = first.events.first().expect("第一页为空");
        let second = db
            .task_event_page(&["session:s1".into()], Some((oldest.at, oldest.id)), 2)
            .expect("读第二页失败");
        assert!(second.has_more);
        assert_eq!(
            second
                .events
                .iter()
                .map(|event| event.at)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );

        let oldest = second.events.first().expect("第二页为空");
        let last = db
            .task_event_page(&["session:s1".into()], Some((oldest.at, oldest.id)), 2)
            .expect("读最后一页失败");
        assert!(!last.has_more);
        assert_eq!(
            last.events.iter().map(|event| event.at).collect::<Vec<_>>(),
            vec![1]
        );
    }

    #[test]
    fn task_events_batches_run_ids_under_the_bind_limit() {
        let (db, _path) = temp_db("task-event-chunks");
        let ids: Vec<String> = (0..1_200).map(|i| format!("session:{i}")).collect();
        // 只给最后一个 Run 写事件；1200 个 IN 绑定必须自动分组，不能直接报错。
        db.task_event(&hivecast_ipc::TaskEvent {
            id: 0,
            task_id: "phase:thumbnail:1199".into(),
            run_id: Some("session:1199".into()),
            kind: "thumbnail".into(),
            stage: "thumbnail".into(),
            state: "done".into(),
            percent: 100,
            message: None,
            at: 1,
        })
        .expect("写事件失败");
        let events = db
            .task_event_page(&ids, None, 100)
            .expect("分批读事件失败")
            .events;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].run_id.as_deref(), Some("session:1199"));
    }

    #[test]
    fn task_history_keeps_more_than_five_hundred_for_task_center() {
        let (db, _path) = temp_db("task-history-cap");
        {
            let conn = db.lock();
            let mut stmt = conn
                .prepare(
                    "INSERT INTO task_history (id, kind, title, state, started_at, finished_at)
                     VALUES (?1, 'post', 'clip', 'done', ?2, ?2)",
                )
                .expect("prepare");
            for i in 0..600 {
                stmt.execute(params![format!("t{i}"), i])
                    .expect("insert task");
            }
        }
        let history = db.task_history(600).expect("read history");
        assert_eq!(history.len(), 600);
        assert_eq!(history[0].started_at, 599);
    }

    #[test]
    fn queued_tasks_are_interrupted_on_startup() {
        let (db, _path) = temp_db("task-queued-interrupted");
        db.lock()
            .execute_batch(
                "INSERT INTO task_history (id, kind, title, state, started_at, queued_at)
                 VALUES ('running-1', 'post', 'a', 'running', 10, 10),
                        ('queued-1', 'post', 'b', 'queued', 20, 20),
                        ('done-1', 'post', 'c', 'done', 30, 30);",
            )
            .expect("播种任务失败");

        assert_eq!(db.mark_tasks_interrupted().expect("标记中断失败"), 2);
        let history = db.task_history(10).expect("读任务历史失败");
        let state = |id: &str| {
            history
                .iter()
                .find(|task| task.id == id)
                .map(|task| task.state.as_str())
        };
        assert_eq!(state("running-1"), Some("interrupted"));
        assert_eq!(state("queued-1"), Some("interrupted"));
        assert_eq!(state("done-1"), Some("done"));
    }

    #[test]
    fn prunes_old_tasks_but_never_running_ones() {
        let (db, _path) = temp_db("task-prune");
        db.lock()
            .execute_batch(
                "INSERT INTO task_history (id, kind, title, state, started_at, finished_at)
                 VALUES ('old', 'post', 'old', 'done', 100, 100),
                        ('running', 'post', 'running', 'running', 100, NULL),
                        ('recent', 'post', 'recent', 'done', 2_000, 2_000);",
            )
            .expect("seed tasks");
        for (task, run, at) in [
            ("old", "session:1", 100),
            ("running", "session:2", 100),
            ("recent", "session:3", 2_000),
            ("gone", "session:4", 100),
        ] {
            db.task_event(&hivecast_ipc::TaskEvent {
                id: 0,
                task_id: task.into(),
                run_id: Some(run.into()),
                kind: "post".into(),
                stage: "post".into(),
                state: "done".into(),
                percent: 100,
                message: None,
                at,
            })
            .expect("seed event");
        }

        let pruned = db
            .prune_task_data(1_000, 1, 10, 2)
            .expect("prune task data");
        assert_eq!(pruned.history, 1);
        assert_eq!(pruned.events, 3);

        let history = db.task_history(10).expect("read history");
        let ids: Vec<&str> = history.iter().map(|task| task.id.as_str()).collect();
        assert_eq!(ids, vec!["recent"]);
        let running: i64 = db
            .lock()
            .query_row(
                "SELECT COUNT(*) FROM task_history WHERE id = 'running'",
                [],
                |row| row.get(0),
            )
            .expect("read running");
        assert_eq!(running, 1);
        let events: i64 = db
            .lock()
            .query_row("SELECT COUNT(*) FROM task_event", [], |row| row.get(0))
            .expect("read events");
        assert_eq!(events, 1);
    }

    #[test]
    fn keeps_phase_events_while_pruning_orphan_events() {
        let (db, _path) = temp_db("task-prune-phase");
        db.lock()
            .execute(
                "INSERT INTO task_history (id, kind, title, state, started_at, finished_at)
                 VALUES ('job:1', 'post', 'clip', 'done', 2_000, 2_000)",
                [],
            )
            .expect("seed task");
        for (task, at) in [("job:1", 2_000), ("phase:post:s1", 2_000), ("gone", 2_000)] {
            db.task_event(&hivecast_ipc::TaskEvent {
                id: 0,
                task_id: task.into(),
                run_id: Some("session:s1".into()),
                kind: "post".into(),
                stage: "post".into(),
                state: "done".into(),
                percent: 100,
                message: None,
                at,
            })
            .expect("seed event");
        }

        let pruned = db
            .prune_task_data(1_000, 10, 10, 10)
            .expect("prune task data");
        assert_eq!(pruned.history, 0);
        assert_eq!(pruned.events, 1);
        let phases: i64 = db
            .lock()
            .query_row(
                "SELECT COUNT(*) FROM task_event WHERE task_id LIKE 'phase:%'",
                [],
                |row| row.get(0),
            )
            .expect("count phase events");
        assert_eq!(phases, 1);
    }

    #[test]
    fn prunes_task_history_and_events_to_hard_caps() {
        let (db, _path) = temp_db("task-prune-caps");
        for i in 0..5 {
            db.lock()
                .execute(
                    "INSERT INTO task_history (id, kind, title, state, started_at, finished_at)
                     VALUES (?1, 'post', 'clip', 'done', ?2, ?2)",
                    params![format!("t{i}"), 1_000 + i],
                )
                .expect("seed task");
            db.task_event(&hivecast_ipc::TaskEvent {
                id: 0,
                task_id: format!("t{i}"),
                run_id: Some(format!("session:{i}")),
                kind: "post".into(),
                stage: "post".into(),
                state: "done".into(),
                percent: 100,
                message: None,
                at: 1_000 + i,
            })
            .expect("seed event");
        }

        let pruned = db.prune_task_data(0, 100, 3, 2).expect("prune task data");
        assert_eq!(pruned.history, 2);
        assert_eq!(pruned.events, 3);

        let history: i64 = db
            .lock()
            .query_row("SELECT COUNT(*) FROM task_history", [], |row| row.get(0))
            .expect("count history");
        assert_eq!(history, 3);
        let events: i64 = db
            .lock()
            .query_row("SELECT COUNT(*) FROM task_event", [], |row| row.get(0))
            .expect("count events");
        assert_eq!(events, 2);
    }

    #[test]
    fn backfills_group_of_a_legacy_first_clip_only() {
        let (db, _path) = temp_db("backfill-groups");
        db.lock()
            .execute_batch(
                "INSERT INTO room (id, platform, room_key, canonical_url, host_name, title, created_at, updated_at)
                 VALUES ('rm-a', 'bilibili', '1', 'https://live.bilibili.com/1', '主播甲', '房间甲', 0, 0);
                 INSERT INTO record_session (id, room_id, started_at, status, file_path, file_size, duration_ms, group_id, clip_index)
                 VALUES ('s1', 'rm-a', 100, 'finished', '/tmp/s1.flv', 10, 100, NULL, 0),
                        ('s2', 'rm-a', 200, 'finished', '/tmp/s2.flv', 10, 100, 's1', 1),
                        ('s3', 'rm-a', 300, 'finished', '/tmp/s3.flv', 10, 100, NULL, 0);",
            )
            .expect("播种场次失败");

        assert_eq!(db.backfill_session_groups().expect("回填失败"), 1);
        let conn = db.lock();
        let group = |id: &str| -> Option<String> {
            conn.query_row(
                "SELECT group_id FROM record_session WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .expect("读分组失败")
        };
        assert_eq!(group("s1").as_deref(), Some("s1"));
        assert_eq!(group("s2").as_deref(), Some("s1"));
        assert_eq!(group("s3"), None, "没有片段指向的旧场次不能猜分组");
        drop(conn);
        // 幂等：再跑一次不会改任何行。
        assert_eq!(db.backfill_session_groups().expect("再次回填失败"), 0);
    }

    #[test]
    fn split_manifest_falls_back_to_any_clip_in_the_group() {
        let (db, _path) = temp_db("split-manifest-group");
        db.lock()
            .execute_batch(
                "INSERT INTO room (id, platform, room_key, canonical_url, host_name, title, created_at, updated_at)
                 VALUES ('rm-a', 'bilibili', '1', 'https://live.bilibili.com/1', '主播甲', '房间甲', 0, 0);
                 INSERT INTO record_session (id, room_id, started_at, status, file_path, file_size, duration_ms, group_id, clip_index)
                 VALUES ('s1', 'rm-a', 100, 'failed', '/tmp/s1.flv', 10, 100, 'g1', 0),
                        ('s2', 'rm-a', 200, 'finished', '/tmp/s2.flv', 10, 100, 'g1', 1);",
            )
            .expect("播种场次失败");
        db.set_split_manifest("s1", Some("/tmp/s1.json"))
            .expect("写清单失败");

        assert_eq!(
            db.split_manifest_for("s2").expect("读清单失败").as_deref(),
            Some("/tmp/s1.json"),
            "从任意片段都能找到整组的收尾清单"
        );
        assert_eq!(
            db.split_manifest_for("s1").unwrap().as_deref(),
            Some("/tmp/s1.json")
        );
    }

    #[test]
    fn set_settings_rolls_back_every_key_when_one_write_fails() {
        let (db, _path) = temp_db("settings-transaction");
        db.set_setting("keep", "old").expect("写初始值失败");
        db.lock()
            .execute_batch(
                "CREATE TRIGGER reject_boom BEFORE INSERT ON app_setting
                 WHEN NEW.key = 'boom' BEGIN SELECT RAISE(ABORT, 'boom'); END;",
            )
            .expect("建触发器失败");

        let err = db
            .set_settings(&[
                ("keep", "new".to_string()),
                ("boom", "x".to_string()),
                ("after", "y".to_string()),
            ])
            .expect_err("中途失败应返回错误");
        assert!(err.to_string().contains("boom"), "应保留底层错误: {err}");
        assert_eq!(db.get_setting("keep").unwrap().as_deref(), Some("old"));
        assert_eq!(
            db.get_setting("after").unwrap(),
            None,
            "失败后不能留下后续键"
        );
    }
}
