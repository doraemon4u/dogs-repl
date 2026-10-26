//! Local configuration and credential persistence.
//!
//! Credentials live in a single JSON file under the platform config directory
//! (`$XDG_CONFIG_HOME/ogs-tui` on Linux, `~/Library/Application Support/...`
//! on macOS, `%APPDATA%` on Windows). On Unix the file is created `0600` and
//! its directory `0700`, so another local user cannot read the refresh token.
//!
//! `OGS_TUI_CONFIG_DIR` overrides the directory, which makes tests and
//! sandboxed runs hermetic without touching the real profile.

use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

/// Directory name under the platform config root.
pub const APP_DIR: &str = "ogs-tui";
/// Credentials file name.
pub const CREDENTIALS_FILE: &str = "credentials.json";

/// The on-disk credential record. Field names are deliberately explicit; this
/// file is a user-visible security boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredCredentials {
    pub username: String,
    #[serde(default)]
    pub user_id: Option<i64>,
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds at which the access token should be treated as expired.
    /// Already includes a safety margin.
    pub expires_at_unix: u64,
    #[serde(default)]
    pub client_id: Option<String>,
}

/// Resolves the config directory, honoring `OGS_TUI_CONFIG_DIR`.
pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("OGS_TUI_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    dirs::config_dir()
        .map(|dir| dir.join(APP_DIR))
        .unwrap_or_else(|| PathBuf::from(".ogs-tui"))
}

/// Default credentials path.
pub fn credentials_path() -> PathBuf {
    config_dir().join(CREDENTIALS_FILE)
}

/// Loads credentials from the default path, or `None` if absent/unreadable.
pub fn load_credentials() -> Option<StoredCredentials> {
    load_credentials_from(&credentials_path()).ok().flatten()
}

/// Saves credentials to the default path.
pub fn save_credentials(credentials: &StoredCredentials) -> io::Result<()> {
    save_credentials_to(&credentials_path(), credentials)
}

/// Removes the default credentials file if it exists.
pub fn clear_credentials() -> io::Result<()> {
    clear_credentials_at(&credentials_path())
}

/// Removes an explicit credentials file if it exists.
pub fn clear_credentials_at(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Loads credentials from an explicit path (used by tests and `--config-dir`).
pub fn load_credentials_from(path: &Path) -> io::Result<Option<StoredCredentials>> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Saves credentials to an explicit path atomically and privately.
pub fn save_credentials_to(path: &Path, credentials: &StoredCredentials) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(credentials)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    write_private(path, &json)
}

/// Writes `contents` to `path` atomically, with restrictive permissions.
pub fn write_private(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Best effort: the directory may already exist with other owners.
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }

    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        use std::io::Write;
        file.write_all(contents)?;
        file.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Reports whether the credentials file is readable by group/other on Unix.
/// Returns `None` on non-Unix platforms where the concept does not apply.
#[cfg(unix)]
pub fn credentials_are_world_readable(path: &Path) -> Option<bool> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = std::fs::metadata(path).ok()?;
    let mode = metadata.permissions().mode();
    Some(mode & 0o077 != 0)
}

#[cfg(not(unix))]
pub fn credentials_are_world_readable(_path: &Path) -> Option<bool> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ogs-tui-test-{}-{}-{tag}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn sample() -> StoredCredentials {
        StoredCredentials {
            username: "sora".to_string(),
            user_id: Some(42),
            access_token: "access".to_string(),
            refresh_token: "refresh".to_string(),
            expires_at_unix: 1_800_000_000,
            client_id: Some("client".to_string()),
        }
    }

    #[test]
    fn credential_roundtrip() {
        let dir = temp_path("roundtrip");
        let path = dir.join("credentials.json");
        save_credentials_to(&path, &sample()).unwrap();
        assert_eq!(load_credentials_from(&path).unwrap(), Some(sample()));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_credentials_file_is_none() {
        let dir = temp_path("missing");
        let path = dir.join("credentials.json");
        assert_eq!(load_credentials_from(&path).unwrap(), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn malformed_credentials_are_an_error() {
        let dir = temp_path("malformed");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("credentials.json");
        std::fs::write(&path, "not json").unwrap();
        assert!(load_credentials_from(&path).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn credentials_are_private_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_path("perms");
        let path = dir.join("credentials.json");
        save_credentials_to(&path, &sample()).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "mode was {mode:o}");
        assert_eq!(credentials_are_world_readable(&path), Some(false));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn atomic_write_leaves_no_temp_file() {
        let dir = temp_path("atomic");
        let path = dir.join("credentials.json");
        save_credentials_to(&path, &sample()).unwrap();
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries.len(), 1, "unexpected files: {entries:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn clearing_credentials_is_idempotent() {
        let dir = temp_path("clear");
        let path = dir.join("credentials.json");
        save_credentials_to(&path, &sample()).unwrap();
        clear_credentials_at(&path).unwrap();
        assert!(!path.exists());
        clear_credentials_at(&path).unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn config_dir_honors_override() {
        // This test only reads the override that is set for the whole process
        // by the test harness when it is present; it must never mutate it.
        if let Some(value) = std::env::var_os("OGS_TUI_CONFIG_DIR") {
            assert_eq!(config_dir(), PathBuf::from(value));
        }
    }
}
