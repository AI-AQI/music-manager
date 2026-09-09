use rusqlite::{params, Connection};
use std::fs;
use std::path::PathBuf;
use tauri::Manager;

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn db_path(app: &tauri::App) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("无法获取数据目录: {e}"))?;

    fs::create_dir_all(&dir)
        .map_err(|e| format!("无法创建数据目录: {e}"))?;

    Ok(dir.join("library.db"))
}

pub fn init(app: &tauri::App) -> Result<Connection, String> {
    let path = db_path(app)?;

    let conn = Connection::open(&path)
        .map_err(|e| format!("无法打开数据库: {e}"))?;

    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| format!("设置 WAL 失败: {e}"))?;

    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|e| format!("设置外键失败: {e}"))?;

    migrate(&conn)?;

    Ok(conn)
}

fn migrate(conn: &Connection) -> Result<(), String> {
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
            name        TEXT PRIMARY KEY COLLATE NOCASE,
            created_at  INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS tags (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            name        TEXT NOT NULL UNIQUE COLLATE NOCASE,
            category    TEXT NOT NULL DEFAULT '' COLLATE NOCASE REFERENCES tag_categories(name),
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

    // 尚未发版，开发期仅做一次幂等初始化；不维护内部 schema 版本序列。
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as i64;
    for category in ["情绪", "节奏", "用途", "人声", "风格", "结构", ""] {
        conn.execute("INSERT OR IGNORE INTO tag_categories (name, created_at) VALUES (?1, ?2)", params![category, now])
            .map_err(|e| format!("初始化 Tag 分类失败: {e}"))?;
    }
    if has_column(conn, "music", "tags") {
        migrate_json_tags(conn, "music", "id", "music_tags", "music_id")?;
        conn.execute("ALTER TABLE music DROP COLUMN tags", [])
            .map_err(|e| format!("移除旧音乐 Tag 列失败: {e}"))?;
    }
    if has_column(conn, "clips", "tags") {
        migrate_json_tags(conn, "clips", "id", "clip_tags", "clip_id")?;
        conn.execute("ALTER TABLE clips DROP COLUMN tags", [])
            .map_err(|e| format!("移除旧片段 Tag 列失败: {e}"))?;
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
    conn.execute("INSERT OR IGNORE INTO tag_categories (name, created_at) SELECT DISTINCT category, created_at FROM tags", [])
        .map_err(|e| format!("初始化 Tag 分类失败: {e}"))?;
    conn.execute("INSERT OR IGNORE INTO tag_categories (name, created_at) VALUES ('', ?1)", params![now])
        .map_err(|e| format!("初始化未分类 Tag 失败: {e}"))?;
    conn.execute("UPDATE tags SET category='' WHERE category='自定义'", [])
        .map_err(|e| format!("迁移自定义 Tag 分类失败: {e}"))?;
    conn.execute("DELETE FROM tag_categories WHERE name='自定义'", [])
        .map_err(|e| format!("移除自定义 Tag 分类失败: {e}"))?;
    conn.execute("CREATE UNIQUE INDEX IF NOT EXISTS idx_music_path ON music(path)", [])
        .map_err(|e| format!("建立音乐路径唯一索引失败: {e}"))?;
    seed_standard_tags(conn)?;
    conn.execute(
        "INSERT INTO app_metadata (key, value) VALUES ('app_version', ?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![APP_VERSION],
    ).map_err(|e| format!("记录应用版本失败: {e}"))?;

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
    for category in ["情绪", "节奏", "用途", "人声", "风格", "结构", ""] {
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
        conn.execute("INSERT OR IGNORE INTO tags (name, category, created_at) VALUES (?1, ?2, ?3)", params![name, tag_category(name), now])
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

fn ensure_tag(conn: &Connection, name: &str, created_at: i64) -> Result<i64, String> {
    conn.execute(
        "INSERT OR IGNORE INTO tags (name, category, created_at) VALUES (?1, ?2, ?3)",
        params![name, tag_category(name), created_at],
    ).map_err(|e| format!("迁移 Tag 失败: {e}"))?;
    conn.query_row("SELECT id FROM tags WHERE name=?1", params![name], |row| row.get(0))
        .map_err(|e| format!("读取 Tag 失败: {e}"))
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
