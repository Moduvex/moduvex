# Orchestration Protocol

## Delegation Context (MANDATORY)

When spawning subagents, **ALWAYS** include:

1. **Work Context Path**: `/Users/tranhoangtu/Desktop/PET/Moduvex`
2. **Reports Path**: `{work_context}/plans/reports/`
3. **Plans Path**: `{work_context}/plans/`
4. **Build commands**: `cargo check --workspace`, `cargo test --workspace`, `cargo clippy --workspace -- -D warnings`

---

## Sequential Chaining
Chain subagents when tasks have dependencies:
- **Planning → Implementation → Testing → Review**: Feature development
- **Research → Design → Code → Docs**: New crate/module
- Each agent completes fully before next begins
- Pass context and outputs between agents

## Parallel Execution
Spawn multiple subagents for independent tasks:
- **Multiple crate fixes**: Different agents fixing isolated crates (no file overlap)
- **Code + Tests + Docs**: Non-conflicting changes across workspace
- **Research tasks**: Multiple researcher agents exploring different topics
- **File ownership**: Each agent owns distinct files — no overlapping edits

## Rust Workspace Considerations
- Moduvex has 10 crates with dependency chains — respect crate boundaries.
- Changes in lower-layer crates (runtime, macros) may break upper layers.
- Always `cargo check --workspace` after cross-crate changes.
- When delegating, specify which crate(s) the agent owns.

---

## Agent Teams (Optional)

For multi-session parallel collaboration, activate the `/team` skill.
See `.claude/skills/team/SKILL.md` for templates and spawn instructions.
