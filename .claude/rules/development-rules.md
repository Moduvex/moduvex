# Development Rules

**IMPORTANT:** Follow **YAGNI - KISS - DRY** principles at all times.

## Rust Conventions
- **Error handling**: Use `Result<T, E>` and `?` operator. No `unwrap()` in library code (only tests/examples).
- **Naming**: snake_case for files, functions, variables. PascalCase for types/traits. SCREAMING_SNAKE for constants.
- **Visibility**: Default to private. Use `pub(crate)` before `pub`. Only expose what's needed.
- **Unsafe**: Avoid unless absolutely necessary. Document safety invariants when used.
- **Dependencies**: Minimize external crates. Moduvex's value is zero 3rd-party async runtime deps.

## File Management
- **File size**: Keep code files under 200 lines. Split into focused modules when exceeding.
- **Module structure**: Use `mod.rs` or inline `mod` declarations. Group related functionality.
- **Doc comments**: `///` for public APIs, `//!` for module-level docs.
- Follow codebase structure in `./docs` during implementation.
- **DO NOT** create new files when updating existing ones suffices.

## Code Quality
- `cargo check --workspace` — must compile cleanly
- `cargo clippy --workspace -- -D warnings` — zero warnings
- `cargo test --workspace` — all tests pass
- Prioritize functionality and readability over strict formatting.
- Use `code-reviewer` agent to review code after implementation.

## Pre-commit/Push Rules
- Run clippy + tests before commit.
- Use conventional commit format: `feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`
- No AI references in commit messages.
- **DO NOT** commit secrets (`.env`, API keys, credentials) to git.
- Keep commits focused on actual code changes.

## Crate Publishing
- Dependency order: runtime/macros/config → core/http/observe → db → starters → umbrella
- Verify `cargo publish --dry-run -p <crate>` before actual publish.
- Update version numbers consistently across workspace.
