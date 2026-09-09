pub mod commands;
pub mod db;

use std::sync::Mutex;
use tauri::Manager;

pub struct AppState {
    pub db: Mutex<rusqlite::Connection>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let conn = db::init(app)?;
            app.manage(AppState {
                db: Mutex::new(conn),
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::init_db,
            commands::probe_file,
            commands::scan_import_sources,
            commands::create_music,
            commands::update_music,
            commands::delete_music,
            commands::delete_music_batch,
            commands::get_music,
            commands::list_music,
            commands::find_existing_music_paths,
            commands::create_clip,
            commands::update_clip,
            commands::delete_clip,
            commands::delete_clip_batch,
            commands::get_clip,
            commands::list_clips,
            commands::list_candidates,
            commands::list_tags,
            commands::create_tag,
            commands::update_tag,
            commands::delete_tag,
            commands::list_tag_categories,
            commands::create_tag_category,
            commands::update_tag_category,
            commands::delete_tag_category,
            commands::create_candidate,
            commands::delete_candidate,
            commands::clear_candidates,
            commands::file_exists,
            commands::get_database_path,
            commands::get_audio_base_url,
            commands::sync_library_locations,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
