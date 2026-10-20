# API migration guide

Upgrade notes for each **minor** release of `soroban-testkit` and
`soroban-testkit-cli`. Under the `0.x` rules in
[`API_STABILITY.md`](API_STABILITY.md), a minor bump may break your code, so
each minor release gets a section here that says what to change.

Patch releases are not listed. They restore documented behavior and add no
breaking changes. Their fixes are in [`CHANGELOG.md`](CHANGELOG.md).

Sections are newest first. Each one is titled with the exact version
(`## 0.1.0`), because the [`docs` workflow](.github/workflows/docs.yml) fails
any minor release that has no matching heading.

## For maintainers: writing a section

1. Copy every `> **Breaking:**` entry from the release's section of
   [`CHANGELOG.md`](CHANGELOG.md).
2. For each one, show the old code and the new code. A contract author should
   be able to update their tests from the example alone.
3. List removed or renamed public items in a table: the old path, and the new
   path or `removed`.
4. Note any changed failure message. Tests often pin these with
   `#[should_panic(expected = "...")]`, so this is the change most likely to
   break a suite without a compile error.
5. If the pinned `soroban-sdk` changed, link the new row in
   [`COMPATIBILITY.md`](COMPATIBILITY.md#pinning-a-compatible-version).

## 0.1.0

First release. No version was published before it, so there is nothing to
migrate from.

Pin the versions listed in
[`COMPATIBILITY.md`](COMPATIBILITY.md#pinning-a-compatible-version).

Breaking changes: none.
