# Documentation Management

## Docs Structure (`./docs/`)

| File | Purpose |
|------|---------|
| `project-overview-pdr.md` | Product development requirements |
| `code-standards.md` | Coding standards and conventions |
| `codebase-summary.md` | Architecture overview and crate descriptions |
| `system-architecture.md` | System design and component interactions |
| `development-roadmap.md` | Project phases, milestones, progress |
| `project-changelog.md` | Record of changes, features, fixes |

## Update Triggers
- **After feature implementation**: Update roadmap + changelog
- **After bug fixes**: Document in changelog with severity
- **After crate changes**: Update codebase-summary if architecture changed
- **After publishing**: Update roadmap milestones

## Update Protocol
1. Read current doc status before changes
2. Keep updates concise — focus on what changed and why
3. Verify cross-references are accurate
4. Delegate to `docs-manager` agent for comprehensive updates

## Plans (`./plans/`)

### Plan Location
Use naming pattern from `## Naming` section injected by hooks.

Example: `plans/260305-0714-pool-notification-fix/`

### File Organization
```
plans/{slug}/
├── plan.md                    # Overview (under 80 lines)
├── phase-01-*.md              # Phase details
├── reports/
│   └── *.md                   # Agent reports
└── research/
    └── *.md                   # Research findings
```

### Phase File Structure
Each phase file contains:
- **Overview**: Priority, status, description
- **Requirements**: Functional + non-functional
- **Related Code Files**: Files to modify/create/delete
- **Implementation Steps**: Numbered, specific instructions
- **Todo List**: Checkbox tracking
- **Success Criteria**: Definition of done
