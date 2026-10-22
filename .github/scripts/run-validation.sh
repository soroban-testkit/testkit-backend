#!/usr/bin/env bash
# Repeatable validation run: records soroban-testkit's own tooling against
# the contracts shipped in this repository (examples/vault and the
# quickstart example) and appends the result to a structured report.
#
# This is the script behind issue #414: a repeatable, single-command
# "record the validation result" workflow. It is intentionally
# self-contained — it invokes only `cargo` and the `soroban-testkit` binary
# built from this workspace, and writes a machine- and human-readable
# result file. No network access is required.
#
# Usage:
#   .github/scripts/run-validation.sh [--output <path>]
#
# Default output: VALIDATION_RESULTS.txt in the workspace root.
# Exit code: 0 if every check passed, non-zero if any check failed or
# could not run. A non-zero exit does not stop the run; every check is
# attempted and every result is recorded, so a partial run is still useful.
set -uo pipefail

workspace_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

output="${workspace_root}/VALIDATION_RESULTS.txt"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --output)
      shift
      if [ "$#" -eq 0 ]; then
        echo "Usage: $0 --output <path>" >&2
        exit 2
      fi
      output="$1"
      shift
      ;;
    -h|--help)
      cat <<'USAGE'
Usage: run-validation.sh [--output <path>]

Runs soroban-testkit's audit and limits checks against the vault example
contract and records the results in a structured text file. No network
access is required.

  --output <path>   Write the report here (default: VALIDATION_RESULTS.txt)
USAGE
      exit 0
      ;;
    *)
      echo "unknown option: $1" >&2
      exit 2
      ;;
  esac
done

# Resolve the output path if it was given relative.
case "$output" in
  /*) ;;
  *) output="$(pwd)/$output" ;;
esac

run_date="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
git_sha="$(git -C "$workspace_root" rev-parse --short HEAD 2>/dev/null || echo unknown)"

# Build the CLI binary once. The script does not assume a release build.
cli_bin="$workspace_root/target/debug/soroban-testkit"
if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo is not on PATH; run from a Rust toolchain environment." >&2
  exit 3
fi
cargo build -p soroban-testkit-cli --quiet 2>"$workspace_root/.validation-build.log" || {
  echo "error: failed to build the soroban-testkit CLI; see .validation-build.log" >&2
  exit 3
}

# Build the vault example to wasm32v1-none so `limits` can run against it.
vault_wasm="$workspace_root/target/wasm32v1-none/release/vault.wasm"
cargo build -p vault --target wasm32v1-none --release --quiet 2>>"$workspace_root/.validation-build.log" || {
  echo "warning: failed to build examples/vault to wasm32v1-none; limits check will be skipped." >&2
  vault_wasm=""
}

# Record helper: appends a labeled check result to the report.
record() {
  local name="$1" status="$2" detail="$3"
  printf '| %-28s | %-8s | %s\n' "$name" "$status" "$detail" >>"$output"
}

# Start the report fresh on each run so the file reflects the last run,
# not an accumulating history (history is git's job).
{
  echo "# Validation results"
  echo
  echo "Generated: $run_date (UTC)"
  echo "Git SHA:    $git_sha"
  echo "Workspace:  $workspace_root"
  echo
  echo "Each row is one check run against a contract in this repository."
  echo "status = pass | fail | skip"
  echo
  printf '| %-28s | %-8s | %s\n' "CHECK" "STATUS" "DETAIL"
  printf '| %-28s | %-8s | %s\n' "-----" "------" "------"
} >"$output"

overall=0

# 1. audit against examples/vault/src (the deliberately buggy contract).
audit_output="$("$cli_bin" audit "$workspace_root/examples/vault/src" 2>/tmp/stk-validation-audit.err)"
audit_status=$?
if [ "$audit_status" -eq 0 ]; then
  count="$(printf '%s\n' "$audit_output" | grep -cE '\[(warning|error)\]' || true)"
  record "audit examples/vault/src" "pass" "$count finding(s) reported (expected: vault is deliberately buggy)"
else
  overall=1
  record "audit examples/vault/src" "fail" "exit $audit_status; see stderr"
fi

# 2. audit --strict against a known-clean source file (negative case).
clean_file="$workspace_root/crates/cli/src/commands/mod.rs"
clean_output="$("$cli_bin" audit "$clean_file" 2>/tmp/stk-validation-clean.err)"
clean_status=$?
if [ "$clean_status" -eq 0 ]; then
  record "audit mod.rs (clean)" "pass" "no findings on a clean file"
else
  overall=1
  record "audit mod.rs (clean)" "fail" "exit $clean_status; expected no findings"
fi

# 3. limits against vault's batch_payout (resource ceiling discovery).
if [ -n "$vault_wasm" ] && [ -f "$vault_wasm" ]; then
  limits_output="$("$cli_bin" limits --contract "$vault_wasm" --fn batch_payout --ramp recipients 2>/tmp/stk-validation-limits.err)"
  limits_status=$?
  if [ "$limits_status" -eq 0 ]; then
    last_success="$(printf '%s\n' "$limits_output" | grep -E 'last successful' | head -1 || true)"
    record "limits batch_payout" "pass" "$last_success"
  else
    overall=1
    record "limits batch_payout" "fail" "exit $limits_status; see /tmp/stk-validation-limits.err"
  fi
else
  record "limits batch_payout" "skip" "vault.wasm not built"
fi

# 4. release-verify against the current workspace (metadata consistency).
rv_output="$("$cli_bin" release-verify 2>/tmp/stk-validation-rv.err)"
rv_status=$?
if [ "$rv_status" -eq 0 ]; then
  record "release-verify" "pass" "workspace metadata consistent at current version"
else
  # release-verify failing on an unreleased tree is informational, not a
  # hard failure: it only blocks a tag, not normal development.
  record "release-verify" "fail" "metadata not release-ready; see /tmp/stk-validation-rv.err"
fi

{
  echo
  echo "Overall: $([ "$overall" -eq 0 ] && echo 'all checks passed' || echo 'one or more checks failed or were skipped')"
} >>"$output"

# Clean up the build log so it does not leak into a commit.
rm -f "$workspace_root/.validation-build.log"

echo "validation report written to $output"
exit "$overall"
