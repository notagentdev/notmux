use crate::status::{UpdateInfo, UpdateStatus};
use anyhow::{Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Download an asset, reporting progress via `UpdateInfo`.
pub async fn download_asset(
    url: String,
    asset_name: String,
    version: String,
    update_info: UpdateInfo,
    cancel_token: u64,
    checksum_url: Option<String>,
) -> Result<PathBuf> {
    smol::unblock(move || {
        let checksum_url = checksum_url.context("release has no SHA256SUMS; refusing unverified update")?;
        let path = download_blocking(&url, &asset_name, &version, &update_info, cancel_token)?;
        if let Err(error) = verify_checksum(&path, &asset_name, &checksum_url) {
            let _ = std::fs::remove_file(&path);
            return Err(error);
        }
        Ok(path)
    })
    .await
}

fn download_blocking(
    url: &str,
    asset_name: &str,
    version: &str,
    update_info: &UpdateInfo,
    cancel_token: u64,
) -> Result<PathBuf> {
    let updates_dir = crate::process::get_config_dir().join("updates");
    std::fs::create_dir_all(&updates_dir).context("failed to create updates directory")?;

    cleanup_updates_dir();

    let dest = updates_dir.join(asset_name);

    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(3600))
        .user_agent(format!("notmux/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .context("failed to build HTTP client")?;

    let resp = client
        .get(url)
        .send()
        .context("failed to start download")?
        .error_for_status()
        .context("server returned an error status")?;

    let total = resp.content_length().unwrap_or(0);
    let mut downloaded: u64 = 0;
    let mut last_pct: u8 = 0;
    let mut file = std::fs::File::create(&dest).context("failed to create download file")?;

    let mut reader = resp;
    let mut buf = [0u8; 65536];

    update_info.set_status(UpdateStatus::Downloading {
        version: version.to_string(),
        progress: 0,
    });

    loop {
        if update_info.is_cancelled(cancel_token) {
            drop(file);
            let _ = std::fs::remove_file(&dest);
            anyhow::bail!("download cancelled");
        }

        let n = std::io::Read::read(&mut reader, &mut buf).context("download read error")?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .context("failed to write download")?;
        downloaded += n as u64;

        if total > 0 {
            let pct = ((downloaded as f64 / total as f64) * 100.0).min(100.0) as u8;
            if pct != last_pct {
                last_pct = pct;
                update_info.set_status(UpdateStatus::Downloading {
                    version: version.to_string(),
                    progress: pct,
                });
            }
        }
    }

    file.flush().context("failed to flush download")?;

    if total > 0 && downloaded != total {
        let _ = std::fs::remove_file(&dest);
        anyhow::bail!(
            "incomplete download: got {} bytes, expected {} bytes",
            downloaded,
            total
        );
    }

    if downloaded == 0 {
        let _ = std::fs::remove_file(&dest);
        anyhow::bail!("downloaded file is empty");
    }

    log::info!("Downloaded {} ({} bytes)", asset_name, downloaded);
    Ok(dest)
}

fn verify_checksum(file_path: &Path, asset_name: &str, checksum_url: &str) -> Result<()> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    log::info!("Verifying checksum for {}", asset_name);

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(format!("notmux/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .context("failed to build HTTP client for checksum")?;

    let body = client
        .get(checksum_url)
        .send()
        .context("failed to fetch checksum file")?
        .error_for_status()
        .context("checksum file request failed")?
        .text()
        .context("failed to read checksum file")?;

    let expected_hash = expected_checksum(&body, asset_name)?;

    let mut file =
        std::fs::File::open(file_path).context("failed to open downloaded file for checksum")?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf).context("read error during checksum")?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let actual_hash = format!("{:x}", hasher.finalize());

    if actual_hash != expected_hash {
        let _ = std::fs::remove_file(file_path);
        anyhow::bail!(
            "checksum mismatch: expected {}, got {}",
            expected_hash,
            actual_hash
        );
    }

    log::info!("Checksum verified for {}", asset_name);
    Ok(())
}

fn expected_checksum(body: &str, asset_name: &str) -> Result<String> {
    let mut found = None;
    for line in body.lines() {
        let parts: Vec<_> = line.split_whitespace().collect();
        if parts.get(1).copied() == Some(asset_name) {
            anyhow::ensure!(parts.len() == 2 && parts[0].len() == 64
                && parts[0].bytes().all(|b| b.is_ascii_hexdigit()), "invalid SHA256SUMS entry");
            anyhow::ensure!(found.is_none(), "duplicate SHA256SUMS entry");
            found = Some(parts[0].to_lowercase());
        }
    }
    found.with_context(|| format!("no checksum found for '{}' in SHA256SUMS", asset_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_manifest_requires_one_valid_matching_entry() {
        let hash = "a".repeat(64);
        let line = format!("{hash}  notmux-macos-arm64.zip\n");
        assert_eq!(expected_checksum(&line, "notmux-macos-arm64.zip").unwrap(), hash);
        assert!(expected_checksum(&line, "notmux-windows-x64.zip").is_err());
        assert!(expected_checksum(&(line.clone() + &line), "notmux-macos-arm64.zip").is_err());
        assert!(expected_checksum("invalid  notmux-macos-arm64.zip", "notmux-macos-arm64.zip").is_err());
    }

    #[test]
    fn missing_checksums_fail_before_downloading() {
        let info = UpdateInfo::new("0.1.0".into());
        let result = smol::block_on(download_asset(
            "invalid-url".into(), "notmux.zip".into(), "0.1.1".into(), info, 0, None,
        ));
        assert!(result.unwrap_err().to_string().contains("no SHA256SUMS"));
    }
}

pub fn cleanup_updates_dir() {
    let updates_dir = crate::process::get_config_dir().join("updates");
    if updates_dir.exists()
        && let Ok(entries) = std::fs::read_dir(&updates_dir)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let _ = std::fs::remove_dir_all(&path);
            } else {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}
