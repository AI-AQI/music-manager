use crate::db::tag_category;
use crate::AppState;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

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
                    let text = decode_id3_text(&tag[pos + 10..pos + 10 + size]);
                    match id {
                        b"TIT2" => metadata.title = text,
                        b"TPE1" => metadata.artist = text,
                        b"TALB" => metadata.album = text,
                        b"TCON" => metadata.genre = text,
                        b"TDRC" | b"TYER" => metadata.year = text.chars().take(4).collect(),
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

    tx.execute(
        "INSERT INTO music (id, name, file_name, path, album, artist, genre, year, channels, sample_rate, bitrate, duration, file_size, mime_type, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        params![
            music.id,
            music.name,
            music.file_name,
            music.path,
            music.album,
            music.artist,
            music.genre,
            music.year,
            music.channels,
            music.sample_rate,
            music.bitrate,
            music.duration,
            music.file_size,
            music.mime_type,
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

    tx.execute(
        "UPDATE music SET name=?1, file_name=?2, path=?3, album=?4, artist=?5, genre=?6, year=?7, channels=?8, sample_rate=?9, bitrate=?10, duration=?11, file_size=?12, mime_type=?13, updated_at=?14 WHERE id=?15",
        params![
            music.name,
            music.file_name,
            music.path,
            music.album,
            music.artist,
            music.genre,
            music.year,
            music.channels,
            music.sample_rate,
            music.bitrate,
            music.duration,
            music.file_size,
            music.mime_type,
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
        "SELECT t.id, t.name, t.category, COUNT(mt.music_id) AS music_count \
         FROM tags t LEFT JOIN music_tags mt ON mt.tag_id = t.id \
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
    if category.is_empty() { return Err("Tag 分类不能为空".into()); }
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
    let mut stmt = conn.prepare("SELECT name FROM tag_categories ORDER BY name COLLATE NOCASE")
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
pub fn file_exists(path: String) -> bool {
    Path::new(&path).is_file()
}

#[tauri::command]
pub fn get_audio_base_url() -> Option<String> {
    // 仅 Plan-B（本地 HTTP 音频流服务）启用时返回。
    None
}
