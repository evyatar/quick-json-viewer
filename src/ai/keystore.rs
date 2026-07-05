//! API-key storage in the macOS Keychain via the `security` CLI.
//!
//! The eframe settings blob is plaintext JSON on disk, so keys never go
//! there. Each provider gets its own Keychain item under a shared service
//! name; on non-macOS builds these are no-ops (the app is macOS-only).

const SERVICE: &str = "quick-json-viewer-ai";

#[cfg(target_os = "macos")]
pub fn get_key(account: &str) -> Option<String> {
    let out = std::process::Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", SERVICE, "-a", account, "-w"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let key = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (!key.is_empty()).then_some(key)
}

#[cfg(target_os = "macos")]
pub fn set_key(account: &str, key: &str) -> Result<(), String> {
    // -U updates an existing item instead of failing on duplicates.
    let out = std::process::Command::new("/usr/bin/security")
        .args(["add-generic-password", "-U", "-s", SERVICE, "-a", account, "-w", key])
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

#[cfg(target_os = "macos")]
pub fn delete_key(account: &str) {
    let _ = std::process::Command::new("/usr/bin/security")
        .args(["delete-generic-password", "-s", SERVICE, "-a", account])
        .output();
}

#[cfg(not(target_os = "macos"))]
pub fn get_key(_account: &str) -> Option<String> {
    None
}

#[cfg(not(target_os = "macos"))]
pub fn set_key(_account: &str, _key: &str) -> Result<(), String> {
    Err("secure key storage is only supported on macOS".to_owned())
}

#[cfg(not(target_os = "macos"))]
pub fn delete_key(_account: &str) {}
