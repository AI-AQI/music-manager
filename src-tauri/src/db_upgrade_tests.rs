use super::*;
use rusqlite::types::Value;

const V010: &str = include_str!("../tests/fixtures/v0.1.0.sql");
const V011: &str = include_str!("../tests/fixtures/v0.1.1.sql");
const V012: &str = include_str!("../tests/fixtures/v0.1.2.sql");
const V020: &str = include_str!("../tests/fixtures/v0.2.0.sql");
const V021: &str = include_str!("../tests/fixtures/v0.2.1.sql");

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("music-manager-upgrade-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

fn columns(conn: &Connection, table: &str) -> String {
    conn.prepare(&format!("PRAGMA table_info({table})")).unwrap()
        .query_map([], |row| row.get::<_, String>(1)).unwrap()
        .map(|name| format!("\"{}\"", name.unwrap())).collect::<Vec<_>>().join(",")
}

fn rows(conn: &Connection, table: &str, columns: &str) -> Vec<Vec<Value>> {
    let mut statement = conn.prepare(&format!("SELECT {columns} FROM {table} ORDER BY rowid")).unwrap();
    let count = statement.column_count();
    statement.query_map([], |row| (0..count).map(|i| row.get(i)).collect()).unwrap()
        .collect::<Result<_, _>>().unwrap()
}

fn snapshot(conn: &Connection) -> Vec<(String, Vec<Vec<Value>>)> {
    let tables = conn.prepare("SELECT name, sql FROM sqlite_master WHERE type='table' ORDER BY name").unwrap()
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))).unwrap()
        .collect::<Result<Vec<_>, _>>().unwrap();
    tables.into_iter().map(|(table, sql)| (sql, rows(conn, &table, "*"))).collect()
}

fn seed(conn: &Connection, audio: &Path) {
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    conn.pragma_update(None, "foreign_keys", "ON").unwrap();
    conn.execute(
        "INSERT INTO music(id,name,file_name,path,album,artist,genre,year,channels,sample_rate,bitrate,duration,file_size,mime_type,cover_art,created_at,updated_at)
         VALUES ('old-music','用户的曲目','原始音频.wav',?1,'旧专辑','艺术家','原风格','2020','立体声',48000,1536,123.5,123456,'audio/wav','old-cover',11,22)",
        [audio.to_string_lossy().as_ref()],
    ).unwrap();
    if has_column(conn, "music", "file_bookmark") {
        conn.execute("UPDATE music SET file_bookmark=X'000102FF', album_source='manual'", []).unwrap();
    }
    if has_column(conn, "music", "play_count") {
        conn.execute("UPDATE music SET play_count=27, last_played_at=123456789", []).unwrap();
    }
    if has_column(conn, "tags", "category") {
        conn.execute_batch("INSERT INTO tag_categories VALUES ('用户分类',3);
            INSERT INTO tags VALUES (10001,'用户标签','用户分类',4);").unwrap();
    } else {
        conn.execute_batch("INSERT INTO tag_categories(name,created_at) VALUES ('用户分类',3);
            INSERT INTO tags(id,name,category_id,parent_id,created_at) VALUES (10001,'用户标签',last_insert_rowid(),NULL,4);
            INSERT INTO tags(id,name,category_id,parent_id,created_at) SELECT 10002,'子标签',category_id,10001,5 FROM tags WHERE id=10001;").unwrap();
    }
    conn.execute_batch("INSERT INTO clips VALUES ('old-clip','old-music','用户的片段',1.25,9.5,33,44);
        INSERT INTO music_tags VALUES ('old-music',10001);
        INSERT INTO clip_tags VALUES ('old-clip',10001);
        INSERT INTO candidate_entries VALUES ('old-music','music',55),('old-clip','clip',66);").unwrap();
    if conn.prepare("SELECT * FROM album_tags").is_ok() {
        conn.execute("INSERT INTO album_tags VALUES ('旧专辑',10001)", []).unwrap();
    }
}

#[test]
fn every_released_database_opens_in_place_with_all_user_data_and_wal_preserved() {
    for (version, schema) in [
        ("0.1.0", V010), ("0.1.1", V011), ("0.1.2", V012), ("0.1.3", V012),
        ("0.2.0", V020), ("0.2.1", V021), ("0.2.2", V021), ("0.2.3", V021),
        ("0.2.4", V021), ("0.2.5", V021),
    ] {
        for external in [false, true] {
            let fixture = Fixture::new();
            let local = fixture.0.join("app-data");
            fs::create_dir(&local).unwrap();
            let library = if external { fixture.0.join("external-drive") } else { local.clone() };
            fs::create_dir_all(&library).unwrap();
            let path = library.join("library.db");
            let audio = library.join("原始音频.wav");
            fs::write(&audio, b"user audio must stay untouched").unwrap();
            let original = Connection::open(&path).unwrap();
            original.execute_batch(schema).unwrap();
            original.execute("UPDATE app_metadata SET value=?1 WHERE key='app_version'", [version]).unwrap();
            seed(&original, &audio);
            let before = snapshot(&original);
            let music_columns = columns(&original, "music");
            let old_music = rows(&original, "music", &music_columns);
            let old_tags = rows(&original, "tags", "id,name,created_at");
            let old_clips = rows(&original, "clips", "*");
            let candidates = rows(&original, "candidate_entries", "*");
            let needs_backup = version.starts_with("0.1.") || version == "0.2.0";
            save_db_path_to(&settings_path(&local), &path).unwrap();
            let settings = fs::read(settings_path(&local)).unwrap();
            assert!(fs::metadata(library.join("library.db-wal")).unwrap().len() > 0);

            let (opened_path, conn) = open_startup_database(&local).unwrap_or_else(|e| panic!("{version}, external={external}: {e}")).unwrap();
            assert_eq!(opened_path, path);
            assert_eq!(rows(&conn, "music", &music_columns), old_music, "music changed for {version}");
            assert_eq!(rows(&conn, "clips", "*"), old_clips);
            assert_eq!(rows(&conn, "tags", "id,name,created_at"), old_tags);
            assert_eq!(rows(&conn, "candidate_entries", "*"), candidates);
            assert_eq!(rows(&conn, "music_tags", "*"), rows(&original, "music_tags", "*"));
            assert_eq!(conn.query_row("SELECT tag_id FROM music_tags", [], |r| r.get::<_, i64>(0)).unwrap(), 10001);
            assert_eq!(conn.query_row("SELECT tag_id FROM clip_tags", [], |r| r.get::<_, i64>(0)).unwrap(), 10001);
            assert_eq!(conn.query_row("SELECT c.name FROM tags t JOIN tag_categories c ON c.id=t.category_id WHERE t.id=10001", [], |r| r.get::<_, String>(0)).unwrap(), "用户分类");
            if !version.starts_with("0.1.0") && !version.starts_with("0.1.1") {
                assert_eq!(conn.query_row("SELECT parent_id FROM tags WHERE id=10002", [], |r| r.get::<_, i64>(0)).unwrap(), 10001);
            }
            if !version.starts_with("0.1.") {
                assert_eq!(conn.query_row("SELECT tag_id FROM album_tags WHERE album='旧专辑'", [], |r| r.get::<_, i64>(0)).unwrap(), 10001);
            }
            assert_eq!(fs::read(settings_path(&local)).unwrap(), settings);
            assert_eq!(fs::read(&audio).unwrap(), b"user audio must stay untouched");
            assert!(conn.pragma_query_value(None, "foreign_keys", |r| r.get::<_, bool>(0)).unwrap());
            assert_eq!(conn.query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0)).unwrap(), "ok");
            let backups = fs::read_dir(&library).unwrap().map(|e| e.unwrap().path())
                .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("library.pre-")).collect::<Vec<_>>();
            assert_eq!(!backups.is_empty(), needs_backup, "backup missing for {version}");
            for backup in &backups {
                let saved = Connection::open_with_flags(backup, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
                assert_eq!(snapshot(&saved), before, "backup lost original data for {version}");
            }
            // Normal writes and subsequent launches keep using this exact same library.
            conn.execute("UPDATE music SET name='升级后继续编辑' WHERE id='old-music'", []).unwrap();
            assert_eq!(original.query_row("SELECT name FROM music", [], |r| r.get::<_, String>(0)).unwrap(), "升级后继续编辑");
            drop(conn);
            drop(original);
            let (_, reopened) = open_startup_database(&local).unwrap().unwrap();
            assert_eq!(reopened.query_row("SELECT name FROM music", [], |r| r.get::<_, String>(0)).unwrap(), "升级后继续编辑");
            assert_eq!(rows(&reopened, "clips", "*"), old_clips);
            assert_eq!(fs::read(settings_path(&local)).unwrap(), settings);
            let backup_count = fs::read_dir(&library).unwrap().map(|e| e.unwrap().path())
                .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("library.pre-")).count();
            assert_eq!(backup_count, backups.len(), "ordinary launches must not create repeated backups");
        }
    }
}

#[test]
fn an_old_default_library_without_settings_is_selected_and_reused_without_reimporting() {
    let fixture = Fixture::new();
    let path = fixture.0.join("library.db");
    let original = Connection::open(&path).unwrap();
    original.execute_batch(V010).unwrap();
    seed(&original, &fixture.0.join("audio.wav"));
    let before = snapshot(&original);
    assert!(open_startup_database(&fixture.0).unwrap().is_none());
    assert_eq!(snapshot(&original), before, "missing configuration must not choose or alter a library");
    let (opened, conn) = configure_startup_database(&fixture.0, &fixture.0, false).unwrap();
    assert_eq!(opened, path.canonicalize().unwrap());
    assert_eq!(conn.query_row("SELECT name FROM music", [], |r| r.get::<_, String>(0)).unwrap(), "用户的曲目");
    assert_eq!(conn.query_row("SELECT name FROM clips", [], |r| r.get::<_, String>(0)).unwrap(), "用户的片段");
    assert_eq!(conn.query_row("SELECT tag_id FROM music_tags", [], |r| r.get::<_, i64>(0)).unwrap(), 10001);
    drop(conn);
    let (remembered, _) = open_startup_database(&fixture.0).unwrap().unwrap();
    assert_eq!(remembered, opened);
}

#[test]
fn valid_json_tags_on_music_and_clips_are_migrated_before_removing_old_columns() {
    let fixture = Fixture::new();
    let path = fixture.0.join("library.db");
    let original = Connection::open(&path).unwrap();
    original.execute_batch(V010).unwrap();
    seed(&original, &fixture.0.join("audio.wav"));
    original.execute_batch("ALTER TABLE music ADD COLUMN tags TEXT NOT NULL DEFAULT '[]';
        ALTER TABLE clips ADD COLUMN tags TEXT NOT NULL DEFAULT '[]';
        UPDATE music SET tags='[\"用户标签\",\"旧音乐标签\"]';
        UPDATE clips SET tags='[\"用户标签\",\"旧片段标签\"]';").unwrap();
    let conn = open_selected_database(&path, false).unwrap();
    assert!(!has_column(&conn, "music", "tags") && !has_column(&conn, "clips", "tags"));
    for (table, owner_column, owner, name) in [
        ("music_tags", "music_id", "old-music", "旧音乐标签"),
        ("clip_tags", "clip_id", "old-clip", "旧片段标签"),
    ] {
        let query = format!("SELECT COUNT(*) FROM {table} j JOIN tags t ON t.id=j.tag_id WHERE j.{owner_column}=?1 AND t.name=?2");
        assert_eq!(conn.query_row(&query, params![owner, name], |r| r.get::<_, i64>(0)).unwrap(), 1);
    }
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM music_tags WHERE tag_id=10001", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM clip_tags WHERE tag_id=10001", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
}

#[test]
fn invalid_legacy_json_rolls_back_the_entire_schema_and_preserves_original_backup() {
    failed_migration_preserves_original(true);
}

#[test]
fn late_constraint_failure_rolls_back_tag_conversion_and_all_schema_changes() {
    failed_migration_preserves_original(false);
}

#[test]
fn a_newer_schema_committed_while_waiting_for_the_write_lock_is_not_migrated() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    static WAITING_FOR_WRITER: AtomicBool = AtomicBool::new(false);
    WAITING_FOR_WRITER.store(false, Ordering::Release);
    let fixture = Fixture::new();
    let path = fixture.0.join("library.db");
    let mut writer = Connection::open(&path).unwrap();
    writer.execute_batch(V021).unwrap();
    seed(&writer, &fixture.0.join("audio.wav"));
    let transaction = writer.transaction_with_behavior(TransactionBehavior::Immediate).unwrap();
    let reader = std::thread::spawn(move || {
        let mut conn = Connection::open(path).unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn.busy_handler(Some(|attempt| {
            WAITING_FOR_WRITER.store(true, Ordering::Release);
            std::thread::sleep(Duration::from_millis(1));
            attempt < 10_000
        })).unwrap();
        let result = migrate(&mut conn);
        let foreign_keys: bool = conn.pragma_query_value(None, "foreign_keys", |r| r.get(0)).unwrap();
        (result, foreign_keys)
    });
    // The busy callback proves the older migration passed its early version check
    // and is now blocked at BEGIN IMMEDIATE, without relying on a scheduling delay.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !WAITING_FOR_WRITER.load(Ordering::Acquire) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    if !WAITING_FOR_WRITER.load(Ordering::Acquire) {
        drop(transaction);
        let _ = reader.join();
        panic!("the older migration never waited for the write lock");
    }
    transaction.execute_batch("UPDATE app_metadata SET value='999' WHERE key='schema_version';
        UPDATE music SET name='新版刚保存的曲目' WHERE id='old-music';
        DROP TABLE album_tags;
        CREATE TABLE future_notes(value TEXT);
        INSERT INTO future_notes VALUES ('新版独有数据');").unwrap();
    let newer_database = snapshot(&transaction);
    transaction.commit().unwrap();

    let (result, foreign_keys) = reader.join().unwrap();
    let error = result.expect_err("an older migration must reject the schema committed while it waited");
    assert!(error.contains("999") && error.contains("高于当前程序支持"), "{error}");
    assert!(foreign_keys, "rejection must restore foreign-key enforcement");
    assert_eq!(snapshot(&writer), newer_database, "the newer schema and all user data must remain unchanged");
}

fn failed_migration_preserves_original(invalid_json: bool) {
    let fixture = Fixture::new();
    let path = fixture.0.join("library.db");
    let original = Connection::open(&path).unwrap();
    original.execute_batch(V010).unwrap();
    seed(&original, &fixture.0.join("audio.wav"));
    if invalid_json {
        original.execute_batch("ALTER TABLE music ADD COLUMN tags TEXT NOT NULL DEFAULT '[]';
            ALTER TABLE clips ADD COLUMN tags TEXT NOT NULL DEFAULT '[]';
            UPDATE music SET tags='[\"必须保留的旧标签\"]';
            UPDATE clips SET tags='invalid-json';").unwrap();
    } else {
        original.execute_batch("DROP INDEX idx_music_path;
            INSERT INTO music(id,name,path,created_at,updated_at) SELECT 'duplicate','另一个曲目',path,1,1 FROM music;").unwrap();
    }
    save_db_path_to(&settings_path(&fixture.0), &path).unwrap();
    let settings = fs::read(settings_path(&fixture.0)).unwrap();
    let before = snapshot(&original);
    let error = open_startup_database(&fixture.0).err().expect("migration must fail");
    assert!(error.contains(if invalid_json { "旧 Tag 数据无效" } else { "唯一索引失败" }), "{error}");
    assert_eq!(snapshot(&original), before, "a failed upgrade must not leave a partially migrated DB");
    assert_eq!(fs::read(settings_path(&fixture.0)).unwrap(), settings);
    let saved = Connection::open_with_flags(fixture.0.join("library.pre-tag-tree-v1.db"), OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(snapshot(&saved), before);
    // A retry must retain the first backup and take a fresh snapshot of later old-app edits.
    original.execute("UPDATE music SET name='失败后旧版继续编辑' WHERE id='old-music'", []).unwrap();
    assert!(open_startup_database(&fixture.0).is_err());
    assert_eq!(snapshot(&saved), before);
    let fresh = fs::read_dir(&fixture.0).unwrap().map(|e| e.unwrap().path())
        .find(|p| p.file_name().unwrap().to_string_lossy().starts_with("library.pre-schema-")).unwrap();
    let fresh = Connection::open_with_flags(fresh, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(snapshot(&fresh), snapshot(&original));
}
