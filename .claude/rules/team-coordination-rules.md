# Team Coordination Rules

> Only applies when operating as a teammate within an Agent Team.

## File Ownership (CRITICAL)
- Each teammate owns distinct files — no overlapping edits
- Define ownership via crate/module boundaries: `File ownership: moduvex-db/src/*`
- Lead resolves ownership conflicts
- Tester reads implementation files but never edits them
- If ownership violation detected: STOP and report to lead

## Git Safety
- Prefer git worktrees for implementation teams
- Never force-push from a teammate session
- Commit frequently with conventional commit messages
- If in a git worktree, commit/push to the worktree branch only

## Communication
- Use `SendMessage(type: "message")` for peer DMs — specify recipient by name
- Use `SendMessage(type: "broadcast")` ONLY for critical blocking issues
- Mark tasks completed via `TaskUpdate` BEFORE sending completion message
- Include actionable findings in messages, not just "I'm done"

## Task Claiming
- Claim lowest-ID unblocked task first
- Check `TaskList` after completing each task
- Set task to `in_progress` before starting work
- If all tasks blocked, notify lead

## Build Verification
Before marking implementation tasks complete:
```
cargo check --workspace
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

## Reports
- Save to `{CK_REPORTS_PATH}` (fallback: `plans/reports/`)
- Naming: `{type}-{date}-{slug}.md`
- Be concise. List unresolved questions at end.

## Shutdown Protocol
- Approve shutdown requests unless mid-critical-operation
- Mark current task completed before approving
- Extract `requestId` from shutdown JSON for `shutdown_response`
