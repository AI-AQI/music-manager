use crate::db::tag_category;
use crate::AppState;
use rusqlite::{params, params_from_iter, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};
use tauri::Manager;

#[cfg(target_os = "macos")]
fn create_file_bookmark(path: &str) -> Result<Vec<u8>, String> {
    use objc2_foundation::{NSString, NSURL, NSURLBookmarkCreationOptions};

    if !Path::new(path).is_file() {
        return Err("无法为不存在的文件创建书签".into());
    }
    let path = NSString::from_str(path);
    let url = NSURL::fileURLWithPath(&path);
    url.bookmarkDataWithOptions_includingResourceValuesForKeys_relativeToURL_error(
        NSURLBookmarkCreationOptions::empty(), None, None,
    )
    .map(|data| data.to_vec())
    .map_err(|error| format!("创建文件书签失败: {error}"))
}

#[cfg(not(target_os = "macos"))]
fn create_file_bookmark(_path: &str) -> Result<Vec<u8>, String> {
    Ok(Vec::new())
}

#[cfg(target_os = "macos")]
fn resolve_file_bookmark(bookmark: &[u8]) -> Result<(String, bool), String> {
    use objc2_foundation::{NSData, NSURL, NSURLBookmarkResolutionOptions};

    if bookmark.is_empty() {
        return Err("没有可用的文件书签".into());
    }
    let data = NSData::with_bytes(bookmark);
    let url = unsafe {
        NSURL::URLByResolvingBookmarkData_options_relativeToURL_bookmarkDataIsStale_error(
            &data,
            NSURLBookmarkResolutionOptions::WithoutUI,
            None,
            std::ptr::null_mut(),
        )
    }
    .map_err(|error| format!("解析文件书签失败: {error}"))?;
    let path = url.path().ok_or_else(|| "书签没有返回本地路径".to_string())?.to_string();
    Ok((path, false))
}

#[cfg(not(target_os = "macos"))]
fn resolve_file_bookmark(_bookmark: &[u8]) -> Result<(String, bool), String> {
    Err("当前系统不支持 macOS 文件书签".into())
}

macro_rules! lock_db {
    ($state:ident) => {
        $state
            .db
            .lock()
            .map_err(|e| format!("数据库锁获取失败: {e}"))?
    };
}

fn mime_from_ext(path: &str) -> String {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();

    match ext.as_str() {
        "mp3" => "audio/mpeg".into(),
        "wav" => "audio/wav".into(),
        "flac" => "audio/flac".into(),
        "m4a" => "audio/mp4".into(),
        "aac" => "audio/aac".into(),
        "ogg" => "audio/ogg".into(),
        "opus" => "audio/opus".into(),
        _ => "audio/*".into(),
    }
}

fn sync_tags(conn: &rusqlite::Connection, owner_id: &str, tags: &[String], join_table: &str, owner_column: &str) -> Result<(), String> {
    let delete = format!("DELETE FROM {join_table} WHERE {owner_column}=?1");
    conn.execute(&delete, params![owner_id]).map_err(|e| format!("清理 Tag 关联失败: {e}"))?;
    for name in tags.iter().map(|tag| tag.trim()).filter(|tag| !tag.is_empty()) {
        conn.execute("INSERT OR IGNORE INTO tags (name, category, created_at) VALUES (?1, ?2, ?3)", params![name, tag_category(name), now_ms()])
            .map_err(|e| format!("保存 Tag 失败: {e}"))?;
        let tag_id: i64 = conn.query_row("SELECT id FROM tags WHERE name=?1", params![name], |row| row.get(0))
            .map_err(|e| format!("读取 Tag 失败: {e}"))?;
        let insert = format!("INSERT OR IGNORE INTO {join_table} ({owner_column}, tag_id) VALUES (?1, ?2)");
        conn.execute(&insert, params![owner_id, tag_id]).map_err(|e| format!("保存 Tag 关联失败: {e}"))?;
    }
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as i64
}

fn load_tags(conn: &rusqlite::Connection, owner_id: &str, join_table: &str, owner_column: &str) -> Result<Vec<String>, String> {
    let sql = format!("SELECT t.name FROM tags t JOIN {join_table} j ON j.tag_id=t.id WHERE j.{owner_column}=?1 ORDER BY t.name COLLATE NOCASE");
    let mut stmt = conn.prepare(&sql).map_err(|e| format!("读取 Tag 失败: {e}"))?;
    let tags = stmt.query_map(params![owner_id], |row| row.get(0))
        .map_err(|e| format!("读取 Tag 失败: {e}"))?
        .collect::<Result<Vec<String>, _>>()
        .map_err(|e| format!("读取 Tag 失败: {e}"))?;
    Ok(tags)
}

fn validate_clip_range(start: f64, end: f64) -> Result<(), String> {
    if !start.is_finite() || start < 0.0 {
        return Err("片段起点不能为负数".into());
    }
    if !end.is_finite() || end <= start {
        return Err("片段终点必须大于起点".into());
    }
    Ok(())
}

fn load_all_tags(conn: &rusqlite::Connection, join_table: &str, owner_column: &str) -> Result<HashMap<String, Vec<String>>, String> {
    let sql = format!("SELECT j.{owner_column}, t.name FROM tags t JOIN {join_table} j ON j.tag_id = t.id ORDER BY t.name COLLATE NOCASE");
    let mut stmt = conn.prepare(&sql).map_err(|e| format!("读取 Tag 失败: {e}"))?;
    let rows = stmt
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map_err(|e| format!("读取 Tag 失败: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("读取 Tag 失败: {e}"))?;
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for (owner, tag) in rows {
        map.entry(owner).or_default().push(tag);
    }
    Ok(map)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeResult {
    path: String,
    name: String,
    file_name: String,
    file_size: u64,
    mime_type: String,
    exists: bool,
    album: String,
    artist: String,
    genre: String,
    year: String,
    channels: String,
    sample_rate: u32,
    bitrate: u32,
    duration: f64,
    folder_album: String,
    cover_art: String,
}

#[derive(Default)]
struct Mp3Metadata {
    title: String,
    artist: String,
    album: String,
    genre: String,
    year: String,
    channels: String,
    sample_rate: u32,
    bitrate: u32,
    duration: f64,
    cover_art: String,
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let value = (chunk[0] as u32) << 16 | (chunk.get(1).copied().unwrap_or(0) as u32) << 8 | chunk.get(2).copied().unwrap_or(0) as u32;
        result.push(TABLE[((value >> 18) & 0x3f) as usize] as char);
        result.push(TABLE[((value >> 12) & 0x3f) as usize] as char);
        result.push(if chunk.len() > 1 { TABLE[((value >> 6) & 0x3f) as usize] as char } else { '=' });
        result.push(if chunk.len() > 2 { TABLE[(value & 0x3f) as usize] as char } else { '=' });
    }
    result
}

fn parse_apic(frame: &[u8]) -> Option<String> {
    if frame.len() < 4 { return None; }
    let encoding = frame[0];
    let mime_end = frame[1..].iter().position(|byte| *byte == 0)? + 1;
    let mime = std::str::from_utf8(&frame[1..mime_end]).ok()?.to_ascii_lowercase();
    let image_mime = match mime.as_str() {
        "image/jpeg" | "image/jpg" => "image/jpeg",
        "image/png" => "image/png",
        _ => return None,
    };
    let description_start = mime_end + 1;
    if description_start >= frame.len() { return None; }
    let image_start = if encoding == 1 || encoding == 2 {
        (description_start..frame.len().saturating_sub(1)).step_by(2)
            .find(|index| frame[*index] == 0 && frame[*index + 1] == 0)
            .map(|index| index + 2)?
    } else {
        frame[description_start..].iter().position(|byte| *byte == 0).map(|index| description_start + index + 1)?
    };
    let image = frame.get(image_start..)?;
    if image.is_empty() || image.len() > 2 * 1024 * 1024 { return None; }
    Some(format!("data:{image_mime};base64,{}", base64_encode(image)))
}

fn synchsafe(bytes: &[u8]) -> usize {
    if bytes.len() < 4 { return 0; }
    ((bytes[0] as usize & 0x7f) << 21)
        | ((bytes[1] as usize & 0x7f) << 14)
        | ((bytes[2] as usize & 0x7f) << 7)
        | (bytes[3] as usize & 0x7f)
}

fn decode_id3_text(bytes: &[u8]) -> String {
    if bytes.len() < 2 { return String::new(); }
    let value = &bytes[1..];
    match bytes[0] {
        0 | 3 => String::from_utf8_lossy(value).trim_matches(char::from(0)).trim().to_string(),
        1 | 2 => {
            let start = if bytes[0] == 1 && value.len() >= 2 && ((value[0] == 0xfe && value[1] == 0xff) || (value[0] == 0xff && value[1] == 0xfe)) { 2 } else { 0 };
            let little_endian = bytes[0] == 1 && value.get(0..2) == Some(&[0xff, 0xfe][..]);
            let units: Vec<u16> = value[start..].chunks_exact(2).map(|pair| {
                if little_endian { u16::from_le_bytes([pair[0], pair[1]]) } else { u16::from_be_bytes([pair[0], pair[1]]) }
            }).take_while(|unit| *unit != 0).collect();
            String::from_utf16_lossy(&units).trim().to_string()
        }
        _ => String::new(),
    }
}

fn parse_mp3_metadata(path: &Path, file_size: u64) -> Mp3Metadata {
    let mut file = match File::open(path) { Ok(file) => file, Err(_) => return Mp3Metadata::default() };
    let mut header = [0u8; 10];
    if file.read_exact(&mut header).is_err() { return Mp3Metadata::default(); }
    let mut metadata = Mp3Metadata::default();
    let mut audio_offset = 0usize;

    if &header[0..3] == b"ID3" {
        let version = header[3];
        let tag_size = synchsafe(&header[6..10]);
        audio_offset = 10 + tag_size;
        if tag_size <= 16 * 1024 * 1024 {
            let mut tag = vec![0u8; tag_size];
            if file.read_exact(&mut tag).is_ok() {
                let mut pos = 0usize;
                while pos + 10 <= tag.len() {
                    let id = &tag[pos..pos + 4];
                    if id.iter().all(|byte| *byte == 0) { break; }
                    let size = if version == 4 { synchsafe(&tag[pos + 4..pos + 8]) } else { u32::from_be_bytes([tag[pos + 4], tag[pos + 5], tag[pos + 6], tag[pos + 7]]) as usize };
                    if size == 0 || pos + 10 + size > tag.len() { break; }
                    let frame = &tag[pos + 10..pos + 10 + size];
                    let text = decode_id3_text(frame);
                    match id {
                        b"TIT2" => metadata.title = text,
                        b"TPE1" => metadata.artist = text,
                        b"TALB" => metadata.album = text,
                        b"TCON" => metadata.genre = text,
                        b"TDRC" | b"TYER" => metadata.year = text.chars().take(4).collect(),
                        b"APIC" if metadata.cover_art.is_empty() => metadata.cover_art = parse_apic(frame).unwrap_or_default(),
                        _ => {}
                    }
                    pos += 10 + size;
                }
            }
        }
    }

    if file.seek(SeekFrom::Start(audio_offset as u64)).is_err() { return metadata; }
    let mut data = vec![0u8; 64 * 1024];
    let read = file.read(&mut data).unwrap_or(0);
    data.truncate(read);
    for index in 0..data.len().saturating_sub(3) {
        if data[index] != 0xff || data[index + 1] & 0xe0 != 0xe0 { continue; }
        let version_bits = (data[index + 1] >> 3) & 0x03;
        let layer_bits = (data[index + 1] >> 1) & 0x03;
        let bitrate_index = (data[index + 2] >> 4) & 0x0f;
        let sample_index = (data[index + 2] >> 2) & 0x03;
        if version_bits == 1 || layer_bits == 0 || bitrate_index == 0 || bitrate_index == 15 || sample_index == 3 { continue; }
        let mpeg1 = version_bits == 3;
        let layer3 = layer_bits == 1;
        let bitrate_table: [u32; 16] = if mpeg1 && layer3 {
            [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0]
        } else if layer3 {
            [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0]
        } else {
            [0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 0]
        };
        let base_rates = [44_100u32, 48_000, 32_000];
        let mut sample_rate = base_rates[sample_index as usize];
        if version_bits == 2 { sample_rate /= 2; }
        if version_bits == 0 { sample_rate /= 4; }
        metadata.sample_rate = sample_rate;
        metadata.bitrate = bitrate_table[bitrate_index as usize];
        metadata.channels = if (data[index + 3] >> 6) == 3 { "单声道".into() } else { "立体声".into() };
        if metadata.bitrate > 0 { metadata.duration = file_size.saturating_sub(audio_offset as u64) as f64 * 8.0 / (metadata.bitrate as f64 * 1000.0); }
        break;
    }
    metadata
}

fn probe_path(path: String) -> ProbeResult {
    let path_obj = Path::new(&path);
    let file_name = path_obj.file_name().and_then(|n| n.to_str()).unwrap_or(&path).to_string();
    let fallback_name = file_name.rsplit_once('.').map(|(stem, _)| stem.to_string()).unwrap_or_else(|| file_name.clone());
    let meta = fs::metadata(&path);
    let (file_size, exists) = match meta { Ok(m) if m.is_file() => (m.len(), true), _ => (0, false) };
    let mp3 = if exists && path_obj.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("mp3")) { parse_mp3_metadata(path_obj, file_size) } else { Mp3Metadata::default() };
    let mime_type = mime_from_ext(path_obj.to_str().unwrap_or_default());
    let folder_album = path_obj.parent().and_then(Path::file_name).and_then(|name| name.to_str()).unwrap_or_default().to_string();
    ProbeResult {
        path,
        name: if mp3.title.is_empty() { fallback_name } else { mp3.title },
        file_name,
        file_size,
        mime_type,
        exists,
        album: mp3.album,
        artist: mp3.artist,
        genre: mp3.genre,
        year: mp3.year,
        channels: mp3.channels,
        sample_rate: mp3.sample_rate,
        bitrate: mp3.bitrate,
        duration: mp3.duration,
        folder_album,
        cover_art: mp3.cover_art,
    }
}

#[tauri::command]
pub fn init_db() -> Result<(), String> {
    // 数据库连接在 setup 阶段已建立并建表，这里确认就绪即可
    Ok(())
}

#[tauri::command]
pub fn probe_file(path: String) -> ProbeResult {
    probe_path(path)
}

fn collect_audio_files(folder: &Path, files: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(folder) { Ok(entries) => entries, Err(_) => return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() { collect_audio_files(&path, files); }
        else if path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("mp3")) { files.push(path); }
    }
}

#[tauri::command]
pub fn scan_import_sources(paths: Vec<String>) -> Vec<ProbeResult> {
    let mut files = Vec::new();
    for raw_path in paths {
        let path = PathBuf::from(raw_path);
        if path.is_dir() { collect_audio_files(&path, &mut files); }
        else if path.is_file() && path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("mp3")) { files.push(path); }
    }
    files.sort();
    files.dedup();
    files.into_iter().map(|path| probe_path(path.to_string_lossy().into_owned())).filter(|item| item.exists).collect()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Music {
    id: String,
    name: String,
    file_name: String,
    path: String,
    album: String,
    album_source: String,
    artist: String,
    genre: String,
    year: String,
    channels: String,
    sample_rate: u32,
    bitrate: u32,
    tags: Vec<String>,
    duration: f64,
    file_size: u64,
    mime_type: String,
    cover_art: String,
    created_at: i64,
    updated_at: i64,
}

impl Music {
    fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Music> {
        Ok(Music {
            id: row.get("id")?,
            name: row.get("name")?,
            file_name: row.get("file_name")?,
            path: row.get("path")?,
            album: row.get("album")?,
            album_source: row.get("album_source")?,
            artist: row.get("artist")?,
            genre: row.get("genre")?,
            year: row.get("year")?,
            channels: row.get("channels")?,
            sample_rate: row.get("sample_rate")?,
            bitrate: row.get("bitrate")?,
            tags: Vec::new(),
            duration: row.get("duration")?,
            file_size: row.get("file_size")?,
            mime_type: row.get("mime_type")?,
            cover_art: row.get("cover_art")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MusicInput {
    id: String,
    name: String,
    file_name: String,
    path: String,
    album: String,
    album_source: String,
    artist: String,
    genre: String,
    year: String,
    channels: String,
    sample_rate: u32,
    bitrate: u32,
    tags: Vec<String>,
    duration: f64,
    file_size: u64,
    mime_type: String,
    cover_art: String,
    created_at: i64,
    updated_at: i64,
}

#[tauri::command]
pub fn create_music(
    state: tauri::State<'_, AppState>,
    music: MusicInput,
) -> Result<(), String> {
    let mut conn = lock_db!(state);
    let tx = conn.transaction().map_err(|e| format!("开启事务失败: {e}"))?;

    let bookmark = create_file_bookmark(&music.path)?;
    tx.execute(
        "INSERT INTO music (id, name, file_name, path, file_bookmark, album, album_source, artist, genre, year, channels, sample_rate, bitrate, duration, file_size, mime_type, cover_art, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
        params![
            music.id,
            music.name,
            music.file_name,
            music.path,
            bookmark,
            music.album,
            music.album_source,
            music.artist,
            music.genre,
            music.year,
            music.channels,
            music.sample_rate,
            music.bitrate,
            music.duration,
            music.file_size,
            music.mime_type,
            music.cover_art,
            music.created_at,
            music.updated_at,
        ],
    )
    .map_err(|e| format!("插入音乐失败: {e}"))?;

    sync_tags(&tx, &music.id, &music.tags, "music_tags", "music_id")?;

    tx.commit().map_err(|e| format!("提交事务失败: {e}"))?;

    Ok(())
}

#[tauri::command]
pub fn update_music(
    state: tauri::State<'_, AppState>,
    music: MusicInput,
) -> Result<(), String> {
    let mut conn = lock_db!(state);
    let tx = conn.transaction().map_err(|e| format!("开启事务失败: {e}"))?;

    let bookmark = create_file_bookmark(&music.path)?;
    tx.execute(
        "UPDATE music SET name=?1, file_name=?2, path=?3, file_bookmark=?4, album=?5, album_source=?6, artist=?7, genre=?8, year=?9, channels=?10, sample_rate=?11, bitrate=?12, duration=?13, file_size=?14, mime_type=?15, cover_art=?16, updated_at=?17 WHERE id=?18",
        params![
            music.name,
            music.file_name,
            music.path,
            bookmark,
            music.album,
            music.album_source,
            music.artist,
            music.genre,
            music.year,
            music.channels,
            music.sample_rate,
            music.bitrate,
            music.duration,
            music.file_size,
            music.mime_type,
            music.cover_art,
            music.updated_at,
            music.id,
        ],
    )
    .map_err(|e| format!("更新音乐失败: {e}"))?;

    sync_tags(&tx, &music.id, &music.tags, "music_tags", "music_id")?;

    tx.commit().map_err(|e| format!("提交事务失败: {e}"))?;

    Ok(())
}

#[tauri::command]
pub fn delete_music(
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    let conn = lock_db!(state);

    conn.execute(
        "DELETE FROM music WHERE id=?1",
        params![id],
    )
    .map_err(|e| format!("删除音乐失败: {e}"))?;

    Ok(())
}

#[tauri::command]
pub fn delete_music_batch(state: tauri::State<'_, AppState>, ids: Vec<String>) -> Result<(), String> {
    let mut conn = lock_db!(state);
    let tx = conn.transaction().map_err(|e| format!("开启批量删除音乐事务失败: {e}"))?;
    for id in ids {
        tx.execute("DELETE FROM music WHERE id=?1", params![id])
            .map_err(|e| format!("批量删除音乐失败: {e}"))?;
    }
    tx.commit().map_err(|e| format!("提交批量删除音乐失败: {e}"))?;
    Ok(())
}

#[tauri::command]
pub fn get_music(
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<Option<Music>, String> {
    let conn = lock_db!(state);

    let mut music = conn
        .query_row(
            "SELECT * FROM music WHERE id=?1",
            params![id],
            Music::from_row,
        )
        .optional()
        .map_err(|e| format!("查询音乐失败: {e}"))?;

    if let Some(item) = music.as_mut() {
        item.tags = load_tags(&conn, &item.id, "music_tags", "music_id")?;
    }
    Ok(music)
}

#[tauri::command]
pub fn list_music(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<Music>, String> {
    let conn = lock_db!(state);

    let mut stmt = conn
        .prepare("SELECT * FROM music ORDER BY created_at ASC")
        .map_err(|e| format!("准备查询失败: {e}"))?;

    let mut music = stmt
        .query_map([], Music::from_row)
        .map_err(|e| format!("查询音乐失败: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("读取音乐失败: {e}"))?;

    let tag_map = load_all_tags(&conn, "music_tags", "music_id")?;
    for item in &mut music {
        item.tags = tag_map.get(&item.id).cloned().unwrap_or_default();
    }
    Ok(music)
}

#[tauri::command]
pub fn find_existing_music_paths(
    state: tauri::State<'_, AppState>,
    paths: Vec<String>,
) -> Result<Vec<String>, String> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }

    let conn = lock_db!(state);
    let mut existing_paths = Vec::new();

    // SQLite 默认每条语句最多 999 个绑定参数，按批次查询以支持大文件夹导入。
    for batch in paths.chunks(900) {
        let placeholders = std::iter::repeat("?")
            .take(batch.len())
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!("SELECT path FROM music WHERE path IN ({placeholders})");
        let mut stmt = conn
            .prepare(&query)
            .map_err(|e| format!("准备已导入路径查询失败: {e}"))?;
        let rows = stmt
            .query_map(params_from_iter(batch.iter()), |row| row.get::<_, String>(0))
            .map_err(|e| format!("查询已导入路径失败: {e}"))?;
        existing_paths.extend(
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("读取已导入路径失败: {e}"))?,
        );
    }

    Ok(existing_paths)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Clip {
    id: String,
    music_id: String,
    name: String,
    start: f64,
    end: f64,
    tags: Vec<String>,
    created_at: i64,
    updated_at: i64,
}

impl Clip {
    fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Clip> {
        Ok(Clip {
            id: row.get("id")?,
            music_id: row.get("music_id")?,
            name: row.get("name")?,
            start: row.get("start")?,
            end: row.get("end")?,
            tags: Vec::new(),
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipInput {
    id: String,
    music_id: String,
    name: String,
    start: f64,
    end: f64,
    tags: Vec<String>,
    created_at: i64,
    updated_at: i64,
}

#[tauri::command]
pub fn create_clip(
    state: tauri::State<'_, AppState>,
    clip: ClipInput,
) -> Result<(), String> {
    validate_clip_range(clip.start, clip.end)?;
    let mut conn = lock_db!(state);
    let tx = conn.transaction().map_err(|e| format!("开启事务失败: {e}"))?;

    tx.execute(
        "INSERT INTO clips (id, music_id, name, start, end, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            clip.id,
            clip.music_id,
            clip.name,
            clip.start,
            clip.end,
            clip.created_at,
            clip.updated_at
        ],
    )
    .map_err(|e| format!("插入片段失败: {e}"))?;

    sync_tags(&tx, &clip.id, &clip.tags, "clip_tags", "clip_id")?;

    tx.commit().map_err(|e| format!("提交事务失败: {e}"))?;

    Ok(())
}

#[tauri::command]
pub fn update_clip(
    state: tauri::State<'_, AppState>,
    clip: ClipInput,
) -> Result<(), String> {
    validate_clip_range(clip.start, clip.end)?;
    let mut conn = lock_db!(state);
    let tx = conn.transaction().map_err(|e| format!("开启事务失败: {e}"))?;

    tx.execute(
        "UPDATE clips SET name=?1, start=?2, end=?3, updated_at=?4 WHERE id=?5",
        params![
            clip.name,
            clip.start,
            clip.end,
            clip.updated_at,
            clip.id
        ],
    )
    .map_err(|e| format!("更新片段失败: {e}"))?;

    sync_tags(&tx, &clip.id, &clip.tags, "clip_tags", "clip_id")?;

    tx.commit().map_err(|e| format!("提交事务失败: {e}"))?;

    Ok(())
}

#[tauri::command]
pub fn delete_clip(
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    let conn = lock_db!(state);

    conn.execute(
        "DELETE FROM clips WHERE id=?1",
        params![id],
    )
    .map_err(|e| format!("删除片段失败: {e}"))?;

    Ok(())
}

#[tauri::command]
pub fn delete_clip_batch(state: tauri::State<'_, AppState>, ids: Vec<String>) -> Result<(), String> {
    let mut conn = lock_db!(state);
    let tx = conn.transaction().map_err(|e| format!("开启批量删除片段事务失败: {e}"))?;
    for id in ids {
        tx.execute("DELETE FROM clips WHERE id=?1", params![id])
            .map_err(|e| format!("批量删除片段失败: {e}"))?;
    }
    tx.commit().map_err(|e| format!("提交批量删除片段失败: {e}"))?;
    Ok(())
}

#[tauri::command]
pub fn get_clip(
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<Option<Clip>, String> {
    let conn = lock_db!(state);

    let mut clip = conn
        .query_row(
            "SELECT * FROM clips WHERE id=?1",
            params![id],
            Clip::from_row,
        )
        .optional()
        .map_err(|e| format!("查询片段失败: {e}"))?;

    if let Some(item) = clip.as_mut() {
        item.tags = load_tags(&conn, &item.id, "clip_tags", "clip_id")?;
    }
    Ok(clip)
}

#[tauri::command]
pub fn list_clips(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<Clip>, String> {
    let conn = lock_db!(state);

    let mut stmt = conn
        .prepare("SELECT * FROM clips ORDER BY created_at ASC")
        .map_err(|e| format!("准备查询失败: {e}"))?;

    let mut clips = stmt
        .query_map([], Clip::from_row)
        .map_err(|e| format!("查询片段失败: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("读取片段失败: {e}"))?;

    let tag_map = load_all_tags(&conn, "clip_tags", "clip_id")?;
    for item in &mut clips {
        item.tags = tag_map.get(&item.id).cloned().unwrap_or_default();
    }
    Ok(clips)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateEntry {
    target_id: String,
    kind: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TagRecord {
    id: i64,
    name: String,
    category: String,
    music_count: i64,
}

#[tauri::command]
pub fn list_tags(state: tauri::State<'_, AppState>) -> Result<Vec<TagRecord>, String> {
    let conn = lock_db!(state);
    let mut stmt = conn.prepare(
        "SELECT t.id, t.name, t.category, COUNT(DISTINCT mt.music_id) + COUNT(DISTINCT ct.clip_id) AS music_count \
         FROM tags t LEFT JOIN music_tags mt ON mt.tag_id = t.id \
         LEFT JOIN clip_tags ct ON ct.tag_id = t.id \
         GROUP BY t.id, t.name, t.category \
         ORDER BY t.category, t.name COLLATE NOCASE"
    )
        .map_err(|e| format!("准备 Tag 查询失败: {e}"))?;
    let records = stmt.query_map([], |row| Ok(TagRecord { id: row.get(0)?, name: row.get(1)?, category: row.get(2)?, music_count: row.get(3)? }))
        .map_err(|e| format!("查询 Tag 失败: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("读取 Tag 失败: {e}"))?;
    Ok(records)
}

fn validate_tag(name: &str, category: &str) -> Result<(String, String), String> {
    let name = name.trim().to_string();
    let category = category.trim().to_string();
    if name.is_empty() { return Err("Tag 名称不能为空".into()); }
    Ok((name, category))
}

#[tauri::command]
pub fn create_tag(state: tauri::State<'_, AppState>, name: String, category: String) -> Result<TagRecord, String> {
    let (name, category) = validate_tag(&name, &category)?;
    let conn = lock_db!(state);
    conn.execute("INSERT OR IGNORE INTO tag_categories (name, created_at) VALUES (?1, ?2)", params![category, now_ms()])
        .map_err(|e| format!("创建 Tag 分类失败: {e}"))?;
    conn.execute("INSERT OR IGNORE INTO tags (name, category, created_at) VALUES (?1, ?2, ?3)", params![name, category, now_ms()])
        .map_err(|e| format!("创建 Tag 失败: {e}"))?;
    let (id, category) = conn.query_row("SELECT id, category FROM tags WHERE name=?1", params![name], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(|e| format!("读取 Tag 失败: {e}"))?;
    Ok(TagRecord { id, name, category, music_count: 0 })
}

#[tauri::command]
pub fn update_tag(state: tauri::State<'_, AppState>, id: i64, name: String, category: String) -> Result<(), String> {
    let (name, category) = validate_tag(&name, &category)?;
    let conn = lock_db!(state);
    conn.execute("INSERT OR IGNORE INTO tag_categories (name, created_at) VALUES (?1, ?2)", params![category, now_ms()])
        .map_err(|e| format!("创建 Tag 分类失败: {e}"))?;
    let changed = conn.execute("UPDATE tags SET name=?1, category=?2 WHERE id=?3", params![name, category, id])
        .map_err(|e| format!("更新 Tag 失败: {e}"))?;
    if changed == 0 { return Err("Tag 不存在".into()); }
    Ok(())
}

#[tauri::command]
pub fn list_tag_categories(state: tauri::State<'_, AppState>) -> Result<Vec<String>, String> {
    let conn = lock_db!(state);
    let mut stmt = conn.prepare("SELECT name FROM tag_categories WHERE name != '' ORDER BY name COLLATE NOCASE")
        .map_err(|e| format!("准备分类查询失败: {e}"))?;
    let categories = stmt.query_map([], |row| row.get(0))
        .map_err(|e| format!("查询分类失败: {e}"))?
        .collect::<Result<Vec<String>, _>>()
        .map_err(|e| format!("读取分类失败: {e}"))?;
    Ok(categories)
}

#[tauri::command]
pub fn create_tag_category(state: tauri::State<'_, AppState>, name: String) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() { return Err("分类名称不能为空".into()); }
    let conn = lock_db!(state);
    conn.execute("INSERT INTO tag_categories (name, created_at) VALUES (?1, ?2)", params![name, now_ms()])
        .map_err(|e| format!("创建分类失败: {e}"))?;
    Ok(())
}

#[tauri::command]
pub fn update_tag_category(state: tauri::State<'_, AppState>, old_name: String, name: String) -> Result<(), String> {
    let old_name = old_name.trim();
    let name = name.trim();
    if old_name.is_empty() || name.is_empty() { return Err("分类名称不能为空".into()); }
    if old_name.eq_ignore_ascii_case(name) { return Ok(()); }

    let mut conn = lock_db!(state);
    let tx = conn.transaction().map_err(|e| format!("开启分类更新事务失败: {e}"))?;
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM tag_categories WHERE name=?1)",
        params![old_name],
        |row| row.get(0),
    ).map_err(|e| format!("读取分类失败: {e}"))?;
    if !exists { return Err("分类不存在".into()); }
    tx.execute("INSERT INTO tag_categories (name, created_at) VALUES (?1, ?2)", params![name, now_ms()])
        .map_err(|e| format!("创建新分类失败: {e}"))?;
    tx.execute("UPDATE tags SET category=?1 WHERE category=?2", params![name, old_name])
        .map_err(|e| format!("迁移分类下 Tag 失败: {e}"))?;
    tx.execute("DELETE FROM tag_categories WHERE name=?1", params![old_name])
        .map_err(|e| format!("删除旧分类失败: {e}"))?;
    tx.commit().map_err(|e| format!("保存分类修改失败: {e}"))?;
    Ok(())
}

#[tauri::command]
pub fn delete_tag_category(state: tauri::State<'_, AppState>, name: String) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() { return Err("分类名称不能为空".into()); }

    let mut conn = lock_db!(state);
    let tx = conn.transaction().map_err(|e| format!("开启分类删除事务失败: {e}"))?;
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM tag_categories WHERE name=?1)",
        params![name],
        |row| row.get(0),
    ).map_err(|e| format!("读取分类失败: {e}"))?;
    if !exists { return Err("分类不存在".into()); }
    tx.execute("INSERT OR IGNORE INTO tag_categories (name, created_at) VALUES ('', ?1)", params![now_ms()])
        .map_err(|e| format!("确保未分类 Tag 失败: {e}"))?;
    tx.execute("UPDATE tags SET category='' WHERE category=?1", params![name])
        .map_err(|e| format!("迁移分类下 Tag 失败: {e}"))?;
    tx.execute("DELETE FROM tag_categories WHERE name=?1", params![name])
        .map_err(|e| format!("删除分类失败: {e}"))?;
    tx.commit().map_err(|e| format!("保存分类删除失败: {e}"))?;
    Ok(())
}

#[tauri::command]
pub fn delete_tag(state: tauri::State<'_, AppState>, id: i64) -> Result<(), String> {
    let conn = lock_db!(state);
    conn.execute("DELETE FROM tags WHERE id=?1", params![id])
        .map_err(|e| format!("删除 Tag 失败: {e}"))?;
    Ok(())
}

#[tauri::command]
pub fn list_candidates(state: tauri::State<'_, AppState>) -> Result<Vec<CandidateEntry>, String> {
    let conn = lock_db!(state);
    let mut stmt = conn.prepare("SELECT target_id, kind FROM candidate_entries ORDER BY created_at ASC")
        .map_err(|e| format!("准备候选查询失败: {e}"))?;
    let entries = stmt.query_map([], |row| Ok(CandidateEntry { target_id: row.get(0)?, kind: row.get(1)? }))
        .map_err(|e| format!("查询候选失败: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("读取候选失败: {e}"))?;
    Ok(entries)
}

#[tauri::command]
pub fn create_candidate(state: tauri::State<'_, AppState>, target_id: String, kind: String, created_at: i64) -> Result<(), String> {
    let conn = lock_db!(state);
    let exists = match kind.as_str() {
        "music" => conn
            .query_row("SELECT 1 FROM music WHERE id=?1", params![target_id], |_| Ok(()))
            .optional()
            .map_err(|e| format!("校验候选失败: {e}"))?
            .is_some(),
        "clip" => conn
            .query_row("SELECT 1 FROM clips WHERE id=?1", params![target_id], |_| Ok(()))
            .optional()
            .map_err(|e| format!("校验候选失败: {e}"))?
            .is_some(),
        _ => return Err("无效的候选类型".into()),
    };
    if !exists {
        return Err("候选目标不存在".into());
    }
    conn.execute("INSERT OR IGNORE INTO candidate_entries (target_id, kind, created_at) VALUES (?1, ?2, ?3)", params![target_id, kind, created_at])
        .map_err(|e| format!("添加候选失败: {e}"))?;
    Ok(())
}

#[tauri::command]
pub fn delete_candidate(state: tauri::State<'_, AppState>, target_id: String) -> Result<(), String> {
    let conn = lock_db!(state);
    conn.execute("DELETE FROM candidate_entries WHERE target_id=?1", params![target_id])
        .map_err(|e| format!("删除候选失败: {e}"))?;
    Ok(())
}

#[tauri::command]
pub fn clear_candidates(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let conn = lock_db!(state);
    conn.execute("DELETE FROM candidate_entries", [])
        .map_err(|e| format!("清空候选失败: {e}"))?;
    Ok(())
}

#[tauri::command]
pub fn file_exists(path: String) -> bool {
    Path::new(&path).is_file()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocationSyncResult {
    updated_paths: usize,
    updated_albums: usize,
}

/// Resolve persisted macOS file bookmarks at launch. A Finder move or rename
/// changes the resolved URL, so the database can be repaired without watching
/// every directory continuously.
#[tauri::command]
pub fn sync_library_locations(state: tauri::State<'_, AppState>) -> Result<LocationSyncResult, String> {
    let mut conn = lock_db!(state);
    let entries = {
        let mut stmt = conn.prepare("SELECT id, path, album, album_source, file_bookmark FROM music")
            .map_err(|e| format!("准备位置同步失败: {e}"))?;
        let rows = stmt.query_map([], |row| Ok((
            row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
            row.get::<_, String>(3)?, row.get::<_, Option<Vec<u8>>>(4)?,
        )))
        .map_err(|e| format!("读取位置同步数据失败: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("读取位置同步数据失败: {e}"))?
    };

    let tx = conn.transaction().map_err(|e| format!("开启位置同步事务失败: {e}"))?;
    let mut result = LocationSyncResult { updated_paths: 0, updated_albums: 0 };
    for (id, old_path, old_album, old_source, bookmark) in entries {
        let mut source = old_source.clone();
        let resolved = match bookmark.as_deref().filter(|data| !data.is_empty()) {
            Some(data) => resolve_file_bookmark(data).ok().map(|(path, _)| path),
            None if Path::new(&old_path).is_file() => Some(old_path.clone()),
            None => None,
        };
        let Some(path) = resolved.filter(|path| Path::new(path).is_file()) else { continue };
        let folder_album = Path::new(&path).parent().and_then(Path::file_name)
            .and_then(|name| name.to_str()).unwrap_or_default().to_string();
        if source == "legacy" || source.is_empty() {
            source = "folder".into();
        }
        let album = if source == "folder" { folder_album } else { old_album.clone() };
        let file_name = Path::new(&path).file_name().and_then(|name| name.to_str()).unwrap_or_default();
        let path_changed = path != old_path;
        let album_changed = album != old_album;
        let source_changed = source != old_source;
        // Keep a valid bookmark unchanged; rebuilding is only needed for old
        // records that lacked one or when resolving revealed a moved location.
        let refreshed_bookmark = if bookmark.as_deref().unwrap_or_default().is_empty() || path_changed {
            create_file_bookmark(&path).unwrap_or_else(|_| bookmark.clone().unwrap_or_default())
        } else {
            bookmark.clone().unwrap_or_default()
        };
        let bookmark_changed = bookmark.as_deref().unwrap_or_default() != refreshed_bookmark.as_slice();
        if path_changed || album_changed || source_changed || bookmark_changed {
            tx.execute(
                "UPDATE music SET path=?1, file_name=?2, file_bookmark=?3, album=?4, album_source=?5, updated_at=?6 WHERE id=?7",
                params![path, file_name, refreshed_bookmark, album, source, now_ms(), id],
            ).map_err(|e| format!("写入同步位置失败: {e}"))?;
            result.updated_paths += usize::from(path_changed);
            result.updated_albums += usize::from(album_changed);
        }
    }
    tx.commit().map_err(|e| format!("提交位置同步失败: {e}"))?;
    Ok(result)
}

#[tauri::command]
pub fn get_database_path(app: tauri::AppHandle) -> Result<String, String> {
    let app_data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("无法获取应用数据目录: {e}"))?;
    Ok(app_data_dir.join("library.db").to_string_lossy().into_owned())
}

#[tauri::command]
pub fn get_audio_base_url() -> Option<String> {
    // 仅 Plan-B（本地 HTTP 音频流服务）启用时返回。
    None
}
