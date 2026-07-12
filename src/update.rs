//! Notify-only update checker.
//!
//! On launch we ask the GitHub Releases API for the latest published release
//! and, if its version is newer than the running build, surface a dismissible
//! banner pointing the user at the release notes and the `brew upgrade`
//! command. We deliberately never download or replace the binary: the `.app`
//! is ad-hoc signed (not notarized) and is normally managed by Homebrew, so an
//! in-place self-update would fight both Gatekeeper and the package manager.
//!
//! Modeled on `loader.rs`: spawn a thread, return a `Receiver` the UI polls
//! each frame with `try_recv()`.

use std::sync::mpsc;

use serde::Deserialize;

/// GitHub repo (owner/name) — mirrors `repository` in Cargo.toml.
const REPO: &str = "evyatar/quick-json-viewer";

/// Suggested upgrade command shown in the banner.
pub const BREW_UPGRADE_CMD: &str = "brew upgrade --cask evyatar/tap/quick-json-viewer";

/// Details of a newer release worth telling the user about.
#[derive(Clone, Debug)]
pub struct ReleaseInfo {
    /// Normalized version, e.g. `1.1.0` (leading `v` stripped).
    pub version:  String,
    /// Browser URL of the release page.
    pub html_url: String,
    /// Release notes body (may be empty).
    pub notes:    String,
}

pub enum UpdateMsg {
    UpToDate,
    Available(ReleaseInfo),
    /// The brew upgrade completed and this version is now installed.
    Installed,
    Error(String),
}

/// Shape of the subset of the GitHub `releases/latest` payload we care about.
#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    body:     Option<String>,
}

/// Run the Homebrew upgrade in a background thread and return a receiver that
/// yields `Installed` on success or `Error` on failure. No Terminal window is
/// opened; the thread exits naturally when brew finishes.
pub fn launch_brew_upgrade() -> mpsc::Receiver<UpdateMsg> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = std::process::Command::new(brew_bin())
            .args(["upgrade", "--cask", "evyatar/tap/quick-json-viewer"])
            .status();
        let msg = match result {
            Ok(s) if s.success() => UpdateMsg::Installed,
            Ok(s) => UpdateMsg::Error(format!("brew exited with {s}")),
            Err(e) => UpdateMsg::Error(format!("failed to run brew: {e}")),
        };
        let _ = tx.send(msg);
    });
    rx
}

/// Resolve the brew binary path. macOS .app bundles don't inherit the user's
/// shell PATH, so `/opt/homebrew/bin` (Apple Silicon) is often missing.
fn brew_bin() -> &'static str {
    if std::path::Path::new("/opt/homebrew/bin/brew").exists() {
        "/opt/homebrew/bin/brew"
    } else if std::path::Path::new("/usr/local/bin/brew").exists() {
        "/usr/local/bin/brew"
    } else {
        "brew"
    }
}

/// Relaunch the app and exit the current process.
pub fn restart_app() {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .args(["-a", "Quick JSON Viewer"])
            .spawn();
    }
    #[cfg(not(target_os = "macos"))]
    {
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::process::Command::new(exe).spawn();
        }
    }
    std::process::exit(0);
}

/// Spawn a background check. The returned receiver yields exactly one message.
pub fn spawn_check() -> mpsc::Receiver<UpdateMsg> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let msg = match fetch_latest() {
            Ok(rel) => {
                let latest = rel.tag_name.trim_start_matches('v').to_string();
                if is_newer(&rel.tag_name, env!("CARGO_PKG_VERSION")) {
                    UpdateMsg::Available(ReleaseInfo {
                        version:  latest,
                        html_url: rel.html_url,
                        notes:    rel.body.unwrap_or_default(),
                    })
                } else {
                    UpdateMsg::UpToDate
                }
            }
            Err(e) => UpdateMsg::Error(e),
        };
        let _ = tx.send(msg);
    });
    rx
}

fn fetch_latest() -> Result<GhRelease, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    // GitHub rejects requests without a User-Agent. A ~10s timeout keeps a
    // stalled connection from leaking the thread indefinitely.
    let resp = ureq::get(&url)
        .timeout(std::time::Duration::from_secs(10))
        .set("User-Agent", "quick-json-viewer")
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("request: {e}"))?;
    let body = resp
        .into_string()
        .map_err(|e| format!("read: {e}"))?;
    let rel: GhRelease = serde_json::from_str(&body)
        .map_err(|e| format!("parse: {e}"))?;
    Ok(rel)
}

/// Is `latest` a strictly newer version than `current`? Both may carry a
/// leading `v`. Versions compare component-wise as integers (so `1.10.0`
/// beats `1.9.0`); a tag that fails to parse is treated as not-newer.
fn is_newer(latest: &str, current: &str) -> bool {
    fn parse(v: &str) -> Option<Vec<u32>> {
        v.trim()
            .trim_start_matches('v')
            .split('.')
            .map(|p| p.parse::<u32>().ok())
            .collect()
    }
    match (parse(latest), parse(current)) {
        (Some(l), Some(c)) => {
            // Compare positionally; missing trailing components count as 0.
            let n = l.len().max(c.len());
            for i in 0..n {
                let a = l.get(i).copied().unwrap_or(0);
                let b = c.get(i).copied().unwrap_or(0);
                if a != b {
                    return a > b;
                }
            }
            false
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::is_newer;

    #[test]
    fn newer_patch_and_minor() {
        assert!(is_newer("1.0.1", "1.0.0"));
        assert!(is_newer("1.1.0", "1.0.0"));
        assert!(is_newer("2.0.0", "1.9.9"));
    }

    #[test]
    fn numeric_not_lexical() {
        assert!(is_newer("1.10.0", "1.9.0"));
        assert!(!is_newer("1.9.0", "1.10.0"));
    }

    #[test]
    fn v_prefix_tolerated() {
        assert!(is_newer("v1.1.0", "1.0.0"));
        assert!(is_newer("v1.1.0", "v1.0.0"));
    }

    #[test]
    fn equal_is_not_newer() {
        assert!(!is_newer("1.0.0", "1.0.0"));
        assert!(!is_newer("v1.0.0", "1.0.0"));
    }

    #[test]
    fn trailing_components_default_zero() {
        assert!(!is_newer("1.0", "1.0.0"));
        assert!(is_newer("1.0.1", "1.0"));
    }

    #[test]
    fn malformed_is_not_newer() {
        assert!(!is_newer("garbage", "1.0.0"));
        assert!(!is_newer("1.x.0", "1.0.0"));
    }
}
