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

    let url = format!(
        "https://api.github.com/repos/{REPO_OWNER}/{REPO_NAME}/releases/latest"
    );

    let release: Release = client
        .get(&url)
        .send()
        .await
        .map_err(|e| SlagError::UpdateFailed(format!("fetch failed: {e}")))?
        .json()
        .await
        .map_err(|e| SlagError::UpdateFailed(format!("parse failed: {e}")))?;

    let latest = release.tag_name.trim_start_matches('v');
    if !is_newer(latest, current_version) {
        println!("  Already up to date (v{current_version}, latest release v{latest})");
        return Ok(());
    }

    println!("  New version available: v{latest}");

    // Determine platform asset name
    let asset_name = platform_asset_name()
        .ok_or_else(|| SlagError::UpdateFailed("unsupported platform".into()))?;

    // Prefer an exact raw-binary asset; fall back to a tarball match.
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == asset_name)
        .or_else(|| release.assets.iter().find(|a| a.name.contains(&asset_name)))
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
    if asset.name.ends_with(".tar.gz") || asset.name.ends_with(".tgz") {
        // cargo-dist ships tarballs; installing raw bytes as the
        // executable yields "exec format error". Extract first.
        extract_binary_from_targz(&bytes, &tmp_path).await?;
    } else {
        tokio::fs::write(&tmp_path, &bytes)
            .await
            .map_err(|e| SlagError::UpdateFailed(format!("write tmp failed: {e}")))?;
    }

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

/// Extract the `slag` binary from a .tar.gz release asset into `dest`.
/// Uses the system tar; the archive may nest the binary one directory deep.
async fn extract_binary_from_targz(
    bytes: &[u8],
    dest: &std::path::Path,
) -> Result<(), SlagError> {
    let work = tempdir_path()?;
    tokio::fs::create_dir_all(&work)
        .await
        .map_err(|e| SlagError::UpdateFailed(format!("mkdir failed: {e}")))?;
    let archive = work.join("asset.tar.gz");
    tokio::fs::write(&archive, bytes)
        .await
        .map_err(|e| SlagError::UpdateFailed(format!("write archive failed: {e}")))?;

    let out = tokio::process::Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(&work)
        .output()
        .await
        .map_err(|e| SlagError::UpdateFailed(format!("tar spawn failed: {e}")))?;
    if !out.status.success() {
        return Err(SlagError::UpdateFailed(format!(
            "tar extract failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }

    let binary = find_slag_binary(&work).ok_or_else(|| {
        SlagError::UpdateFailed("no `slag` binary inside release archive".into())
    })?;
    tokio::fs::copy(&binary, dest)
        .await
        .map_err(|e| SlagError::UpdateFailed(format!("copy binary failed: {e}")))?;
    let _ = tokio::fs::remove_dir_all(&work).await;
    Ok(())
}

/// Find a file named `slag` up to two levels deep.
fn find_slag_binary(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    fn scan(dir: &std::path::Path, depth: u8) -> Option<std::path::PathBuf> {
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_file() && path.file_name().is_some_and(|n| n == "slag") {
                return Some(path);
            }
            if path.is_dir() && depth > 0 {
                if let Some(found) = scan(&path, depth - 1) {
                    return Some(found);
                }
            }
        }
        None
    }
    scan(dir, 2)
}

fn tempdir_path() -> Result<std::path::PathBuf, SlagError> {
    let base = std::env::temp_dir();
    let unique = format!(
        "slag-update-{}",
        std::process::id()
    );
    Ok(base.join(unique))
}

/// True only when `latest` is a strictly newer x.y.z than `current`.
/// Refuses downgrades: a stale remote release must never replace a
/// newer local build. Unparseable versions compare as not-newer.
fn is_newer(latest: &str, current: &str) -> bool {
    fn parse(v: &str) -> Option<[u64; 3]> {
        let mut parts = v.split('.').map(|p| p.trim().parse::<u64>().ok());
        Some([parts.next()??, parts.next()??, parts.next()??])
    }
    match (parse(latest), parse(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
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
