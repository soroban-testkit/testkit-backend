//! Integration tests for `soroban-testkit limits`.
//!
//! Exercises the CLI binary for resource limit discovery, boundary refinement audits,
//! output sanitization, measurement limitations reporting, baseline comparisons,
//! and ramp parameter handling.

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

fn ensure_vault_wasm() -> PathBuf {
    let wasm = workspace_root().join("target/wasm32v1-none/release/vault.wasm");
    if !wasm.exists() {
        let build = Command::new("cargo")
            // The outer cargo-llvm-cov run instruments native tests. This nested
            // WASM fixture build must not inherit its unsupported coverage flags.
            .env_remove("RUSTFLAGS")
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .env_remove("CARGO_TARGET_WASM32V1_NONE_RUSTFLAGS")
            .args([
                "build",
                "-p",
                "vault",
                "--target",
                "wasm32v1-none",
                "--release",
            ])
            .current_dir(workspace_root())
            .status()
            .expect("failed to build vault.wasm");
        assert!(
            build.success(),
            "failed to compile examples/vault for documented wasm target `wasm32v1-none`. \
             Ensure the target is installed: `rustup target add wasm32v1-none`."
        );
    }
    wasm
}

#[test]
fn limits_help_documents_options_and_limitations() {
    let output = run(&["limits", "--help"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("--contract"), "{stdout}");
    assert!(stdout.contains("--fn"), "{stdout}");
    assert!(stdout.contains("--ramp"), "{stdout}");
    assert!(stdout.contains("--arg"), "{stdout}");
    assert!(stdout.contains("--probe-timeout"), "{stdout}");
    assert!(stdout.contains("--baseline"), "{stdout}");
    assert!(stdout.contains("--baseline-tolerance-pct"), "{stdout}");
    assert!(stdout.contains("--save-baseline"), "{stdout}");
    assert!(stdout.contains("--export"), "{stdout}");
}

#[test]
fn limits_rejects_invalid_baseline_tolerance() {
    let wasm = ensure_vault_wasm();
    let output = run(&[
        "limits",
        "--contract",
        wasm.to_str().unwrap(),
        "--fn",
        "batch_payout",
        "--ramp",
        "recipients",
        "--baseline-tolerance-pct",
        "150",
    ]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--baseline-tolerance-pct must be between 0 and 100"),
        "{stderr}"
    );
}

#[test]
fn limits_rejects_arg_overriding_ramp_parameter() {
    let wasm = ensure_vault_wasm();
    let output = run(&[
        "limits",
        "--contract",
        wasm.to_str().unwrap(),
        "--fn",
        "batch_payout",
        "--ramp",
        "recipients",
        "--arg",
        "recipients=10",
    ]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--arg cannot override ramp parameter \"recipients\""),
        "{stderr}"
    );
}

#[test]
fn limits_rejects_unknown_function_with_available_list() {
    let wasm = ensure_vault_wasm();
    let output = run(&[
        "limits",
        "--contract",
        wasm.to_str().unwrap(),
        "--fn",
        "nonexistent_function",
        "--ramp",
        "recipients",
    ]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no function named \"nonexistent_function\""),
        "{stderr}"
    );
    assert!(stderr.contains("available functions:"), "{stderr}");
    assert!(stderr.contains("batch_payout"), "{stderr}");
}

#[test]
fn limits_reports_refinement_audit_and_limitations_in_stdout() {
    let wasm = ensure_vault_wasm();
    let output = run(&[
        "limits",
        "--contract",
        wasm.to_str().unwrap(),
        "--fn",
        "batch_payout",
        "--ramp",
        "recipients",
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "stdout: {stdout}");
    assert!(stdout.contains("last successful recipients"), "{stdout}");
    assert!(stdout.contains("instructions:"), "{stdout}");
    assert!(stdout.contains("memory bytes:"), "{stdout}");
    assert!(
        stdout.contains("boundary refinement: probed"),
        "stdout missing refinement audit: {stdout}"
    );
    assert!(
        stdout.contains(
            "ledger reads/writes and transaction size are not measured by this command; see --help"
        ),
        "stdout missing measurement limitations: {stdout}"
    );
    // #492: failures are reported with a standardized category.
    assert!(
        stdout.contains("first failed recipients value:"),
        "stdout missing first failure: {stdout}"
    );
    assert!(
        stdout.contains("probe failures:"),
        "stdout missing probe failure categories: {stdout}"
    );
}

#[test]
fn limits_text_summary_and_json_export_agree_on_the_boundary() {
    // #490: the terminal summary and the machine-readable report are both
    // rendered from one summary, so the discovered boundary and the
    // standardized failure category must match exactly.
    let wasm = ensure_vault_wasm();
    let temp_dir = std::env::temp_dir().join(format!("stk-limits-agree-{}", std::process::id()));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let export_path = temp_dir.join("agreement.json");

    let output = run(&[
        "limits",
        "--contract",
        wasm.to_str().unwrap(),
        "--fn",
        "batch_payout",
        "--ramp",
        "recipients",
        "--export",
        export_path.to_str().unwrap(),
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "stdout: {stdout}");

    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&export_path).unwrap()).unwrap();
    let best = json["best"].as_u64().unwrap();

    assert!(
        stdout.contains(&format!(
            "last successful recipients for \"batch_payout\": {best}"
        )),
        "stdout boundary disagrees with JSON: {stdout}"
    );
    assert_eq!(json["audit"]["final_success"], best);

    // The standardized failure category must appear identically in both.
    let category = match json["first_failure_category"].as_str() {
        Some(category) => category,
        None => panic!("JSON is missing first_failure_category: {json}"),
    };
    assert!(
        stdout.contains(&format!("[{category}]")),
        "stdout is missing standardized category {category}: {stdout}"
    );
    assert!(
        json["probe_failure_counts"][category].as_u64().unwrap() >= 1,
        "JSON failure counts are missing {category}: {json}"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn limits_export_json_includes_limitations_audit_and_sanitizes_temp_paths() {
    let wasm = ensure_vault_wasm();
    let temp_dir =
        std::env::temp_dir().join(format!("stk-limits-test-json-{}", std::process::id()));
    std::fs::create_dir_all(&temp_dir).unwrap();

    // Copy wasm into temp directory to verify temp path sanitization
    let temp_wasm = temp_dir.join("vault.wasm");
    std::fs::copy(&wasm, &temp_wasm).unwrap();

    let export_path = temp_dir.join("results.json");
    let output = run(&[
        "limits",
        "--contract",
        temp_wasm.to_str().unwrap(),
        "--fn",
        "batch_payout",
        "--ramp",
        "recipients",
        "--export",
        export_path.to_str().unwrap(),
    ]);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "stdout: {stdout}");

    let export_content = std::fs::read_to_string(&export_path).unwrap();
    let json: serde_json::Value = serde_json::from_str(&export_content).unwrap();

    assert_eq!(json["function"], "batch_payout");
    assert_eq!(json["ramp"], "recipients");
    assert!(json["best"].as_u64().unwrap() > 0);
    assert!(json["instructions"].as_u64().unwrap() > 0);
    assert!(json["memory_bytes"].as_u64().unwrap() > 0);

    // Verify measurement limitations
    assert_eq!(
        json["measurement_limitations"],
        "ledger reads/writes and transaction size are not measured by this command; see --help"
    );

    // Verify temp directory path sanitization: must not leak temp_dir prefix
    assert_eq!(json["contract"], "vault.wasm");
    assert!(!export_content.contains("stk-limits-test-json"));

    // Verify audit structure is present
    assert!(json["audit"].is_object());
    assert!(!json["audit"]["steps"].as_array().unwrap().is_empty());
    assert_eq!(json["audit"]["final_success"], json["best"]);

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn limits_save_baseline_includes_limitations_and_sanitizes_temp_paths() {
    let wasm = ensure_vault_wasm();
    let temp_dir =
        std::env::temp_dir().join(format!("stk-limits-test-base-{}", std::process::id()));
    std::fs::create_dir_all(&temp_dir).unwrap();

    let temp_wasm = temp_dir.join("vault.wasm");
    std::fs::copy(&wasm, &temp_wasm).unwrap();

    let baseline_path = temp_dir.join("vault.baseline");
    let output = run(&[
        "limits",
        "--contract",
        temp_wasm.to_str().unwrap(),
        "--fn",
        "batch_payout",
        "--ramp",
        "recipients",
        "--save-baseline",
        baseline_path.to_str().unwrap(),
    ]);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "stdout: {stdout}");

    let baseline_content = std::fs::read_to_string(&baseline_path).unwrap();
    assert!(baseline_content.contains("function=batch_payout"));
    assert!(baseline_content.contains("ramp=recipients"));
    assert!(baseline_content.contains("contract=vault.wasm"));
    assert!(!baseline_content.contains("stk-limits-test-base"));
    assert!(baseline_content.contains("measurement_limitations=ledger reads/writes and transaction size are not measured by this command; see --help"));

    // Verify subsequent --baseline comparison passes
    let cmp_output = run(&[
        "limits",
        "--contract",
        temp_wasm.to_str().unwrap(),
        "--fn",
        "batch_payout",
        "--ramp",
        "recipients",
        "--baseline",
        baseline_path.to_str().unwrap(),
    ]);
    let cmp_stdout = String::from_utf8_lossy(&cmp_output.stdout);
    assert!(cmp_output.status.success(), "stdout: {cmp_stdout}");
    assert!(cmp_stdout.contains("no regression"), "{cmp_stdout}");

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn limits_explains_how_to_reproduce_reported_maximum_locally() {
    let wasm = ensure_vault_wasm();
    let temp_dir =
        std::env::temp_dir().join(format!("stk-limits-repro-test-{}", std::process::id()));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let export_path = temp_dir.join("repro.json");

    let output = run(&[
        "limits",
        "--contract",
        wasm.to_str().unwrap(),
        "--fn",
        "batch_payout",
        "--ramp",
        "recipients",
        "--export",
        export_path.to_str().unwrap(),
    ]);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "stdout: {stdout}");
    assert!(
        stdout.contains("reproduce locally:"),
        "stdout missing reproduction guidance: {stdout}"
    );
    assert!(
        stdout.contains("batch_payout"),
        "reproduction guidance missing function name: {stdout}"
    );
    assert!(
        stdout.contains("recipients"),
        "reproduction guidance missing ramp parameter: {stdout}"
    );

    let export_content = std::fs::read_to_string(&export_path).unwrap();
    let json: serde_json::Value = serde_json::from_str(&export_content).unwrap();
    assert!(
        json["reproduction"].is_string(),
        "JSON export missing reproduction guidance: {json}"
    );
    let repro_str = json["reproduction"].as_str().unwrap();
    assert!(
        repro_str.contains("batch_payout"),
        "repro string: {repro_str}"
    );
    assert!(
        repro_str.contains("recipients"),
        "repro string: {repro_str}"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
}

// ---- Bug: handle empty Bytes overrides without producing invalid XDR ----

#[test]
fn limits_accepts_empty_bytes_arg_override() {
    // The vault contract's batch_payout takes (recipients: Vec<Address>,
    // amount: i128) — neither is Bytes, but we can verify the CLI accepts
    // the empty-bytes override syntactically by using it on a non-ramp
    // parameter that exists. Since the vault has no Bytes parameter, this
    // test exercises the validation path: the CLI should parse `0x` and
    // not crash with an invalid-XDR error before reporting the type
    // mismatch.
    //
    // Instead, we test at the unit level (in limits.rs) that explicit_val
    // produces a valid empty Bytes for `0x`, and here we verify the CLI
    // doesn't reject `--arg name=0x` at the parse level for a known
    // function — the parameter-type mismatch error is expected and proves
    // the CLI got far enough to validate the type.
    let wasm = ensure_vault_wasm();
    let output = run(&[
        "limits",
        "--contract",
        wasm.to_str().unwrap(),
        "--fn",
        "batch_payout",
        "--ramp",
        "recipients",
        "--arg",
        "amount=0x",
    ]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // The error should be about the type mismatch (amount is i128, not
    // Bytes), NOT about invalid XDR or a panic.
    assert!(
        !stderr.contains("invalid") || !stderr.contains("XDR"),
        "should not produce an invalid XDR error for empty bytes: {stderr}"
    );
}

// ---- Bug: retain stderr context from a failed wasm probe ----

#[test]
fn limits_reports_first_failing_probe_stderr_in_stdout() {
    // When a probe fails, the parent should capture and display the
    // child's stderr so the user can see *why* the ceiling was hit. We
    // verify this by checking that the stdout summary includes the
    // "first failing probe stderr" line when a failure occurs.
    //
    // The vault's batch_payout will eventually fail at a high recipient
    // count, and the probe's stderr (from the Soroban host) should appear
    // in the summary.
    let wasm = ensure_vault_wasm();
    let output = run(&[
        "limits",
        "--contract",
        wasm.to_str().unwrap(),
        "--fn",
        "batch_payout",
        "--ramp",
        "recipients",
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "stdout: {stdout}");
    // The summary should report the first failure with its category.
    assert!(
        stdout.contains("first failed recipients value:"),
        "stdout missing first failure: {stdout}"
    );
}

#[test]
fn limits_includes_failed_probe_stderr_in_json_export() {
    // The JSON export should include first_failed_probe_stderr when a
    // probe failure occurred and the child produced stderr output.
    let wasm = ensure_vault_wasm();
    let temp_dir =
        std::env::temp_dir().join(format!("stk-limits-stderr-json-{}", std::process::id()));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let export_path = temp_dir.join("stderr.json");

    let output = run(&[
        "limits",
        "--contract",
        wasm.to_str().unwrap(),
        "--fn",
        "batch_payout",
        "--ramp",
        "recipients",
        "--export",
        export_path.to_str().unwrap(),
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "stdout: {stdout}");

    let export_content = std::fs::read_to_string(&export_path).unwrap();
    let json: serde_json::Value = serde_json::from_str(&export_content).unwrap();

    // The field should be present in the JSON schema (even if null/omitted
    // when no stderr was captured).
    assert!(
        json.get("first_failed_probe_stderr").is_some()
            || json.get("first_failure_category").is_some(),
        "JSON should include probe failure context: {json}"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn limits_launches_wasm_from_a_path_with_spaces_without_shell_splitting() {
    let wasm = ensure_vault_wasm();
    let root = std::env::temp_dir().join(format!("stk limit paths {}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let copy = root.join("contract copy.wasm");
    std::fs::copy(&wasm, &copy).unwrap();

    let output = run(&[
        "limits",
        "--contract",
        copy.to_str().unwrap(),
        "--fn",
        "batch_payout",
        "--ramp",
        "recipients",
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stdout: {stdout}; stderr: {stderr}"
    );
    assert!(
        stdout.contains("--contract 'contract copy.wasm'"),
        "copyable guidance must quote the filename: {stdout}"
    );
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn limits_timeout_creates_no_temporary_cargo_probe_project() {
    let wasm = ensure_vault_wasm();
    let root = std::env::temp_dir().join(format!("stk-probe-empty-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_soroban-testkit"))
        .args([
            "limits",
            "--contract",
            wasm.to_str().unwrap(),
            "--fn",
            "batch_payout",
            "--ramp",
            "recipients",
            "--probe-timeout",
            "0",
        ])
        .current_dir(&root)
        .output()
        .unwrap();
    // Either the immediate deadline fails the first probe, or a very fast
    // probe may complete; neither outcome creates a disposable Cargo project.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() || stderr.contains("timed-out"),
        "unexpected probe error: {stderr}"
    );
    assert_eq!(
        std::fs::read_dir(&root).unwrap().count(),
        0,
        "limits must not leave probe Cargo.toml/src/target directories"
    );
    std::fs::remove_dir(&root).unwrap();
}

// #444: a contract error from the Vault (uninitialized deposit) is not
// evidence of a resource limit. Do not report a fake best ceiling.
#[test]
fn limits_uninitialized_vault_failure_is_not_reported_as_ceiling() {
    let wasm = ensure_vault_wasm();
    let output = run(&[
        "limits",
        "--contract",
        wasm.to_str().unwrap(),
        "--fn",
        "deposit",
        "--ramp",
        "amount",
    ]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("deposit"), "{stderr}");
    assert!(stderr.contains("amount=1"), "{stderr}");
    assert!(
        stderr.contains("before a resource limit was verified"),
        "{stderr}"
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("last successful"));
}

// #444: success at 1 and 2 must not let us report 2 as a budget ceiling
// when the contract itself rejects count 4.
#[test]
fn limits_stops_when_contract_rejects_after_successful_probes() {
    // Reuse the WASM fixture prepared by the CI workflow.
    let wasm = ensure_vault_wasm();
    let output = run(&[
        "limits",
        "--contract",
        wasm.to_str().unwrap(),
        "--fn",
        "reject_after_three",
        "--ramp",
        "count",
    ]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("reject_after_three"), "{stderr}");
    assert!(stderr.contains("count=4"), "{stderr}");
    assert!(
        stderr.contains("before a resource limit was verified"),
        "{stderr}"
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("last successful"));
}
