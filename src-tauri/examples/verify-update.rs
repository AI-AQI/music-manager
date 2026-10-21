#[cfg(target_os = "macos")]
fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert_eq!(args.len(), 3, "Usage: verify-update FILE SIGNATURE_FILE");
    let bytes = std::fs::read(&args[1]).expect("read update");
    let signature = std::fs::read_to_string(&args[2]).expect("read signature");
    music_manager_lib::updater::verify_cached(
        &bytes, &signature, include_str!("../updater-public-key.txt"),
    ).expect("signature must match the public key shipped in the app");
    println!("Update signature verified");
}
#[cfg(not(target_os = "macos"))]
fn main() { panic!("Use the macOS release runner"); }
