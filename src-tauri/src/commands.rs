use crate::AppState;
use rusqlite::{params, params_from_iter, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

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
        /* 先按名字复用已有 Tag：调用方（列表里的「＋ Tag」、批量打 Tag）只传名字，
           拿到名字不代表要新建。同名 Tag 可能存在于任意维度下，按「不在未分类维度
           → 根层优先 → id 最小」挑一个最像正主的；确实没有同名才新建到「未分类」。
           过去这里无条件往「未分类」插一条，于是给音乐打一个已有 Tag 就会多出一份副本。 */
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

        let tag_id = match existing {
            Some(id) => id,
            None => {
                conn.execute("INSERT OR IGNORE INTO tag_categories (name, created_at) VALUES ('未分类', ?1)", params![now_ms()])
                    .map_err(|e| format!("创建未分类维度失败: {e}"))?;
                conn.execute("INSERT OR IGNORE INTO tags (name, category_id, created_at) VALUES (?1, (SELECT id FROM tag_categories WHERE name='未分类'), ?2)", params![name, now_ms()])
                    .map_err(|e| format!("保存 Tag 失败: {e}"))?;
                conn.query_row("SELECT id FROM tags WHERE name=?1 AND category_id=(SELECT id FROM tag_categories WHERE name='未分类') AND parent_id IS NULL", params![name], |row| row.get(0))
                    .map_err(|e| format!("读取 Tag 失败: {e}"))?
            }
        };

        let insert = format!("INSERT OR IGNORE INTO {join_table} ({owner_column}, tag_id) VALUES (?1, ?2)");
        conn.execute(&insert, params![owner_id, tag_id]).map_err(|e| format!("保存 Tag 关联失败: {e}"))?;
    }
    Ok(())
}

fn sync_tag_ids(conn: &rusqlite::Connection, owner_id: &str, tag_ids: &[i64], new_tags: &[String], join_table: &str, owner_column: &str) -> Result<(), String> {
    let delete = format!("DELETE FROM {join_table} WHERE {owner_column}=?1");
    conn.execute(&delete, params![owner_id]).map_err(|e| format!("清理 Tag 关联失败: {e}"))?;
    for tag_id in tag_ids {
        let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM tags WHERE id=?1)", params![tag_id], |row| row.get(0))
            .map_err(|e| format!("读取 Tag 失败: {e}"))?;
        if !exists { return Err("所选 Tag 不存在".into()); }
        let insert = format!("INSERT OR IGNORE INTO {join_table} ({owner_column}, tag_id) VALUES (?1, ?2)");
        conn.execute(&insert, params![owner_id, tag_id]).map_err(|e| format!("保存 Tag 关联失败: {e}"))?;
    }
    for name in new_tags.iter().map(|tag| tag.trim()).filter(|tag| !tag.is_empty()) {
        let existing: Option<i64> = conn.query_row(
            "SELECT t.id FROM tags t JOIN tag_categories c ON c.id=t.category_id WHERE t.name=?1 COLLATE NOCASE ORDER BY c.name='未分类', t.parent_id IS NOT NULL, t.id LIMIT 1",
            params![name],
            |row| row.get(0),
        ).optional().map_err(|e| format!("读取 Tag 失败: {e}"))?;
        let tag_id = if let Some(id) = existing {
            id
        } else {
            conn.execute("INSERT OR IGNORE INTO tag_categories (name, created_at) VALUES ('未分类', ?1)", params![now_ms()])
                .map_err(|e| format!("创建未分类维度失败: {e}"))?;
            conn.execute("INSERT OR IGNORE INTO tags (name, category_id, created_at) VALUES (?1, (SELECT id FROM tag_categories WHERE name='未分类'), ?2)", params![name, now_ms()])
                .map_err(|e| format!("保存 Tag 失败: {e}"))?;
            conn.query_row("SELECT id FROM tags WHERE name=?1 COLLATE NOCASE AND category_id=(SELECT id FROM tag_categories WHERE name='未分类') AND parent_id IS NULL", params![name], |row| row.get(0))
                .map_err(|e| format!("读取 Tag 失败: {e}"))?
        };
        let insert = format!("INSERT OR IGNORE INTO {join_table} ({owner_column}, tag_id) VALUES (?1, ?2)");
        conn.execute(&insert, params![owner_id, tag_id]).map_err(|e| format!("保存 Tag 关联失败: {e}"))?;
    }
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as i64
}

fn load_tags(conn: &rusqlite::Connection, owner_id: &str, join_table: &str, owner_column: &str) -> Result<Vec<String>, String> {
    let sql = format!("SELECT t.name FROM tags t JOIN {join_table} j ON j.tag_id=t.id WHERE j.{owner_column}=?1 ORDER BY t.name COLLATE NOCASE, t.id");
    let mut stmt = conn.prepare(&sql).map_err(|e| format!("读取 Tag 失败: {e}"))?;
    let tags = stmt.query_map(params![owner_id], |row| row.get(0))
        .map_err(|e| format!("读取 Tag 失败: {e}"))?
        .collect::<Result<Vec<String>, _>>()
        .map_err(|e| format!("读取 Tag 失败: {e}"))?;
    Ok(tags)
}

fn load_tag_ids(conn: &rusqlite::Connection, owner_id: &str, join_table: &str, owner_column: &str) -> Result<Vec<i64>, String> {
    let sql = format!("SELECT t.id FROM tags t JOIN {join_table} j ON j.tag_id=t.id WHERE j.{owner_column}=?1 ORDER BY t.name COLLATE NOCASE, t.id");
    let mut stmt = conn.prepare(&sql).map_err(|e| format!("读取 Tag ID 失败: {e}"))?;
    let ids = stmt.query_map(params![owner_id], |row| row.get(0))
        .map_err(|e| format!("读取 Tag ID 失败: {e}"))?
        .collect::<Result<Vec<i64>, _>>()
        .map_err(|e| format!("读取 Tag ID 失败: {e}"))?;
    Ok(ids)
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
    let sql = format!("SELECT j.{owner_column}, t.name FROM tags t JOIN {join_table} j ON j.tag_id = t.id ORDER BY t.name COLLATE NOCASE, t.id");
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

fn load_all_tag_ids(conn: &rusqlite::Connection, join_table: &str, owner_column: &str) -> Result<HashMap<String, Vec<i64>>, String> {
    let sql = format!("SELECT j.{owner_column}, t.id FROM tags t JOIN {join_table} j ON j.tag_id=t.id ORDER BY t.name COLLATE NOCASE, t.id");
    let mut stmt = conn.prepare(&sql).map_err(|e| format!("读取 Tag ID 失败: {e}"))?;
    let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))
        .map_err(|e| format!("读取 Tag ID 失败: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("读取 Tag ID 失败: {e}"))?;
    let mut map: HashMap<String, Vec<i64>> = HashMap::new();
    for (owner, tag_id) in rows { map.entry(owner).or_default().push(tag_id); }
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
    tag_ids: Vec<i64>,
    duration: f64,
    file_size: u64,
    mime_type: String,
    cover_art: String,
    created_at: i64,
    updated_at: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryCounts {
    music: i64,
    clips: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MusicCoverArt {
    id: String,
    cover_art: String,
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
            tag_ids: Vec::new(),
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
    #[serde(default)]
    tag_ids: Option<Vec<i64>>,
    #[serde(default)]
    new_tags: Vec<String>,
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

    match music.tag_ids.as_deref() {
        Some(ids) => sync_tag_ids(&tx, &music.id, ids, &music.new_tags, "music_tags", "music_id")?,
        None => sync_tags(&tx, &music.id, &music.tags, "music_tags", "music_id")?,
    }

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

    match music.tag_ids.as_deref() {
        Some(ids) => sync_tag_ids(&tx, &music.id, ids, &music.new_tags, "music_tags", "music_id")?,
        None => sync_tags(&tx, &music.id, &music.tags, "music_tags", "music_id")?,
    }

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
        item.tag_ids = load_tag_ids(&conn, &item.id, "music_tags", "music_id")?;
    }
    Ok(music)
}

#[tauri::command]
pub fn list_music(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<Music>, String> {
    let conn = lock_db!(state);

    let mut stmt = conn
        // 列表快照不携带可能很大的 base64 封面。当前页封面由
        // get_music_cover_arts 按需加载，避免数千首音乐在一次 IPC 中传输。
        .prepare("SELECT id, name, file_name, path, album, album_source, artist, genre, year,
                         channels, sample_rate, bitrate, duration, file_size, mime_type,
                         '' AS cover_art, created_at, updated_at
                  FROM music ORDER BY created_at ASC")
        .map_err(|e| format!("准备查询失败: {e}"))?;

    let mut music = stmt
        .query_map([], Music::from_row)
        .map_err(|e| format!("查询音乐失败: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("读取音乐失败: {e}"))?;

    let tag_map = load_all_tags(&conn, "music_tags", "music_id")?;
    let tag_id_map = load_all_tag_ids(&conn, "music_tags", "music_id")?;
    for item in &mut music {
        item.tags = tag_map.get(&item.id).cloned().unwrap_or_default();
        item.tag_ids = tag_id_map.get(&item.id).cloned().unwrap_or_default();
    }
    Ok(music)
}

#[tauri::command]
pub fn get_library_counts(state: tauri::State<'_, AppState>) -> Result<LibraryCounts, String> {
    let conn = lock_db!(state);
    let music = conn.query_row("SELECT COUNT(*) FROM music", [], |row| row.get(0))
        .map_err(|e| format!("统计音乐失败: {e}"))?;
    let clips = conn.query_row("SELECT COUNT(*) FROM clips", [], |row| row.get(0))
        .map_err(|e| format!("统计片段失败: {e}"))?;
    Ok(LibraryCounts { music, clips })
}

#[tauri::command]
pub fn get_music_cover_arts(
    state: tauri::State<'_, AppState>,
    ids: Vec<String>,
) -> Result<Vec<MusicCoverArt>, String> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let conn = lock_db!(state);
    let mut result = Vec::new();
    for batch in ids.chunks(900) {
        let placeholders = std::iter::repeat("?").take(batch.len()).collect::<Vec<_>>().join(", ");
        let query = format!("SELECT id, cover_art FROM music WHERE id IN ({placeholders})");
        let mut stmt = conn.prepare(&query).map_err(|e| format!("准备封面查询失败: {e}"))?;
        let rows = stmt.query_map(params_from_iter(batch.iter()), |row| Ok(MusicCoverArt {
            id: row.get(0)?,
            cover_art: row.get(1)?,
        })).map_err(|e| format!("查询封面失败: {e}"))?;
        result.extend(rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("读取封面失败: {e}"))?);
    }
    Ok(result)
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
    tag_ids: Vec<i64>,
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
            tag_ids: Vec::new(),
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
    #[serde(default)]
    tag_ids: Option<Vec<i64>>,
    #[serde(default)]
    new_tags: Vec<String>,
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

    match clip.tag_ids.as_deref() {
        Some(ids) => sync_tag_ids(&tx, &clip.id, ids, &clip.new_tags, "clip_tags", "clip_id")?,
        None => sync_tags(&tx, &clip.id, &clip.tags, "clip_tags", "clip_id")?,
    }

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

    match clip.tag_ids.as_deref() {
        Some(ids) => sync_tag_ids(&tx, &clip.id, ids, &clip.new_tags, "clip_tags", "clip_id")?,
        None => sync_tags(&tx, &clip.id, &clip.tags, "clip_tags", "clip_id")?,
    }

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
        item.tag_ids = load_tag_ids(&conn, &item.id, "clip_tags", "clip_id")?;
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
    let tag_id_map = load_all_tag_ids(&conn, "clip_tags", "clip_id")?;
    for item in &mut clips {
        item.tags = tag_map.get(&item.id).cloned().unwrap_or_default();
        item.tag_ids = tag_id_map.get(&item.id).cloned().unwrap_or_default();
    }
    Ok(clips)
}

#[tauri::command]
pub fn list_clips_for_music(
    state: tauri::State<'_, AppState>,
    music_id: String,
) -> Result<Vec<Clip>, String> {
    let conn = lock_db!(state);
    let mut stmt = conn
        .prepare("SELECT * FROM clips WHERE music_id=?1 ORDER BY start ASC, created_at ASC")
        .map_err(|e| format!("准备音乐片段查询失败: {e}"))?;
    let mut clips = stmt
        .query_map(params![music_id], Clip::from_row)
        .map_err(|e| format!("查询音乐片段失败: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("读取音乐片段失败: {e}"))?;
    for item in &mut clips {
        item.tags = load_tags(&conn, &item.id, "clip_tags", "clip_id")?;
        item.tag_ids = load_tag_ids(&conn, &item.id, "clip_tags", "clip_id")?;
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
    category_id: i64,
    category: String,
    parent_id: Option<i64>,
    path: String,
    music_count: i64,
}

#[tauri::command]
pub fn list_tags(state: tauri::State<'_, AppState>) -> Result<Vec<TagRecord>, String> {
    let conn = lock_db!(state);
    let mut stmt = conn.prepare(
        "WITH RECURSIVE tree(id, path) AS (SELECT t.id, t.name FROM tags t WHERE t.parent_id IS NULL UNION ALL SELECT t.id, tree.path || ' / ' || t.name FROM tags t JOIN tree ON t.parent_id=tree.id) \
         SELECT t.id, t.name, t.category_id, CASE WHEN c.name='未分类' THEN '' ELSE c.name END, t.parent_id, tree.path, COUNT(DISTINCT mt.music_id) + COUNT(DISTINCT ct.clip_id) + COUNT(DISTINCT at.album) AS music_count \
         FROM tags t LEFT JOIN music_tags mt ON mt.tag_id = t.id \
         LEFT JOIN clip_tags ct ON ct.tag_id = t.id \
         LEFT JOIN album_tags at ON at.tag_id = t.id \
         JOIN tag_categories c ON c.id=t.category_id JOIN tree ON tree.id=t.id \
         GROUP BY t.id, t.name, t.category_id, c.name, t.parent_id, tree.path \
         ORDER BY c.name COLLATE NOCASE, tree.path COLLATE NOCASE"
    )
        .map_err(|e| format!("准备 Tag 查询失败: {e}"))?;
    let records = stmt.query_map([], |row| Ok(TagRecord { id: row.get(0)?, name: row.get(1)?, category_id: row.get(2)?, category: row.get(3)?, parent_id: row.get(4)?, path: row.get(5)?, music_count: row.get(6)? }))
        .map_err(|e| format!("查询 Tag 失败: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("读取 Tag 失败: {e}"))?;
    Ok(records)
}

fn validate_tag(name: &str) -> Result<String, String> {
    let name = name.trim().to_string();
    if name.is_empty() { return Err("Tag 名称不能为空".into()); }
    Ok(name)
}

#[tauri::command]
pub fn create_tag(state: tauri::State<'_, AppState>, name: String, category_id: Option<i64>, category: Option<String>, parent_id: Option<i64>) -> Result<TagRecord, String> {
    let name = validate_tag(&name)?;
    let conn = lock_db!(state);
    let category_id = if let Some(parent) = parent_id {
        let parent_category: i64 = conn.query_row("SELECT category_id FROM tags WHERE id=?1", params![parent], |row| row.get(0)).map_err(|_| "父 Tag 不存在")?;
        if category_id.is_some_and(|id| id != parent_category) { return Err("父 Tag 必须属于同一筛选维度".into()); }
        parent_category
    } else if let Some(id) = category_id {
        id
    } else {
        let category = category.unwrap_or_else(|| "未分类".into());
        let category = if category.trim().is_empty() { "未分类" } else { category.trim() };
        conn.execute("INSERT OR IGNORE INTO tag_categories (name, created_at) VALUES (?1, ?2)", params![category, now_ms()])
            .map_err(|e| format!("创建 Tag 分类失败: {e}"))?;
        conn.query_row("SELECT id FROM tag_categories WHERE name=?1", params![category], |row| row.get(0))
            .map_err(|e| format!("读取 Tag 分类失败: {e}"))?
    };
    conn.execute("INSERT INTO tags (name, category_id, parent_id, created_at) VALUES (?1, ?2, ?3, ?4)", params![name, category_id, parent_id, now_ms()])
        .map_err(|e| format!("创建 Tag 失败: {e}"))?;
    let id = conn.last_insert_rowid();
    let category: String = conn.query_row("SELECT CASE WHEN name='未分类' THEN '' ELSE name END FROM tag_categories WHERE id=?1", params![category_id], |row| row.get(0)).map_err(|_| "筛选维度不存在")?;
    let path = if let Some(parent) = parent_id { conn.query_row("WITH RECURSIVE tree(id,path) AS (SELECT id,name FROM tags WHERE parent_id IS NULL UNION ALL SELECT t.id, tree.path || ' / ' || t.name FROM tags t JOIN tree ON t.parent_id=tree.id) SELECT path FROM tree WHERE id=?1", params![parent], |r| r.get::<_, String>(0)).map(|p| format!("{p} / {name}")).unwrap_or(name.clone()) } else { name.clone() };
    Ok(TagRecord { id, name, category_id, category, parent_id, path, music_count: 0 })
}

#[tauri::command]
pub fn update_tag(state: tauri::State<'_, AppState>, id: i64, name: String, category: Option<String>) -> Result<(), String> {
    let name = validate_tag(&name)?;
    let mut conn = lock_db!(state);
    let tx = conn.transaction().map_err(|e| format!("开启 Tag 更新事务失败: {e}"))?;
    let changed = tx.execute("UPDATE tags SET name=?1 WHERE id=?2", params![name, id])
        .map_err(|e| format!("更新 Tag 失败: {e}"))?;
    if changed == 0 { return Err("Tag 不存在".into()); }
    if let Some(category) = category {
        let category = if category.trim().is_empty() { "未分类" } else { category.trim() };
        tx.execute("INSERT OR IGNORE INTO tag_categories (name, created_at) VALUES (?1, ?2)", params![category, now_ms()])
            .map_err(|e| format!("创建 Tag 分类失败: {e}"))?;
        let target: i64 = tx.query_row("SELECT id FROM tag_categories WHERE name=?1", params![category], |row| row.get(0))
            .map_err(|e| format!("读取 Tag 分类失败: {e}"))?;
        let current: i64 = tx.query_row("SELECT category_id FROM tags WHERE id=?1", params![id], |row| row.get(0))
            .map_err(|e| format!("读取 Tag 失败: {e}"))?;
        let clash: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM tags WHERE category_id=?1 AND parent_id IS NULL AND name=?2 COLLATE NOCASE AND id<>?3)",
            params![target, name, id],
            |row| row.get(0),
        ).map_err(|e| format!("校验目标位置失败: {e}"))?;
        if clash { return Err(format!("目标位置已有同名 Tag「{name}」")); }
        tx.execute("UPDATE tags SET parent_id=NULL, category_id=?1 WHERE id=?2", params![target, id])
            .map_err(|e| format!("移动 Tag 失败: {e}"))?;
        if target != current {
            tx.execute(
                "WITH RECURSIVE subtree(id) AS (SELECT id FROM tags WHERE parent_id=?1 UNION ALL SELECT t.id FROM tags t JOIN subtree s ON t.parent_id=s.id) UPDATE tags SET category_id=?2 WHERE id IN (SELECT id FROM subtree)",
                params![id, target],
            ).map_err(|e| format!("同步子 Tag 维度失败: {e}"))?;
        }
    }
    tx.commit().map_err(|e| format!("提交 Tag 更新失败: {e}"))?;
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TagCategory { id: i64, name: String }

#[tauri::command]
pub fn list_tag_category_records(state: tauri::State<'_, AppState>) -> Result<Vec<TagCategory>, String> {
    let conn = lock_db!(state);
    let mut stmt = conn.prepare("SELECT id, name FROM tag_categories ORDER BY name COLLATE NOCASE")
        .map_err(|e| format!("准备分类查询失败: {e}"))?;
    let categories = stmt.query_map([], |row| Ok(TagCategory { id: row.get(0)?, name: row.get(1)? }))
        .map_err(|e| format!("查询分类失败: {e}"))?
        .collect::<Result<Vec<TagCategory>, _>>()
        .map_err(|e| format!("读取分类失败: {e}"))?;
    Ok(categories)
}

#[tauri::command]
pub fn list_tag_categories(state: tauri::State<'_, AppState>) -> Result<Vec<String>, String> {
    let conn = lock_db!(state);
    let mut stmt = conn.prepare("SELECT name FROM tag_categories WHERE name != '未分类' ORDER BY name COLLATE NOCASE")
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

    let conn = lock_db!(state);
    let changed = conn.execute("UPDATE tag_categories SET name=?1 WHERE name=?2", params![name, old_name]).map_err(|e| format!("更新筛选维度失败: {e}"))?;
    if changed == 0 { return Err("分类不存在".into()); }
    Ok(())
}

#[tauri::command]
pub fn delete_tag_category(state: tauri::State<'_, AppState>, name: String) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() { return Err("分类名称不能为空".into()); }

    let conn = lock_db!(state);
    let id: i64 = conn.query_row("SELECT id FROM tag_categories WHERE name=?1", params![name], |row| row.get(0)).map_err(|_| "分类不存在")?;
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM tags WHERE category_id=?1", params![id], |row| row.get(0)).map_err(|e| format!("读取 Tag 失败: {e}"))?;
    if count > 0 { return Err("非空筛选维度不能删除，请先移动或删除其中的 Tag".into()); }
    conn.execute("DELETE FROM tag_categories WHERE id=?1", params![id]).map_err(|e| format!("删除筛选维度失败: {e}"))?;
    Ok(())
}

#[tauri::command]
pub fn delete_tag(state: tauri::State<'_, AppState>, id: i64) -> Result<(), String> {
    let mut conn = lock_db!(state);
    let tx = conn.transaction().map_err(|e| format!("开启删除 Tag 事务失败: {e}"))?;
    let parent_id: Option<i64> = tx.query_row("SELECT parent_id FROM tags WHERE id=?1", params![id], |row| row.get(0)).map_err(|_| "Tag 不存在")?;
    tx.execute("UPDATE tags SET parent_id=?1 WHERE parent_id=?2", params![parent_id, id]).map_err(|e| format!("上移子 Tag 失败: {e}"))?;
    tx.execute("DELETE FROM tags WHERE id=?1", params![id])
        .map_err(|e| format!("删除 Tag 失败: {e}"))?;
    tx.commit().map_err(|e| format!("保存 Tag 删除失败: {e}"))?;
    Ok(())
}

#[tauri::command]
pub fn move_tag(state: tauri::State<'_, AppState>, id: i64, parent_id: Option<i64>, category_id: Option<i64>) -> Result<(), String> {
    let mut conn = lock_db!(state);
    move_tag_in(&mut conn, id, parent_id, category_id)
}

/* 移动 Tag 的实逻辑，和 Tauri State 解耦以便测试。
   parent_id 给定时挂成那个 Tag 的子级；为 None 时落到 category_id（缺省为原维度）的根层。 */
fn move_tag_in(conn: &mut rusqlite::Connection, id: i64, parent_id: Option<i64>, category_id: Option<i64>) -> Result<(), String> {
    if parent_id == Some(id) { return Err("不能移动到自身".into()); }

    let (current_category, name): (i64, String) = conn
        .query_row("SELECT category_id, name FROM tags WHERE id=?1", params![id], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(|_| "Tag 不存在")?;

    /* 目标维度：挂到某个 Tag 下面时，跟随那个父 Tag 所属的维度（维度由位置决定，
       不接受调用方另传一个不一致的维度）；放到维度根层时才用调用方指定的维度。
       老调用只传 parentId，此时沿用原维度，行为与之前一致。 */
    let target_category = match parent_id {
        Some(parent) => {
            let parent_category: i64 = conn.query_row("SELECT category_id FROM tags WHERE id=?1", params![parent], |row| row.get(0)).map_err(|_| "父 Tag 不存在")?;
            let descendant: bool = conn.query_row("WITH RECURSIVE descendants(id) AS (SELECT id FROM tags WHERE parent_id=?1 UNION ALL SELECT t.id FROM tags t JOIN descendants d ON t.parent_id=d.id) SELECT EXISTS(SELECT 1 FROM descendants WHERE id=?2)", params![id, parent], |row| row.get(0)).map_err(|e| format!("验证 Tag 层级失败: {e}"))?;
            if descendant { return Err("不能移动到自己的后代节点".into()); }
            parent_category
        }
        None => category_id.unwrap_or(current_category),
    };

    /* 目标位置已有同名 Tag 时给一句人话，别把唯一索引的报错原样抛给用户。 */
    let clash: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM tags WHERE category_id=?1 AND COALESCE(parent_id, -1)=COALESCE(?2, -1) AND name=?3 COLLATE NOCASE AND id<>?4)",
            params![target_category, parent_id, name, id],
            |row| row.get(0),
        )
        .map_err(|e| format!("校验目标位置失败: {e}"))?;
    if clash { return Err(format!("目标位置已有同名 Tag「{name}」")); }

    let tx = conn.transaction().map_err(|e| format!("开启移动 Tag 事务失败: {e}"))?;
    tx.execute("UPDATE tags SET parent_id=?1, category_id=?2 WHERE id=?3", params![parent_id, target_category, id])
        .map_err(|e| format!("移动 Tag 失败: {e}"))?;
    /* 子 Tag 要跟着父 Tag 一起换维度，否则会出现「父在 A 维度、子在 B 维度」的错位，
       列表按维度分组时会露馅。 */
    if target_category != current_category {
        tx.execute(
            "WITH RECURSIVE subtree(id) AS (SELECT id FROM tags WHERE parent_id=?1 UNION ALL SELECT t.id FROM tags t JOIN subtree s ON t.parent_id=s.id) UPDATE tags SET category_id=?2 WHERE id IN (SELECT id FROM subtree)",
            params![id, target_category],
        ).map_err(|e| format!("同步子 Tag 维度失败: {e}"))?;
    }
    tx.commit().map_err(|e| format!("提交移动 Tag 失败: {e}"))?;
    Ok(())
}

/* 专辑 Tag：专辑以名字为键（库里没有专辑实体），关联行挂在 album_tags 上。
   Finder 改名的跟随迁移见 sync_library_locations / apply_album_renames。 */

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumTagEntry {
    album: String,
    tag_ids: Vec<i64>,
}

#[tauri::command]
pub fn list_album_tags(state: tauri::State<'_, AppState>) -> Result<Vec<AlbumTagEntry>, String> {
    let conn = lock_db!(state);
    let map = load_all_tag_ids(&conn, "album_tags", "album")?;
    let mut entries: Vec<AlbumTagEntry> = map
        .into_iter()
        .map(|(album, tag_ids)| AlbumTagEntry { album, tag_ids })
        .collect();
    entries.sort_by(|a, b| a.album.cmp(&b.album));
    Ok(entries)
}

#[tauri::command]
pub fn set_album_tags(
    state: tauri::State<'_, AppState>,
    album: String,
    tag_ids: Vec<i64>,
    new_tags: Vec<String>,
) -> Result<Vec<i64>, String> {
    let album = album.trim().to_string();
    if album.is_empty() {
        return Err("专辑名不能为空".into());
    }
    let conn = lock_db!(state);
    sync_tag_ids(&conn, &album, &tag_ids, &new_tags, "album_tags", "album")?;
    load_tag_ids(&conn, &album, "album_tags", "album")
}

#[tauri::command]
pub fn delete_album_tags(state: tauri::State<'_, AppState>, album: String) -> Result<(), String> {
    let conn = lock_db!(state);
    conn.execute("DELETE FROM album_tags WHERE album=?1", params![album])
        .map_err(|e| format!("删除专辑 Tag 失败: {e}"))?;
    Ok(())
}

/// 专辑随 Finder 文件夹改名后，专辑 Tag 跟到新名字：先 OR IGNORE 合并
/// （新名字下已有同 Tag 的冲突行保持不动），再删掉仍挂在旧名字下的剩余行。
fn apply_album_renames(conn: &rusqlite::Connection, renames: &HashMap<String, String>) -> Result<(), String> {
    for (old, new) in renames {
        if old == new {
            continue;
        }
        conn.execute("UPDATE OR IGNORE album_tags SET album=?1 WHERE album=?2", params![new, old])
            .map_err(|e| format!("迁移专辑 Tag 失败: {e}"))?;
        conn.execute("DELETE FROM album_tags WHERE album=?1", params![old])
            .map_err(|e| format!("清理专辑 Tag 失败: {e}"))?;
    }
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
    let entries = {
        let conn = lock_db!(state);
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

    let mut result = LocationSyncResult { updated_paths: 0, updated_albums: 0 };
    let mut updates = Vec::new();
    let mut album_renames: HashMap<String, String> = HashMap::new();
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
        let file_name = Path::new(&path).file_name().and_then(|name| name.to_str()).unwrap_or_default().to_string();
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
            result.updated_paths += usize::from(path_changed);
            result.updated_albums += usize::from(album_changed);
            if album_changed {
                album_renames.insert(old_album.clone(), album.clone());
            }
            updates.push((path, file_name, refreshed_bookmark, album, source, id));
        }
    }
    if !updates.is_empty() {
        let mut conn = lock_db!(state);
        let tx = conn.transaction().map_err(|e| format!("开启位置同步事务失败: {e}"))?;
        for (path, file_name, bookmark, album, source, id) in updates {
            tx.execute(
                "UPDATE music SET path=?1, file_name=?2, file_bookmark=?3, album=?4, album_source=?5, updated_at=?6 WHERE id=?7",
                params![path, file_name, bookmark, album, source, now_ms(), id],
            ).map_err(|e| format!("写入同步位置失败: {e}"))?;
        }
        // 专辑 Tag 以名字为键，文件夹改名要跟过去，否则 Tag 悄悄丢在旧名字下。
        apply_album_renames(&tx, &album_renames)?;
        tx.commit().map_err(|e| format!("提交位置同步失败: {e}"))?;
    }
    Ok(result)
}

#[tauri::command]
pub fn get_database_path(state: tauri::State<'_, AppState>) -> Result<String, String> {
    let path = state
        .db_path
        .lock()
        .map_err(|e| format!("数据库路径锁获取失败: {e}"))?;
    Ok(path.to_string_lossy().into_owned())
}

#[tauri::command]
pub fn set_database_location(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
    dir: String,
    overwrite: bool,
) -> Result<String, String> {
    let dir_path = PathBuf::from(&dir);
    let old_path = state
        .db_path
        .lock()
        .map_err(|e| format!("数据库路径锁获取失败: {e}"))?
        .clone();
    if old_path.parent() == Some(dir_path.as_path()) {
        return Err("新位置与当前位置相同".into());
    }
    fs::create_dir_all(&dir_path).map_err(|e| format!("无法创建目标目录: {e}"))?;
    let probe = dir_path.join(".music-manager-write-test");
    File::create(&probe).map_err(|e| format!("目标目录不可写: {e}"))?;
    let _ = fs::remove_file(&probe);

    let target = dir_path.join("library.db");
    if target.exists() {
        if !overwrite {
            return Err("目标位置已有数据库文件".into());
        }
        for suffix in ["", "-wal", "-shm"] {
            let file = dir_path.join(format!("library.db{suffix}"));
            if file.exists() {
                fs::remove_file(&file).map_err(|e| format!("无法删除目标位置的旧数据库: {e}"))?;
            }
        }
    }

    {
        let mut conn = lock_db!(state);
        crate::db::migrate_database_file(&conn, &target)?;
        /* 所有数据库访问都经过同一把锁，在锁内换连接：在途命令要么已结束，
           要么排在锁之后直接用新库，无需重启应用。 */
        let new_conn = crate::db::open_conn(&target)?;
        drop(std::mem::replace(&mut *conn, new_conn));
    }

    crate::db::save_db_path(&app, &target)?;

    let old = old_path.to_string_lossy().into_owned();
    for file in [old_path.clone(), PathBuf::from(format!("{old}-wal")), PathBuf::from(format!("{old}-shm"))] {
        if file != target && file.exists() {
            let _ = fs::remove_file(file);
        }
    }

    *state
        .db_path
        .lock()
        .map_err(|e| format!("数据库路径锁获取失败: {e}"))? = target.clone();
    Ok(target.to_string_lossy().into_owned())
}

#[tauri::command]
pub fn get_audio_base_url() -> Option<String> {
    // 仅 Plan-B（本地 HTTP 音频流服务）启用时返回。
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_conn() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE tag_categories (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE COLLATE NOCASE,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE tags (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL COLLATE NOCASE,
                category_id INTEGER NOT NULL REFERENCES tag_categories(id) ON DELETE RESTRICT,
                parent_id INTEGER REFERENCES tags(id) ON DELETE RESTRICT,
                created_at INTEGER NOT NULL
            );
            CREATE UNIQUE INDEX idx_tags_sibling_name ON tags(category_id, COALESCE(parent_id, -1), name COLLATE NOCASE);
            CREATE TABLE music_tags (
                music_id TEXT NOT NULL,
                tag_id INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
                PRIMARY KEY (music_id, tag_id)
            );
            CREATE TABLE clip_tags (
                clip_id TEXT NOT NULL,
                tag_id INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
                PRIMARY KEY (clip_id, tag_id)
            );
            CREATE TABLE album_tags (
                album TEXT NOT NULL,
                tag_id INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
                PRIMARY KEY (album, tag_id)
            );
            INSERT INTO tag_categories (name, created_at) VALUES ('情绪', 1), ('未分类', 2);
            "#,
        ).unwrap();
        conn
    }

    fn add_tag(conn: &rusqlite::Connection, name: &str, category: &str, parent_id: Option<i64>) -> i64 {
        conn.execute(
            "INSERT INTO tags (name, category_id, parent_id, created_at)
             VALUES (?1, (SELECT id FROM tag_categories WHERE name=?2), ?3, 0)",
            params![name, category, parent_id],
        ).unwrap();
        conn.last_insert_rowid()
    }

    fn count_tags(conn: &rusqlite::Connection, name: &str) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM tags WHERE name=?1", params![name], |r| r.get(0)).unwrap()
    }

    /// 就是这个 bug：给音乐打一个已有 Tag，不能再造一份副本出来。
    #[test]
    fn sync_tags_reuses_existing_tag_by_name() {
        let conn = test_conn();
        let real = add_tag(&conn, "温暖", "情绪", None);

        sync_tags(&conn, "m1", &["温暖".into()], "music_tags", "music_id").unwrap();

        assert_eq!(count_tags(&conn, "温暖"), 1, "不应新建同名 Tag");
        let tag_id: i64 = conn.query_row("SELECT tag_id FROM music_tags WHERE music_id='m1'", [], |r| r.get(0)).unwrap();
        assert_eq!(tag_id, real, "应关联到已有的那个 Tag");
    }

    /// 真·新名字才新建，而且落在「未分类」。
    #[test]
    fn sync_tags_creates_unknown_tag_in_uncategorized() {
        let conn = test_conn();

        sync_tags(&conn, "m1", &["全新".into()], "music_tags", "music_id").unwrap();

        assert_eq!(count_tags(&conn, "全新"), 1);
        let category: String = conn.query_row(
            "SELECT c.name FROM tags t JOIN tag_categories c ON c.id=t.category_id WHERE t.name='全新'",
            [], |r| r.get(0),
        ).unwrap();
        assert_eq!(category, "未分类");
    }

    /// 库已经被旧版污染（同名两份都在）时，要挑真正的那个，而不是未分类里的副本。
    #[test]
    fn sync_tags_prefers_non_uncategorized_over_stale_duplicate() {
        let conn = test_conn();
        let real = add_tag(&conn, "人声", "情绪", None);
        let stale = add_tag(&conn, "人声", "未分类", None);

        sync_tags(&conn, "m1", &["人声".into()], "music_tags", "music_id").unwrap();

        let tag_id: i64 = conn.query_row("SELECT tag_id FROM music_tags WHERE music_id='m1'", [], |r| r.get(0)).unwrap();
        assert_eq!(tag_id, real, "应挑非未分类的那个，而不是 {stale}");
        assert_eq!(count_tags(&conn, "人声"), 2, "解析不该顺手动数据结构");
    }

    /// 根层优先于子级：同名时挑根层那个。
    #[test]
    fn sync_tags_prefers_root_over_child() {
        let conn = test_conn();
        let parent = add_tag(&conn, "温暖", "情绪", None);
        let child = add_tag(&conn, "明亮", "情绪", Some(parent));

        sync_tags(&conn, "m1", &["明亮".into()], "music_tags", "music_id").unwrap();

        let tag_id: i64 = conn.query_row("SELECT tag_id FROM music_tags WHERE music_id='m1'", [], |r| r.get(0)).unwrap();
        assert_eq!(tag_id, child, "只有子级同名时就用它");
    }

    /// 大小写与空白不该造出重复项。
    #[test]
    fn sync_tags_ignores_case_and_padding() {
        let conn = test_conn();
        let real = add_tag(&conn, "Ambient", "情绪", None);

        sync_tags(&conn, "m1", &["  ambient  ".into()], "music_tags", "music_id").unwrap();

        assert_eq!(count_tags(&conn, "Ambient"), 1);
        let tag_id: i64 = conn.query_row("SELECT tag_id FROM music_tags WHERE music_id='m1'", [], |r| r.get(0)).unwrap();
        assert_eq!(tag_id, real);
    }

    /// 多个 Tag 一起同步，也各自解析。
    #[test]
    fn sync_tags_handles_mixed_existing_and_new() {
        let conn = test_conn();
        let existing = add_tag(&conn, "人声", "情绪", None);

        sync_tags(&conn, "m1", &["人声".into(), "全新".into()], "music_tags", "music_id").unwrap();

        assert_eq!(count_tags(&conn, "人声"), 1, "已有 Tag 不该被复制");
        assert_eq!(count_tags(&conn, "全新"), 1, "新 Tag 应被创建");
        let linked: Vec<i64> = conn.prepare("SELECT tag_id FROM music_tags WHERE music_id='m1'").unwrap()
            .query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
        assert_eq!(linked.len(), 2);
        assert!(linked.contains(&existing));
    }

    /// 专辑改名时专辑 Tag 要跟到新名字，并和新名字下已有 Tag 合并去重。
    #[test]
    fn apply_album_renames_moves_and_merges_tags() {
        let conn = test_conn();
        let tag_a = add_tag(&conn, "温暖", "情绪", None);
        let tag_b = add_tag(&conn, "人声", "情绪", None);
        conn.execute("INSERT INTO album_tags (album, tag_id) VALUES ('旧专辑', ?1)", params![tag_a]).unwrap();
        conn.execute("INSERT INTO album_tags (album, tag_id) VALUES ('旧专辑', ?1)", params![tag_b]).unwrap();
        conn.execute("INSERT INTO album_tags (album, tag_id) VALUES ('新专辑', ?1)", params![tag_a]).unwrap();

        let renames = HashMap::from([("旧专辑".to_string(), "新专辑".to_string())]);
        apply_album_renames(&conn, &renames).unwrap();

        let remaining_old: i64 = conn
            .query_row("SELECT COUNT(*) FROM album_tags WHERE album='旧专辑'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(remaining_old, 0, "旧名字下不该再挂 Tag");
        let mut new_tags: Vec<i64> = conn
            .prepare("SELECT tag_id FROM album_tags WHERE album='新专辑' ORDER BY tag_id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        new_tags.sort();
        assert_eq!(new_tags, vec![tag_a.min(tag_b), tag_a.max(tag_b)], "冲突行应合并去重");
    }

    #[test]
    fn sync_tag_ids_preserves_exact_same_name_identity() {
        let conn = test_conn();
        let emotional = add_tag(&conn, "现场", "情绪", None);
        let uncategorized = add_tag(&conn, "现场", "未分类", None);

        sync_tag_ids(&conn, "m1", &[uncategorized], &[], "music_tags", "music_id").unwrap();

        let linked: i64 = conn.query_row("SELECT tag_id FROM music_tags WHERE music_id='m1'", [], |row| row.get(0)).unwrap();
        assert_eq!(linked, uncategorized);
        assert_ne!(linked, emotional);
    }

    #[test]
    fn sync_tag_ids_adds_new_free_text_tags() {
        let conn = test_conn();
        let existing = add_tag(&conn, "现场", "情绪", None);

        sync_tag_ids(&conn, "m1", &[existing], &["新标签".into()], "music_tags", "music_id").unwrap();

        let linked: Vec<i64> = conn.prepare("SELECT tag_id FROM music_tags WHERE music_id='m1'").unwrap()
            .query_map([], |row| row.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
        assert_eq!(linked.len(), 2);
        assert_eq!(count_tags(&conn, "新标签"), 1);
    }

    fn category_of(conn: &rusqlite::Connection, id: i64) -> String {
        conn.query_row(
            "SELECT c.name FROM tags t JOIN tag_categories c ON c.id=t.category_id WHERE t.id=?1",
            params![id], |r| r.get(0),
        ).unwrap()
    }

    fn parent_of(conn: &rusqlite::Connection, id: i64) -> Option<i64> {
        conn.query_row("SELECT parent_id FROM tags WHERE id=?1", params![id], |r| r.get(0)).unwrap()
    }

    fn category_id_of(conn: &rusqlite::Connection, name: &str) -> i64 {
        conn.query_row("SELECT id FROM tag_categories WHERE name=?1", params![name], |r| r.get(0)).unwrap()
    }

    /// 用户加错维度时最需要的功能：把 Tag 拖到另一个维度，它真的换维度。
    #[test]
    fn move_tag_changes_category() {
        let mut conn = test_conn();
        let tag = add_tag(&conn, "温暖", "情绪", None);

        let uncategorized = category_id_of(&conn, "未分类");
        move_tag_in(&mut conn, tag, None, Some(uncategorized)).unwrap();

        assert_eq!(category_of(&conn, tag), "未分类");
        assert_eq!(parent_of(&conn, tag), None, "落到根层");
    }

    /// 只传 parentId（老调用）时，维度不该被悄悄改掉。
    #[test]
    fn move_tag_keeps_category_when_only_parent_given() {
        let mut conn = test_conn();
        let parent = add_tag(&conn, "温暖", "情绪", None);
        let child = add_tag(&conn, "沙发", "情绪", None);

        move_tag_in(&mut conn, child, Some(parent), None).unwrap();

        assert_eq!(parent_of(&conn, child), Some(parent));
        assert_eq!(category_of(&conn, child), "情绪");
    }

    /// 挂到别的维度的 Tag 下面时，维度由父 Tag 决定。
    #[test]
    fn move_tag_follows_parent_category() {
        let mut conn = test_conn();
        let root = add_tag(&conn, "温暖", "未分类", None);
        let other = add_tag(&conn, "节奏感", "情绪", None);

        move_tag_in(&mut conn, root, Some(other), None).unwrap();

        assert_eq!(parent_of(&conn, root), Some(other));
        assert_eq!(category_of(&conn, root), "情绪", "应跟随父 Tag 的维度");
    }

    /// 子 Tag 必须跟着一起换维度，不能留下「父在 A、子在 B」的错位。
    #[test]
    fn move_tag_moves_descendants_along() {
        let mut conn = test_conn();
        let root = add_tag(&conn, "温暖", "情绪", None);
        let child = add_tag(&conn, "沙发", "情绪", Some(root));
        let grandchild = add_tag(&conn, "绒面", "情绪", Some(child));

        let uncategorized = category_id_of(&conn, "未分类");
        move_tag_in(&mut conn, root, None, Some(uncategorized)).unwrap();

        assert_eq!(category_of(&conn, root), "未分类");
        assert_eq!(category_of(&conn, child), "未分类", "子 Tag 要跟过来");
        assert_eq!(category_of(&conn, grandchild), "未分类", "孙 Tag 也要跟过来");
        assert_eq!(parent_of(&conn, child), Some(root), "层级结构不该被破坏");
        assert_eq!(parent_of(&conn, grandchild), Some(child));
    }

    /// 不能把 Tag 拖进自己的子树里。
    #[test]
    fn move_tag_rejects_descendant_target() {
        let mut conn = test_conn();
        let root = add_tag(&conn, "温暖", "情绪", None);
        let child = add_tag(&conn, "沙发", "情绪", Some(root));

        let error = move_tag_in(&mut conn, root, Some(child), None).unwrap_err();

        assert!(error.contains("后代"), "应提示不能移到后代：{error}");
        assert_eq!(parent_of(&conn, root), None, "失败后结构不变");
    }

    /// 目标位置已有同名 Tag 时，给一句人话而不是抛唯一索引错误。
    #[test]
    fn move_tag_reports_name_clash() {
        let mut conn = test_conn();
        let tag = add_tag(&conn, "温暖", "情绪", None);
        add_tag(&conn, "温暖", "未分类", None);

        let uncategorized = category_id_of(&conn, "未分类");
        let error = move_tag_in(&mut conn, tag, None, Some(uncategorized)).unwrap_err();

        assert!(error.contains("同名"), "应提示同名：{error}");
        assert_eq!(category_of(&conn, tag), "情绪", "失败后不该移动");
    }

    /// 拖到自身或同维度根层都不能算错。
    #[test]
    fn move_tag_rejects_self_and_allows_noop() {
        let mut conn = test_conn();
        let tag = add_tag(&conn, "温暖", "情绪", None);

        assert!(move_tag_in(&mut conn, tag, Some(tag), None).unwrap_err().contains("自身"));
        let emotional = category_id_of(&conn, "情绪");
        move_tag_in(&mut conn, tag, None, Some(emotional)).unwrap();
        assert_eq!(category_of(&conn, tag), "情绪");
    }
}
