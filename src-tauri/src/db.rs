use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const SCHEMA_VERSION: i64 = 3;

#[derive(Serialize, Deserialize, Default)]
struct AppSettings {
    db_path: Option<String>,
}

fn settings_path(dir: &Path) -> PathBuf {
    dir.join("settings.json")
}

fn load_settings(settings_file: &Path) -> Result<Option<AppSettings>, String> {
    match fs::read_to_string(settings_file) {
        Ok(raw) => serde_json::from_str(&raw).map(Some)
            .map_err(|e| format!("音乐库位置设置损坏，已停止打开音乐库，请保留并检查 {}：{e}", settings_file.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
            && matches!(settings_file.symlink_metadata(), Err(ref cause) if cause.kind() == std::io::ErrorKind::NotFound) => Ok(None),
        Err(error) => Err(format!("无法读取音乐库位置设置，已停止打开音乐库：{}：{error}", settings_file.display())),
    }
}

pub fn open_startup_database(default_dir: &Path) -> Result<Option<(PathBuf, Connection)>, String> {
    let settings_file = settings_path(default_dir);
    if let Some(settings) = load_settings(&settings_file)? {
        let path = settings.db_path.filter(|value| !value.trim().is_empty())
            .map(PathBuf::from).filter(|path| path.is_absolute())
            .ok_or_else(|| format!("音乐库位置设置缺失或无效，已停止打开音乐库，请检查 {}", settings_file.display()))?;
        // 已配置的位置只能打开现有库，绝不补建目录、创建替代库或回退默认库。
        let conn = open_selected_database(&path, false).map_err(|error| format!(
            "无法打开上次使用的音乐库：\n{}\n\n如果音乐库位于移动硬盘，请连接硬盘，并确认原 library.db 文件仍在后重试。\n\n{error}", path.display()
        ))?;
        return Ok(Some((path, conn)));
    }
    // 缺配置不能证明首次使用。交给用户选择已有库或明确创建新库。
    Ok(None)
}

pub fn configure_startup_database(default_dir: &Path, library_dir: &Path, create_new: bool) -> Result<(PathBuf, Connection), String> {
    let settings_file = settings_path(default_dir);
    if load_settings(&settings_file)?.is_some() {
        return Err("已经保存过音乐库位置，请连接原硬盘后重试，不会创建替代音乐库".into());
    }
    let dir = library_dir.canonicalize().map_err(|e| format!("所选目录不可用，请检查移动硬盘是否已连接: {e}"))?;
    if !dir.is_dir() { return Err("请选择音乐库所在的文件夹".into()); }
    let path = dir.join("library.db");
    let mode = if create_new { OpenLibraryMode::CreateNew } else { OpenLibraryMode::Existing };
    let conn = open_database_file(&path, mode)?;
    fs::create_dir_all(default_dir).map_err(|e| format!("无法保存音乐库位置: {e}"))?;
    save_db_path_to(&settings_file, &path)?;
    Ok((path, conn))
}

pub fn save_db_path(app: &tauri::AppHandle, path: &Path) -> Result<(), String> {
    use tauri::Manager;
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
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("保存数据库位置失败: {e}"))?.as_nanos();
    let temporary = settings_file.with_extension(format!("json.{}.{nonce}.tmp", std::process::id()));
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&temporary)
        .map_err(|e| format!("保存数据库位置失败: {e}"))?;
    let result = (|| {
        file.write_all(raw.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, settings_file)
    })();
    if result.is_err() { let _ = fs::remove_file(&temporary); }
    result.map_err(|e: std::io::Error| format!("保存数据库位置失败: {e}"))
}

#[cfg(test)]
fn init(path: &Path) -> Result<Connection, String> {
    open_selected_database(path, true)
}

fn backup_legacy_database(conn: &Connection, path: &std::path::Path) -> Result<(), String> {
    let backup = path.with_file_name("library.pre-tag-tree-v1.db");
    if backup.exists() { return Ok(()); }
    conn.execute("VACUUM main INTO ?1", params![backup.to_string_lossy().as_ref()])
        .map_err(|e| format!("备份旧数据库失败: {e}"))?;
    Ok(())
}

fn open_selected_database(target: &Path, create_if_missing: bool) -> Result<Connection, String> {
    open_database_file(target, if create_if_missing { OpenLibraryMode::CreateIfMissing } else { OpenLibraryMode::Existing })
}

#[derive(Clone, Copy)]
enum OpenLibraryMode { Existing, CreateIfMissing, CreateNew }

fn open_database_file(target: &Path, mode: OpenLibraryMode) -> Result<Connection, String> {
    if !matches!(mode, OpenLibraryMode::Existing) && !target.exists()
        && ["library.db-wal", "library.db-shm"].iter().any(|name| target.with_file_name(name).exists()) {
        return Err("数据库主文件缺失，但仍有 SQLite 辅助文件，请先保留并恢复原库，不会创建空库".into());
    }
    // 只在用户明确确认创建时使用 create_new，绝不截断已有文件。
    let created = if !matches!(mode, OpenLibraryMode::Existing) {
        match fs::OpenOptions::new().write(true).create_new(true).open(target) {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && matches!(mode, OpenLibraryMode::CreateIfMissing) => false,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Err("该位置已有 library.db，请选择“打开已有音乐库”，不会覆盖它".into()),
            Err(error) => return Err(format!("无法创建数据库: {error}")),
        }
    } else {
        false
    };
    // 不使用 SQLITE_OPEN_CREATE：已经存在的库在切换过程中消失时应报错，不能偷偷新建空库。
    let mut conn = Connection::open_with_flags(target, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(|e| format!("无法打开所选数据库，请检查文件和移动硬盘是否可用: {e}"))?;
    if !created {
        let check: String = conn.query_row("PRAGMA quick_check", [], |row| row.get(0))
            .map_err(|e| format!("所选数据库无效: {e}"))?;
        if check != "ok" { return Err(format!("所选数据库校验失败: {check}")); }
        for query in ["SELECT id, name, path FROM music LIMIT 0", "SELECT id, music_id, start, end FROM clips LIMIT 0"] {
            conn.prepare(query).map_err(|e| format!("所选文件不是有效的声场档案数据库: {e}"))?;
        }
        // 兼容性检查必须先于 WAL 设置和表结构升级，拒绝新版本库时不写入它。
        check_schema_version(&conn)?;
    }
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| format!("设置 WAL 失败: {e}"))?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|e| format!("设置外键失败: {e}"))?;
    if has_column(&conn, "tags", "category") || has_column(&conn, "music", "tags") || has_column(&conn, "clips", "tags") {
        backup_legacy_database(&conn, target)?;
    }
    migrate(&mut conn)?;
    Ok(conn)
}

pub fn switch_database_location(
    conn: &mut Connection,
    current_path: &mut PathBuf,
    dir: &Path,
    create_if_missing: bool,
    persist: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<String, String> {
    let dir = dir.canonicalize().map_err(|e| format!("所选目录不可用，请检查移动硬盘是否已连接: {e}"))?;
    if !dir.is_dir() { return Err("请选择数据库所在的文件夹".into()); }
    let target = dir.join("library.db");
    if target == *current_path || target.canonicalize().ok().is_some_and(|resolved| current_path.canonicalize().ok() == Some(resolved)) {
        return Err("所选位置就是当前数据库位置".into());
    }
    let new_conn = open_selected_database(&target, create_if_missing)?;
    // 设置保存成功后才发布新连接；失败时原连接和路径保持有效。两个库都不删除。
    persist(&target)?;
    *conn = new_conn;
    *current_path = target.clone();
    Ok(target.to_string_lossy().into_owned())
}

fn check_schema_version(conn: &Connection) -> Result<(), String> {
    let has_metadata: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='app_metadata')",
        [], |row| row.get(0),
    ).map_err(|e| format!("读取数据库版本失败: {e}"))?;
    if !has_metadata { return Ok(()); }
    let recorded_schema = conn.query_row(
        "SELECT value FROM app_metadata WHERE key='schema_version'", [], |row| row.get::<_, String>(0),
    ).optional().map_err(|e| format!("读取数据库版本失败: {e}"))?
        .map(|value| value.parse::<i64>().map_err(|_| "数据库版本记录无效".to_string()))
        .transpose()?.unwrap_or(0);
    if recorded_schema > SCHEMA_VERSION {
        return Err(format!("数据库版本 {recorded_schema} 高于当前程序支持的版本 {SCHEMA_VERSION}，已停止写入"));
    }
    Ok(())
}

fn migrate(conn: &mut Connection) -> Result<(), String> {
    check_schema_version(conn)?;
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
        ("last_played_at", "INTEGER"), ("play_count", "INTEGER NOT NULL DEFAULT 0"),
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
    fn missing_settings_require_explicit_selection_and_creation() {
        let dir = temp_test_dir("settings");
        let local = dir.join("app-data");
        let settings_file = settings_path(&local);
        assert!(load_settings(&settings_file).unwrap().is_none());
        assert!(open_startup_database(&local).unwrap().is_none());
        assert!(!local.exists(), "缺配置不能触发任何建库或建目录");
        let selected = dir.join("selected");
        fs::create_dir(&selected).unwrap();
        let (path, conn) = configure_startup_database(&local, &selected, true).unwrap();
        assert_eq!(path, selected.canonicalize().unwrap().join("library.db"));
        assert_eq!(load_settings(&settings_file).unwrap().unwrap().db_path.as_deref(), path.to_str());
        insert_music(&conn, "default-music");
        drop(conn);
        let (_, conn) = open_startup_database(&local).unwrap().unwrap();
        assert_eq!(music_ids(&conn), ["default-music"]);
        drop(conn);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_settings_do_not_automatically_open_even_an_existing_default_library() {
        let dir = temp_test_dir("startup-legacy-default");
        let path = dir.join("library.db");
        let conn = init(&path).unwrap();
        insert_music(&conn, "existing-default-music");
        drop(conn);
        assert!(!settings_path(&dir).exists());
        let before = fs::read(&path).unwrap();
        assert!(open_startup_database(&dir).unwrap().is_none());
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(!settings_path(&dir).exists());
        let (selected_path, conn) = configure_startup_database(&dir, &dir, false).unwrap();
        assert_eq!(music_ids(&conn), ["existing-default-music"]);
        assert_eq!(load_settings(&settings_path(&dir)).unwrap().unwrap().db_path.as_deref(), selected_path.to_str());
        drop(conn);
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dangling_settings_link_is_not_treated_as_first_startup() {
        let dir = temp_test_dir("startup-dangling-settings");
        let link = settings_path(&dir);
        std::os::unix::fs::symlink(dir.join("offline/settings.json"), &link).unwrap();
        assert!(open_startup_database(&dir).is_err());
        assert!(!dir.join("library.db").exists());
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn disconnected_drive_never_creates_directories_or_falls_back_and_can_retry() {
        let dir = temp_test_dir("startup-disconnected");
        let local = dir.join("app-data");
        fs::create_dir(&local).unwrap();
        let local_conn = init(&local.join("library.db")).unwrap();
        insert_music(&local_conn, "local-library");
        drop(local_conn);
        let mounted = dir.join("external-drive");
        fs::create_dir(&mounted).unwrap();
        let external_path = mounted.join("library.db");
        let external = init(&external_path).unwrap();
        insert_music(&external, "external-library");
        drop(external);
        let settings = settings_path(&local);
        save_db_path_to(&settings, &external_path).unwrap();
        let settings_before = fs::read(&settings).unwrap();
        let external_before = fs::read(&external_path).unwrap();
        let local_before = fs::read(local.join("library.db")).unwrap();

        let offline = dir.join("disconnected");
        fs::rename(&mounted, &offline).unwrap();
        let error = open_startup_database(&local).err().expect("未接硬盘必须停止启动");
        assert!(error.contains("请连接硬盘"), "{error}");
        assert!(error.contains(external_path.to_str().unwrap()));
        assert!(!mounted.exists(), "不能补建挂载点或父目录");
        assert_eq!(fs::read(&settings).unwrap(), settings_before);
        assert_eq!(fs::read(offline.join("library.db")).unwrap(), external_before);
        assert_eq!(fs::read(local.join("library.db")).unwrap(), local_before, "不能打开或改写默认库");
        assert!(configure_startup_database(&local, &dir, true).is_err(), "已有配置不能通过启动配置接口创建替代库");
        assert!(!dir.join("library.db").exists());
        assert_eq!(fs::read(&settings).unwrap(), settings_before);

        fs::rename(&offline, &mounted).unwrap();
        let (opened_path, conn) = open_startup_database(&local).unwrap().unwrap();
        assert_eq!(opened_path, external_path);
        assert_eq!(music_ids(&conn), ["external-library"], "重试应重新打开硬盘原库");
        drop(conn);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn configured_file_missing_in_existing_directory_is_not_recreated() {
        let dir = temp_test_dir("startup-missing-file");
        let target_dir = dir.join("external");
        fs::create_dir(&target_dir).unwrap();
        let path = target_dir.join("library.db");
        let conn = init(&path).unwrap();
        insert_music(&conn, "keep-original");
        drop(conn);
        let settings = settings_path(&dir);
        save_db_path_to(&settings, &path).unwrap();
        let saved = target_dir.join("original.db");
        fs::rename(&path, &saved).unwrap();
        let before = fs::read(&saved).unwrap();
        assert!(open_startup_database(&dir).is_err());
        assert!(!path.exists(), "父目录存在也不能新建替代数据库");
        assert!(!dir.join("library.db").exists(), "不能回退并创建默认库");
        assert_eq!(fs::read(saved).unwrap(), before);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn remembered_default_database_is_not_recreated_if_missing() {
        let dir = temp_test_dir("startup-missing-default");
        let (path, conn) = configure_startup_database(&dir, &dir, true).unwrap();
        drop(conn);
        fs::remove_file(&path).unwrap();
        assert!(open_startup_database(&dir).is_err());
        assert!(!path.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn invalid_settings_never_fall_back_or_change_the_settings() {
        let dir = temp_test_dir("startup-invalid-settings");
        let settings = settings_path(&dir);
        for invalid in ["{ 不合法 json", "{}", r#"{"db_path":null}"#, r#"{"db_path":" "}"#, r#"{"db_path":"relative/library.db"}"#] {
            fs::write(&settings, invalid).unwrap();
            assert!(open_startup_database(&dir).is_err(), "应拒绝：{invalid}");
            assert!(!dir.join("library.db").exists());
            assert_eq!(fs::read_to_string(&settings).unwrap(), invalid);
        }
        fs::remove_file(&settings).unwrap();
        fs::create_dir(&settings).unwrap();
        assert!(open_startup_database(&dir).is_err(), "设置不可读时不能当作首次启动");
        assert!(!dir.join("library.db").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn orphaned_wal_is_preserved_even_when_user_requests_a_new_library() {
        let dir = temp_test_dir("startup-orphaned-wal");
        fs::write(dir.join("library.db-wal"), b"preserve-wal").unwrap();
        assert!(open_startup_database(&dir).unwrap().is_none());
        assert!(configure_startup_database(&dir, &dir, true).is_err());
        assert!(!dir.join("library.db").exists());
        assert!(!settings_path(&dir).exists());
        assert_eq!(fs::read(dir.join("library.db-wal")).unwrap(), b"preserve-wal");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn startup_creation_never_overwrites_an_existing_library() {
        let dir = temp_test_dir("startup-create-existing");
        let local = dir.join("app-data");
        let selected = dir.join("external");
        fs::create_dir(&selected).unwrap();
        let path = selected.join("library.db");
        let original = init(&path).unwrap();
        insert_music(&original, "old-library");
        drop(original);
        let before = fs::read(&path).unwrap();
        assert!(configure_startup_database(&local, &selected, true).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(!local.exists());
        let (_, conn) = configure_startup_database(&local, &selected, false).unwrap();
        assert_eq!(music_ids(&conn), ["old-library"]);
        drop(conn);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn startup_open_existing_never_creates_a_missing_library() {
        let dir = temp_test_dir("startup-open-missing");
        let local = dir.join("app-data");
        let selected = dir.join("external");
        fs::create_dir(&selected).unwrap();
        assert!(configure_startup_database(&local, &selected, false).is_err());
        assert!(!selected.join("library.db").exists());
        assert!(!local.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    fn insert_music(conn: &Connection, id: &str) {
        conn.execute("INSERT INTO music(id, name, path, created_at, updated_at) VALUES (?1, ?1, ?1, 1, 1)", [id]).unwrap();
    }

    fn music_ids(conn: &Connection) -> Vec<String> {
        conn.prepare("SELECT id FROM music ORDER BY id").unwrap()
            .query_map([], |row| row.get(0)).unwrap().collect::<Result<_, _>>().unwrap()
    }

    #[test]
    fn switch_from_empty_library_preserves_external_music_tags_clips_and_wal() {
        let dir = temp_test_dir("switch-external");
        let source = dir.join("library.db");
        let mut current = source.clone();
        let mut conn = init(&source).unwrap();
        let external = dir.join("external");
        fs::create_dir(&external).unwrap();
        let target = external.join("library.db");
        let old_library = init(&target).unwrap();
        old_library.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        insert_music(&old_library, "old-music");
        let tag = add_tag(&old_library, "朋友整理的标签", "情绪", None);
        old_library.execute("INSERT INTO music_tags VALUES ('old-music', ?1)", [tag]).unwrap();
        old_library.execute("INSERT INTO clips VALUES ('old-clip', 'old-music', '旧片段', 1, 3, 1, 1)", []).unwrap();
        old_library.execute("INSERT INTO clip_tags VALUES ('old-clip', ?1)", [tag]).unwrap();
        old_library.execute("INSERT INTO candidate_entries VALUES ('old-clip', 'clip', 1)", []).unwrap();
        let wal = target.with_file_name("library.db-wal");
        assert!(fs::metadata(wal).unwrap().len() > 0);

        let settings_file = settings_path(&dir);
        switch_database_location(&mut conn, &mut current, &external, false, |path| save_db_path_to(&settings_file, path)).unwrap();
        assert_eq!(music_ids(&conn), ["old-music"]);
        assert_eq!(conn.query_row("SELECT name, start, end FROM clips WHERE id='old-clip'", [], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?, r.get::<_, f64>(2)?))).unwrap(), ("旧片段".into(), 1.0, 3.0));
        assert_eq!(conn.query_row("SELECT tag_id FROM music_tags WHERE music_id='old-music'", [], |r| r.get::<_, i64>(0)).unwrap(), tag);
        assert_eq!(conn.query_row("SELECT tag_id FROM clip_tags WHERE clip_id='old-clip'", [], |r| r.get::<_, i64>(0)).unwrap(), tag);
        assert_eq!(conn.query_row("SELECT target_id FROM candidate_entries", [], |r| r.get::<_, String>(0)).unwrap(), "old-clip");
        assert_eq!(load_settings(&settings_file).unwrap().unwrap().db_path.as_deref(), current.to_str());

        insert_music(&conn, "new-music");
        assert_eq!(music_ids(&old_library), ["new-music", "old-music"], "新操作应写到外置硬盘的库");
        assert!(source.exists(), "默认空库必须保留");
        assert!(music_ids(&Connection::open(&source).unwrap()).is_empty());
        switch_database_location(&mut conn, &mut current, &dir, false, |_| Ok(())).unwrap();
        assert!(music_ids(&conn).is_empty(), "切回后应重新打开原库");
        assert_eq!(music_ids(&old_library), ["new-music", "old-music"]);
        drop(conn);
        drop(old_library);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn switch_to_empty_directory_creates_empty_library_without_moving_source() {
        let dir = temp_test_dir("switch-new");
        let source = dir.join("library.db");
        let mut current = source.clone();
        let mut conn = init(&source).unwrap();
        insert_music(&conn, "source-music");
        let target_dir = dir.join("empty");
        fs::create_dir(&target_dir).unwrap();
        assert!(switch_database_location(&mut conn, &mut current, &target_dir, false, |_| Ok(())).is_err());
        assert!(!target_dir.join("library.db").exists(), "未确认创建时不能生成空库");
        assert_eq!(music_ids(&conn), ["source-music"]);
        switch_database_location(&mut conn, &mut current, &target_dir, true, |_| Ok(())).unwrap();
        assert!(music_ids(&conn).is_empty(), "创建的是空库，不是原库副本");
        assert_eq!(music_ids(&Connection::open(&source).unwrap()), ["source-music"]);
        drop(conn);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn create_confirmation_never_overwrites_a_database_that_appears_later() {
        let dir = temp_test_dir("switch-race");
        let source = dir.join("library.db");
        let mut current = source.clone();
        let mut conn = init(&source).unwrap();
        insert_music(&conn, "source-music");
        let target_dir = dir.join("target");
        fs::create_dir(&target_dir).unwrap();
        let target = target_dir.join("library.db");
        let existing = init(&target).unwrap();
        insert_music(&existing, "do-not-overwrite");
        drop(existing);
        switch_database_location(&mut conn, &mut current, &target_dir, true, |_| Ok(())).unwrap();
        assert_eq!(music_ids(&conn), ["do-not-overwrite"]);
        assert_eq!(music_ids(&Connection::open(&source).unwrap()), ["source-music"], "两个库都应保留，不复制或合并");
        drop(conn);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn invalid_or_unrelated_database_is_not_modified_and_source_stays_active() {
        let dir = temp_test_dir("switch-invalid");
        let source = dir.join("library.db");
        let mut current = source.clone();
        let mut conn = init(&source).unwrap();
        insert_music(&conn, "source-music");
        let target_dir = dir.join("invalid");
        fs::create_dir(&target_dir).unwrap();
        let target = target_dir.join("library.db");
        fs::write(&target, b"not a database").unwrap();
        assert!(switch_database_location(&mut conn, &mut current, &target_dir, false, |_| panic!("不应保存设置")).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"not a database");
        fs::remove_file(&target).unwrap();
        let other = Connection::open(&target).unwrap();
        other.execute_batch("CREATE TABLE other_data(value TEXT); INSERT INTO other_data VALUES ('keep');").unwrap();
        drop(other);
        let before = fs::read(&target).unwrap();
        assert!(switch_database_location(&mut conn, &mut current, &target_dir, false, |_| panic!("不应保存设置")).is_err());
        assert_eq!(fs::read(&target).unwrap(), before);
        assert_eq!(current, source);
        assert_eq!(music_ids(&conn), ["source-music"]);
        assert!(switch_database_location(&mut conn, &mut current, &dir.join("missing-drive"), true, |_| Ok(())).is_err());
        assert!(!dir.join("missing-drive").exists());
        drop(conn);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn newer_database_is_rejected_before_any_schema_changes() {
        let dir = temp_test_dir("switch-newer");
        let mut current = dir.join("library.db");
        let mut conn = init(&current).unwrap();
        let target_dir = dir.join("newer");
        fs::create_dir(&target_dir).unwrap();
        let target = target_dir.join("library.db");
        let newer = init(&target).unwrap();
        insert_music(&newer, "future-music");
        newer.execute("UPDATE app_metadata SET value='999' WHERE key='schema_version'", []).unwrap();
        newer.execute("DROP TABLE album_tags", []).unwrap();
        drop(newer);
        let before = fs::read(&target).unwrap();
        let error = switch_database_location(&mut conn, &mut current, &target_dir, false, |_| panic!("不应保存设置")).unwrap_err();
        assert!(error.contains("高于当前程序支持"), "{error}");
        assert_eq!(fs::read(&target).unwrap(), before);
        assert_eq!(current, dir.join("library.db"));
        drop(conn);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn settings_failure_keeps_original_connection_and_both_libraries() {
        let dir = temp_test_dir("switch-settings-failure");
        let source = dir.join("library.db");
        let mut current = source.clone();
        let mut conn = init(&source).unwrap();
        insert_music(&conn, "source-music");
        let target_dir = dir.join("target");
        fs::create_dir(&target_dir).unwrap();
        let target = target_dir.join("library.db");
        let existing = init(&target).unwrap();
        insert_music(&existing, "target-music");
        drop(existing);
        let error = switch_database_location(&mut conn, &mut current, &target_dir, false, |_| Err("设置保存失败".into())).unwrap_err();
        assert!(error.contains("设置保存失败"));
        assert_eq!(current, source);
        assert_eq!(music_ids(&conn), ["source-music"]);
        assert_eq!(music_ids(&Connection::open(&target).unwrap()), ["target-music"]);
        assert!(switch_database_location(&mut conn, &mut current, &dir, false, |_| Ok(())).unwrap_err().contains("当前数据库位置"));
        drop(conn);
        fs::remove_dir_all(dir).unwrap();
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
