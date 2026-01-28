use crate::error::SlagError;
use serde::Deserialize;

const REPO_OWNER: &str = "sliday";
const REPO_NAME: &str = "slag";

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// Check for and perform self-update via GitHub Releases.
pub async fn self_update() -> Result<(), SlagError> {
    let current_version = env!("CARGO_PKG_VERSION");
    println!("  Current version: v{current_version}");
    println!("  Checking for updates...");

    let client = reqwest::Client::builder()
        .user_agent(format!("slag/{current_version}"))
        .build()
        .map_err(|e| SlagError::UpdateFailed(format!("http client: {e}")))?;

    let url = format!("https://api.github.com/repos/{REPO_OWNER}/{REPO_NAME}/releases/latest");

    let release: Release = client
        .get(&url)
        .send()
        .await
        .map_err(|e| SlagError::UpdateFailed(format!("fetch failed: {e}")))?
        .json()
        .await
        .map_err(|e| SlagError::UpdateFailed(format!("parse failed: {e}")))?;

    let latest = release.tag_name.trim_start_matches('v');
    if latest == current_version {
        println!("  Already up to date (v{current_version})");
        return Ok(());
    }

    println!("  New version available: v{latest}");

    // Determine platform asset name
    let asset_name = platform_asset_name()
        .ok_or_else(|| SlagError::UpdateFailed("unsupported platform".into()))?;

    let asset = release
        .assets
        .iter()
        .find(|a| a.name.contains(&asset_name))
        .ok_or_else(|| {
            SlagError::UpdateFailed(format!("no asset matching {asset_name} in release"))
        })?;

    println!("  Downloading {}...", asset.name);

    let bytes = client
        .get(&asset.browser_download_url)
        .send()
        .await
        .map_err(|e| SlagError::UpdateFailed(format!("download failed: {e}")))?
        .bytes()
        .await
        .map_err(|e| SlagError::UpdateFailed(format!("read failed: {e}")))?;

    // Write to temp file and replace current binary
    let current_exe = std::env::current_exe()
        .map_err(|e| SlagError::UpdateFailed(format!("cannot find current exe: {e}")))?;

    let tmp_path = current_exe.with_extension("tmp");
    tokio::fs::write(&tmp_path, &bytes)
        .await
        .map_err(|e| SlagError::UpdateFailed(format!("write tmp failed: {e}")))?;

    // Make executable on unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&tmp_path, perms)
            .map_err(|e| SlagError::UpdateFailed(format!("chmod failed: {e}")))?;
    }

    // Replace current binary
    std::fs::rename(&tmp_path, &current_exe)
        .map_err(|e| SlagError::UpdateFailed(format!("replace failed: {e}")))?;

    println!("  Updated to v{latest}");
    Ok(())
}

fn platform_asset_name() -> Option<String> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    let target = match (os, arch) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        _ => return None,
    };

    Some(target.to_string())
}
