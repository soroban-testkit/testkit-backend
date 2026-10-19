# API migration guide

One section per **minor** release of `soroban-testkit` and
`soroban-testkit-cli`, newest first. Under the `0.x` policy in
[`API_STABILITY.md`](../API_STABILITY.md), a minor bump may break your code,
so each section lists what changed and what to edit in your own tests.
Patch releases do not get a section unless they change migration steps.

This guide covers the testkit API only. For upgrading the pinned
`soroban-sdk`, see [`sdk-migration.md`](sdk-migration.md).

The [`docs` workflow](../.github/workflows/docs.yml) requires a section for
the current minor release, and the [release checklist](../RELEASING.md) asks
for it before tagging.

---

## Template

Copy this block when writing a new section.

```markdown
## 0.X — short title

Upgrading from 0.(X-1).Y.

### Breaking changes

- `old_name` → `new_name` (or: removed, use `replacement`)

### Migration steps

1. Before/after code snippet or flag change.

### No-op changes

- Anything that looks breaking but is not, so readers do not worry.
```

---

## 0.1 — initial release

Upgrading from: none. This is the first release, so there is nothing to
migrate from.

Pin the versions in [`COMPATIBILITY.md`](../COMPATIBILITY.md#pinning-compatible-versions)
so the testkit and `soroban-sdk` upgrade together.
