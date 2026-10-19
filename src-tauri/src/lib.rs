pub mod commands;
pub mod db;

use std::sync::Mutex;
use tauri::Manager;

pub struct AppState {
    pub db: Mutex<rusqlite::Connection>,
    pub db_path: Mutex<std::path::PathBuf>,
}

struct DatabaseStartupLock(Mutex<()>);

pub fn initialize_database(app: &tauri::AppHandle) -> Result<bool, String> {
    let startup = app.state::<DatabaseStartupLock>();
    let _guard = startup.0.lock().map_err(|e| format!("音乐库初始化锁获取失败: {e}"))?;
    if app.try_state::<AppState>().is_some() { return Ok(true); }
    let dir = app.path().app_data_dir().map_err(|e| format!("无法获取数据目录: {e}"))?;
    let Some((path, conn)) = db::open_startup_database(&dir)? else { return Ok(false); };
    // 只有真正打开了目标库才注册数据库状态，失败时不提供临时或默认连接。
    app.manage(AppState { db: Mutex::new(conn), db_path: Mutex::new(path) });
    Ok(true)
}

pub fn configure_database_startup(app: &tauri::AppHandle, dir: &std::path::Path, create_new: bool) -> Result<(), String> {
    let startup = app.state::<DatabaseStartupLock>();
    let _guard = startup.0.lock().map_err(|e| format!("音乐库初始化锁获取失败: {e}"))?;
    if app.try_state::<AppState>().is_some() { return Err("音乐库已经打开，请使用“切换音乐库”".into()); }
    let default_dir = app.path().app_data_dir().map_err(|e| format!("无法获取数据目录: {e}"))?;
    let (path, conn) = db::configure_startup_database(&default_dir, dir, create_new)?;
    app.manage(AppState { db: Mutex::new(conn), db_path: Mutex::new(path) });
    Ok(())
}

/* cargo tauri dev 跑的是裸二进制，没有 .app bundle，macOS 无从读取 bundle 图标——
   dev 构建下运行时手动把图标设到 Dock 上；release 走 bundle 里的 icon.icns，不需要 */
#[cfg(all(target_os = "macos", debug_assertions))]
fn set_dock_icon() {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSString;

    let path = NSString::from_str(concat!(env!("CARGO_MANIFEST_DIR"), "/icons/icon.png"));
    unsafe {
        let ns_image = AnyClass::get(c"NSImage").expect("NSImage class");
        let image: *mut AnyObject = msg_send![ns_image, alloc];
        let image: *mut AnyObject = msg_send![image, initWithContentsOfFile: &*path];
        if image.is_null() {
            return;
        }
        let ns_app = AnyClass::get(c"NSApplication").expect("NSApplication class");
        let app: *mut AnyObject = msg_send![ns_app, sharedApplication];
        let _: () = msg_send![app, setApplicationIconImage: image];
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            #[cfg(all(target_os = "macos", debug_assertions))]
            set_dock_icon();

            // 初始化交给首个 init_db 请求，失败时窗口仍能提示连接硬盘并重试。
            app.manage(DatabaseStartupLock(Mutex::new(())));

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::init_db,
            commands::configure_database_startup,
            commands::cancel_database_startup,
            commands::probe_file,
            commands::export_clip_wav,
            commands::scan_import_sources,
            commands::create_music,
            commands::update_music,
            commands::delete_music,
            commands::delete_music_batch,
            commands::get_music,
            commands::list_music,
            commands::mark_music_played,
            commands::get_library_counts,
            commands::get_music_cover_arts,
            commands::find_existing_music_paths,
            commands::create_clip,
            commands::update_clip,
            commands::delete_clip,
            commands::delete_clip_batch,
            commands::get_clip,
            commands::list_clips,
            commands::list_clips_for_music,
            commands::list_candidates,
            commands::list_tags,
            commands::create_tag,
            commands::update_tag,
            commands::move_tag,
            commands::delete_tag,
            commands::list_tag_categories,
            commands::list_tag_category_records,
            commands::list_album_tags,
            commands::set_album_tags,
            commands::delete_album_tags,
            commands::create_tag_category,
            commands::update_tag_category,
            commands::delete_tag_category,
            commands::create_candidate,
            commands::delete_candidate,
            commands::clear_candidates,
            commands::file_exists,
            commands::get_database_path,
            commands::set_database_location,
            commands::get_audio_base_url,
            commands::sync_library_locations,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
