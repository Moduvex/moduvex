//! Migration engine — applies versioned SQL files in order, tracking applied
//! migrations in a `_moduvex_migrations` table.
//!
//! Usage:
//! ```rust,ignore
//! let engine = MigrationEngine::new("migrations/");
//! engine.run(&mut conn).await?;
//! ```

pub mod runner;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::error::{DbError, Result};
use crate::protocol::postgres::{PgConnection, PgRowSet};
use runner::{insert_applied_sql, load_migrations, CREATE_TRACKING_TABLE_SQL, SELECT_APPLIED_SQL};

// ── MigrationEngine ───────────────────────────────────────────────────────────

/// Up-only migration engine backed by a `migrations/` directory.
pub struct MigrationEngine {
    migrations_dir: PathBuf,
}

impl MigrationEngine {
    /// Create an engine that reads migrations from `migrations_dir`.
    pub fn new(migrations_dir: impl Into<PathBuf>) -> Self {
        Self {
            migrations_dir: migrations_dir.into(),
        }
    }

    /// Run all pending migrations against `conn`.
    ///
    /// Steps:
    /// 1. Create the `_moduvex_migrations` tracking table if absent.
    /// 2. Load all `.sql` files from the migrations directory.
    /// 3. Query already-applied versions.
    /// 4. Apply pending migrations in version order (each in its own transaction).
    /// 5. Record each applied migration in the tracking table.
    pub async fn run(&self, conn: &mut PgConnection) -> Result<MigrationReport> {
        // Step 1: ensure tracking table exists
        conn.execute(CREATE_TRACKING_TABLE_SQL).await?;

        // Step 2: load from disk
        let all = load_migrations(Path::new(&self.migrations_dir))?;

        // Step 3: fetch applied versions
        let applied = self.fetch_applied(conn).await?;

        // Step 4 & 5: apply pending
        let mut applied_count = 0;
        let mut skipped_count = 0;
        for migration in &all {
            if applied.contains(&migration.version) {
                skipped_count += 1;
                continue;
            }
            // Apply inside a transaction for atomicity
            conn.execute("BEGIN").await?;
            match conn.execute(&migration.sql).await {
                Ok(_) => {
                    let record_sql = insert_applied_sql(migration.version, &migration.filename);
                    conn.execute(&record_sql).await?;
                    conn.execute("COMMIT").await?;
                    applied_count += 1;
                }
                Err(e) => {
                    // Roll back this migration, abort the run
                    let _ = conn.execute("ROLLBACK").await;
                    return Err(DbError::Migration(format!(
                        "migration {} ({}) failed: {e}",
                        migration.version, migration.filename
                    )));
                }
            }
        }

        Ok(MigrationReport {
            total: all.len(),
            applied: applied_count,
            skipped: skipped_count,
        })
    }

    /// Fetch applied migration versions from the tracking table.
    async fn fetch_applied(&self, conn: &mut PgConnection) -> Result<HashSet<u64>> {
        let rowset: PgRowSet = conn.query(SELECT_APPLIED_SQL).await?;
        let mut set = HashSet::new();
        for row in rowset.iter() {
            // version column is field 0 — parse as i64 text
            if let Some(Some(bytes)) = row.fields.first() {
                if let Ok(s) = std::str::from_utf8(bytes) {
                    if let Ok(v) = s.trim().parse::<i64>() {
                        set.insert(v as u64);
                    }
                }
            }
        }
        Ok(set)
    }
}

// ── MigrationReport ───────────────────────────────────────────────────────────

/// Summary returned after running migrations.
#[derive(Debug, Clone)]
pub struct MigrationReport {
    /// Total migrations found on disk.
    pub total: usize,
    /// Migrations applied in this run.
    pub applied: usize,
    /// Migrations already applied (skipped).
    pub skipped: usize,
}

impl std::fmt::Display for MigrationReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Migrations: {} total, {} applied, {} skipped",
            self.total, self.applied, self.skipped
        )
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_report_display() {
        let r = MigrationReport {
            total: 5,
            applied: 2,
            skipped: 3,
        };
        let s = r.to_string();
        assert!(s.contains("5 total"));
        assert!(s.contains("2 applied"));
        assert!(s.contains("3 skipped"));
    }

    #[test]
    fn engine_new_stores_path() {
        let engine = MigrationEngine::new("migrations/");
        assert_eq!(engine.migrations_dir, PathBuf::from("migrations/"));
    }
}
