use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use tauri::Manager;

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const SCHEMA_VERSION: i64 = 3;

#[derive(Serialize, Deserialize, Default)]
struct AppSettings {
    db_path: Option<String>,
}

fn settings_path(dir: &Path) -> PathBuf {
    dir.join("settings.json")
}

fn load_settings(settings_file: &Path) -> AppSettings {
    fs::read_to_string(settings_file)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn resolve_db_path(custom: Option<&str>, default_dir: &Path) -> Result<PathBuf, String> {
    match custom {
        Some(custom) if !custom.trim().is_empty() => {
            let path = PathBuf::from(custom);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("无法创建数据库目录: {e}"))?;
            }
            Ok(path)
        }
        _ => {
            fs::create_dir_all(default_dir)
                .map_err(|e| format!("无法创建数据目录: {e}"))?;
            Ok(default_dir.join("library.db"))
        }
    }
}

pub fn db_path(app: &tauri::App) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("无法获取数据目录: {e}"))?;
    let settings = load_settings(&settings_path(&dir));
    resolve_db_path(settings.db_path.as_deref(), &dir)
}

pub fn save_db_path(app: &tauri::AppHandle, path: &Path) -> Result<(), String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("无法获取数据目录: {e}"))?;
    save_db_path_to(&settings_path(&dir), path)
}

fn save_db_path_to(settings_file: &Path, path: &Path) -> Result<(), String> {
    let settings = AppSettings { db_path: Some(path.to_string_lossy().into_owned()) };
    let raw = serde_json::to_string_pretty(&settings)
        .map_err(|e| format!("序列化设置失败: {e}"))?;
    fs::write(settings_file, raw)
        .map_err(|e| format!("保存数据库位置失败: {e}"))
}

pub fn open_conn(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open(path)
        .map_err(|e| format!("无法打开数据库: {e}"))?;

    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| format!("设置 WAL 失败: {e}"))?;

    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|e| format!("设置外键失败: {e}"))?;

    Ok(conn)
}

pub fn init(path: &Path) -> Result<Connection, String> {
    let mut conn = open_conn(path)?;

    if has_column(&conn, "tags", "category")
        || has_column(&conn, "music", "tags")
        || has_column(&conn, "clips", "tags")
    {
        backup_legacy_database(&conn, path)?;
    }

    migrate(&mut conn)?;

    Ok(conn)
}

fn backup_legacy_database(conn: &Connection, path: &std::path::Path) -> Result<(), String> {
    let backup = path.with_file_name("library.pre-tag-tree-v1.db");
    if backup.exists() { return Ok(()); }
    conn.execute("VACUUM main INTO ?1", params![backup.to_string_lossy().as_ref()])
        .map_err(|e| format!("备份旧数据库失败: {e}"))?;
    Ok(())
}

pub fn migrate_database_file(conn: &Connection, target: &Path) -> Result<(), String> {
    conn.execute("VACUUM main INTO ?1", params![target.to_string_lossy().as_ref()])
        .map_err(|e| format!("迁移数据库失败: {e}"))?;
    let check = Connection::open(target)
        .map_err(|e| format!("校验新数据库失败: {e}"))?;
    if let Err(e) = check.query_row("SELECT COUNT(*) FROM music", [], |row| row.get::<_, i64>(0)) {
        let _ = fs::remove_file(target);
        return Err(format!("校验新数据库失败: {e}"));
    }
    Ok(())
}

fn migrate(conn: &mut Connection) -> Result<(), String> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS music (
            id          TEXT PRIMARY KEY,
            name        TEXT NOT NULL,
            file_name   TEXT NOT NULL DEFAULT '',
            path        TEXT NOT NULL,
            file_bookmark BLOB,
            album       TEXT DEFAULT '',
            album_source TEXT NOT NULL DEFAULT 'folder',
            artist      TEXT DEFAULT '',
            genre       TEXT DEFAULT '',
            year        TEXT DEFAULT '',
            channels    TEXT DEFAULT '',
            sample_rate INTEGER DEFAULT 0,
            bitrate     INTEGER DEFAULT 0,
            duration    REAL DEFAULT 0,
            file_size   INTEGER DEFAULT 0,
            mime_type   TEXT DEFAULT '',
            cover_art   TEXT DEFAULT '',
            created_at  INTEGER NOT NULL,
            updated_at  INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_music_name ON music(name);
        CREATE INDEX IF NOT EXISTS idx_music_album ON music(album);
        CREATE INDEX IF NOT EXISTS idx_music_artist ON music(artist);
        CREATE INDEX IF NOT EXISTS idx_music_genre ON music(genre);
        CREATE INDEX IF NOT EXISTS idx_music_year ON music(year);
        CREATE INDEX IF NOT EXISTS idx_music_created ON music(created_at);

        CREATE TABLE IF NOT EXISTS clips (
            id          TEXT PRIMARY KEY,
            music_id    TEXT NOT NULL REFERENCES music(id) ON DELETE CASCADE,
            name        TEXT NOT NULL DEFAULT '未命名片段',
            start       REAL NOT NULL CHECK(start >= 0),
            end         REAL NOT NULL CHECK(end > start),
            created_at  INTEGER NOT NULL,
            updated_at  INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_clips_music ON clips(music_id);
        CREATE INDEX IF NOT EXISTS idx_clips_created ON clips(created_at);

        CREATE TABLE IF NOT EXISTS candidate_entries (
            target_id   TEXT PRIMARY KEY,
            kind        TEXT NOT NULL CHECK(kind IN ('music', 'clip')),
            created_at  INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_candidate_created ON candidate_entries(created_at);

        CREATE TABLE IF NOT EXISTS app_metadata (
            key         TEXT PRIMARY KEY,
            value       TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS tag_categories (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            name        TEXT NOT NULL UNIQUE COLLATE NOCASE,
            created_at  INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS tags (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            name        TEXT NOT NULL COLLATE NOCASE,
            category_id INTEGER NOT NULL REFERENCES tag_categories(id) ON DELETE RESTRICT,
            parent_id   INTEGER REFERENCES tags(id) ON DELETE RESTRICT,
            created_at  INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS music_tags (
            music_id    TEXT NOT NULL REFERENCES music(id) ON DELETE CASCADE,
            tag_id      INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
            PRIMARY KEY (music_id, tag_id)
        );
        CREATE INDEX IF NOT EXISTS idx_music_tags_tag ON music_tags(tag_id);

        CREATE TABLE IF NOT EXISTS clip_tags (
            clip_id     TEXT NOT NULL REFERENCES clips(id) ON DELETE CASCADE,
            tag_id      INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
            PRIMARY KEY (clip_id, tag_id)
        );
        CREATE INDEX IF NOT EXISTS idx_clip_tags_tag ON clip_tags(tag_id);

        /* 专辑不是独立实体，用专辑名做键；Finder 改名同步时由
           sync_library_locations 把关联行迁到新名字。 */
        CREATE TABLE IF NOT EXISTS album_tags (
            album       TEXT NOT NULL,
            tag_id      INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
            PRIMARY KEY (album, tag_id)
        );
        CREATE INDEX IF NOT EXISTS idx_album_tags_tag ON album_tags(tag_id);

        CREATE TRIGGER IF NOT EXISTS trg_music_candidate_cleanup
        AFTER DELETE ON music
        BEGIN
            DELETE FROM candidate_entries WHERE kind = 'music' AND target_id = OLD.id;
        END;

        CREATE TRIGGER IF NOT EXISTS trg_clips_candidate_cleanup
        AFTER DELETE ON clips
        BEGIN
            DELETE FROM candidate_entries WHERE kind = 'clip' AND target_id = OLD.id;
        END;
        "#,
    )
    .map_err(|e| format!("建表失败: {e}"))?;

    // 旧版本的分类表以 name 为主键，tags 以 category 文本引用。先在保留
    // Tag ID 的前提下重建为维度 + 自关联节点，关联表无需改写数据。
    let recorded_schema = conn.query_row(
        "SELECT value FROM app_metadata WHERE key='schema_version'",
        [],
        |row| row.get::<_, String>(0),
    ).optional().map_err(|e| format!("读取数据库版本失败: {e}"))?
        .and_then(|value| value.parse::<i64>().ok()).unwrap_or(0);
    if recorded_schema > SCHEMA_VERSION {
        return Err(format!("数据库版本 {recorded_schema} 高于当前程序支持的版本 {SCHEMA_VERSION}，已停止写入"));
    }

    if has_column(conn, "tags", "category") { migrate_legacy_tag_tree(conn)?; }
    conn.execute_batch("CREATE UNIQUE INDEX IF NOT EXISTS idx_tags_sibling_name ON tags(category_id, COALESCE(parent_id, -1), name COLLATE NOCASE); CREATE INDEX IF NOT EXISTS idx_tags_category_parent ON tags(category_id, parent_id);")
        .map_err(|e| format!("建立 Tag 树索引失败: {e}"))?;
    if has_column(conn, "music", "tags") {
        let tx = conn.transaction().map_err(|e| format!("开启音乐 Tag 迁移事务失败: {e}"))?;
        migrate_json_tags(&tx, "music", "id", "music_tags", "music_id")?;
        tx.execute("ALTER TABLE music DROP COLUMN tags", [])
            .map_err(|e| format!("移除旧音乐 Tag 列失败: {e}"))?;
        tx.commit().map_err(|e| format!("提交音乐 Tag 迁移失败: {e}"))?;
    }
    if has_column(conn, "clips", "tags") {
        let tx = conn.transaction().map_err(|e| format!("开启片段 Tag 迁移事务失败: {e}"))?;
        migrate_json_tags(&tx, "clips", "id", "clip_tags", "clip_id")?;
        tx.execute("ALTER TABLE clips DROP COLUMN tags", [])
            .map_err(|e| format!("移除旧片段 Tag 列失败: {e}"))?;
        tx.commit().map_err(|e| format!("提交片段 Tag 迁移失败: {e}"))?;
    }
    for (column, definition) in [
        ("file_bookmark", "BLOB"), ("album_source", "TEXT NOT NULL DEFAULT 'folder'"),
        ("artist", "TEXT DEFAULT ''"), ("genre", "TEXT DEFAULT ''"), ("year", "TEXT DEFAULT ''"),
        ("channels", "TEXT DEFAULT ''"), ("sample_rate", "INTEGER DEFAULT 0"),
        ("bitrate", "INTEGER DEFAULT 0"), ("cover_art", "TEXT DEFAULT ''"),
    ] {
        if !has_column(conn, "music", column) {
            conn.execute(&format!("ALTER TABLE music ADD COLUMN {column} {definition}"), [])
                .map_err(|e| format!("补齐音乐属性失败: {e}"))?;
        }
    }
    // 首个包含该字段的正式版本将既有曲目视为“按文件夹识别”，
    // 以便启动同步后直接反映 Finder 中的专辑文件夹改名。
    conn.execute("UPDATE music SET album_source='folder' WHERE album_source IS NULL OR album_source='' OR album_source='legacy'", [])
        .map_err(|e| format!("迁移专辑来源失败: {e}"))?;
    conn.execute("CREATE UNIQUE INDEX IF NOT EXISTS idx_music_path ON music(path)", [])
        .map_err(|e| format!("建立音乐路径唯一索引失败: {e}"))?;
    seed_standard_tags(conn)?;
    conn.execute(
        "INSERT INTO app_metadata (key, value) VALUES ('app_version', ?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![APP_VERSION],
    ).map_err(|e| format!("记录应用版本失败: {e}"))?;
    conn.execute(
        "INSERT INTO app_metadata (key, value) VALUES ('schema_version', ?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![SCHEMA_VERSION.to_string()],
    ).map_err(|e| format!("记录数据库版本失败: {e}"))?;

    Ok(())
}

fn has_column(conn: &Connection, table: &str, column: &str) -> bool {
    let query = format!("PRAGMA table_info({table})");
    conn.prepare(&query)
        .and_then(|mut stmt| stmt.query_map([], |row| row.get::<_, String>(1))?.collect::<Result<Vec<_>, _>>())
        .map(|columns| columns.iter().any(|name| name == column))
        .unwrap_or(false)
}

fn seed_standard_tags(conn: &Connection) -> Result<(), String> {
    let existing: i64 = conn
        .query_row("SELECT COUNT(*) FROM tags", [], |row| row.get(0))
        .unwrap_or(0);
    if existing > 0 {
        return Ok(());
    }

    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as i64;
    for category in ["情绪", "节奏", "用途", "人声", "风格", "结构", "未分类"] {
        conn.execute("INSERT OR IGNORE INTO tag_categories (name, created_at) VALUES (?1, ?2)", params![category, now])
            .map_err(|e| format!("初始化 Tag 分类失败: {e}"))?;
    }
    for name in [
        // 情绪
        "温暖", "治愈", "紧张", "悬疑", "悲伤", "激昂", "庄重",
        // 节奏
        "舒缓", "中速", "快节奏", "卡点",
        // 用途 / 场景
        "纪录片", "剧情", "预告", "转场", "片头", "片尾",
        // 人声
        "纯音乐", "旁白友好", "人声",
        // 风格
        "电影感", "氛围", "史诗", "电子",
        // 剪辑结构
        "渐强", "高潮", "铺底", "循环友好",
    ] {
        conn.execute("INSERT OR IGNORE INTO tags (name, category_id, created_at) VALUES (?1, (SELECT id FROM tag_categories WHERE name=?2), ?3)", params![name, tag_category(name), now])
            .map_err(|e| format!("初始化预设 Tag 失败: {e}"))?;
    }
    Ok(())
}

pub(crate) fn tag_category(name: &str) -> &'static str {
    match name {
        "温暖" | "治愈" | "紧张" | "悬疑" | "悲伤" | "激昂" | "庄重" => "情绪",
        "舒缓" | "中速" | "快节奏" | "卡点" => "节奏",
        "纪录片" | "剧情" | "预告" | "转场" | "片头" | "片尾" => "用途",
        "纯音乐" | "旁白友好" | "人声" => "人声",
        "电影感" | "氛围" | "史诗" | "电子" => "风格",
        "渐强" | "高潮" | "循环友好" | "铺底" => "结构",
        _ => "",
    }
}

fn ensure_tag(conn: &Connection, name: &str, created_at: i64) -> Result<i64, String> {    /* 先复用同名 Tag，避免迁移时给已有 Tag 再造一份副本（维度不同也算同一个名字）。 */
    let existing: Option<i64> = conn
        .query_row(
            "SELECT t.id FROM tags t JOIN tag_categories c ON c.id=t.category_id
             WHERE t.name=?1 COLLATE NOCASE
             ORDER BY c.name='未分类', t.parent_id IS NOT NULL, t.id
             LIMIT 1",
            params![name],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("读取 Tag 失败: {e}"))?;
    if let Some(id) = existing {
        return Ok(id);
    }

    /* 预设名单里的 Tag 落到它所属的维度；名单外的（用户自建）进「未分类」，
       不能拿 tag_category 的空串去建维度，否则会多出一个没名字的维度。 */
    let standard = tag_category(name);
    let category = if standard.is_empty() { "未分类" } else { standard };
    conn.execute("INSERT OR IGNORE INTO tag_categories (name, created_at) VALUES (?1, ?2)", params![category, created_at])
        .map_err(|e| format!("创建 Tag 维度失败: {e}"))?;
    conn.execute(
        "INSERT OR IGNORE INTO tags (name, category_id, created_at) VALUES (?1, (SELECT id FROM tag_categories WHERE name=?2), ?3)",
        params![name, category, created_at],
    ).map_err(|e| format!("迁移 Tag 失败: {e}"))?;
    conn.query_row("SELECT id FROM tags WHERE name=?1 AND category_id=(SELECT id FROM tag_categories WHERE name=?2)", params![name, category], |row| row.get(0))
        .map_err(|e| format!("读取 Tag 失败: {e}"))
}

fn migrate_legacy_tag_tree(conn: &mut Connection) -> Result<(), String> {
    conn.pragma_update(None, "foreign_keys", "OFF").map_err(|e| format!("关闭外键失败: {e}"))?;
    let result = (|| -> Result<(), String> {
        let old_tag_count: i64 = conn.query_row("SELECT COUNT(*) FROM tags", [], |row| row.get(0))
            .map_err(|e| format!("统计旧 Tag 失败: {e}"))?;
        let tx = conn.transaction().map_err(|e| format!("开启 Tag 迁移事务失败: {e}"))?;
        tx.execute_batch("DROP TABLE IF EXISTS tag_categories_v2; DROP TABLE IF EXISTS tags_v2;")
            .map_err(|e| format!("准备 Tag 迁移失败: {e}"))?;
        tx.execute_batch("CREATE TABLE tag_categories_v2 (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE COLLATE NOCASE, created_at INTEGER NOT NULL); CREATE TABLE tags_v2 (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL COLLATE NOCASE, category_id INTEGER NOT NULL REFERENCES tag_categories_v2(id) ON DELETE RESTRICT, parent_id INTEGER REFERENCES tags_v2(id) ON DELETE RESTRICT, created_at INTEGER NOT NULL);")
            .map_err(|e| format!("创建新版 Tag 表失败: {e}"))?;
        tx.execute("INSERT INTO tag_categories_v2 (name, created_at) SELECT CASE WHEN name='' OR name='自定义' THEN '未分类' ELSE name END, MIN(created_at) FROM tag_categories GROUP BY CASE WHEN name='' OR name='自定义' THEN '未分类' ELSE name END", [])
            .map_err(|e| format!("迁移 Tag 维度失败: {e}"))?;
        tx.execute("INSERT OR IGNORE INTO tag_categories_v2 (name, created_at) SELECT CASE WHEN category='' OR category='自定义' THEN '未分类' ELSE category END, MIN(created_at) FROM tags GROUP BY CASE WHEN category='' OR category='自定义' THEN '未分类' ELSE category END", [])
            .map_err(|e| format!("补齐 Tag 维度失败: {e}"))?;
        tx.execute("INSERT INTO tags_v2 (id, name, category_id, parent_id, created_at) SELECT t.id, t.name, c.id, NULL, t.created_at FROM tags t JOIN tag_categories_v2 c ON c.name=CASE WHEN t.category='' OR t.category='自定义' THEN '未分类' ELSE t.category END", [])
            .map_err(|e| format!("迁移 Tag 节点失败: {e}"))?;
        let new_tag_count: i64 = tx.query_row("SELECT COUNT(*) FROM tags_v2", [], |row| row.get(0))
            .map_err(|e| format!("验证新版 Tag 失败: {e}"))?;
        if new_tag_count != old_tag_count { return Err("Tag 迁移数量校验失败".into()); }
        tx.execute_batch("DROP TABLE tags; DROP TABLE tag_categories; ALTER TABLE tag_categories_v2 RENAME TO tag_categories; ALTER TABLE tags_v2 RENAME TO tags; CREATE UNIQUE INDEX idx_tags_sibling_name ON tags(category_id, COALESCE(parent_id, -1), name COLLATE NOCASE); CREATE INDEX idx_tags_category_parent ON tags(category_id, parent_id);")
            .map_err(|e| format!("切换新版 Tag 表失败: {e}"))?;
        let mut check = tx.prepare("PRAGMA foreign_key_check").map_err(|e| format!("准备外键校验失败: {e}"))?;
        if check.query([]).map_err(|e| format!("执行外键校验失败: {e}"))?.next().map_err(|e| format!("读取外键校验失败: {e}"))?.is_some() {
            return Err("Tag 迁移后的外键校验失败".into());
        }
        drop(check);
        tx.commit().map_err(|e| format!("提交 Tag 迁移失败: {e}"))?;
        Ok(())
    })();
    conn.pragma_update(None, "foreign_keys", "ON").map_err(|e| format!("恢复外键失败: {e}"))?;
    result
}

fn migrate_json_tags(conn: &Connection, table: &str, id_column: &str, join_table: &str, join_id: &str) -> Result<(), String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let sql = format!("SELECT {id_column}, tags FROM {table}");
    let mut stmt = conn.prepare(&sql).map_err(|e| format!("读取旧 Tag 失败: {e}"))?;
    let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map_err(|e| format!("读取旧 Tag 失败: {e}"))?;
    for row in rows {
        let (owner_id, raw_tags) = row.map_err(|e| format!("读取旧 Tag 失败: {e}"))?;
        let names: Vec<String> = serde_json::from_str(&raw_tags).unwrap_or_default();
        for name in names.into_iter().map(|tag| tag.trim().to_string()).filter(|tag| !tag.is_empty()) {
            let tag_id = ensure_tag(conn, &name, now)?;
            let insert = format!("INSERT OR IGNORE INTO {join_table} ({join_id}, tag_id) VALUES (?1, ?2)");
            conn.execute(&insert, params![owner_id, tag_id]).map_err(|e| format!("迁移 Tag 关联失败: {e}"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_test_dir(name: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("music-manager-{name}-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn settings_roundtrip_and_fallback_to_default() {
        let dir = temp_test_dir("settings");
        let settings_file = settings_path(&dir);

        let resolved = resolve_db_path(load_settings(&settings_file).db_path.as_deref(), &dir).unwrap();
        assert_eq!(resolved, dir.join("library.db"), "缺 settings.json 时用默认路径");

        let custom = dir.join("custom").join("sub").join("library.db");
        save_db_path_to(&settings_file, &custom).unwrap();
        let resolved = resolve_db_path(load_settings(&settings_file).db_path.as_deref(), &dir).unwrap();
        assert_eq!(resolved, custom, "应读回自定义路径");
        assert!(custom.parent().unwrap().is_dir(), "自定义路径的父目录应已创建");

        std::fs::write(&settings_file, "{ 不是合法 json").unwrap();
        let resolved = resolve_db_path(load_settings(&settings_file).db_path.as_deref(), &dir).unwrap();
        assert_eq!(resolved, dir.join("library.db"), "settings.json 损坏时回退默认路径");

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn migrate_database_copies_wal_data() {
        let dir = temp_test_dir("migrate");
        let source = dir.join("library.db");
        let conn = Connection::open(&source).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.execute_batch("CREATE TABLE music(id TEXT PRIMARY KEY); INSERT INTO music VALUES ('m1');").unwrap();

        let target = dir.join("moved").join("library.db");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        migrate_database_file(&conn, &target).unwrap();

        let migrated = Connection::open(&target).unwrap();
        let count: i64 = migrated.query_row("SELECT COUNT(*) FROM music", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 1, "WAL 中未 checkpoint 的数据也要迁过去");
        drop(migrated);
        drop(conn);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn migrate_database_rejects_existing_target() {
        let dir = temp_test_dir("migrate-reject");
        let source = dir.join("library.db");
        let conn = Connection::open(&source).unwrap();
        conn.execute_batch("CREATE TABLE music(id TEXT PRIMARY KEY);").unwrap();

        let target = dir.join("target.db");
        std::fs::write(&target, b"occupied").unwrap();
        let error = migrate_database_file(&conn, &target).unwrap_err();
        assert!(error.contains("迁移数据库失败"), "{error}");
        drop(conn);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_backup_contains_committed_data() {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("music-manager-backup-test-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("library.db");
        let conn = Connection::open(&source).unwrap();
        conn.execute_batch("CREATE TABLE sample(value TEXT); INSERT INTO sample VALUES ('ok');").unwrap();

        backup_legacy_database(&conn, &source).unwrap();

        let backup = Connection::open(dir.join("library.pre-tag-tree-v1.db")).unwrap();
        let value: String = backup.query_row("SELECT value FROM sample", [], |row| row.get(0)).unwrap();
        assert_eq!(value, "ok");
        drop(backup);
        drop(conn);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn add_tag(conn: &Connection, name: &str, category: &str, parent_id: Option<i64>) -> i64 {
        conn.execute(
            "INSERT INTO tags (name, category_id, parent_id, created_at)
             VALUES (?1, (SELECT id FROM tag_categories WHERE name=?2), ?3, 0)",
            params![name, category, parent_id],
        ).unwrap();
        conn.last_insert_rowid()
    }

    fn tag_count(conn: &Connection, name: &str) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM tags WHERE name=?1", params![name], |r| r.get(0)).unwrap()
    }

    #[test]
    fn migrates_legacy_tag_schema_and_preserves_ids() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE tag_categories (
                name TEXT PRIMARY KEY COLLATE NOCASE,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE tags (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE COLLATE NOCASE,
                category TEXT NOT NULL DEFAULT '' COLLATE NOCASE REFERENCES tag_categories(name),
                created_at INTEGER NOT NULL
            );
            INSERT INTO tag_categories (name, created_at) VALUES ('情绪', 1), ('', 2);
            INSERT INTO tags (id, name, category, created_at) VALUES (42, '温暖', '情绪', 3), (77, '自建', '', 4);
            "#,
        ).unwrap();

        migrate(&mut conn).unwrap();

        assert!(!has_column(&conn, "tags", "category"));
        assert!(has_column(&conn, "tags", "category_id"));
        let rows: Vec<(i64, String)> = conn.prepare(
            "SELECT t.id, c.name FROM tags t JOIN tag_categories c ON c.id=t.category_id WHERE t.id IN (42,77) ORDER BY t.id"
        ).unwrap().query_map([], |row| Ok((row.get(0)?, row.get(1)?))).unwrap()
            .collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(rows, vec![(42, "情绪".into()), (77, "未分类".into())]);
        let version: String = conn.query_row("SELECT value FROM app_metadata WHERE key='schema_version'", [], |row| row.get(0)).unwrap();
        assert_eq!(version, SCHEMA_VERSION.to_string());
    }

    #[test]
    fn legacy_tag_migration_preserves_join_rows() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE tag_categories (name TEXT PRIMARY KEY COLLATE NOCASE, created_at INTEGER NOT NULL);
            CREATE TABLE tags (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE COLLATE NOCASE, category TEXT NOT NULL DEFAULT '' COLLATE NOCASE REFERENCES tag_categories(name), created_at INTEGER NOT NULL);
            CREATE TABLE music_tags (music_id TEXT NOT NULL, tag_id INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE, PRIMARY KEY (music_id, tag_id));
            CREATE TABLE clip_tags (clip_id TEXT NOT NULL, tag_id INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE, PRIMARY KEY (clip_id, tag_id));
            INSERT INTO tag_categories (name, created_at) VALUES ('情绪', 1);
            INSERT INTO tags (id, name, category, created_at) VALUES (42, '温暖', '情绪', 2);
            INSERT INTO music_tags (music_id, tag_id) VALUES ('m1', 42);
            INSERT INTO clip_tags (clip_id, tag_id) VALUES ('c1', 42);
            "#,
        ).unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();

        migrate_legacy_tag_tree(&mut conn).unwrap();

        let music_tag: i64 = conn.query_row("SELECT tag_id FROM music_tags WHERE music_id='m1'", [], |row| row.get(0)).unwrap();
        let clip_tag: i64 = conn.query_row("SELECT tag_id FROM clip_tags WHERE clip_id='c1'", [], |row| row.get(0)).unwrap();
        assert_eq!((music_tag, clip_tag), (42, 42));
    }

    #[test]
    fn newer_schema_is_rejected() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        conn.execute("UPDATE app_metadata SET value='999' WHERE key='schema_version'", []).unwrap();
        let error = migrate(&mut conn).unwrap_err();
        assert!(error.contains("高于当前程序支持"));
    }

    #[test]
    fn migrate_creates_album_tags_table() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='album_tags')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(exists, "迁移后应存在 album_tags 表");
        // 重复迁移（已有库升级路径）也不能报错。
        migrate(&mut conn).unwrap();
    }

    #[test]
    fn same_name_tags_in_different_categories_are_preserved() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        add_tag(&conn, "现场", "情绪", None);
        add_tag(&conn, "现场", "未分类", None);
        migrate(&mut conn).unwrap();
        assert_eq!(tag_count(&conn, "现场"), 2);
    }
}
