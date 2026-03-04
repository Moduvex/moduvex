//! Lifecycle phase enum and transition validation.
//!
//! The lifecycle is a linear state machine. Each phase must complete before
//! the next begins. Invalid transitions are caught at runtime and surface
//! as `LifecycleError`.

use std::fmt;

use crate::error::{classify::LifecycleError, ModuvexError, Result};

// ── Phase ─────────────────────────────────────────────────────────────────────

/// The lifecycle phase of a Moduvex application.
///
/// Phases advance linearly from `Config` to `Ready` during startup, then
/// transition to `Stopping` → `Stopped` on shutdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Load and merge configuration from all sources.
    Config,
    /// Validate configuration and dependency graph.
    Validate,
    /// Create all singletons and register them in `AppContext`.
    Init,
    /// Call `on_start()` on all modules in dependency order.
    Start,
    /// Application is fully operational and accepting traffic.
    Ready,
    /// Graceful shutdown in progress; `on_stop()` being called in reverse order.
    Stopping,
    /// All modules have stopped; process may exit.
    Stopped,
}

impl Phase {
    /// Returns `true` if transitioning from `self` to `next` is valid.
    pub fn can_transition_to(self, next: Phase) -> bool {
        matches!(
            (self, next),
            (Phase::Config,    Phase::Validate)
            | (Phase::Validate, Phase::Init)
            | (Phase::Init,     Phase::Start)
            | (Phase::Start,    Phase::Ready)
            | (Phase::Ready,    Phase::Stopping)
            // Allow direct transition from any non-Stopped phase to Stopping
            // so a boot failure mid-phase can trigger rollback.
            | (Phase::Config,   Phase::Stopping)
            | (Phase::Validate, Phase::Stopping)
            | (Phase::Init,     Phase::Stopping)
            | (Phase::Start,    Phase::Stopping)
            | (Phase::Stopping, Phase::Stopped)
        )
    }

    /// Validate and return the next phase, or an error if the transition is illegal.
    pub fn transition(from: Phase, to: Phase) -> Result<Phase> {
        if from.can_transition_to(to) {
            Ok(to)
        } else {
            Err(ModuvexError::Lifecycle(LifecycleError::new(format!(
                "invalid lifecycle transition: {:?} → {:?}",
                from, to
            ))))
        }
    }

    /// Return the phase that naturally follows this one during normal startup.
    ///
    /// Returns `None` for `Ready` (awaiting shutdown signal) and `Stopped`.
    pub fn next_startup_phase(self) -> Option<Phase> {
        match self {
            Phase::Config => Some(Phase::Validate),
            Phase::Validate => Some(Phase::Init),
            Phase::Init => Some(Phase::Start),
            Phase::Start => Some(Phase::Ready),
            Phase::Ready => None,
            Phase::Stopping => Some(Phase::Stopped),
            Phase::Stopped => None,
        }
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Phase::Config => write!(f, "Config"),
            Phase::Validate => write!(f, "Validate"),
            Phase::Init => write!(f, "Init"),
            Phase::Start => write!(f, "Start"),
            Phase::Ready => write!(f, "Ready"),
            Phase::Stopping => write!(f, "Stopping"),
            Phase::Stopped => write!(f, "Stopped"),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_startup_sequence() {
        let phases = [
            Phase::Config,
            Phase::Validate,
            Phase::Init,
            Phase::Start,
            Phase::Ready,
        ];
        for window in phases.windows(2) {
            assert!(
                window[0].can_transition_to(window[1]),
                "{:?} -> {:?} should be valid",
                window[0],
                window[1]
            );
        }
    }

    #[test]
    fn shutdown_sequence_valid() {
        assert!(Phase::Ready.can_transition_to(Phase::Stopping));
        assert!(Phase::Stopping.can_transition_to(Phase::Stopped));
    }

    #[test]
    fn emergency_stop_from_mid_boot_valid() {
        assert!(Phase::Init.can_transition_to(Phase::Stopping));
        assert!(Phase::Start.can_transition_to(Phase::Stopping));
    }

    #[test]
    fn backwards_transition_invalid() {
        assert!(!Phase::Start.can_transition_to(Phase::Config));
        assert!(!Phase::Ready.can_transition_to(Phase::Init));
    }

    #[test]
    fn skip_phase_invalid() {
        assert!(!Phase::Config.can_transition_to(Phase::Init));
        assert!(!Phase::Init.can_transition_to(Phase::Ready));
    }

    #[test]
    fn transition_fn_returns_ok() {
        let next = Phase::transition(Phase::Config, Phase::Validate).unwrap();
        assert_eq!(next, Phase::Validate);
    }

    #[test]
    fn transition_fn_returns_err() {
        let err = Phase::transition(Phase::Stopped, Phase::Config).unwrap_err();
        assert!(matches!(err, ModuvexError::Lifecycle(_)));
    }

    #[test]
    fn next_startup_phase_sequence() {
        assert_eq!(Phase::Config.next_startup_phase(), Some(Phase::Validate));
        assert_eq!(Phase::Validate.next_startup_phase(), Some(Phase::Init));
        assert_eq!(Phase::Init.next_startup_phase(), Some(Phase::Start));
        assert_eq!(Phase::Start.next_startup_phase(), Some(Phase::Ready));
        assert_eq!(Phase::Ready.next_startup_phase(), None);
    }
}
