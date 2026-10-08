//! Lightpanda download for `agent-browser install`.
//!
//! Binaries come from the Lightpanda GitHub releases and are stored next to
//! the Chrome for Testing builds in `~/.agent-browser/browsers/lightpanda-<version>/`.

use std::fs;
use std::path::{Path, PathBuf};

use crate::install::get_browsers_dir;

/// Release installed by default. Override with `AGENT_BROWSER_LIGHTPANDA_VERSION`
/// (e.g. `nightly`).
pub const LIGHTPANDA_VERSION: &str = "1.0.0";
const RELEASES_URL: &str = "https://github.com/lightpanda-io/browser/releases/download";

pub fn lightpanda_version() -> String {
    std::env::var("AGENT_BROWSER_LIGHTPANDA_VERSION")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| LIGHTPANDA_VERSION.to_string())
}

/// Release asset suffix for this platform, or `None` where Lightpanda ships no build.
pub fn platform_asset() -> Option<&'static str> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("aarch64-macos")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("x86_64-macos")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("x86_64-linux")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some("aarch64-linux")
    } else {
        None
    }
}

pub fn download_url(version: &str, asset: &str) -> String {
    format!("{}/{}/lightpanda-{}", RELEASES_URL, version, asset)
}

fn binary_in_dir(dir: &Path) -> PathBuf {
    dir.join("lightpanda")
}

/// Finds the most recently installed Lightpanda under the browsers directory.
pub fn find_installed_lightpanda() -> Option<PathBuf> {
    let entries = fs::read_dir(get_browsers_dir()).ok()?;
    entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with("lightpanda-"))
        })
        .map(|e| binary_in_dir(&e.path()))
        .filter(|bin| bin.is_file())
        .max_by_key(|bin| fs::metadata(bin).and_then(|m| m.modified()).ok())
}

/// Downloads Lightpanda unless the requested version is already present.
/// Returns the installed binary path.
pub async fn install_lightpanda() -> Result<(PathBuf, bool), String> {
    let asset = platform_asset().ok_or("Lightpanda does not provide builds for this platform")?;
    let version = lightpanda_version();
    let dest = get_browsers_dir().join(format!("lightpanda-{}", version));
    let bin = binary_in_dir(&dest);
    // A nightly is a moving target, so it is always refreshed.
    if bin.is_file() && version != "nightly" {
        return Ok((bin, false));
    }

    let url = download_url(&version, asset);
    println!("  Downloading Lightpanda {} for {}", version, asset);
    println!("  {}", url);
    let bytes = crate::install::download_bytes(&url).await?;
    if bytes.len() < 1024 {
        return Err(format!(
            "Downloaded Lightpanda binary is unexpectedly small ({} bytes)",
            bytes.len()
        ));
    }

    fs::create_dir_all(&dest).map_err(|e| format!("Failed to create {}: {}", dest.display(), e))?;
    let tmp = dest.join("lightpanda.download");
    fs::write(&tmp, &bytes).map_err(|e| format!("Failed to write {}: {}", tmp.display(), e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("Failed to mark Lightpanda executable: {}", e))?;
    }
    fs::rename(&tmp, &bin).map_err(|e| format!("Failed to install Lightpanda: {}", e))?;
    Ok((bin, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_url_points_at_release_asset() {
        assert_eq!(
            download_url("1.0.0", "x86_64-linux"),
            "https://github.com/lightpanda-io/browser/releases/download/1.0.0/lightpanda-x86_64-linux"
        );
    }

    #[test]
    fn supported_platforms_have_an_asset() {
        if cfg!(any(target_os = "macos", target_os = "linux"))
            && cfg!(any(target_arch = "x86_64", target_arch = "aarch64"))
        {
            assert!(platform_asset().is_some());
        }
        if cfg!(windows) {
            assert!(platform_asset().is_none());
        }
    }
}
