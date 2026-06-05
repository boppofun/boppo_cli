use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Credentials and connection info for a single Boppo device.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeviceCredentials {
    /// Bearer token returned by the device pairing flow.
    pub password: String,
    /// Optional human-readable name for the device.
    pub nickname: Option<String>,
}

/// Returns the base HTTPS URL for a device given its serial number.
pub fn device_url(serial: &str) -> String {
    format!("https://boppo-{}.local", serial)
}

/// Persistent store of device credentials, backed by a TOML file.
///
/// The file format looks like:
/// ```toml
/// default = "0120001234"
///
/// [devices.0120001234]
/// password = "secret"
/// nickname = "my-tablet"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CredentialStore {
    /// Serial number of the default device, if one has been set.
    pub default: Option<String>,
    #[serde(default)]
    pub devices: HashMap<String, DeviceCredentials>,
}

impl CredentialStore {
    /// Load the store from `path`. Returns an empty store if the file does not exist.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read credential store at {}", path.display()))?;
        let store: Self = toml::from_str(&content)
            .with_context(|| format!("failed to parse credential store at {}", path.display()))?;
        Ok(store)
    }

    /// Serialize the store to `path`, creating parent directories if needed.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directories for {}", path.display()))?;
        }
        let content = toml::to_string_pretty(self)
            .context("failed to serialize credential store to TOML")?;
        std::fs::write(path, content)
            .with_context(|| format!("failed to write credential store to {}", path.display()))?;
        Ok(())
    }

    /// Look up a device by its serial number.
    pub fn get_device(&self, serial: &str) -> Option<&DeviceCredentials> {
        self.devices.get(serial)
    }

    /// Add or replace the credentials for a device, keyed by serial number.
    pub fn set_device(&mut self, serial: impl Into<String>, creds: DeviceCredentials) {
        self.devices.insert(serial.into(), creds);
    }

    /// Remove a device by serial number. Returns `true` if it existed.
    pub fn remove_device(&mut self, serial: &str) -> bool {
        self.devices.remove(serial).is_some()
    }

    /// Set the default device by serial number.
    pub fn set_default(&mut self, serial: impl Into<String>) {
        self.default = Some(serial.into());
    }

    /// Clear the default device.
    pub fn clear_default(&mut self) {
        self.default = None;
    }

    /// Find a device by serial number or nickname.
    ///
    /// Serial number is checked first; if no match is found, all nicknames are
    /// searched. Returns the canonical serial number alongside the credentials,
    /// or `None` if no device matches.
    pub fn resolve_device<'a>(
        &'a self,
        identifier: &str,
    ) -> Option<(&'a str, &'a DeviceCredentials)> {
        if let Some((serial, creds)) = self.devices.get_key_value(identifier) {
            return Some((serial.as_str(), creds));
        }
        for (serial, creds) in &self.devices {
            if creds.nickname.as_deref() == Some(identifier) {
                return Some((serial.as_str(), creds));
            }
        }
        None
    }

    /// Return the credentials for the default device, if one is configured.
    pub fn default_device<'a>(&'a self) -> Option<(&'a str, &'a DeviceCredentials)> {
        let serial = self.default.as_deref()?;
        let creds = self.devices.get(serial)?;
        Some((serial, creds))
    }
}

/// Returns the platform-appropriate default path for the credential store file
/// (`~/.config/boppo/devices.toml` on Linux/macOS).
pub fn default_store_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("boppo")
        .join("devices.toml")
}
