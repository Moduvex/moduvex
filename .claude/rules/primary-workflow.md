# Primary Workflow

**IMPORTANT**: Ensure token efficiency while maintaining high quality.

#### 1. Code Implementation
- Before non-trivial work, delegate to `planner` agent to create implementation plan in `./plans`.
- When planning, use `researcher` agents in parallel for relevant technical topics.
- Follow Rust idioms: `Result<T, E>` for errors, ownership/borrowing, zero-cost abstractions.
- **DO NOT** create new files when updating existing ones suffices.
- **[IMPORTANT]** After modifying code, run `cargo check --workspace` to verify compilation.
- Run `cargo clippy --workspace -- -D warnings` before considering code complete.

#### 2. Testing
- Run `cargo test --workspace` after implementation.
- Delegate to `tester` agent for comprehensive test validation when needed.
- Tests verify the FINAL code that will be reviewed and merged.
- **DO NOT** ignore failing tests. Fix root causes, not symptoms.
- **DO NOT** use mocks, cheats, or temporary workarounds to pass tests.

#### 3. Code Quality
- After tests pass, delegate to `code-reviewer` agent for review.
- Ensure `cargo clippy --workspace -- -D warnings` passes with zero warnings.
- Follow Rust conventions: snake_case, proper visibility, derive macros.
- Add doc comments (`///`) for public APIs.

#### 4. Integration
- Follow the plan from `planner` agent.
- Maintain backward compatibility across crate boundaries.
- Document breaking changes in commit messages and changelog.
- Delegate to `docs-manager` agent to update `./docs` if needed.

#### 5. Debugging
- Delegate to `debugger` agent for bug reports and CI failures.
- Read the report, implement fix, run `cargo test --workspace`.
- If tests fail, fix and re-test. Repeat until all pass.

#### 6. Build Verification Checklist
Before marking any task complete:
```
cargo check --workspace
cargo clippy --workspace -- -D warnings
cargo test --workspace
```
All three must pass with zero errors and zero warnings.
