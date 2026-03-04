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
            IsolationLevel::ReadCommitted   => "READ COMMITTED",
            IsolationLevel::RepeatableRead  => "REPEATABLE READ",
            IsolationLevel::Serializable    => "SERIALIZABLE",
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
        assert_eq!(IsolationLevel::ReadCommitted.as_sql(),   "READ COMMITTED");
        assert_eq!(IsolationLevel::RepeatableRead.as_sql(),  "REPEATABLE READ");
        assert_eq!(IsolationLevel::Serializable.as_sql(),    "SERIALIZABLE");
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
        let c = a.clone();
        assert_eq!(a, c);
    }
}
