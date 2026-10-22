//! Integration tests for `soroban-testkit audit`.
//!
//! Exercises the CLI binary's documented scan roots (directory, single file,
//! and stdin) and its directory exclusions (`target/`, `vendor/`,
//! `node_modules/`, and hidden directories), plus the actionable error for
//! a missing path.

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
    run_from(&workspace_root(), args)
}

fn run_from(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_soroban-testkit"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("failed to run soroban-testkit")
}

struct AuditFixture(PathBuf);

impl AuditFixture {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("stk-audit-{name}-{}", std::process::id()));
        std::fs::create_dir_all(root.join("crates/vault/src")).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        Self(root)
    }

    fn write(&self, path: &str, content: impl AsRef<[u8]>) -> PathBuf {
        let path = self.0.join(path);
        std::fs::write(&path, content).unwrap();
        path
    }

    fn scan(&self, args: &[&str]) -> Output {
        let mut scan_args = vec!["audit", "."];
        if !args.contains(&"--format") {
            scan_args.extend_from_slice(&["--format", "json"]);
        }
        scan_args.extend_from_slice(args);
        run_from(&self.0, &scan_args)
    }
}

impl Drop for AuditFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run_with_stdin(args: &[&str], stdin: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_soroban-testkit"))
        .args(args)
        .current_dir(workspace_root())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to run soroban-testkit");
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), stdin).unwrap();
    child.wait_with_output().unwrap()
}

/// A source snippet that trips `missing-require-auth`.
const UNAUTHORIZED_ENTRY_POINT: &str = r#"
#[contractimpl]
impl Contract {
    pub fn withdraw(env: Env, from: Address) {
        env.storage().instance().set(&from, &1i128);
    }
}
"#;

#[test]
fn audit_help_documents_supported_scan_roots_and_exclusions() {
    let output = run(&["audit", "--help"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Scan roots.
    assert!(stdout.contains("stdin"), "{stdout}");
    assert!(stdout.contains("directory"), "{stdout}");
    // Documented exclusions.
    assert!(stdout.contains("target/"), "{stdout}");
    assert!(stdout.contains("vendor/"), "{stdout}");
    assert!(stdout.contains("node_modules/"), "{stdout}");
    assert!(stdout.contains("hidden"), "{stdout}");
    // Standard options remain documented.
    assert!(stdout.contains("--format"), "{stdout}");
    assert!(stdout.contains("--strict"), "{stdout}");
}

#[test]
fn audit_scans_a_directory_recursively() {
    let root = std::env::temp_dir().join(format!("stk-audit-dir-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src/nested")).unwrap();
    std::fs::write(root.join("src/lib.rs"), UNAUTHORIZED_ENTRY_POINT).unwrap();
    std::fs::write(root.join("src/nested/more.rs"), UNAUTHORIZED_ENTRY_POINT).unwrap();

    let output = run(&["audit", root.to_str().unwrap()]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "stdout: {stdout}");
    assert!(stdout.contains("src/lib.rs"), "{stdout}");
    assert!(stdout.contains("src/nested/more.rs"), "{stdout}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn audit_skips_excluded_directories() {
    let root = std::env::temp_dir().join(format!("stk-audit-exclude-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("target/debug")).unwrap();
    std::fs::create_dir_all(root.join("vendor/dep/src")).unwrap();
    std::fs::create_dir_all(root.join("src/nested/target/debug")).unwrap();
    std::fs::create_dir_all(root.join("src/nested/vendor/dep")).unwrap();
    std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
    std::fs::create_dir_all(root.join(".git/objects")).unwrap();
    std::fs::write(root.join("src/vault.rs"), UNAUTHORIZED_ENTRY_POINT).unwrap();
    std::fs::write(
        root.join("target/debug/generated.rs"),
        UNAUTHORIZED_ENTRY_POINT,
    )
    .unwrap();
    std::fs::write(
        root.join("node_modules/pkg/dep.rs"),
        UNAUTHORIZED_ENTRY_POINT,
    )
    .unwrap();
    std::fs::write(root.join(".git/objects/blob.rs"), UNAUTHORIZED_ENTRY_POINT).unwrap();
    for path in [
        "vendor/dep/src/vendored.rs",
        "src/nested/target/debug/generated_nested.rs",
        "src/nested/vendor/dep/vendored_nested.rs",
    ] {
        // Prune the whole tree: even invalid Rust must not cause a scan error.
        std::fs::write(root.join(path), "fn invalid(").unwrap();
    }

    let output = run(&["audit", root.to_str().unwrap()]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "stdout: {stdout}");
    assert!(stdout.contains("src/vault.rs"), "{stdout}");
    // Excluded trees must never be scanned.
    assert!(!stdout.contains("generated.rs"), "{stdout}");
    assert!(!stdout.contains("dep.rs"), "{stdout}");
    assert!(!stdout.contains("blob.rs"), "{stdout}");
    assert!(!stdout.contains("vendored"), "{stdout}");
    assert!(output.stderr.is_empty(), "{output:?}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn audit_scans_explicitly_selected_excluded_roots_and_similarly_named_directories() {
    let fixture = AuditFixture::new("explicit-exclusions");
    for directory in ["target", "vendor", "vendors", "target_contract"] {
        std::fs::create_dir_all(fixture.0.join(directory)).unwrap();
        let file = fixture.write(&format!("{directory}/lib.rs"), UNAUTHORIZED_ENTRY_POINT);
        for root in [file.parent().unwrap(), file.as_path()] {
            let output = run_from(
                &fixture.0,
                &[
                    "audit",
                    root.to_str().unwrap(),
                    "--format",
                    "json",
                    "--strict",
                ],
            );
            let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert!(!output.status.success(), "{output:?}");
            assert_eq!(report["summary"]["finding_count"], 1, "{report}");
            assert_eq!(report["findings"][0]["rule"], "missing-require-auth");
        }
    }

    let output = fixture.scan(&[]);
    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["summary"]["finding_count"], 2, "{report}");
}

#[test]
fn audit_raw_identifiers_report_the_same_rules_as_ordinary_identifiers() {
    let fixture = AuditFixture::new("raw-identifiers");
    let ordinary = r#"
        fn withdraw(env: Env, caller: &sdk::Address, amount: i128, client: Client) {
            let balance: i128 = 10;
            let _ = balance + 1;
            let _ = amount + 1;
            env.storage().persistent().get(&"balance");
            client.try_transfer(&caller, &caller, &amount);
        }
    "#;
    let raw = r#"
        fn r#type(r#env: Env, r#caller: &sdk::r#Address, r#amount: r#i128, client: Client) {
            let r#balance: r#i128 = 10;
            let _ = balance + 1;
            let _ = amount + 1;
            env.r#storage().r#persistent().r#get(&"balance");
            client.r#try_transfer(&caller, &caller, &amount);
        }
    "#;
    let mut reports = Vec::new();
    for source in [ordinary, raw] {
        fixture.write("crates/vault/src/lib.rs", source);
        let output = fixture.scan(&["--strict"]);
        assert!(!output.status.success(), "{output:?}");
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let findings = report["findings"].as_array().unwrap();
        let mut rules: Vec<_> = findings
            .iter()
            .map(|f| f["rule"].as_str().unwrap().to_owned())
            .collect();
        rules.sort();
        reports.push(rules);
        for finding in findings {
            assert!(finding["line"].as_u64().unwrap() > 0, "{finding}");
            if source == raw {
                assert!(
                    finding["message"].as_str().unwrap().contains("fn r#type"),
                    "{finding}"
                );
            }
        }
    }
    // Six raw findings fire (require-auth, ttl, positive-amount, transfer,
    // and two i128 additions), but the two i128 findings share a
    // file/line/column/rule and collapse, so both spellings report five.
    assert_eq!(reports[0].len(), 5, "{reports:?}");
    assert_eq!(reports[0], reports[1]);
}

#[test]
fn audit_raw_identifiers_preserve_checked_operations() {
    let fixture = AuditFixture::new("raw-identifiers-checked");
    fixture.write(
        "crates/vault/src/lib.rs",
        r#"
        impl Contract {
            fn r#type(env: Env, caller: &r#Address, r#amount: r#i128, client: Client) {
                caller.r#require_auth();
                assert!(amount > 0);
                let balance: i128 = 10;
                let _ = r#balance.r#checked_add(r#amount);
                env.r#storage().r#persistent().r#get(&"balance");
                env.r#storage().r#persistent().r#extend_ttl(&"balance", 1, 2);
                let _result = client.r#try_transfer(&caller, &caller, &amount);
            }
        }
    "#,
    );
    let output = fixture.scan(&["--strict"]);
    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["summary"]["finding_count"], 0, "{report}");
}

#[test]
fn audit_accepts_empty_files_and_stdin_in_every_format() {
    let fixture = AuditFixture::new("empty-source");
    for source in ["", " \n\t\r\n", "// no items\n/* still empty */\n"] {
        let file = fixture.write("crates/vault/src/lib.rs", source);
        for format in ["text", "json", "sarif"] {
            let outputs = [
                fixture.scan(&["--format", format, "--strict"]),
                run_from(
                    &fixture.0,
                    &[
                        "audit",
                        file.to_str().unwrap(),
                        "--format",
                        format,
                        "--strict",
                    ],
                ),
                run_with_stdin(
                    &["audit", "-", "--format", format, "--strict"],
                    source.as_bytes(),
                ),
            ];
            for output in outputs {
                assert!(output.status.success(), "{format}: {output:?}");
                assert!(output.stderr.is_empty(), "{format}: {output:?}");
                match format {
                    "text" => assert!(
                        String::from_utf8_lossy(&output.stdout).contains("audit: no findings")
                    ),
                    "json" => {
                        let report: serde_json::Value =
                            serde_json::from_slice(&output.stdout).unwrap();
                        assert_eq!(report["summary"]["finding_count"], 0);
                    }
                    "sarif" => {
                        let report: serde_json::Value =
                            serde_json::from_slice(&output.stdout).unwrap();
                        assert_eq!(report["runs"][0]["results"].as_array().unwrap().len(), 0);
                    }
                    _ => unreachable!(),
                }
            }
        }
    }
}

#[test]
fn audit_empty_file_does_not_hide_findings_in_other_files() {
    let fixture = AuditFixture::new("empty-with-findings");
    fixture.write("crates/vault/src/empty.rs", "");
    fixture.write("crates/vault/src/lib.rs", UNAUTHORIZED_ENTRY_POINT);
    let output = fixture.scan(&["--strict"]);
    assert!(!output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["summary"]["finding_count"], 1, "{report}");
    assert_eq!(report["findings"][0]["rule"], "missing-require-auth");
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("audit incomplete"),
        "{output:?}"
    );
}

#[test]
fn audit_scans_a_single_file_root() {
    let root = std::env::temp_dir().join(format!("stk-audit-single-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("only.rs");
    std::fs::write(&file, UNAUTHORIZED_ENTRY_POINT).unwrap();

    let output = run(&["audit", file.to_str().unwrap()]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "stdout: {stdout}");
    assert!(stdout.contains("only.rs"), "{stdout}");
    assert!(stdout.contains("missing-require-auth"), "{stdout}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn audit_scans_stdin_with_dash() {
    let output = run_with_stdin(&["audit", "-"], UNAUTHORIZED_ENTRY_POINT.as_bytes());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "stdout: {stdout}");
    assert!(stdout.contains("<stdin>"), "{stdout}");
    assert!(stdout.contains("missing-require-auth"), "{stdout}");
}

#[test]
fn audit_reports_a_missing_path_actionably() {
    let missing = std::env::temp_dir()
        .join("stk-audit-path-that-does-not-exist")
        .join("nope");
    let output = run(&["audit", missing.to_str().unwrap()]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("does not exist"), "{stderr}");
    assert!(stderr.contains("stdin"), "{stderr}");
}

#[test]
fn audit_json_output_reports_findings_in_a_temp_crate() {
    let root = std::env::temp_dir().join(format!("stk-audit-json-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/vault.rs"), UNAUTHORIZED_ENTRY_POINT).unwrap();

    let output = run(&["audit", root.to_str().unwrap(), "--format", "json"]);
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["tool"], "soroban-testkit");
    assert!(report["summary"]["finding_count"].as_u64().unwrap() >= 1);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn audit_sarif_output_reports_findings_and_rules() {
    let root = std::env::temp_dir().join(format!("stk-audit-sarif-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/vault.rs"), UNAUTHORIZED_ENTRY_POINT).unwrap();

    let output = run(&["audit", root.to_str().unwrap(), "--format", "sarif"]);
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["version"], "2.1.0");
    assert_eq!(
        report["runs"][0]["tool"]["driver"]["name"],
        "soroban-testkit"
    );
    let results = report["runs"][0]["results"].as_array().unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0]["ruleId"], "missing-require-auth");
    assert!(
        results[0]["locations"][0]["physicalLocation"]["artifactLocation"]["uri"]
            .as_str()
            .unwrap()
            .contains("vault.rs")
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn audit_diagnostic_formats_agree_on_finding_counts() {
    let root = std::env::temp_dir().join(format!("stk-audit-diag-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/vault.rs"), UNAUTHORIZED_ENTRY_POINT).unwrap();

    let text_out = run(&["audit", root.to_str().unwrap(), "--format", "text"]);
    let json_out = run(&["audit", root.to_str().unwrap(), "--format", "json"]);
    let sarif_out = run(&["audit", root.to_str().unwrap(), "--format", "sarif"]);

    assert!(text_out.status.success());
    assert!(json_out.status.success());
    assert!(sarif_out.status.success());

    let json_report: serde_json::Value = serde_json::from_slice(&json_out.stdout).unwrap();
    let json_count = json_report["summary"]["finding_count"].as_u64().unwrap();

    let sarif_report: serde_json::Value = serde_json::from_slice(&sarif_out.stdout).unwrap();
    let sarif_count = sarif_report["runs"][0]["results"].as_array().unwrap().len() as u64;

    assert_eq!(json_count, sarif_count);

    let text_str = String::from_utf8_lossy(&text_out.stdout);
    assert!(text_str.contains(&format!("{json_count} finding(s)")));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn audit_formats_handle_clean_crate_consistently() {
    let root = std::env::temp_dir().join(format!("stk-audit-clean-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/clean.rs"),
        "// clean code with no findings\n",
    )
    .unwrap();

    let text_out = run(&["audit", root.to_str().unwrap(), "--format", "text"]);
    let json_out = run(&["audit", root.to_str().unwrap(), "--format", "json"]);
    let sarif_out = run(&["audit", root.to_str().unwrap(), "--format", "sarif"]);

    assert!(text_out.status.success());
    assert!(json_out.status.success());
    assert!(sarif_out.status.success());

    let text_str = String::from_utf8_lossy(&text_out.stdout);
    assert!(text_str.contains("audit: no findings"));

    let json_report: serde_json::Value = serde_json::from_slice(&json_out.stdout).unwrap();
    assert_eq!(json_report["summary"]["finding_count"], 0);

    let sarif_report: serde_json::Value = serde_json::from_slice(&sarif_out.stdout).unwrap();
    assert_eq!(
        sarif_report["runs"][0]["results"].as_array().unwrap().len(),
        0
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn audit_reports_macro_generated_source_caveat_in_all_formats() {
    let source = br#"
        macro_rules! generated_vault {
            () => {
                pub fn withdraw(from: Address) { debit(from); }
            };
        }
        generated_vault!();
    "#;
    let text = run_with_stdin(&["audit", "-", "--format", "text"], source);
    assert!(text.status.success());
    let report = String::from_utf8_lossy(&text.stdout);
    assert!(report.contains("audit: no findings"), "{report}");
    assert!(report.contains("macro_rules!"), "{report}");
    assert!(report.contains("not expanded"), "{report}");

    let json = run_with_stdin(&["audit", "-", "--format", "json"], source);
    assert!(json.status.success());
    let report: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(report["summary"]["finding_count"], 0);
    assert!(report["summary"]["limitations"][0]
        .as_str()
        .unwrap()
        .contains("not expanded"));

    let sarif = run_with_stdin(&["audit", "-", "--format", "sarif"], source);
    assert!(sarif.status.success());
    let report: serde_json::Value = serde_json::from_slice(&sarif.stdout).unwrap();
    assert_eq!(report["runs"][0]["results"].as_array().unwrap().len(), 0);
    assert!(report["runs"][0]["properties"]["limitations"][0]
        .as_str()
        .unwrap()
        .contains("generated entry points"));
}

#[test]
fn audit_reports_written_contractimpl_functions_but_not_generated_bodies() {
    let text = run_with_stdin(
        &["audit", "-", "--format", "text"],
        UNAUTHORIZED_ENTRY_POINT.as_bytes(),
    );
    assert!(text.status.success());
    let report = String::from_utf8_lossy(&text.stdout);
    assert!(report.contains("missing-require-auth"), "{report}");
    assert!(report.contains("source-only scan"), "{report}");
}

#[test]
fn audit_help_explains_macro_expansion_boundary() {
    let output = run(&["audit", "--help"]);
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("macro_rules!"), "{help}");
    assert!(help.contains("generated code"), "{help}");
}

#[test]
fn audit_json_round_trips_control_characters_in_findings() {
    let fixture = AuditFixture::new("json-controls");
    // Windows forbids these characters in file names. All finding fields,
    // including the complete control range, are also covered by a unit test.
    #[cfg(unix)]
    let name = "crates/vault/src/quote\"-slash\\-tab\t-newline\n-return\r-escape\u{1b}-雪.rs";
    #[cfg(not(unix))]
    let name = "crates/vault/src/unicode-雪.rs";
    fixture.write(name, UNAUTHORIZED_ENTRY_POINT);
    fixture.write(
        ".soroban-testkit.toml",
        "[rules.missing-require-auth]\nseverity = \"custom\\t\\n\\r\\u001b\\\"\\\\雪\"\n",
    );
    let output = fixture.scan(&["--strict"]);
    assert!(!output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["findings"][0]["file"], name);
    assert_eq!(
        report["findings"][0]["severity"],
        "custom\t\n\r\u{1b}\"\\雪"
    );
    // The snippet quoted from the file body carries no control sequences of
    // its own, so it round-trips verbatim even though the file path does not.
    assert_eq!(
        report["findings"][0]["source_snippet"],
        "    pub fn withdraw(env: Env, from: Address) {"
    );
    assert_eq!(report["summary"]["finding_count"], 1);
    assert!(String::from_utf8_lossy(&output.stderr).contains("1 audit finding(s)"));
}

#[test]
fn audit_continues_after_read_and_parse_errors_in_every_format() {
    let fixture = AuditFixture::new("incomplete");
    fixture.write("a-unreadable.rs", [0xff, 0xfe]);
    fixture.write("b-invalid.rs", "fn broken( {\n");
    fixture.write("z-valid.rs", UNAUTHORIZED_ENTRY_POINT);
    for format in ["text", "json", "sarif"] {
        let output = fixture.scan(&["--format", format]);
        assert!(!output.status.success());
        let errors = String::from_utf8_lossy(&output.stderr);
        assert!(errors.contains("failed to read Rust source"), "{errors}");
        assert!(errors.contains("a-unreadable.rs"), "{errors}");
        assert!(errors.contains("failed to parse Rust source"), "{errors}");
        assert!(errors.contains("b-invalid.rs:1:"), "{errors}");
        assert!(
            errors.contains("audit incomplete: 2 parse warning(s)"),
            "{errors}"
        );
        assert!(errors.contains("audit: warning:"), "{errors}");
        let findings = String::from_utf8_lossy(&output.stdout);
        assert!(findings.contains("z-valid.rs"), "{findings}");
        assert!(findings.contains("missing-require-auth"), "{findings}");
        if format != "text" {
            serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap();
        }
    }
}

#[test]
fn audit_reports_parse_errors_for_single_files_and_stdin() {
    let fixture = AuditFixture::new("parse-errors");
    let path = fixture.write("invalid.rs", "fn broken() {\n    let = ;\n}\n");
    for output in [
        run(&["audit", path.to_str().unwrap()]),
        run_with_stdin(&["audit", "-"], b"fn broken() {\n    let = ;\n}\n"),
    ] {
        assert!(!output.status.success());
        let errors = String::from_utf8_lossy(&output.stderr);
        assert!(errors.contains("failed to parse Rust source"), "{errors}");
        assert!(errors.contains(":2:9:"), "{errors}");
        assert!(!errors.contains("failed to read"), "{errors}");
    }
}

#[cfg(unix)]
#[test]
fn audit_continues_past_permission_denied_files_and_directories() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = AuditFixture::new("permissions");
    let file = fixture.write("a-denied.rs", UNAUTHORIZED_ENTRY_POINT);
    let dir = fixture.0.join("b-denied");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("hidden.rs"), UNAUTHORIZED_ENTRY_POINT).unwrap();
    fixture.write("z-readable.rs", UNAUTHORIZED_ENTRY_POINT);
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o0)).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o0)).unwrap();
    let access_denied = std::fs::File::open(&file).is_err();
    let output = fixture.scan(&[]);
    // Restore access before any assertions so a failure still cleans up.
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    // A root test runner can read mode-000 files; exercise actual EACCES
    // when the process lacks that privilege.
    if access_denied {
        assert!(!output.status.success());
        let errors = String::from_utf8_lossy(&output.stderr);
        assert!(errors.contains("failed to read Rust source"), "{errors}");
        assert!(errors.contains("a-denied.rs"), "{errors}");
        assert!(errors.contains("failed to scan directory"), "{errors}");
        assert!(errors.contains("b-denied"), "{errors}");
        assert!(errors.contains("2 parse warning(s)"), "{errors}");
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["findings"][0]["file"], "z-readable.rs");
        assert_eq!(report["summary"]["finding_count"], 1);
    }
}

#[test]
fn audit_config_errors_distinguish_io_from_toml_syntax() {
    let fixture = AuditFixture::new("config-errors");
    fixture.write("valid.rs", UNAUTHORIZED_ENTRY_POINT);
    for (content, kind) in [
        (b"[rules".as_slice(), "parse"),
        (b"\xff".as_slice(), "read"),
    ] {
        let path = fixture.write(".soroban-testkit.toml", content);
        let output = fixture.scan(&[]);
        assert!(!output.status.success());
        let errors = String::from_utf8_lossy(&output.stderr);
        assert!(
            errors.contains(&format!("failed to {kind} audit config")),
            "{errors}"
        );
        assert!(errors.contains(path.to_str().unwrap()), "{errors}");
    }
}

#[test]
fn audit_baseline_errors_include_the_path_and_error_kind() {
    let fixture = AuditFixture::new("baseline-errors");
    for (content, kind) in [
        (Some(b"[invalid".as_slice()), "parse baseline JSON"),
        (Some(b"\xff".as_slice()), "read baseline file"),
        (None, "read baseline file"),
    ] {
        let path = fixture.0.join("baseline.json");
        if let Some(content) = content {
            std::fs::write(&path, content).unwrap();
        } else {
            std::fs::remove_file(&path).unwrap();
        }
        let output = fixture.scan(&["--baseline", "baseline.json"]);
        assert!(!output.status.success());
        let errors = String::from_utf8_lossy(&output.stderr);
        assert!(errors.contains(&format!("failed to {kind}")), "{errors}");
        assert!(errors.contains("baseline.json"), "{errors}");
    }
}

#[test]
fn audit_workspace_paths_and_config_are_independent_of_cwd_and_scan_root() {
    let fixture = AuditFixture::new("workspace-paths");
    let file = fixture.write("crates/vault/src/lib.rs", UNAUTHORIZED_ENTRY_POINT);
    fixture.write(
        "crates/vault/Cargo.toml",
        "[package]\nname = 'vault'\nversion = '0.1.0'\n",
    );
    fixture.write(
        "soroban-testkit.toml",
        "[rules.missing-require-auth]\nseverity = 'error'\n",
    );
    let expected = fixture.scan(&["--strict"]);
    assert!(!expected.status.success());
    let expected: serde_json::Value = serde_json::from_slice(&expected.stdout).unwrap();
    assert_eq!(expected["findings"][0]["file"], "crates/vault/src/lib.rs");
    assert_eq!(expected["findings"][0]["severity"], "error");

    for (dir, target) in [
        (fixture.0.clone(), "./crates/vault/src/../src"),
        (fixture.0.join("crates/vault"), "src/lib.rs"),
        (fixture.0.join("crates/vault/src"), "."),
        (std::env::temp_dir(), file.to_str().unwrap()),
    ] {
        let output = run_from(&dir, &["audit", target, "--format", "json", "--strict"]);
        assert!(!output.status.success());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
            expected
        );
    }

    // A baseline generated at the workspace root works from a member crate,
    // with both workspace-relative and absolute paths inside the baseline.
    for baseline_file in ["./crates/vault/src/../src/lib.rs", file.to_str().unwrap()] {
        fixture.write(
            "baseline.json",
            serde_json::to_vec(&serde_json::json!([{
                "file": baseline_file,
                "line": expected["findings"][0]["line"],
                "rule": "missing-require-auth"
            }]))
            .unwrap(),
        );
        let output = run_from(
            &fixture.0.join("crates/vault"),
            &[
                "audit",
                "src/lib.rs",
                "--format",
                "json",
                "--strict",
                "--baseline",
                "../../baseline.json",
            ],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["summary"]["finding_count"], 0);
    }
}

#[test]
fn audit_nearest_config_wins_without_leaking_across_project_boundaries() {
    let fixture = AuditFixture::new("config-boundaries");
    fixture.write("crates/vault/src/lib.rs", UNAUTHORIZED_ENTRY_POINT);
    fixture.write(
        ".soroban-testkit.toml",
        "[rules.missing-require-auth]\nenabled = false\n",
    );
    fixture.write("soroban-testkit.toml", "invalid TOML");
    let output = fixture.scan(&["--strict"]);
    assert!(output.status.success());
    // The nearest config overrides the workspace config, and the dotted
    // config wins over the non-dotted config within one directory.
    fixture.write(
        "crates/vault/soroban-testkit.toml",
        "[rules.missing-require-auth]\nseverity = 'error'\n",
    );
    let output = run_from(
        &fixture.0,
        &["audit", "crates/vault/src", "--format", "json"],
    );
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["findings"][0]["severity"], "error");

    // An independent nested workspace must not inherit the outer config.
    fixture.write("crates/vault/Cargo.toml", "[workspace]\nmembers = []\n");
    std::fs::remove_file(fixture.0.join("crates/vault/soroban-testkit.toml")).unwrap();
    let output = run_from(
        &fixture.0,
        &["audit", "crates/vault/src", "--format", "json"],
    );
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["findings"][0]["severity"], "warning");
    assert_eq!(report["findings"][0]["file"], "src/lib.rs");
}

#[test]
fn audit_stdin_discovers_config_from_the_current_workspace() {
    let fixture = AuditFixture::new("stdin-config");
    fixture.write(
        ".soroban-testkit.toml",
        "[rules.missing-require-auth]\nenabled = false\n",
    );
    let path = fixture.write("source.txt", UNAUTHORIZED_ENTRY_POINT);
    let output = Command::new(env!("CARGO_BIN_EXE_soroban-testkit"))
        .args(["audit", "-", "--strict", "--format", "json"])
        .current_dir(fixture.0.join("crates/vault/src"))
        .stdin(std::fs::File::open(path).unwrap())
        .output()
        .unwrap();
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["summary"]["finding_count"], 0);
}

#[test]
fn audit_strict_mode_exits_zero_when_findings_are_suppressed() {
    let root = std::env::temp_dir().join(format!(
        "stk-audit-strict-suppressed-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        r#"
        #[contractimpl]
        impl Contract {
            // soroban_testkit::audit(skip, rule = "missing-require-auth")
            pub fn withdraw(env: Env, from: Address) {
                env.storage().instance().set(&from, &1i128);
            }
        }
        "#,
    )
    .unwrap();

    let output = run(&["audit", root.to_str().unwrap(), "--strict"]);
    assert!(
        output.status.success(),
        "strict exit status should be 0 when findings are suppressed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("audit: no findings"), "{stdout}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn audit_recognizes_qualified_require_auth_calls() {
    let root = std::env::temp_dir().join(format!("stk-audit-qual-auth-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        r#"
        #[contractimpl]
        impl Contract {
            pub fn withdraw(env: Env, from: Address) {
                Address::require_auth(&from);
            }
        }
        "#,
    )
    .unwrap();

    let output = run(&["audit", root.to_str().unwrap()]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("missing-require-auth"), "{stdout}");
    assert!(stdout.contains("audit: no findings"), "{stdout}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn audit_negated_auth_branch_is_not_treated_as_enforced_in_cli() {
    let root = std::env::temp_dir().join(format!("stk-audit-negated-auth-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        r#"
        #[contractimpl]
        impl Contract {
            pub fn withdraw(env: Env, from: Address) {
                if !is_admin {
                    from.require_auth();
                }
            }
        }
        "#,
    )
    .unwrap();

    let output = run(&["audit", root.to_str().unwrap(), "--strict"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("audit finding(s)"), "{stderr}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("missing-require-auth"), "{stdout}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn audit_runs_are_byte_for_byte_deterministic_across_repeated_invocations() {
    let root = std::env::temp_dir().join(format!("stk-audit-deterministic-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/z_file.rs"),
        r#"
        #[contractimpl]
        impl ContractZ {
            pub fn withdraw(env: Env, from: Address) {}
        }
        "#,
    )
    .unwrap();
    std::fs::write(
        root.join("src/a_file.rs"),
        r#"
        #[contractimpl]
        impl ContractA {
            pub fn deposit(amount: i128) -> i128 {
                amount + 10
            }
        }
        "#,
    )
    .unwrap();

    for format in ["text", "json", "sarif"] {
        let run1 = run(&["audit", root.to_str().unwrap(), "--format", format]);
        let run2 = run(&["audit", root.to_str().unwrap(), "--format", format]);
        assert!(run1.status.success());
        assert!(run2.status.success());
        assert_eq!(
            run1.stdout, run2.stdout,
            "repeated audit run with format '{format}' must be byte-for-byte identical"
        );
    }

    let _ = std::fs::remove_dir_all(&root);
}

// ---- #481: focused fixtures, one per supported Rust syntax form ----

/// Every syntactic shape the audit visitor supports, in its own file so a
/// per-file assertion points at the exact form. Except for the test fixture,
/// every function takes an Address parameter and never calls require_auth,
/// so each trips `missing-require-auth` and only that rule.
const SYNTAX_FORM_FIXTURES: &[(&str, &str)] = &[
    (
        "form_free_item_fn.rs",
        "pub fn withdraw(env: Env, from: Address) { let _ = &from; }",
    ),
    (
        "form_const_item_fn.rs",
        "pub const fn withdraw(env: Env, from: Address) { let _ = &from; }",
    ),
    (
        "form_impl_method.rs",
        "impl Vault { pub fn withdraw(env: Env, from: Address) { let _ = &from; } }",
    ),
    (
        "form_contractimpl_method.rs",
        "#[contractimpl] impl Vault { pub fn withdraw(env: Env, from: Address) { let _ = &from; } }",
    ),
    (
        "form_trait_impl_method.rs",
        "trait Vault { fn withdraw(env: Env, from: Address); } impl Vault for Vault2 { pub fn withdraw(env: Env, from: Address) { let _ = &from; } }",
    ),
    (
        "form_nested_module_fn.rs",
        "mod treasury { pub fn withdraw(env: Env, from: Address) { let _ = &from; } }",
    ),
    (
        "form_test_item_fn.rs",
        "#[test] fn test_reset_all_auths(env: Env) { env.mock_all_auths(); }",
    ),
];

#[test]
fn audit_reports_a_finding_for_each_supported_syntax_form() {
    let fixture = AuditFixture::new("syntax-forms");
    for (name, source) in SYNTAX_FORM_FIXTURES {
        fixture.write(name, source);
    }

    let output = fixture.scan(&["--strict"]);
    assert!(!output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let findings = report["findings"].as_array().unwrap();
    assert_eq!(
        report["summary"]["finding_count"],
        SYNTAX_FORM_FIXTURES.len(),
        "{findings:?}"
    );

    for (name, source) in SYNTAX_FORM_FIXTURES {
        let expected_rule = if name.contains("test") {
            "broad-mock-all-auths"
        } else {
            "missing-require-auth"
        };
        let matching: Vec<_> = findings
            .iter()
            .filter(|finding| {
                finding["file"]
                    .as_str()
                    .is_some_and(|file| file.ends_with(name))
            })
            .collect();
        assert_eq!(matching.len(), 1, "fixture {name}: {findings:?}");
        let finding = matching[0];
        assert_eq!(finding["rule"], expected_rule, "fixture {name}");
        assert_eq!(finding["severity"], "warning", "fixture {name}");
        assert_eq!(finding["line"], 1, "fixture {name}");
        // Every fixture is a single physical line, so the reported snippet is
        // the fixture text itself.
        assert_eq!(finding["source_snippet"], *source, "fixture {name}");
    }
}

// ---- #482: findings disclose that they are heuristic, not definitive ----

#[test]
fn audit_explain_rules_document_the_heuristic_nature_of_findings() {
    for rule in [
        "missing-require-auth",
        "unchecked-i128-arithmetic",
        "missing-ttl-bump",
        "broad-mock-all-auths",
        "ignored-token-transfer-result",
        "missing-positive-amount-validation",
        "unchecked-timestamp-arithmetic",
    ] {
        let output = run(&["audit", "explain", rule]);
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains(rule), "{stdout}");
        assert!(stdout.contains("heuristic"), "{stdout}");
        assert!(
            stdout.contains("not a definitive defect report"),
            "{stdout}"
        );
    }
}

#[test]
fn audit_rules_listing_documents_the_heuristic_nature_of_findings() {
    let text = run(&["audit", "rules", "--format", "text"]);
    assert!(text.status.success());
    let stdout = String::from_utf8_lossy(&text.stdout);
    assert!(stdout.contains("missing-require-auth"), "{stdout}");
    assert!(stdout.contains("note: heuristic"), "{stdout}");

    let json = run(&["audit", "rules", "--format", "json"]);
    assert!(json.status.success());
    let report: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let limitations = report["summary"]["limitations"].as_array().unwrap();
    assert!(
        limitations
            .iter()
            .any(|l| l.as_str().unwrap().contains("heuristic")),
        "{limitations:?}"
    );
}

// ---- #483: rule severity mapping is consistent across output formats ----

#[test]
fn audit_sarif_levels_match_the_severity_shown_in_text_and_json() {
    let fixture = AuditFixture::new("severity-consistency");
    fixture.write("crates/vault/src/lib.rs", UNAUTHORIZED_ENTRY_POINT);
    for (configured, sarif_level) in [("warning", "warning"), ("error", "error"), ("note", "note")]
    {
        fixture.write(
            ".soroban-testkit.toml",
            format!("[rules.missing-require-auth]\nseverity = '{configured}'\n"),
        );
        let text = fixture.scan(&["--format", "text"]);
        assert!(text.status.success());
        let stdout = String::from_utf8_lossy(&text.stdout);
        assert!(stdout.contains(&format!("[{configured}]")), "{stdout}");

        let json = fixture.scan(&["--format", "json"]);
        assert!(json.status.success());
        let report: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
        assert_eq!(report["findings"][0]["severity"], configured, "{report}");

        let sarif = fixture.scan(&["--format", "sarif"]);
        assert!(sarif.status.success());
        let report: serde_json::Value = serde_json::from_slice(&sarif.stdout).unwrap();
        assert_eq!(
            report["runs"][0]["results"][0]["level"], sarif_level,
            "{report}"
        );
        assert_eq!(
            report["runs"][0]["tool"]["driver"]["rules"][0]["defaultConfiguration"]["level"],
            sarif_level,
            "{report}"
        );
    }
}

// ---- #484: source snippets without terminal control sequences ----

#[test]
fn audit_reports_source_snippets_without_leaking_terminal_control_sequences() {
    let fixture = AuditFixture::new("snippet-controls");
    fixture.write(
        "crates/vault/src/lib.rs",
        "pub fn withdraw(env: Env, from: Address) { let _ = &from; } // \u{1b}[31mred\u{1b}[0m",
    );

    let text = fixture.scan(&["--format", "text"]);
    assert!(text.status.success());
    let stdout = String::from_utf8_lossy(&text.stdout);
    assert!(
        stdout
            .contains("    --> pub fn withdraw(env: Env, from: Address) { let _ = &from; } // red"),
        "{stdout}"
    );
    assert!(!stdout.contains('\u{1b}'), "{stdout}");

    let json = fixture.scan(&["--format", "json"]);
    assert!(json.status.success());
    let report: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(
        report["findings"][0]["source_snippet"],
        "pub fn withdraw(env: Env, from: Address) { let _ = &from; } // red"
    );

    let sarif = fixture.scan(&["--format", "sarif"]);
    assert!(sarif.status.success());
    let report: serde_json::Value = serde_json::from_slice(&sarif.stdout).unwrap();
    assert_eq!(
        report["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["region"]["snippet"]
            ["text"],
        "pub fn withdraw(env: Env, from: Address) { let _ = &from; } // red"
    );
}

// ---- #485: findings sort by file, line, rule, and message ----

#[test]
fn audit_orders_findings_by_rule_rather_than_scan_order_or_column() {
    let fixture = AuditFixture::new("finding-order");
    // Both functions sit on one physical line: `alpha` starts at column 1
    // and is visited first, but `missing-positive-amount-validation` sorts
    // before `missing-require-auth`, so the report follows the rule — not
    // the scan order or the column.
    fixture.write(
        "crates/vault/src/lib.rs",
        "pub fn alpha(env: Env, from: Address) { let _ = &from; } \
         pub fn beta(amount: i128) { save(amount); }\n",
    );

    let first = fixture.scan(&["--strict"]);
    assert!(!first.status.success(), "{first:?}");
    let report: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(report["summary"]["finding_count"], 2, "{report}");
    let rules: Vec<_> = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|finding| finding["rule"].as_str().unwrap())
        .collect();
    assert_eq!(
        rules,
        vec!["missing-positive-amount-validation", "missing-require-auth"],
        "{report}"
    );

    // The order is deterministic across runs.
    let second = fixture.scan(&["--strict"]);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&second.stdout).unwrap(),
        report
    );
}

// ---- #488: one canonical path normalization routine ----

#[test]
fn audit_normalizes_equivalent_spellings_of_one_scan_root() {
    let fixture = AuditFixture::new("canonical-roots");
    fixture.write("crates/vault/src/lib.rs", UNAUTHORIZED_ENTRY_POINT);

    let mut reports = Vec::new();
    for root in [
        "crates/vault/src/lib.rs",
        "./crates/vault/./src/../src/lib.rs",
        "crates/vault/src/../../vault/src/lib.rs",
    ] {
        let output = run_from(&fixture.0, &["audit", root, "--format", "json"]);
        assert!(output.status.success(), "root {root}: {output:?}");
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            report["findings"][0]["file"], "crates/vault/src/lib.rs",
            "root {root}: {report}"
        );
        reports.push(report);
    }
    assert_eq!(reports[0], reports[1]);
    assert_eq!(reports[1], reports[2]);
}

// ---- #487: a parse warning is not a finding ----

#[test]
fn audit_reports_parse_warnings_on_stderr_and_findings_in_the_report() {
    let fixture = AuditFixture::new("finding-vs-warning");
    fixture.write("broken.rs", "fn broken( {\n");
    fixture.write("good.rs", UNAUTHORIZED_ENTRY_POINT);

    let output = fixture.scan(&["--format", "json", "--strict"]);
    assert!(!output.status.success(), "{output:?}");

    // The report counts only parsed source; the skipped file never becomes
    // a finding.
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["summary"]["finding_count"], 1, "{report}");
    let files: Vec<_> = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|finding| finding["file"].as_str().unwrap())
        .collect();
    assert_eq!(files, vec!["good.rs"], "{report}");

    // The warning lives only on stderr, labelled as a warning, with the
    // finding/warning distinction stated alongside it.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("audit: warning: failed to parse Rust source"),
        "{stderr}"
    );
    assert!(
        stderr.contains("parse warnings are not findings"),
        "{stderr}"
    );
    assert!(
        stderr.contains("audit incomplete: 1 parse warning(s)"),
        "{stderr}"
    );
    // The incomplete audit — not --strict — is what failed this run.
    assert!(!stderr.contains("(--strict)"), "{stderr}");
}

#[test]
fn audit_parse_warnings_fail_the_run_without_acting_like_strict_findings() {
    let fixture = AuditFixture::new("warnings-not-findings");
    fixture.write("clean.rs", "// nothing to scan\n");
    fixture.write("broken.rs", "fn broken( {\n");

    let output = fixture.scan(&["--format", "json", "--strict"]);
    assert!(!output.status.success(), "{output:?}");

    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["summary"]["finding_count"], 0, "{report}");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("audit incomplete: 1 parse warning(s)"),
        "{stderr}"
    );
    assert!(
        stderr.contains("parse warnings are not findings"),
        "{stderr}"
    );
    // No heuristic finding existed, so the strict-failure wording must not
    // appear: parse warnings never reach --strict.
    assert!(!stderr.contains("audit finding(s)"), "{stderr}");
}

#[test]
fn audit_strict_failure_reports_findings_and_no_parse_warnings() {
    let fixture = AuditFixture::new("strict-findings-only");
    fixture.write("crates/vault/src/lib.rs", UNAUTHORIZED_ENTRY_POINT);

    let output = fixture.scan(&["--format", "json", "--strict"]);
    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("1 audit finding(s) (--strict)"), "{stderr}");
    assert!(!stderr.contains("parse warning"), "{stderr}");
    assert!(!stderr.contains("audit: warning:"), "{stderr}");
}

// ---- #486: regression snapshots for strict and report-only modes ----

/// The exact report both modes must print. `--strict` changes only the exit
/// status and the error on stderr — never a byte of the report itself.
const FINDINGS_REPORT_SNAPSHOT: &str = include_str!("snapshots/audit_findings_report.txt");
const CLEAN_REPORT_SNAPSHOT: &str = include_str!("snapshots/audit_clean_report.txt");
const STRICT_ERROR_SNAPSHOT: &str = include_str!("snapshots/audit_strict_error.txt");

/// One physical line, exactly one finding: the source both mode snapshots
/// scan, so the snapshot pins report shape rather than scan ordering.
const SNAPSHOT_ENTRY_POINT: &str = "pub fn withdraw(env: Env, from: Address) { let _ = &from; }\n";

#[test]
fn audit_report_only_mode_matches_its_regression_snapshot() {
    let fixture = AuditFixture::new("snapshot-report-only");
    fixture.write("crates/vault/src/lib.rs", SNAPSHOT_ENTRY_POINT);

    let output = fixture.scan(&["--format", "text"]);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        FINDINGS_REPORT_SNAPSHOT,
        "report-only snapshot changed"
    );
}

#[test]
fn audit_strict_mode_matches_its_regression_snapshot() {
    let fixture = AuditFixture::new("snapshot-strict");
    fixture.write("crates/vault/src/lib.rs", SNAPSHOT_ENTRY_POINT);

    let output = fixture.scan(&["--format", "text", "--strict"]);
    assert!(!output.status.success(), "{output:?}");
    // Strict fails the run without changing a single byte of the report.
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        FINDINGS_REPORT_SNAPSHOT,
        "strict-mode report snapshot changed"
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        STRICT_ERROR_SNAPSHOT,
        "strict-mode error snapshot changed"
    );
}

#[test]
fn audit_clean_report_is_snapshotted_in_both_modes() {
    let fixture = AuditFixture::new("snapshot-clean");
    fixture.write("crates/vault/src/lib.rs", "// nothing to flag\n");
    let modes: &[&[&str]] = &[&["--format", "text"], &["--format", "text", "--strict"]];
    for args in modes {
        let output = fixture.scan(args);
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            CLEAN_REPORT_SNAPSHOT,
            "clean-report snapshot changed for {args:?}"
        );
    }
}

// #436: Unix paths may contain non-UTF-8 bytes; text output must be legible.
#[cfg(unix)]
#[test]
fn audit_text_escapes_invalid_utf8_path_without_loss() {
    use std::os::unix::ffi::OsStringExt;
    let root = std::env::temp_dir().join(format!("stk-audit-invalid-path-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let filename = std::ffi::OsString::from_vec(b"invalid-\xff.rs".to_vec());
    let path = root.join(filename);
    std::fs::write(&path, UNAUTHORIZED_ENTRY_POINT).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_soroban-testkit"))
        .arg("audit")
        .arg(&path)
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success(), "{stdout}");
    assert!(
        stdout.to_ascii_lowercase().contains(r"\xff"),
        "expected escaped original byte: {stdout}"
    );
    assert!(!stdout.contains('\u{fffd}'), "path was lossy: {stdout}");
    assert!(stdout.contains("missing-require-auth"), "{stdout}");
    let _ = std::fs::remove_dir_all(&root);
}

// #438: show file, table/key and line for a wrongly-typed rule option.
#[test]
fn audit_config_error_names_path_and_setting() {
    let root = std::env::temp_dir().join(format!("stk-audit-bad-config-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("src.rs"), UNAUTHORIZED_ENTRY_POINT).unwrap();
    std::fs::write(
        root.join(".soroban-testkit.toml"),
        "[rules.missing-require-auth]\nenabled = 'definitely'\n",
    )
    .unwrap();
    let output = run(&["audit", root.to_str().unwrap()]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(".soroban-testkit.toml"), "{stderr}");
    assert!(stderr.contains("enabled"), "{stderr}");
    assert!(stderr.contains("line 2"), "{stderr}");
    let _ = std::fs::remove_dir_all(&root);
}

// #432: helper methods compiled only in tests must not raise production warnings.
#[test]
fn audit_skips_test_only_helpers_but_checks_production_methods() {
    let source = r#"
struct Vault;
impl Vault {
    pub fn live(&self, from: Address) { let _ = from; }
}
#[cfg(test)]
mod tests {
    pub fn helper(from: Address) { let _ = from; }
    struct Helpers;
    impl Helpers { pub fn internal(&self, from: Address) { let _ = from; } }
}
#[cfg(test)]
pub fn another_helper(from: Address) { let _ = from; }
#[cfg(not(test))]
pub fn production_only(from: Address) { let _ = from; }
#[cfg(any(test, feature = "logging"))]
pub fn also_production(from: Address) { let _ = from; }
#[cfg(all(test, feature = "logging"))]
pub fn helper_with_feature(from: Address) { let _ = from; }
"#;
    let output = run_with_stdin(&["audit", "-", "--format", "json"], source.as_bytes());
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let findings = report["findings"].as_array().unwrap();
    assert!(findings
        .iter()
        .any(|finding| finding["message"].as_str().unwrap().contains("live")));
    assert!(findings.iter().any(|finding| finding["message"]
        .as_str()
        .unwrap()
        .contains("production_only")));
    assert!(findings.iter().any(|finding| finding["message"]
        .as_str()
        .unwrap()
        .contains("also_production")));
    assert!(!findings.iter().any(|finding| {
        let msg = finding["message"].as_str().unwrap();
        msg.contains("helper") || msg.contains("internal")
    }));
}

#[test]
fn audit_does_not_treat_integration_test_files_as_production() {
    let root = std::env::temp_dir().join(format!("stk-audit-test-tree-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("tests")).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("tests/helpers.rs"),
        "fn helper(from: Address) { let _ = from; }",
    )
    .unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        "fn live(from: Address) { let _ = from; }",
    )
    .unwrap();
    let output = run(&["audit", root.to_str().unwrap(), "--format", "json"]);
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let findings = report["findings"].as_array().unwrap();
    assert!(findings
        .iter()
        .any(|finding| finding["file"].as_str().unwrap().ends_with("src/lib.rs")));
    assert!(!findings.iter().any(|finding| finding["file"]
        .as_str()
        .unwrap()
        .ends_with("tests/helpers.rs")));
    let _ = std::fs::remove_dir_all(&root);
}

// ---- #419: no false positives for auth checks delegated to helper functions ----

/// An entry point that delegates `require_auth` to a private helper must not
/// be flagged as missing auth.  This is the most common pattern for sharing
/// auth logic across multiple entry points in a contract.
#[test]
fn audit_does_not_flag_entry_points_that_delegate_auth_to_a_helper() {
    let source = br#"
fn check_auth(caller: &Address) {
    caller.require_auth();
}

#[contractimpl]
impl Vault {
    pub fn withdraw(env: Env, from: Address) {
        check_auth(&from);
        let balance: i128 = env.storage().instance().get(&"bal").unwrap_or(0);
        env.storage().instance().set(&"bal", &(balance - 1i128));
    }
}
"#;
    let output = run_with_stdin(&["audit", "-", "--format", "json", "--strict"], source);
    // strict must not fail — the entry point is auth-protected via helper
    assert!(
        output.status.success(),
        "false positive: delegated auth helper still triggered missing-require-auth; \
         stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let auth_findings: Vec<_> = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["rule"] == "missing-require-auth")
        .collect();
    assert!(
        auth_findings.is_empty(),
        "unexpected missing-require-auth findings: {auth_findings:?}"
    );
}

/// Various naming conventions for auth helpers — all should suppress the finding.
#[test]
fn audit_recognises_multiple_auth_helper_naming_conventions() {
    for (helper_decl, helper_call) in &[
        (
            "fn verify_auth(a: &Address) { a.require_auth(); }",
            "verify_auth(&from);",
        ),
        (
            "fn assert_authorized(a: &Address) { a.require_auth(); }",
            "assert_authorized(&from);",
        ),
        (
            "fn require_caller(a: &Address) { a.require_auth(); }",
            "require_caller(&from);",
        ),
    ] {
        let source = format!(
            r#"
{helper_decl}
#[contractimpl]
impl Vault {{
    pub fn withdraw(env: Env, from: Address) {{
        {helper_call}
    }}
}}
"#
        );
        let output = run_with_stdin(
            &["audit", "-", "--format", "json", "--strict"],
            source.as_bytes(),
        );
        assert!(
            output.status.success(),
            "false positive for helper `{helper_call}`; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let auth_findings: Vec<_> = report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["rule"] == "missing-require-auth")
            .collect();
        assert!(
            auth_findings.is_empty(),
            "unexpected findings for helper `{helper_call}`: {auth_findings:?}"
        );
    }
}

/// An unrelated helper (not auth-semantics) must not suppress the finding.
#[test]
fn audit_still_flags_entry_points_when_only_unrelated_helpers_are_called() {
    let source = br#"
fn process_payment(from: &Address) { /* no auth */ }

#[contractimpl]
impl Vault {
    pub fn withdraw(env: Env, from: Address) {
        process_payment(&from);
    }
}
"#;
    let output = run_with_stdin(&["audit", "-", "--format", "json"], source);
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let auth_findings: Vec<_> = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["rule"] == "missing-require-auth")
        .collect();
    assert!(
        !auth_findings.is_empty(),
        "missing-require-auth should fire when only an unrelated helper is called"
    );
}
