//! PostgreSQL transaction isolation levels.

// ── IsolationLevel ────────────────────────────────────────────────────────────

/// Transaction isolation level as defined by the SQL standard.
///
/// PostgreSQL supports all four levels but treats `ReadUncommitted` the same
/// as `ReadCommitted` internally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IsolationLevel {
    /// Dirty reads are possible (PG treats same as ReadCommitted).
    ReadUncommitted,
    /// Default PostgreSQL isolation level.
    #[default]
    ReadCommitted,
    /// Repeatable reads; phantom reads possible only for range queries.
    RepeatableRead,
    /// Full serializability — highest isolation, may cause serialization errors.
    Serializable,
}

impl IsolationLevel {
    /// Return the SQL clause used in `BEGIN ISOLATION LEVEL …`.
    pub fn as_sql(&self) -> &'static str {
        match self {
            IsolationLevel::ReadUncommitted => "READ UNCOMMITTED",
            IsolationLevel::ReadCommitted => "READ COMMITTED",
            IsolationLevel::RepeatableRead => "REPEATABLE READ",
            IsolationLevel::Serializable => "SERIALIZABLE",
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_strings() {
        assert_eq!(IsolationLevel::ReadUncommitted.as_sql(), "READ UNCOMMITTED");
        assert_eq!(IsolationLevel::ReadCommitted.as_sql(), "READ COMMITTED");
        assert_eq!(IsolationLevel::RepeatableRead.as_sql(), "REPEATABLE READ");
        assert_eq!(IsolationLevel::Serializable.as_sql(), "SERIALIZABLE");
    }

    #[test]
    fn default_is_read_committed() {
        assert_eq!(IsolationLevel::default(), IsolationLevel::ReadCommitted);
    }

    #[test]
    fn clone_and_copy() {
        let a = IsolationLevel::Serializable;
        let b = a; // Copy
        assert_eq!(a, b);
        let c = a;
        assert_eq!(a, c);
    }

    // ── Additional isolation level tests ──────────────────────────────────────

    #[test]
    fn isolation_read_committed_sql() {
        assert_eq!(IsolationLevel::ReadCommitted.as_sql(), "READ COMMITTED");
    }

    #[test]
    fn isolation_repeatable_read_sql() {
        assert_eq!(IsolationLevel::RepeatableRead.as_sql(), "REPEATABLE READ");
    }

    #[test]
    fn isolation_serializable_sql() {
        assert_eq!(IsolationLevel::Serializable.as_sql(), "SERIALIZABLE");
    }

    #[test]
    fn isolation_read_uncommitted_sql() {
        assert_eq!(IsolationLevel::ReadUncommitted.as_sql(), "READ UNCOMMITTED");
    }

    #[test]
    fn isolation_all_variants_have_sql() {
        // Verify no variant returns empty string
        for level in [
            IsolationLevel::ReadUncommitted,
            IsolationLevel::ReadCommitted,
            IsolationLevel::RepeatableRead,
            IsolationLevel::Serializable,
        ] {
            assert!(!level.as_sql().is_empty());
        }
    }

    #[test]
    fn isolation_levels_are_all_different() {
        let sqls = [
            IsolationLevel::ReadUncommitted.as_sql(),
            IsolationLevel::ReadCommitted.as_sql(),
            IsolationLevel::RepeatableRead.as_sql(),
            IsolationLevel::Serializable.as_sql(),
        ];
        // All four SQL strings must be distinct
        let unique: std::collections::HashSet<_> = sqls.iter().collect();
        assert_eq!(unique.len(), 4);
    }

    #[test]
    fn isolation_default_is_read_committed() {
        assert_eq!(IsolationLevel::default(), IsolationLevel::ReadCommitted);
    }

    #[test]
    fn isolation_clone_produces_equal() {
        let a = IsolationLevel::Serializable;
        let b = a;
        assert_eq!(a, b);
    }
}
