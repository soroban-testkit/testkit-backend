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
    assert_eq!(reports[0].len(), 6, "{reports:?}");
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
            errors.contains("audit incomplete: 2 scan error(s)"),
            "{errors}"
        );
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
        assert!(errors.contains("2 scan error(s)"), "{errors}");
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
