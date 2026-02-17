## Description

Clear explanation of what this PR accomplishes and why it matters.
Include business context where relevant.

## Changes

Detailed list of what changed:

- File/module and what changed
- File/module and what changed
- Any architecture/runtime/infra implications

For slice-related changes, include:

- Affected slices
- Cross-slice communication changes (if any) and why
- Migration requirements (if any)

## Motivation

Business motivation:

Technical motivation:

Alternative approaches considered:

## Scope and impact

- Affected slices:
- Data/API changes:
- Backward compatibility:
- Performance impact:
- Security impact:

## Testing

- [ ] Unit tests
- [ ] Integration tests
- [ ] Manual verification
- [ ] Performance checks (if applicable)
- [ ] Security checks (if applicable)

Commands/results:

```bash
# Example:
cargo make ci
```

## Related issues and documentation

- Fixes:
- Related:
- Architecture docs: `docs/architecture/README.md`
- Relevant ARD/ADR:
- Operations/runbook updates:

## Reviewer checklist

- [ ] Code follows project standards and architecture constraints
- [ ] Slice boundaries are respected (`docs/architecture/ard/0003-slice-dependency-contracts.md`)
- [ ] Tests added/updated and passing
- [ ] Documentation updated (README/docs/operations as needed)
- [ ] No undocumented breaking change
- [ ] Performance trade-offs documented where relevant
- [ ] Security considerations addressed where relevant

## Additional notes

Trade-offs, follow-up work, or deviations from standards.
