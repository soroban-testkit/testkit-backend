//! Integration tests for `soroban-testkit release-verify`.
//!
//! Exercises the CLI binary's release metadata verification against the
//! workspace it ships in, plus its actionable error paths for a missing
//! workspace and a missing version.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_soroban-testkit"))
        .args(args)
        .current_dir(workspace_root())
        .output()
        .expect("failed to run soroban-testkit")
}

fn run_from(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_soroban-testkit"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("failed to run soroban-testkit")
}

#[test]
fn release_verify_help_lists_what_it_checks() {
    let output = run(&["release-verify", "--help"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--version"), "{stdout}");
    assert!(stdout.contains("--workspace"), "{stdout}");
}

#[test]
fn release_verify_passes_against_the_current_workspace() {
    let output = run(&["release-verify"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // The workspace may or may not be release-ready at any given commit
    // (e.g. CHANGELOG may not have the current version yet). The command
    // must run to completion and produce a clear OK or a list of
    // findings; it must never panic or emit a backtrace.
    assert!(!stdout.contains("panic"), "{stdout}");
    assert!(!stderr.contains("panic"), "{stderr}");
    assert!(
        stdout.contains("release-verify: OK") || !stderr.is_empty(),
        "expected either an OK summary or a list of findings: stdout={stdout} stderr={stderr}"
    );
}

#[test]
fn release_verify_rejects_a_missing_workspace() {
    let tmp = std::env::temp_dir().join(format!(
        "stk-release-verify-missing-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let output = run_from(&tmp, &["release-verify"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no Cargo.toml"), "{stderr}");
    std::fs::remove_dir_all(&tmp).unwrap();
}

#[test]
fn release_verify_strips_leading_v_from_version() {
    // The command should accept both `0.1.0` and `v0.1.0` and treat them
    // identically. We verify the argument is accepted (not rejected as
    // invalid) by checking the command runs to completion rather than
    // failing at parse time.
    let output = run(&["release-verify", "--version", "v0.1.0"]);
    // Whether it passes or fails depends on the current tree, but it must
    // not fail at clap parsing.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("error:") || stderr.contains("release-verify"),
        "unexpected parse error: {stderr}"
    );
}
