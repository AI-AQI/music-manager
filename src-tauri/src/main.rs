// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // A separate read-only process, before creating any window or opening a writable DB.
    if std::env::args().nth(1).as_deref() == Some("--installer-check-rollback") {
        let result = (|| -> Result<(), String> {
            let schema = std::env::args().nth(2).and_then(|s| s.parse::<i64>().ok())
                .ok_or_else(|| "缺少旧版支持的数据库版本".to_string())?;
            let home = std::env::var_os("HOME").ok_or_else(|| "无法获取用户目录".to_string())?;
            let context: tauri::Context<tauri::Wry> = tauri::generate_context!();
            let identifier = context.config().identifier.clone();
            let dir = std::path::PathBuf::from(home).join("Library/Application Support").join(identifier);
            music_manager_lib::db::check_rollback_compatibility(&dir, schema)
        })();
        match result {
            Ok(()) => println!("compatible"),
            Err(error) => { eprintln!("{error}"); std::process::exit(1); }
        }
        return;
    }
    music_manager_lib::run()
}
