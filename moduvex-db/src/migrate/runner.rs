//! File-based migration runner.
//!
//! Reads SQL files from a `migrations/` directory, sorted by filename prefix
//! (e.g. `001_create_users.sql`, `002_add_index.sql`), and applies any that
//! have not yet been recorded in the `_moduvex_migrations` tracking table.
//!
//! Advisory locking is NOT implemented for MVP (requires PG-specific commands
//! which need a live connection; deferred to integration phase).

use std::path::{Path, PathBuf};

use crate::error::{DbError, Result};

// ── Migration ─────────────────────────────────────────────────────────────────

/// A single parsed migration file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    /// Numeric version extracted from the filename prefix (e.g. `1` from `001_…`).
    pub version: u64,
    /// Full filename (e.g. `001_create_users.sql`).
    pub filename: String,
    /// SQL content of the file.
    pub sql: String,
}

impl PartialOrd for Migration {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Migration {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.version.cmp(&other.version)
    }
}

// ── load_migrations ───────────────────────────────────────────────────────────

/// Load and sort all `.sql` migration files from `dir`.
///
/// Files must start with a numeric prefix (`001_`, `1_`, `20240101_`, etc.).
/// Files that cannot be parsed are skipped with a warning (not an error).
pub fn load_migrations(dir: &Path) -> Result<Vec<Migration>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut migrations = Vec::new();

    let entries = std::fs::read_dir(dir)
        .map_err(|e| DbError::Migration(format!("cannot read migrations dir: {e}")))?;

    for entry in entries {
        let entry = entry.map_err(|e| DbError::Migration(format!("directory entry error: {e}")))?;
        let path: PathBuf = entry.path();

        if path.extension().and_then(|s| s.to_str()) != Some("sql") {
            continue;
        }

        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();

        let version = match parse_version(&filename) {
            Some(v) => v,
            None => continue, // skip non-versioned files
        };

        let sql = std::fs::read_to_string(&path)
            .map_err(|e| DbError::Migration(format!("cannot read {filename}: {e}")))?;

        migrations.push(Migration {
            version,
            filename,
            sql,
        });
    }

    migrations.sort();
    Ok(migrations)
}

/// Extract the leading numeric version from a migration filename.
///
/// Examples:
/// - `001_create_users.sql` → `Some(1)`
/// - `20240101_add_index.sql` → `Some(20240101)`
/// - `no_prefix.sql` → `None`
pub fn parse_version(filename: &str) -> Option<u64> {
    let digits: String = filename
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

// ── SQL helpers ───────────────────────────────────────────────────────────────

/// SQL to create the migration tracking table if it doesn't exist.
pub const CREATE_TRACKING_TABLE_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS _moduvex_migrations (
    version   BIGINT      NOT NULL PRIMARY KEY,
    filename  TEXT        NOT NULL,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
)
"#;

/// SQL to fetch all applied migration versions (ordered ascending).
pub const SELECT_APPLIED_SQL: &str = "SELECT version FROM _moduvex_migrations ORDER BY version ASC";

/// Build the SQL to record a migration as applied.
pub fn insert_applied_sql(version: u64, filename: &str) -> String {
    format!(
        "INSERT INTO _moduvex_migrations (version, filename) VALUES ({version}, '{}')",
        filename.replace('\'', "''")
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    #[test]
    fn parse_version_leading_zeros() {
        assert_eq!(parse_version("001_create_users.sql"), Some(1));
        assert_eq!(parse_version("042_add_index.sql"), Some(42));
        assert_eq!(parse_version("20240101_init.sql"), Some(20240101));
    }

    #[test]
    fn parse_version_no_prefix() {
        assert_eq!(parse_version("no_prefix.sql"), None);
        assert_eq!(parse_version("create_table.sql"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn migration_ordering() {
        let mut ms = [
            Migration {
                version: 3,
                filename: "003.sql".into(),
                sql: "".into(),
            },
            Migration {
                version: 1,
                filename: "001.sql".into(),
                sql: "".into(),
            },
            Migration {
                version: 2,
                filename: "002.sql".into(),
                sql: "".into(),
            },
        ];
        ms.sort();
        assert_eq!(ms[0].version, 1);
        assert_eq!(ms[1].version, 2);
        assert_eq!(ms[2].version, 3);
    }

    #[test]
    fn load_migrations_from_temp_dir() {
        let dir = tempdir_with_files(&[
            ("001_create_users.sql", "CREATE TABLE users (id SERIAL);"),
            (
                "002_add_email.sql",
                "ALTER TABLE users ADD COLUMN email TEXT;",
            ),
            ("readme.txt", "not a migration"),
        ]);
        let migrations = load_migrations(&dir).unwrap();
        assert_eq!(migrations.len(), 2);
        assert_eq!(migrations[0].version, 1);
        assert_eq!(migrations[1].version, 2);
        assert!(migrations[0].sql.contains("CREATE TABLE"));
    }

    #[test]
    fn load_migrations_empty_dir() {
        let dir = tempdir_with_files(&[]);
        let ms = load_migrations(&dir).unwrap();
        assert!(ms.is_empty());
    }

    #[test]
    fn load_migrations_nonexistent_dir_returns_empty() {
        let ms = load_migrations(Path::new("/tmp/nonexistent_moduvex_test_dir_xyz")).unwrap();
        assert!(ms.is_empty());
    }

    #[test]
    fn insert_applied_sql_escapes_filename() {
        let sql = insert_applied_sql(1, "001_it's_a_test.sql");
        assert!(sql.contains("it''s_a_test"));
    }

    // ── helpers ──────────────────────────────────────────────────────────────

    fn tempdir_with_files(files: &[(&str, &str)]) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("moduvex_db_test_{}_{}", std::process::id(), id,));
        // Clean any leftover from previous runs
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for (name, content) in files {
            let mut f = fs::File::create(dir.join(name)).unwrap();
            f.write_all(content.as_bytes()).unwrap();
        }
        dir
    }
}
