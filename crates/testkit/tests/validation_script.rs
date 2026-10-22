//! Regression test: the repeatable validation script exists and is wired
//! to the repository's documented validation surface.
//!
//! Issue #414 asks for a repeatable script for recording validation
//! results. The script lives at `.github/scripts/run-validation.sh` and
//! is the single-command entry point behind the second validation report
//! in `VALIDATION.md`. This test guards its presence and the invariants
//! it relies on, so a stray rename or deletion is caught before a
//! contributor reaches for a script that no longer exists.

mod common;

use common::read_repo_file;

#[test]
fn validation_script_exists_and_is_executable_shaped() {
    let script = read_repo_file(".github/scripts/run-validation.sh");
    assert!(
        script.starts_with("#!/usr/bin/env bash"),
        "run-validation.sh must start with a bash shebang so it runs the same way everywhere."
    );
    assert!(
        script.contains("set -uo pipefail"),
        "run-validation.sh must use `set -uo pipefail` so an unset variable or a failing command \
         cannot silently produce a misleading report."
    );
}

#[test]
fn validation_script_runs_the_documented_checks() {
    let script = read_repo_file(".github/scripts/run-validation.sh");
    // The script must invoke the same checks the VALIDATION.md second
    // report describes: audit, limits, and release-verify.
    assert!(
        script.contains("audit"),
        "run-validation.sh must run `audit` against the vault example."
    );
    assert!(
        script.contains("limits"),
        "run-validation.sh must run `limits` against the vault's batch_payout."
    );
    assert!(
        script.contains("release-verify"),
        "run-validation.sh must run `release-verify` against the workspace."
    );
}

#[test]
fn validation_script_writes_a_structured_report() {
    let script = read_repo_file(".github/scripts/run-validation.sh");
    assert!(
        script.contains("VALIDATION_RESULTS.txt"),
        "run-validation.sh must write its result to VALIDATION_RESULTS.txt by default."
    );
    assert!(
        script.contains("--output"),
        "run-validation.sh must accept a --output override so CI can place the report."
    );
}

#[test]
fn validation_md_documents_the_second_report() {
    let validation = read_repo_file("VALIDATION.md");
    assert!(
        validation.contains("Second public contract suite validation report"),
        "VALIDATION.md must contain the second validation report added by issue #413."
    );
    assert!(
        validation.contains("examples/vault"),
        "The second report must name examples/vault as the target contract."
    );
}
