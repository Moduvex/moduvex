//! Profile detection — determines which config overlay to load.
//!
//! Resolution order: `MODUVEX_PROFILE` env var → default "dev".

use std::fmt;

/// Application profile controlling which config overlay is loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Profile {
    Dev,
    Staging,
    Prod,
    Custom(String),
}

impl Profile {
    /// Resolve the active profile from the `MODUVEX_PROFILE` env var.
    /// Falls back to `Dev` if unset.
    pub fn from_env() -> Self {
        match std::env::var("MODUVEX_PROFILE") {
            Ok(val) => Self::parse(&val),
            Err(_) => Self::Dev,
        }
    }

    /// Parse a profile string (case-insensitive).
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "dev" | "development" => Self::Dev,
            "staging" | "stage" => Self::Staging,
            "prod" | "production" => Self::Prod,
            other => Self::Custom(other.to_string()),
        }
    }

    /// Returns the profile name used in file resolution (e.g. `app-prod.toml`).
    pub fn as_str(&self) -> &str {
        match self {
            Self::Dev => "dev",
            Self::Staging => "staging",
            Self::Prod => "prod",
            Self::Custom(s) => s.as_str(),
        }
    }
}

impl fmt::Display for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_known_profiles() {
        assert_eq!(Profile::parse("dev"), Profile::Dev);
        assert_eq!(Profile::parse("development"), Profile::Dev);
        assert_eq!(Profile::parse("PROD"), Profile::Prod);
        assert_eq!(Profile::parse("staging"), Profile::Staging);
        assert_eq!(Profile::parse("stage"), Profile::Staging);
    }

    #[test]
    fn parse_custom_profile() {
        assert_eq!(Profile::parse("qa"), Profile::Custom("qa".into()));
    }

    #[test]
    fn as_str_roundtrip() {
        assert_eq!(Profile::Dev.as_str(), "dev");
        assert_eq!(Profile::Prod.as_str(), "prod");
        assert_eq!(Profile::Custom("qa".into()).as_str(), "qa");
    }

    #[test]
    fn display_matches_as_str() {
        assert_eq!(format!("{}", Profile::Staging), "staging");
    }
}
