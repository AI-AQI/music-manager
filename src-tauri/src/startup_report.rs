//! A per-launch handshake for the installer; never contains library contents.
use serde::Serialize;
use std::{fs, path::PathBuf, sync::Mutex};

static WRITE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Serialize)]
struct Report<'a> {
    protocol: u8,
    session: &'a str,
    pid: u32,
    version: &'a str,
    state: &'a str,
}

pub fn write(state: &str) {
    let Ok(_guard) = WRITE_LOCK.lock() else { return; };
    let (Ok(path), Ok(session)) = (
        std::env::var("MUSIC_MANAGER_STARTUP_REPORT"),
        std::env::var("MUSIC_MANAGER_STARTUP_SESSION"),
    ) else { return; };
    let path = PathBuf::from(path);
    if !path.is_absolute() || session.len() > 128 || session.is_empty() { return; }
    let report = Report {
        protocol: 1, session: &session, pid: std::process::id(),
        version: crate::db::APP_VERSION, state,
    };
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
        fs::write(&temporary, serde_json::to_vec(&report)?)?;
        fs::rename(temporary, path)?;
        Ok(())
    })();
    if let Err(error) = result { eprintln!("无法记录安装启动状态: {error}"); }
}

pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        write("failed");
        previous(info);
    }));
}

#[tauri::command]
pub fn report_startup_state(state: String) -> Result<(), String> {
    if !matches!(state.as_str(), "ready" | "waiting_library" | "library_unavailable" | "failed") {
        return Err("未知启动状态".into());
    }
    write(&state);
    Ok(())
}
