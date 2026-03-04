//! Environment variable scanner and merger.
//!
//! Scans `MODUVEX__*` env vars and merges them into a TOML value tree.
//! Convention: `MODUVEX__SECTION__KEY=value` → `[section] key = value`.
//! Double underscore `__` separates section from key.

use toml::Value;

/// Scan environment variables and merge overrides into the config.
///
/// Only processes vars prefixed with `MODUVEX__`.
/// Returns a new merged value (env wins over file).
pub fn merge_env_overrides(base: Value) -> Value {
    let mut table = match base {
        Value::Table(t) => t,
        _ => return base,
    };

    for (key, val) in std::env::vars() {
        if let Some(rest) = key.strip_prefix("MODUVEX__") {
            if let Some((section, field)) = rest.split_once("__") {
                let section_lower = section.to_lowercase();
                let field_lower = field.to_lowercase();
                let parsed = parse_env_value(&val);

                // Get or create section table
                let section_table = table
                    .entry(section_lower)
                    .or_insert_with(|| Value::Table(toml::map::Map::new()));

                if let Value::Table(ref mut tbl) = section_table {
                    tbl.insert(field_lower, parsed);
                }
            }
        }
    }

    Value::Table(table)
}

/// Parse an env var value into an appropriate TOML value.
/// Tries: bool → integer → float → string (fallback).
fn parse_env_value(s: &str) -> Value {
    // Boolean
    match s.to_lowercase().as_str() {
        "true" => return Value::Boolean(true),
        "false" => return Value::Boolean(false),
        _ => {}
    }
    // Integer
    if let Ok(i) = s.parse::<i64>() {
        return Value::Integer(i);
    }
    // Float
    if let Ok(f) = s.parse::<f64>() {
        return Value::Float(f);
    }
    // String fallback
    Value::String(s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_env_bool() {
        assert_eq!(parse_env_value("true"), Value::Boolean(true));
        assert_eq!(parse_env_value("FALSE"), Value::Boolean(false));
    }

    #[test]
    fn parse_env_integer() {
        assert_eq!(parse_env_value("8080"), Value::Integer(8080));
        assert_eq!(parse_env_value("-1"), Value::Integer(-1));
    }

    #[test]
    fn parse_env_float() {
        assert_eq!(parse_env_value("2.72"), Value::Float(2.72));
    }

    #[test]
    fn parse_env_string_fallback() {
        assert_eq!(
            parse_env_value("localhost"),
            Value::String("localhost".into())
        );
    }

    #[test]
    fn merge_env_creates_section_and_field() {
        // Set env var for this test
        std::env::set_var("MODUVEX__TESTMERGE__PORT", "9090");
        let base: Value = "[testmerge]\nhost = \"localhost\"\n".parse().unwrap();
        let merged = merge_env_overrides(base);
        let section = merged["testmerge"].as_table().unwrap();
        assert_eq!(section["port"].as_integer().unwrap(), 9090);
        assert_eq!(section["host"].as_str().unwrap(), "localhost");
        std::env::remove_var("MODUVEX__TESTMERGE__PORT");
    }
}
