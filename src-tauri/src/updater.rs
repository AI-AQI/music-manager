//! Download signed releases; hand off only to the verified standalone installer.
//! The updater never opens, copies, or migrates a music library.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{atomic::{AtomicBool, Ordering}, Mutex},
    time::{Duration, Instant},
};
use tauri::{Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

const PUBLIC_KEY: &str = include_str!("../updater-public-key.txt");
const APP_NAME: &str = "声场档案.app";
const INSTALLER_NAME: &str = "安装与更新声场档案.app";

#[derive(Default)]
pub struct UpdateManager {
    busy: AtomicBool,
    available: Mutex<Option<Update>>,
    downloaded: Mutex<Option<PathBuf>>,
}
struct Busy<'a>(&'a AtomicBool);
impl Drop for Busy<'_> {
    fn drop(&mut self) { self.0.store(false, Ordering::Release); }
}
impl UpdateManager {
    fn begin(&self) -> Result<Busy<'_>, String> {
        self.busy.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "更新操作正在进行，请稍候".to_string())?;
        Ok(Busy(&self.busy))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    current_version: String,
    version: Option<String>,
    notes: String,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    phase: &'static str,
    downloaded: u64,
    total: Option<u64>,
}
fn progress(app: &tauri::AppHandle, phase: &'static str, downloaded: u64, total: Option<u64>) {
    let _ = app.emit("app-update-progress", Progress { phase, downloaded, total });
}
fn asset_arch() -> Result<&'static str, String> {
    // An Intel app running under Rosetta must still fetch the native Apple Silicon release.
    if Command::new("/usr/sbin/sysctl").args(["-n", "hw.optional.arm64"]).output()
        .is_ok_and(|o| o.status.success() && o.stdout.starts_with(b"1")) {
        return Ok("aarch64");
    }
    match std::env::consts::ARCH {
        "aarch64" => Ok("aarch64"),
        "x86_64" => Ok("x64"),
        _ => Err("这台设备暂不支持应用内更新".into()),
    }
}
fn validate_release(version: &str, url: &str, arch: &str, current: &str) -> Result<(), String> {
    let next = semver::Version::parse(version).map_err(|_| "更新版本号无效")?;
    let current = semver::Version::parse(current).map_err(|_| "当前版本号无效")?;
    if next <= current || !next.pre.is_empty() || !next.build.is_empty() {
        return Err("更新版本必须是高于当前版本的正式版本".into());
    }
    let expected = format!("https://github.com/AI-AQI/music-manager/releases/download/v{version}/music-manager_{version}_{arch}.dmg");
    if url != expected { return Err("更新包地址与版本或设备架构不匹配".into()); }
    Ok(())
}
pub fn verify_cached(bytes: &[u8], signature: &str, public_key: &str) -> Result<(), String> {
    let decode = |value: &str| -> Result<String, String> {
        String::from_utf8(STANDARD.decode(value.trim()).map_err(|_| "更新签名编码无效")?)
            .map_err(|_| "更新签名内容无效".into())
    };
    let key = minisign_verify::PublicKey::decode(&decode(public_key)?).map_err(|_| "更新公钥无效")?;
    let signature = minisign_verify::Signature::decode(&decode(signature)?).map_err(|_| "更新签名无效")?;
    key.verify(bytes, &signature, true).map_err(|_| "更新包签名校验失败，请重新下载".into())
}

#[tauri::command]
pub async fn check_app_update(app: tauri::AppHandle, state: tauri::State<'_, UpdateManager>) -> Result<UpdateInfo, String> {
    let _busy = state.begin()?;
    let arch = asset_arch()?;
    let target = if arch == "x64" { "darwin-x86_64" } else { "darwin-aarch64" };
    let mut update = app.updater_builder().target(target).timeout(Duration::from_secs(20))
        .build().map_err(|e| {
            eprintln!("初始化更新失败：{e}");
            "无法初始化更新。请从“应用程序”中打开已安装的声场档案后重试。".to_string()
        })?
        .check().await.map_err(|e| {
            eprintln!("检查更新失败：{e}");
            "暂时无法获取更新。请检查网络后重试；也可能暂未发布支持应用内升级的新版本。".to_string()
        })?;
    if let Some(next) = &mut update {
        validate_release(&next.version, next.download_url.as_str(), arch, crate::db::APP_VERSION)?;
        next.timeout = Some(Duration::from_secs(600));
    }
    let info = UpdateInfo {
        current_version: crate::db::APP_VERSION.into(),
        version: update.as_ref().map(|u| u.version.clone()),
        notes: update.as_ref().and_then(|u| u.body.clone()).unwrap_or_default(),
    };
    *state.downloaded.lock().map_err(|e| e.to_string())? = None;
    *state.available.lock().map_err(|e| e.to_string())? = update;
    Ok(info)
}

#[tauri::command]
pub async fn download_app_update(app: tauri::AppHandle, state: tauri::State<'_, UpdateManager>) -> Result<(), String> {
    let _busy = state.begin()?;
    let update = state.available.lock().map_err(|e| e.to_string())?.clone()
        .ok_or("请先检查更新")?;
    *state.downloaded.lock().map_err(|e| e.to_string())? = None;
    let mut received = 0u64;
    let mut last_event = Instant::now() - Duration::from_secs(1);
    progress(&app, "downloading", 0, None);
    let bytes = update.download(|size, total| {
        received += size as u64;
        if last_event.elapsed() >= Duration::from_millis(100) || total == Some(received) {
            progress(&app, "downloading", received, total);
            last_event = Instant::now();
        }
    }, || progress(&app, "verifying", 0, None)).await
        .map_err(|e| format!("下载或签名校验失败，当前应用未被更改。\n{e}"))?;
    let cache = app.path().app_cache_dir().map_err(|e| e.to_string())?.join("updates");
    fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
    let job = tempfile::Builder::new().prefix("update-").tempdir_in(cache).map_err(|e| e.to_string())?;
    fs::write(job.path().join("update.dmg"), bytes).map_err(|e| e.to_string())?;
    // Keep the handoff payload alive after the main application exits.
    let path = job.keep();
    *state.downloaded.lock().map_err(|e| e.to_string())? = Some(path);
    progress(&app, "ready", received, Some(received));
    Ok(())
}

fn run(command: &str, args: &[&std::ffi::OsStr]) -> Result<(), String> {
    let output = Command::new(command).args(args).output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!("{command} 未完成：{}", String::from_utf8_lossy(&output.stderr)));
    }
    Ok(())
}
fn plist(app: &Path, key: &str) -> Result<String, String> {
    let output = Command::new("/usr/libexec/PlistBuddy")
        .args(["-c", &format!("Print :{key}")]).arg(app.join("Contents/Info.plist"))
        .output().map_err(|e| e.to_string())?;
    if !output.status.success() { return Err("更新包的应用信息不完整".into()); }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
fn validate_payload(installer: &Path, version: &str) -> Result<(), String> {
    run("/usr/bin/codesign", &["--verify".as_ref(), "--deep".as_ref(), "--strict".as_ref(), installer.as_os_str()])?;
    let application = installer.join("Contents/Resources").join(APP_NAME);
    // The entire DMG is signed. Check its authenticated inner version against the
    // unsigned feed, so an old signed package cannot be relabelled as a newer release.
    for (bundle, id) in [(installer, "com.musicmanager.installer"), (application.as_path(), "com.musicmanager.app")] {
        if plist(bundle, "CFBundleIdentifier")? != id || plist(bundle, "CFBundleShortVersionString")? != version {
            return Err("更新包的身份或实际版本与更新清单不符，已停止更新".into());
        }
    }
    if plist(installer, "MusicManagerInAppUpdateProtocol")? != "1" {
        return Err("此安装包不支持应用内更新".into());
    }
    Ok(())
}
fn installed_location(executable: &Path, home: &Path) -> Result<PathBuf, String> {
    let app = executable.parent().and_then(Path::parent).and_then(Path::parent).ok_or("无法确认应用安装位置")?;
    let allowed = [PathBuf::from("/Applications").join(APP_NAME), home.join("Applications").join(APP_NAME)];
    if !allowed.iter().any(|p| p == app) || executable != app.join("Contents/MacOS/music-manager") {
        return Err("请先将声场档案安装到“应用程序”，再使用应用内更新。开发模式和 DMG 中的应用不能原地升级。".into());
    }
    Ok(app.to_owned())
}

#[derive(Deserialize)]
struct HandoffReply {
    token: String,
    version: String,
    pid: u32,
    error: Option<String>,
}
fn prepare_handoff(job: &Path, update: &Update, destination: &Path) -> Result<(), String> {
    let bytes = fs::read(job.join("update.dmg")).map_err(|e| e.to_string())?;
    verify_cached(&bytes, &update.signature, PUBLIC_KEY)?;
    let mount = tempfile::Builder::new().prefix("mount-").tempdir_in(job).map_err(|e| e.to_string())?;
    let helper_dir = tempfile::Builder::new().prefix("helper-").tempdir_in(job).map_err(|e| e.to_string())?;
    run("/usr/bin/hdiutil", &["attach".as_ref(), "-readonly".as_ref(), "-nobrowse".as_ref(),
        "-mountpoint".as_ref(), mount.path().as_os_str(), job.join("update.dmg").as_os_str()])?;
    let helper = helper_dir.path().join(INSTALLER_NAME);
    let copy_result = (|| {
        let source = mount.path().join(INSTALLER_NAME);
        validate_payload(&source, &update.version)?;
        run("/usr/bin/ditto", &[source.as_os_str(), helper.as_os_str()])?;
        validate_payload(&helper, &update.version)?;
        Ok::<_, String>(())
    })();
    let detach_result = run("/usr/bin/hdiutil", &["detach".as_ref(), mount.path().as_os_str()]);
    copy_result?;
    detach_result?;
    let helper_root = helper_dir.keep();
    let reply_path = helper_root.join("handoff.json");
    let token = helper_root.file_name().unwrap().to_string_lossy().into_owned();
    let log = fs::File::create(helper_root.join("handoff.log")).map_err(|e| e.to_string())?;
    let mut child = Command::new(helper.join("Contents/MacOS/music-manager-installer"))
        .arg("--update-destination").arg(destination)
        .arg("--parent-pid").arg(std::process::id().to_string())
        .arg("--expected-version").arg(&update.version)
        .arg("--handoff").arg(&reply_path).arg("--token").arg(&token)
        .stdin(Stdio::null()).stdout(log.try_clone().map_err(|e| e.to_string())?).stderr(log)
        .spawn().map_err(|e| format!("无法启动更新程序，当前应用继续运行：{e}"))?;
    let ready = (|| {
        let deadline = Instant::now() + Duration::from_secs(25);
        while Instant::now() < deadline {
            if let Ok(data) = fs::read(&reply_path) {
                let reply: HandoffReply = serde_json::from_slice(&data).map_err(|_| "更新程序响应无效")?;
                if reply.token != token || reply.version != update.version || reply.pid != child.id() {
                    return Err("更新程序响应与本次操作不匹配".to_string());
                }
                if let Some(error) = reply.error { return Err(error); }
                return Ok(());
            }
            if child.try_wait().map_err(|e| e.to_string())?.is_some() {
                return Err("更新程序启动失败，当前应用继续运行".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err("更新程序未确认就绪，当前应用继续运行，请重试".into())
    })();
    if ready.is_err() {
        // The helper must wait for this parent to exit before modifying anything.
        let _ = child.kill();
        let _ = child.wait();
    }
    ready
}

#[tauri::command]
pub async fn restart_for_app_update(app: tauri::AppHandle, state: tauri::State<'_, UpdateManager>) -> Result<(), String> {
    let _busy = state.begin()?;
    let update = state.available.lock().map_err(|e| e.to_string())?.clone().ok_or("请先检查更新")?;
    let job = state.downloaded.lock().map_err(|e| e.to_string())?.clone().ok_or("请先下载更新")?;
    let destination = installed_location(
        &std::env::current_exe().map_err(|e| e.to_string())?,
        &app.path().home_dir().map_err(|e| e.to_string())?,
    )?;
    progress(&app, "preparing", 0, None);
    tauri::async_runtime::spawn_blocking(move || prepare_handoff(&job, &update, &destination))
        .await.map_err(|e| e.to_string())??;
    app.exit(0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cached_update_signature_rejects_corruption_and_wrong_keys() {
        let bytes = include_bytes!("../../scripts/fixtures/updater/signed-test.txt");
        let signature = include_str!("../../scripts/fixtures/updater/signed-test.txt.sig");
        let test_key = include_str!("../../scripts/fixtures/updater/test-public-key.txt");
        assert!(verify_cached(bytes, signature, test_key).is_ok());
        assert!(verify_cached(b"tampered", signature, test_key).is_err());
        assert!(verify_cached(bytes, signature, PUBLIC_KEY).is_err());
        assert!(verify_cached(bytes, "not a signature", test_key).is_err());
        let config: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(config["plugins"]["updater"]["pubkey"].as_str().unwrap(), PUBLIC_KEY.trim());
    }
    #[test]
    fn release_must_match_version_architecture_and_repository() {
        let url = "https://github.com/AI-AQI/music-manager/releases/download/v0.2.6/music-manager_0.2.6_aarch64.dmg";
        assert!(validate_release("0.2.6", url, "aarch64", "0.2.5").is_ok());
        for (version, arch, current) in [("0.2.6", "x64", "0.2.5"), ("0.2.7", "aarch64", "0.2.5"),
            ("0.2.6", "aarch64", "0.2.6"), ("0.2.6-beta.1", "aarch64", "0.2.5")] {
            assert!(validate_release(version, url, arch, current).is_err());
        }
        assert!(validate_release("0.2.6", &url.replace("github.com", "example.com"), "aarch64", "0.2.5").is_err());
    }
    #[test]
    fn development_and_disk_image_apps_cannot_be_replaced() {
        let home = Path::new("/Users/test");
        assert!(installed_location(Path::new("/Applications/声场档案.app/Contents/MacOS/music-manager"), home).is_ok());
        assert!(installed_location(Path::new("/Users/test/Applications/声场档案.app/Contents/MacOS/music-manager"), home).is_ok());
        for path in ["/Volumes/Test/声场档案.app/Contents/MacOS/music-manager", "/tmp/music-manager", "/Applications/Other.app/Contents/MacOS/music-manager"] {
            assert!(installed_location(Path::new(path), home).is_err());
        }
    }
    #[test]
    fn concurrent_update_operations_are_rejected_and_failures_release_lock() {
        let state = UpdateManager::default();
        let busy = state.begin().unwrap();
        assert!(state.begin().is_err());
        drop(busy);
        assert!(state.begin().is_ok());
    }
}
