# CLAUDE.md

Moduvex: Rust modular backend framework. 10 crates, custom async runtime, zero 3rd-party async deps.

## Build Commands
```
cargo check --workspace          # Compile check
cargo clippy --workspace -- -D warnings  # Lint (zero warnings required)
cargo test --workspace           # Run all tests
cargo doc --workspace --no-deps  # Generate docs
```

## Crate Dependency Order
```
Layer 1: moduvex-runtime, moduvex-macros, moduvex-config  (no internal deps)
Layer 2: moduvex-core, moduvex-http, moduvex-observe
Layer 3: moduvex-db
Layer 4: moduvex-starter-web, moduvex-starter-data
Layer 5: moduvex (umbrella)
```

## Workflows
- Primary workflow: `./.claude/rules/primary-workflow.md`
- Development rules: `./.claude/rules/development-rules.md`
- Orchestration protocols: `./.claude/rules/orchestration-protocol.md`
- Documentation management: `./.claude/rules/documentation-management.md`
- Team coordination: `./.claude/rules/team-coordination-rules.md`

## Key Rules
- **ALWAYS** run `cargo check --workspace` after modifying code.
- **ALWAYS** run `cargo clippy --workspace -- -D warnings` before marking tasks complete.
- Follow Rust conventions: `Result<T, E>`, `?` operator, snake_case, `pub(crate)` before `pub`.
- Read `./README.md` before planning or implementing.
- Follow YAGNI - KISS - DRY principles.
- Update existing files — do not create new enhanced copies.

## Modularization
- Keep code files under 200 lines. Split into focused modules when exceeding.
- Check existing modules before creating new ones.
- Use snake_case for Rust file names (language convention).
- Write doc comments (`///`) for public APIs.

## Documentation (`./docs/`)
```
./docs
├── project-overview-pdr.md
├── code-standards.md
├── codebase-summary.md
├── system-architecture.md
├── development-roadmap.md
└── project-changelog.md
```

## Hook Response Protocol

### Privacy Block Hook (`@@PRIVACY_PROMPT@@`)
When blocked by privacy hook: parse JSON, use `AskUserQuestion` for user approval, then `bash cat "filepath"` if approved.

## Python Scripts (Skills)
Use venv: `.claude/skills/.venv/bin/python3 scripts/xxx.py`
