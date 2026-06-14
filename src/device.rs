use anyhow::Context;
use crate::device_https_client::{BoppoDevice, DirEntry, ProgressFactory};
use std::collections::HashMap;
use std::sync::Arc;

/// Callbacks used by [`sync_dir`] to report progress without coupling to a specific output mechanism.
#[derive(Clone)]
pub struct SyncStatus {
    /// Called with the device directory path each time `sync_dir` enters a directory.
    pub set_dir: Arc<dyn Fn(&str) + Send + Sync>,
    /// Called to emit a persistent line of output (e.g. "Removing …", "Uploading …").
    pub println: Arc<dyn Fn(&str) + Send + Sync>,
}

/// A device discovered via mDNS.
#[derive(Debug)]
pub struct DiscoveredDevice {
    pub serial: String,
    pub device_name: String,
}

/// Initiate the pairing flow for a device and return the bearer token.
///
/// Generates a request ID from the current timestamp and delegates to
/// [`crate::device_https_client::get_password`], which polls until the user
/// approves or rejects the request on the device.
pub async fn pair_device(serial: &str) -> anyhow::Result<String> {
    let url = format!("https://boppo-{}.local", serial);
    let request_id: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    crate::device_https_client::get_password(&url, request_id).await
}

/// Browse for Boppo devices on the local network via mDNS, blocking for up to 5 seconds.
///
/// Listens for `_boppo._tcp.local.` announcements. Prefers IPv4 addresses.
/// The HTTPS API is always on port 443 regardless of the mDNS-advertised port.
///
/// This is a blocking call; wrap it in `tokio::task::spawn_blocking` when calling
/// from an async context.
pub fn browse_mdns() -> anyhow::Result<Vec<DiscoveredDevice>> {
    use mdns_sd::{ServiceDaemon, ServiceEvent};
    use std::time::{Duration, Instant};

    let mdns = ServiceDaemon::new().context("failed to start mDNS daemon")?;
    let receiver = mdns
        .browse("_boppo._tcp.local.")
        .context("failed to browse mDNS")?;

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut devices = Vec::new();

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match receiver.recv_timeout(remaining) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                let hostname = info.get_hostname(); // "boppo-0120001234.local."
                let serial = hostname
                    .strip_prefix("boppo-")
                    .and_then(|s| s.strip_suffix(".local."))
                    .unwrap_or(hostname)
                    .to_owned();
                let device_name = info
                    .get_property_val_str("device_name")
                    .unwrap_or("unknown")
                    .to_owned();
                devices.push(DiscoveredDevice { serial, device_name });
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }

    let _ = mdns.shutdown();
    Ok(devices)
}

/// Sync a local directory tree to a path on the device.
///
/// Files are uploaded when missing or when their size differs from the host copy.
/// With `delete = true`, files present on the device but absent on the host are removed.
/// With `dry_run = true`, all changes are logged but nothing is actually written.
///
/// When `progress_factory` is provided, each file upload shows a streaming progress
/// bar via the returned [`ProgressCallback`](crate::device_https_client::ProgressCallback).
/// When `None`, a plain text line is printed instead.
#[async_recursion::async_recursion]
pub async fn sync_dir(
    device: &dyn BoppoDevice,
    host_dir: &str,
    device_dir: &str,
    delete: bool,
    dry_run: bool,
    progress_factory: Option<ProgressFactory>,
    status: Option<SyncStatus>,
) -> anyhow::Result<()> {
    if let Some(s) = &status {
        (s.set_dir)(device_dir);
    }
    let host_entries = list_host_dir(host_dir)?;
    let device_entries_vec = device.read_dir(device_dir).await?;
    let device_entries: HashMap<String, &DirEntry> =
        device_entries_vec.iter().map(|e| (e.name.clone(), e)).collect();

    // Upload files that are missing or have a different size.
    for (name, host_attr) in &host_entries {
        if host_attr.is_dir {
            continue;
        }
        let reason = match device_entries.get(name) {
            Some(device_entry) => {
                if device_entry.size == host_attr.size {
                    continue;
                }
                "sizes are different"
            }
            None => "file is missing on device",
        };
        let host_path = format!("{}/{}", host_dir, name);
        let device_path = format!("{}/{}", device_dir, name);
        let contents = std::fs::read(&host_path)
            .with_context(|| format!("failed to read {}", host_path))?;

        let total = contents.len() as u64;

        // When there's no progress factory (or dry run), emit a text line.
        if dry_run || progress_factory.is_none() {
            let msg = format!("Uploading {} -> {} ({} bytes, {})", host_path, device_path, total, reason);
            if let Some(s) = &status {
                (s.println)(&msg);
            } else {
                eprintln!("{}", msg);
            }
        }
        if !dry_run {
            let progress = progress_factory.as_ref().map(|f| f(&device_path, total));
            device
                .upload_file(&device_path, contents, progress)
                .await
                .with_context(|| format!("failed to upload {}", host_path))?;
        }
    }

    // Recurse into subdirectories.
    for (name, host_attr) in &host_entries {
        if !host_attr.is_dir {
            continue;
        }
        let new_host_dir = format!("{}/{}", host_dir, name);
        let new_device_dir = format!("{}/{}", device_dir, name);
        sync_dir(
            device,
            &new_host_dir,
            &new_device_dir,
            delete,
            dry_run,
            progress_factory.clone(),
            status.clone(),
        )
        .await
        .with_context(|| format!("failed to sync host dir: {}", host_dir))?;
    }

    // Delete files on the device that are absent from the host.
    if delete {
        for (name, device_entry) in &device_entries {
            if device_entry.is_dir {
                continue;
            }
            if host_entries.contains_key(name) {
                continue;
            }
            let device_path = format!("{}/{}", device_dir, name);
            if let Some(s) = &status {
                (s.println)(&format!("Removing {}", device_path));
            } else {
                eprintln!("Removing {}", device_path);
            }
            if !dry_run {
                device
                    .remove_file(&device_path)
                    .await
                    .with_context(|| format!("failed to remove {}", device_path))?;
            }
        }
    }

    Ok(())
}

struct HostFileAttr {
    size: u64,
    is_dir: bool,
}

fn list_host_dir(host_dir: &str) -> anyhow::Result<HashMap<String, HostFileAttr>> {
    let mut entries = HashMap::new();
    for entry in std::fs::read_dir(host_dir)
        .with_context(|| format!("failed to read host directory {}", host_dir))?
    {
        let entry = entry?;
        let metadata = entry.metadata()?;
        let is_dir = metadata.is_dir();
        let size = metadata.len();
        let file_name = entry
            .path()
            .file_name()
            .unwrap()
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("non UTF-8 file name"))?
            .to_owned();
        entries.insert(file_name, HostFileAttr { size, is_dir });
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::sync_dir;
    use crate::device_https_client::{DirEntry, MockBoppoDevice};
    use mockall::predicate::{always, eq};
    use tempfile::TempDir;

    fn write_file(dir: &std::path::Path, name: &str, content: &[u8]) {
        std::fs::write(dir.join(name), content).unwrap();
    }

    /// A new file on the host that is absent from the device should be uploaded.
    #[tokio::test]
    async fn sync_dir_uploads_missing_files() {
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "hello.txt", b"hello world");

        let mut mock = MockBoppoDevice::new();
        mock.expect_read_dir()
            .with(eq("/device"))
            .once()
            .returning(|_| Ok(vec![]));
        mock.expect_upload_file()
            .with(eq("/device/hello.txt"), always(), always())
            .once()
            .returning(|_, _, _| Ok(()));

        sync_dir(&mock, tmp.path().to_str().unwrap(), "/device", false, false, None, None)
            .await
            .unwrap();
    }

    /// A file that already exists on the device with the same size should not be re-uploaded.
    #[tokio::test]
    async fn sync_dir_skips_files_with_matching_size() {
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "same.txt", b"12345"); // 5 bytes

        let mut mock = MockBoppoDevice::new();
        mock.expect_read_dir()
            .with(eq("/device"))
            .once()
            .returning(|_| Ok(vec![DirEntry {
                name: "same.txt".to_owned(),
                size: 5,
                is_dir: false,
            }]));
        // No expect_upload_file — mockall will panic if upload_file is called unexpectedly.

        sync_dir(&mock, tmp.path().to_str().unwrap(), "/device", false, false, None, None)
            .await
            .unwrap();
    }

    /// A file whose size differs from the device copy should be re-uploaded.
    #[tokio::test]
    async fn sync_dir_reuploads_files_with_different_size() {
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "changed.txt", b"new content"); // 11 bytes

        let mut mock = MockBoppoDevice::new();
        mock.expect_read_dir()
            .with(eq("/device"))
            .once()
            .returning(|_| Ok(vec![DirEntry {
                name: "changed.txt".to_owned(),
                size: 5, // stale; differs from 11
                is_dir: false,
            }]));
        mock.expect_upload_file()
            .with(eq("/device/changed.txt"), always(), always())
            .once()
            .returning(|_, _, _| Ok(()));

        sync_dir(&mock, tmp.path().to_str().unwrap(), "/device", false, false, None, None)
            .await
            .unwrap();
    }

    /// Files inside subdirectories should be synced recursively.
    #[tokio::test]
    async fn sync_dir_recurses_into_subdirectories() {
        let tmp = TempDir::new().unwrap();
        let docs = tmp.path().join("docs");
        std::fs::create_dir(&docs).unwrap();
        write_file(tmp.path(), "root.txt", b"root");
        write_file(&docs, "readme.txt", b"readme content");

        let mut mock = MockBoppoDevice::new();
        // Top-level listing contains the docs subdir (root.txt is missing on device).
        mock.expect_read_dir()
            .with(eq("/device"))
            .once()
            .returning(|_| Ok(vec![DirEntry {
                name: "docs".to_owned(),
                size: 0,
                is_dir: true,
            }]));
        // Subdir is empty on the device.
        mock.expect_read_dir()
            .with(eq("/device/docs"))
            .once()
            .returning(|_| Ok(vec![]));
        mock.expect_upload_file()
            .with(eq("/device/root.txt"), always(), always())
            .once()
            .returning(|_, _, _| Ok(()));
        mock.expect_upload_file()
            .with(eq("/device/docs/readme.txt"), always(), always())
            .once()
            .returning(|_, _, _| Ok(()));

        sync_dir(&mock, tmp.path().to_str().unwrap(), "/device", false, false, None, None)
            .await
            .unwrap();
    }

    /// With `--delete`, files present on the device but absent on the host should be removed.
    #[tokio::test]
    async fn sync_dir_deletes_extra_device_files() {
        let tmp = TempDir::new().unwrap(); // empty host directory

        let mut mock = MockBoppoDevice::new();
        mock.expect_read_dir()
            .with(eq("/device"))
            .once()
            .returning(|_| Ok(vec![DirEntry {
                name: "orphan.txt".to_owned(),
                size: 100,
                is_dir: false,
            }]));
        mock.expect_remove_file()
            .with(eq("/device/orphan.txt"))
            .once()
            .returning(|_| Ok(()));

        sync_dir(&mock, tmp.path().to_str().unwrap(), "/device", true, false, None, None)
            .await
            .unwrap();
    }
}
