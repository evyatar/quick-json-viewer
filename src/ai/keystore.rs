//! API-key storage in the platform credential store: the macOS Keychain
//! (via the `security` CLI) or the Windows Credential Manager (via the
//! `keyring` crate).
//!
//! The eframe settings blob is plaintext JSON on disk, so keys never go
//! there. Each provider gets its own item under a shared service name; on
//! other platforms these are no-ops.

/// User-facing name of the platform credential store.
pub const STORE_NAME: &str = if cfg!(target_os = "macos") {
    "macOS Keychain"
} else if cfg!(target_os = "windows") {
    "Windows Credential Manager"
} else {
    "system credential store"
};

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

#[cfg(target_os = "windows")]
pub fn get_key(account: &str) -> Option<String> {
    let key = keyring::Entry::new(SERVICE, account).ok()?.get_password().ok()?;
    (!key.is_empty()).then_some(key)
}

#[cfg(target_os = "windows")]
pub fn set_key(account: &str, key: &str) -> Result<(), String> {
    keyring::Entry::new(SERVICE, account)
        .and_then(|e| e.set_password(key))
        .map_err(|e| e.to_string())
}

#[cfg(target_os = "windows")]
pub fn delete_key(account: &str) {
    let _ = keyring::Entry::new(SERVICE, account).and_then(|e| e.delete_credential());
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn get_key(_account: &str) -> Option<String> {
    None
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn set_key(_account: &str, _key: &str) -> Result<(), String> {
    Err("secure key storage is only supported on macOS and Windows".to_owned())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn delete_key(_account: &str) {}
