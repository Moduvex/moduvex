//! TOML file loading with profile overlay support.
//!
//! Reads `{name}.toml` as base, then merges `{name}-{profile}.toml` on top.

use crate::error::ConfigError;
use crate::profile::Profile;
use std::path::{Path, PathBuf};

/// Load and merge TOML files for the given config name and profile.
///
/// Resolution: `{dir}/{name}.toml` (base) + `{dir}/{name}-{profile}.toml` (overlay).
/// Returns merged `toml::Value::Table`.
pub fn load_toml(dir: &Path, name: &str, profile: &Profile) -> Result<toml::Value, ConfigError> {
    let base_path = dir.join(format!("{}.toml", name));
    let profile_path = dir.join(format!("{}-{}.toml", name, profile.as_str()));

    // Base file is required
    let base = read_toml_file(&base_path)?;

    // Profile file is optional — merge if exists
    if profile_path.exists() {
        let overlay = read_toml_file(&profile_path)?;
        Ok(deep_merge(base, overlay))
    } else {
        Ok(base)
    }
}

/// Read and parse a single TOML file.
fn read_toml_file(path: &PathBuf) -> Result<toml::Value, ConfigError> {
    let content = std::fs::read_to_string(path).map_err(|e| ConfigError::FileRead {
        path: path.display().to_string(),
        source: e.to_string(),
    })?;
    toml::from_str::<toml::Value>(&content).map_err(|e| ConfigError::Parse {
        path: path.display().to_string(),
        source: e.to_string(),
    })
}

/// Deep-merge two TOML values. `overlay` wins on conflict.
/// Tables are merged recursively; all other types are replaced.
pub fn deep_merge(base: toml::Value, overlay: toml::Value) -> toml::Value {
    match (base, overlay) {
        (toml::Value::Table(mut base_tbl), toml::Value::Table(overlay_tbl)) => {
            for (key, overlay_val) in overlay_tbl {
                let merged = match base_tbl.remove(&key) {
                    Some(base_val) => deep_merge(base_val, overlay_val),
                    None => overlay_val,
                };
                base_tbl.insert(key, merged);
            }
            toml::Value::Table(base_tbl)
        }
        // Non-table: overlay replaces base entirely
        (_, overlay) => overlay,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_dir(base: &str, profile: Option<&str>) -> tempdir::TempDir {
        // Use std::env::temp_dir instead of tempdir crate
        let dir = std::env::temp_dir().join(format!("moduvex-test-{}", rand_suffix()));
        fs::create_dir_all(&dir).unwrap();

        fs::write(dir.join("app.toml"), base).unwrap();
        if let Some(p) = profile {
            fs::write(dir.join("app-prod.toml"), p).unwrap();
        }

        // Return a wrapper that cleans up
        tempdir::TempDir(dir)
    }

    fn rand_suffix() -> u64 {
        use std::time::SystemTime;
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
    }

    mod tempdir {
        pub struct TempDir(pub std::path::PathBuf);
        impl TempDir {
            pub fn path(&self) -> &std::path::Path {
                &self.0
            }
        }
        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn load_base_only() {
        let dir = setup_dir("[server]\nport = 8080\n", None);
        let val = load_toml(dir.path(), "app", &Profile::Dev).unwrap();
        let tbl = val.as_table().unwrap();
        let server = tbl["server"].as_table().unwrap();
        assert_eq!(server["port"].as_integer().unwrap(), 8080);
    }

    #[test]
    fn profile_overrides_base() {
        let dir = setup_dir(
            "[server]\nport = 8080\nhost = \"localhost\"\n",
            Some("[server]\nport = 9090\n"),
        );
        let val = load_toml(dir.path(), "app", &Profile::Prod).unwrap();
        let server = val["server"].as_table().unwrap();
        assert_eq!(server["port"].as_integer().unwrap(), 9090);
        assert_eq!(server["host"].as_str().unwrap(), "localhost");
    }

    #[test]
    fn missing_base_file_returns_error() {
        let dir = std::env::temp_dir().join("moduvex-test-missing");
        let _ = std::fs::create_dir_all(&dir);
        let result = load_toml(&dir, "nonexistent", &Profile::Dev);
        assert!(result.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deep_merge_nested_tables() {
        let base: toml::Value = toml::from_str("[db]\nhost = \"localhost\"\npool = 5\n").unwrap();
        let overlay: toml::Value = toml::from_str("[db]\npool = 20\nssl = true\n").unwrap();
        let merged = deep_merge(base, overlay);
        let db = merged["db"].as_table().unwrap();
        assert_eq!(db["host"].as_str().unwrap(), "localhost");
        assert_eq!(db["pool"].as_integer().unwrap(), 20);
        assert!(db["ssl"].as_bool().unwrap());
    }
}
