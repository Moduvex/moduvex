# Code Standards & Development Guidelines

## Language & Compilation

### Rust Edition & MSRV
- **Edition:** 2021
- **MSRV (Minimum Supported Rust Version):** 1.80+
- **Target Channels:** Stable only (beta/nightly not required)

### Clippy & Linting

**Workspace lints** (`Cargo.toml` [workspace.lints]):
```toml
[workspace.lints.clippy]
all = { level = "warn", priority = -1 }
pedantic = { level = "warn", priority = -1 }
cast_possible_truncation = "allow"
cast_sign_loss = "allow"
cast_possible_wrap = "allow"
module_name_repetitions = "allow"
must_use_candidate = "allow"
missing_errors_doc = "allow"
missing_panics_doc = "allow"

[workspace.lints.rust]
unsafe_code = "warn"
missing_debug_implementations = "warn"
```

**Pre-commit checks:**
```bash
cargo clippy --workspace -- -D warnings
cargo fmt --check
```

## Module Organization

### File Naming
- Use **snake_case** for all `.rs` files (Rust convention)
- Descriptive names that self-document purpose for LLM tools
- Examples: `error.rs`, `lifecycle/phase.rs`, `query/builder.rs`
- **Avoid:** `mod.rs` imports alone; structure with submodules + re-exports in parent `lib.rs`

### Crate Structure Pattern
```
moduvex-{name}/
├── Cargo.toml
└── src/
    ├── lib.rs          # Public API + prelude
    ├── error.rs        # Error types
    ├── module-a/
    │   ├── mod.rs      # Module interface
    │   └── internal.rs # Private implementation
    └── module-b/
        └── ...
```

### Module Re-exports in lib.rs
Each crate's `lib.rs` must:
1. Declare `pub mod X;` for major modules
2. Re-export key types and functions at top level
3. Provide a `prelude` module with common imports

**Pattern:**
```rust
pub mod error;
pub mod di;
pub mod lifecycle;

pub use error::{ModuvexError, Result};
pub use di::{AppContext, Inject};

pub mod prelude {
    pub use crate::error::{ModuvexError, Result};
    pub use crate::di::{AppContext, Inject};
}
```

## Type & API Design

### Type Naming
- **Structs/Enums:** PascalCase (`Request`, `Response`, `ModuvexError`)
- **Functions/Methods:** snake_case (`send_request()`, `get_config()`)
- **Constants:** SCREAMING_SNAKE_CASE (`MAX_POOL_SIZE`, `DEFAULT_TIMEOUT_SECS`)
- **Type Parameters:** Single capital letter or descriptive (`T`, `E`, `State`)

### Error Handling

**All public APIs must return `Result<T>`:**
```rust
pub type Result<T, E = ModuvexError> = std::result::Result<T, E>;

pub fn parse_request(bytes: &[u8]) -> Result<Request> {
    // ... impl
}
```

**Error Classification** (4 variants):
- `ModuvexError::Domain(impl DomainError)` — Business logic errors (e.g., "user not found")
- `ModuvexError::Infra(impl InfraError)` — Infrastructure errors (e.g., "db connection failed")
- `ModuvexError::Config(ConfigError)` — Configuration errors (e.g., "missing env var")
- `ModuvexError::Lifecycle(LifecycleError)` — Framework errors (e.g., "module init failed")

**Using .context() for error chains:**
```rust
use moduvex_core::ErrorContext;

my_operation()
    .context("failed to initialize module")?
    .and_then(|x| process(x))
    .context("processing failed")?
```

### Unsafe Code Policy

**Allowed only when:**
1. Documented with `// SAFETY: ...` explaining invariants
2. Audited for memory safety
3. Marked with `#[allow(unsafe_code)]` if lint requires
4. Enclosed in minimal scope

**Example:**
```rust
// SAFETY: We have exclusive access to this pointer via &mut self.
//         The layout matches our internal struct definition.
unsafe { (*ptr).field = value; }
```

## Testing

### Unit Tests
- One test module per public function
- File structure: `#[cfg(test)] mod tests { ... }` at end of file
- Test naming: `test_{function}_{scenario}()`

**Example:**
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_request_valid() {
        let bytes = b"GET / HTTP/1.1\r\n\r\n";
        let req = parse_request(bytes).unwrap();
        assert_eq!(req.method, Method::Get);
    }

    #[test]
    fn test_parse_request_invalid_method() {
        let bytes = b"BADMETHOD / HTTP/1.1\r\n\r\n";
        assert!(parse_request(bytes).is_err());
    }
}
```

### Integration Tests
- Placed in `{crate}/tests/` directory
- Test cross-module interactions and public API
- Naming: `{component}_tests.rs`

### Coverage Target
- Minimum **80%** line coverage per crate
- Focus on error paths, not just happy paths
- Use `cargo tarpaulin` for reports

## Documentation

### Doc Comments
- All public items must have `///` doc comments
- First line is a brief summary (imperative mood)
- Longer docs: blank line + detailed explanation
- Include examples for complex types

**Example:**
```rust
/// Parse an HTTP request from raw bytes.
///
/// Handles HTTP/1.1 requests with optional body. Returns an error
/// if the request is malformed or exceeds size limits.
///
/// # Example
/// ```
/// let bytes = b"GET / HTTP/1.1\r\n\r\n";
/// let req = parse_request(bytes)?;
/// ```
pub fn parse_request(bytes: &[u8]) -> Result<Request> {
```

### Inline Comments
- Explain **why**, not **what** (code is what, comments are why)
- Use `//` for single-line, `/* */` for multi-line only if needed
- Complex algorithms deserve 2-3 line explanations

### README in Each Crate
Top-level `README.md` in workspace covers all crates. Crate-specific docs go in `lib.rs` only.

## Traits & Generic Code

### Public Trait Design
- Minimal methods (single responsibility)
- Use `where` clauses to clarify bounds
- Provide blanket implementations where possible

**Example:**
```rust
pub trait Module: Send + Sync {
    fn name(&self) -> &'static str;
}

pub trait DependsOn {
    type Required: AllDepsOk;
}
```

### Generic Function Limits
- Keep generic parameters ≤ 3 per function
- Use `impl Trait` for closure params
- Prefer concrete types over generics when performance matters

## Code Organization Boundaries

### When to Create a New Module
- 50+ lines of cohesive functionality
- Represents a distinct sub-system
- Has a clear, documentable API

### When NOT to Modularize
- Simple utility functions (keep in `lib.rs`)
- Single-purpose enums or structs
- Test helpers (keep in `#[cfg(test)]`)

### Max File Size
- Code files: **200 lines** (excluding tests + docs)
- Markdown docs: **800 lines**
- Violation → split into sub-modules or new file

## Dependency Management

### Internal Dependencies
- Use workspace [workspace.package] for version consistency
- Semver: `version.workspace = true`
- Avoid workspace path deps in published crates (use version only)

### External Dependencies
- Minimize count (each adds compile time + surface area)
- Prefer no-std crates when possible
- Zero async runtime deps (custom moduvex-runtime only)
- Required crypto: sha2, hmac, base64ct

## Performance Considerations

### Hot Path Optimizations
- Avoid allocations in request handling (pre-allocate buffers)
- Use `Arc<T>` clones instead of `Box<T>` for singletons
- Zero-copy parsing in HTTP protocol
- Lock-free metrics (atomic ops only)

### Benchmarking
- Write benchmarks for critical paths
- Use `criterion` for stable measurements
- Document performance assumptions in comments

## Formatting & Style

### Automatic Formatting
```bash
cargo fmt --all
```
Enforced via pre-commit (all PRs must pass `cargo fmt --check`).

### Line Length
- No strict limit, but aim for ≤100 chars (readability)
- Exceed only if breaking line makes code less clear

### Import Organization
```rust
// Crate imports (std first, then external, then internal)
use std::sync::Arc;
use serde::Deserialize;
use crate::error::ModuvexError;
```

## Security & Validation

### Input Validation
- Always validate untrusted input at boundary (HTTP handlers, config loaders)
- Use strong type wrappers (e.g., `NewType` pattern) for validated data

**Example:**
```rust
pub struct ValidEmail(String);

impl ValidEmail {
    pub fn new(email: &str) -> Result<Self> {
        if !email.contains('@') {
            return Err(ModuvexError::Domain(...));
        }
        Ok(ValidEmail(email.to_string()))
    }
}
```

### Secrets
- Never log sensitive data (passwords, tokens, keys)
- Use opaque types (`NewType`) to prevent accidental logging
- Document `#[derive(Debug)]` when impl needed (must omit sensitive fields)

### Dependency Auditing
```bash
cargo audit
```
Run before every release. Fix or document CVEs.

## Commit & Release Standards

### Commit Messages
- Format: `{type}({scope}): {message}`
- Types: `feat`, `fix`, `docs`, `refactor`, `test`, `chore`
- Example: `feat(http): add path parameter extraction`

### Pre-Push Checks
```bash
cargo fmt --all
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

### Version Bumping
- SemVer: `0.x.y` (pre-1.0), `x.y.z` (post-1.0)
- Breaking changes = minor bump in 0.x.y era
- Update all workspace versions together

## Workspace Build Command Reference

```bash
# Development (fastest)
cargo build

# With all tests
cargo test --workspace

# With linting
cargo clippy --workspace -- -D warnings

# Documentation
cargo doc --no-deps --open

# Before commit
cargo fmt --all && cargo clippy --workspace -- -D warnings && cargo test --workspace

# Before release
cargo audit && cargo test --all --all-features
```

---

**Last Updated:** Phase 8 (Documentation)
**Enforced Via:** CI/CD pipeline (GitHub Actions)
