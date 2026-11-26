use super::tests::seed_recovery_session;
use super::*;

fn clip(id: &str, output: &Path, start: u32, end: Option<u32>) -> ManifestClip {
    ManifestClip {
        id: id.into(),
        output: output.display().to_string(),
        path: output.display().to_string(),
        final_path: None,
        post_state: None,
        post_error: None,
        start,
        end,
        reason: None,
        started_at: 100 + i64::from(start),
        ended_at: 101 + i64::from(start),
    }
}

fn receipt(source: &Path, clips: Vec<ManifestClip>) -> SplitManifest {
    SplitManifest {
        version: 2,
        session_id: "s1".into(),
        room_id: "r1".into(),
        source: source.display().to_string(),
        status: "aborted".into(),
        reason: "app_exit".into(),
        error: None,
        quality: None,
        quality_history: vec![],
        line: None,
        post: Default::default(),
        clips,
        rendered: 0,
        completed: false,
    }
}

#[tokio::test]
async fn foreign_ts_is_not_a_completed_clip() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("live.flv");
    let db = Db::open(Path::new(":memory:")).unwrap();
    seed_recovery_session(&db, &source);
    let file = manifest_path(&source);
    let mut pending = clip("s1", &source, 0, None);
    let empty = dir.path().join("empty.mp4");
    std::fs::write(&empty, []).unwrap();
    pending.final_path = Some(empty.display().to_string());
    write_manifest(&file, &receipt(&source, vec![pending])).unwrap();
    db.set_split_manifest("s1", file.to_str()).unwrap();
    let part = PathBuf::from(segment::part_name(&source, 0));
    let foreign = source.with_extension("ts");
    std::fs::write(&foreign, b"unrelated-file").unwrap();
    std::fs::write(&part, [7; 752]).unwrap();
    assert_eq!(
        recover_split_manifest(&db, &file, "s1").await,
        Some((1, 752))
    );
    let row = db.get_session("s1").unwrap().unwrap();
    let path = PathBuf::from(row.file_path.unwrap());
    assert_ne!(path, foreign);
    assert_eq!(std::fs::read(&foreign).unwrap(), b"unrelated-file");
    assert_eq!(std::fs::read(path).unwrap(), [7; 752]);
    assert!(!part.exists() && !file.exists());
}

#[tokio::test]
async fn missing_and_truncated_split_receipts_preserve_later_parts_until_retry() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("live.flv");
    let db = Db::open(Path::new(":memory:")).unwrap();
    seed_recovery_session(&db, &first);
    db.close_orphan_sessions(&Default::default()).unwrap();
    let file = manifest_path(&first);
    let later = PathBuf::from(segment::part_name(&first, 2));
    std::fs::write(&first, [1; 210]).unwrap();
    std::fs::write(&later, [2; 250]).unwrap();
    db.set_split_manifest("s1", file.to_str()).unwrap();
    for _ in 0..2 {
        assert_eq!(recover_parts(&db, &Default::default()).await, (0, 0));
        assert_eq!(db.manifest_sessions().unwrap().len(), 1);
        assert!(db
            .get_session("s1")
            .unwrap()
            .unwrap()
            .post_error
            .unwrap()
            .contains("清单"));
        assert!(db.retention_rows().unwrap().is_empty());
        assert_eq!(std::fs::read(&later).unwrap(), [2; 250]);
        std::fs::write(&file, b"{").unwrap();
    }
    let second = dir.path().join("live.clip01.flv");
    write_manifest(
        &file,
        &receipt(
            &first,
            vec![clip("s1", &first, 0, Some(2)), clip("s2", &second, 2, None)],
        ),
    )
    .unwrap();
    assert_eq!(recover_parts(&db, &Default::default()).await, (2, 460));
    assert_eq!(db.get_session("s2").unwrap().unwrap().file_size, 250);
    assert_eq!(recover_parts(&db, &Default::default()).await, (0, 0));
}

#[tokio::test]
async fn registration_error_reuses_exact_output_and_recovers_chat() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("live.flv");
    let dbfile = dir.path().join("test.db");
    let db = Db::open(&dbfile).unwrap();
    seed_recovery_session(&db, &source);
    let chat = source.with_extension("jsonl");
    std::fs::write(
        &chat,
        b"{\"t\":0,\"c\":\"first\"}\n{\"t\":12,\"c\":\"second\"}\n{\"t\":",
    )
    .unwrap();
    std::fs::write(segment::part_name(&source, 0), [6; 188]).unwrap();
    let sql = rusqlite::Connection::open(&dbfile).unwrap();
    sql.execute(
        "UPDATE record_session SET post_state='recording',danmaku_path=?1",
        [chat.to_str().unwrap()],
    )
    .unwrap();
    db.close_orphan_sessions(&Default::default()).unwrap();
    sql.execute_batch("CREATE TRIGGER qa_fail BEFORE UPDATE OF post_state ON record_session BEGIN SELECT RAISE(ABORT,'qa-fail'); END;").unwrap();
    assert_eq!(recover_parts(&db, &Default::default()).await, (0, 0));
    let saved = read_manifest(&manifest_path(&source))
        .unwrap()
        .clips
        .remove(0)
        .final_path
        .unwrap();
    assert_eq!(db.get_session("s1").unwrap().unwrap().file_size, 0);
    let ass = source.with_extension("ass");
    let subtitle = std::fs::read_to_string(&ass).unwrap();
    assert_eq!(
        subtitle
            .lines()
            .filter(|line| line.starts_with("Dialogue:"))
            .count(),
        2
    );
    std::fs::write(&ass, b"preserve existing subtitles").unwrap();
    sql.execute_batch("DROP TRIGGER qa_fail").unwrap();
    assert_eq!(recover_parts(&db, &Default::default()).await, (1, 188));
    let row = db.get_session("s1").unwrap().unwrap();
    assert_eq!(row.file_path, Some(saved));
    assert_eq!(row.danmaku_count, 2);
    let (listed, total) = db
        .media_list(&crate::db::MediaFilter {
            only_danmaku: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(total, 1);
    assert_eq!(listed[0].id, "s1");
    assert_eq!(std::fs::read(&ass).unwrap(), b"preserve existing subtitles");
    assert_eq!(recover_parts(&db, &Default::default()).await, (0, 0));
}

#[tokio::test]
#[ignore = "需要真实 FFmpeg 和 ffprobe"]
async fn real_media_registration_retry_keeps_inputs_and_reuses_the_playable_output() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("valid.mp4");
    let db = Db::open(Path::new(":memory:")).unwrap();
    seed_recovery_session(&db, &source);
    db.close_orphan_sessions(&Default::default()).unwrap();
    let part = PathBuf::from(segment::part_name(&source, 0));
    assert!(tokio::process::Command::new(crate::ffmpeg::binary())
        .args([
            "-nostdin",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x120:rate=10",
            "-t",
            "1",
            "-c:v",
            "libx264",
            "-f",
            "mpegts"
        ])
        .arg(&part)
        .status()
        .await
        .unwrap()
        .success());
    db.exec_batch_for_test("CREATE TRIGGER qa BEFORE UPDATE OF post_state ON record_session BEGIN SELECT RAISE(ABORT,'qa'); END;").unwrap();
    assert_eq!(recover_parts(&db, &Default::default()).await, (0, 0));
    assert!(part.is_file());
    let stamp = source.metadata().unwrap().modified().unwrap();
    let size = source.metadata().unwrap().len();
    assert!(size > 1000);
    db.exec_batch_for_test("DROP TRIGGER qa").unwrap();
    assert_eq!(recover_parts(&db, &Default::default()).await, (1, size));
    assert_eq!(source.metadata().unwrap().modified().unwrap(), stamp);
    assert!(!part.exists());
    assert_eq!(
        db.get_session("s1").unwrap().unwrap().post_state,
        crate::post::STATE_DONE
    );
    assert!(crate::post::media_duration_ms(&source)
        .await
        .is_some_and(|ms| (900..=1100).contains(&ms)));
    assert_eq!(recover_parts(&db, &Default::default()).await, (0, 0));
}

#[tokio::test]
async fn legacy_incomplete_subtitles_are_repaired_after_manifest_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("old.flv");
    let track = source.with_extension("jsonl");
    let ass = source.with_extension("ass");
    let dbfile = dir.path().join("test.db");
    let db = Db::open(&dbfile).unwrap();
    seed_recovery_session(&db, &source);
    std::fs::write(&source, b"already registered").unwrap();
    std::fs::write(
        &track,
        b"{\"t\":0,\"c\":\"one\"}\n{\"t\":10,\"c\":\"two\"}\n",
    )
    .unwrap();
    rusqlite::Connection::open(&dbfile).unwrap().execute(
        "UPDATE record_session SET file_size=18,status='aborted',reason='app_exit',post_state='done',danmaku_count=2,danmaku_path=?1",
        [track.to_str().unwrap()]).unwrap();
    for partial in [&b""[..], b"[Script Info]\nScriptTy"] {
        std::fs::write(&ass, partial).unwrap();
        assert_eq!(recover_parts(&db, &Default::default()).await, (1, 18));
        let subtitles = std::fs::read_to_string(&ass).unwrap();
        assert_eq!(
            subtitles
                .lines()
                .filter(|l| l.starts_with("Dialogue: "))
                .count(),
            2
        );
        assert!(!manifest_path(&source).exists());
        assert_eq!(recover_parts(&db, &Default::default()).await, (0, 0));
        assert!(dir.path().read_dir().unwrap().flatten().any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".hivecast-ass-backup-")
                && std::fs::read(e.path().join("original.ass")).unwrap() == partial
        }));
    }
    // An older, intentionally shortened but valid subtitle has no ownership receipt.
    let export = std::fs::read_to_string(&ass)
        .unwrap()
        .replace("; HIVECAST subtitle export\n", "");
    let second = export.rfind("Dialogue: ").unwrap();
    let edited = &export[..second];
    std::fs::write(&ass, edited).unwrap();
    assert_eq!(recover_parts(&db, &Default::default()).await, (0, 0));
    assert_eq!(std::fs::read(&ass).unwrap(), edited.as_bytes());
    let custom = b"[Script Info]\nTitle: custom subtitles\n[Events]\nDialogue: user edits\n";
    std::fs::write(&ass, custom).unwrap();
    assert_eq!(recover_parts(&db, &Default::default()).await, (0, 0));
    assert_eq!(std::fs::read(ass).unwrap(), custom);
}

#[tokio::test]
async fn old_completed_crash_record_repairs_only_the_owned_track_once() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("old.flv");
    let chat = source.with_extension("jsonl");
    let dbfile = dir.path().join("test.db");
    let db = Db::open(&dbfile).unwrap();
    seed_recovery_session(&db, &source);
    std::fs::write(&source, b"already registered").unwrap();
    std::fs::write(&chat, b"{truncated").unwrap();
    let sql = rusqlite::Connection::open(&dbfile).unwrap();
    sql.execute("UPDATE record_session SET file_size=18, status='aborted',reason='app_exit',post_state='done',danmaku_path=?1", [chat.to_str().unwrap()]).unwrap();
    assert_eq!(recover_parts(&db, &Default::default()).await, (0, 0));
    std::fs::write(&chat, b"{\"t\":0,\"c\":\"saved\"}\n").unwrap();
    assert_eq!(recover_parts(&db, &Default::default()).await, (1, 18));
    assert_eq!(db.get_session("s1").unwrap().unwrap().danmaku_count, 1);
    assert!(chat.with_extension("ass").is_file());
    assert_eq!(recover_parts(&db, &Default::default()).await, (0, 0));
}

#[tokio::test]
async fn pending_recording_after_two_hundred_candidates_is_recovered() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("old.flv");
    let db = Db::open(Path::new(":memory:")).unwrap();
    seed_recovery_session(&db, &source);
    let mut row = db.get_session("s1").unwrap().unwrap();
    row.status = "failed".into();
    for i in 0..200 {
        row.id = format!("a{i:03}");
        row.started_at = 101 + i;
        row.file_path = Some(dir.path().join(format!("row{i}.flv")).display().to_string());
        db.create_session(&row).unwrap();
    }
    db.close_orphan_sessions(&Default::default()).unwrap();
    std::fs::write(segment::part_name(&source, 0), [1; 188]).unwrap();
    assert_eq!(recover_parts(&db, &Default::default()).await, (1, 188));
    assert_eq!(db.get_session("s1").unwrap().unwrap().file_size, 188);
    assert_eq!(recover_parts(&db, &Default::default()).await, (0, 0));
}

#[tokio::test]
#[ignore = "需要真实 FFmpeg 和系统进程枚举"]
async fn missing_marker_reaps_only_the_matching_live_process() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("test.flv");
    let db = std::sync::Arc::new(Db::open(Path::new(":memory:")).unwrap());
    seed_recovery_session(&db, &source);
    let start = |path: &Path| {
        tokio::process::Command::new(crate::ffmpeg::binary())
            .kill_on_drop(true)
            .args([
                "-nostdin",
                "-v",
                "error",
                "-re",
                "-f",
                "lavfi",
                "-i",
                "color=size=128x96",
                "-t",
                "45",
                "-c:v",
                "mpeg2video",
                "-f",
                "segment",
                "-segment_time",
                "1",
            ])
            .arg(segment::parts_pattern(path))
            .spawn()
            .unwrap()
    };
    let mut owned = start(&source);
    let mut unrelated = start(&dir.path().join("other.flv"));
    tokio::time::timeout(Duration::from_secs(8), async {
        while !Path::new(&segment::part_name(&source, 0)).exists() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let scan_db = db.clone();
    let scan = tokio::task::spawn_blocking(move || reap_orphans(&scan_db).unwrap());
    let (deferred, exit) = tokio::join!(scan, owned.wait());
    let deferred = deferred.unwrap();
    assert!(deferred.is_empty() && !exit.unwrap().success());
    assert!(unrelated.try_wait().unwrap().is_none());
    assert_eq!(db.close_orphan_sessions(&deferred).unwrap(), 1);
    unrelated.kill().await.unwrap();
}

#[tokio::test]
async fn missing_danmaku_track_with_online_parent_completes_recovery_once() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("chat_gone.flv");
    let missing_chat = source.with_extension("jsonl");
    let dbfile = dir.path().join("test.db");
    let db = Db::open(&dbfile).unwrap();
    seed_recovery_session(&db, &source);
    std::fs::write(segment::part_name(&source, 0), [5; 188]).unwrap();
    let sql = rusqlite::Connection::open(&dbfile).unwrap();
    sql.execute(
        "UPDATE record_session SET post_state='recording', danmaku_path=?1",
        [missing_chat.to_str().unwrap()],
    )
    .unwrap();
    db.close_orphan_sessions(&Default::default()).unwrap();

    assert_eq!(recover_parts(&db, &Default::default()).await, (1, 188));
    let row = db.get_session("s1").unwrap().unwrap();
    assert_eq!(row.file_size, 188);
    assert_eq!(row.danmaku_count, 0);
    assert!(row.danmaku_path.is_none());
    assert!(
        !row.post_error
            .as_deref()
            .is_some_and(|err| err.contains("弹幕")),
        "missing .jsonl with online parent must not record danmaku error: {:?}",
        row.post_error
    );
    assert!(db.manifest_sessions().unwrap().is_empty());
    assert!(!manifest_path(&source).exists());
    assert!(!Path::new(&segment::part_name(&source, 0)).exists());
    // 二次启动必须幂等，不能重复恢复。
    assert_eq!(recover_parts(&db, &Default::default()).await, (0, 0));
}

#[tokio::test]
async fn missing_danmaku_track_with_offline_parent_keeps_manifest_for_retry() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("live.flv");
    let offline_chat = dir.path().join("unmounted-drive").join("live.jsonl");
    let dbfile = dir.path().join("test.db");
    let db = Db::open(&dbfile).unwrap();
    seed_recovery_session(&db, &source);
    std::fs::write(segment::part_name(&source, 0), [5; 188]).unwrap();
    let sql = rusqlite::Connection::open(&dbfile).unwrap();
    sql.execute(
        "UPDATE record_session SET post_state='recording', danmaku_path=?1",
        [offline_chat.to_str().unwrap()],
    )
    .unwrap();
    db.close_orphan_sessions(&Default::default()).unwrap();

    assert_eq!(recover_parts(&db, &Default::default()).await, (1, 188));
    let row = db.get_session("s1").unwrap().unwrap();
    assert_eq!(row.danmaku_path.as_deref(), offline_chat.to_str());
    assert!(row
        .post_error
        .as_deref()
        .is_some_and(|e| e.contains("弹幕")));
    assert_eq!(db.manifest_sessions().unwrap().len(), 1);
    assert!(manifest_path(&source).is_file());
}
