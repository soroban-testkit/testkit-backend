use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand, ValueEnum};
use quote::ToTokens;
use serde::{Deserialize, Serialize};
use syn::visit::{self, Visit};
use syn::{BinOp, Expr, FnArg, ImplItemFn, ItemFn, Local, Pat, Signature, Type, UnOp};
use walkdir::WalkDir;

use super::CliError;

/// This CLI scans written Rust syntax without expanding procedural or
/// declarative macros. Generated code requires separate inspection.
pub const MACRO_SOURCE_LIMITATION: &str =
    "source-only scan: procedural macros and macro_rules! are not expanded; generated entry points and other generated code require separate inspection";

/// Arguments for `soroban-testkit audit`.
#[derive(Args)]
pub struct AuditArgs {
    #[command(subcommand)]
    command: Option<AuditCommand>,
    /// Path to the contract crate to audit, or `-` to scan Rust source from
    /// stdin.
    ///
    /// Supported scan roots:
    /// - a directory: every `.rs` file beneath it is scanned recursively;
    /// - a single `.rs` file;
    /// - `-`: read Rust source from stdin.
    ///
    /// While scanning a directory these are excluded (skipped, never
    /// descended into): `target/`, `node_modules/`, and any hidden directory
    /// (its name begins with `.`, e.g. `.git/`, `.github/`, `.cargo/`). A
    /// missing path is reported as an actionable error rather than an empty
    /// "no findings" result.
    /// Findings and baseline entries use workspace-relative paths. Config is
    /// discovered from the source directory up to the workspace root (or
    /// from the current directory for stdin). Files that cannot be read or
    /// parsed are reported on stderr; other files are scanned, then the
    /// command exits non-zero to indicate an incomplete audit.
    ///
    /// This static scan does not expand procedural macros or macro_rules!
    /// invocations. Inspect generated code separately (e.g. cargo expand).
    #[arg(value_name = "PATH", default_value = ".", global = true)]
    path: PathBuf,
    /// Exit non-zero if any findings are reported.
    #[arg(long, global = true)]
    strict: bool,
    /// Output format. JSON and SARIF are intended for automation and code scanning.
    #[arg(long, value_enum, default_value_t = AuditOutputFormat::Text, global = true)]
    format: AuditOutputFormat,
    /// Path to a baseline file containing findings to ignore, relative to
    /// the current directory. Paths inside it are relative to the workspace.
    #[arg(long, global = true)]
    baseline: Option<PathBuf>,
}

#[derive(Subcommand)]
enum AuditCommand {
    /// Explain a specific audit rule.
    Explain {
        /// Rule ID to explain.
        rule: String,
    },
    /// List all rules with their descriptions in machine-readable format.
    Rules,
}

/// Machine- and human-readable audit output formats.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum AuditOutputFormat {
    /// Human-readable compiler-style diagnostics.
    #[default]
    Text,
    /// A stable JSON report containing all findings.
    Json,
    /// SARIF 2.1.0 for GitHub and other code-scanning systems.
    Sarif,
}

/// Configuration for audit rules, loaded from `.soroban-testkit.toml` or
/// `soroban-testkit.toml` in the repository root.
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct AuditConfig {
    #[serde(default)]
    pub rules: HashMap<String, RuleConfig>,
}

/// Per-rule configuration.
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct RuleConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub severity: Option<String>,
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Finding {
    pub file: PathBuf,
    pub line: usize,
    pub rule: &'static str,
    pub severity: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct BaselineEntry {
    file: String,
    line: usize,
    rule: String,
}

impl Finding {
    fn matches_baseline(&self, entry: &BaselineEntry) -> bool {
        let file_str = self.file.to_string_lossy();
        let entry_file = entry.file.as_str();
        let file_matches = file_str == entry_file
            || file_str.strip_prefix("./").unwrap_or(&file_str)
                == entry_file.strip_prefix("./").unwrap_or(entry_file);

        file_matches && self.line == entry.line && self.rule == entry.rule
    }
}

/// Static checks over a contract crate. **Not a security product** — a
/// linter with seven heuristics, each independently useful and each
/// capable of missing real bugs or flagging non-bugs. The absence of a
/// finding here is never a security guarantee.
///
/// 1. Entry points taking an `Address` parameter that never call
///    `require_auth` anywhere in the function body.
/// 2. Arithmetic (`+ - * /`) on an explicitly `i128`-typed binding, done
///    outside a `checked_`/`saturating_`/`wrapping_` call.
/// 3. A `.persistent()`/`.temporary()` storage read with no
///    `.extend_ttl(` call anywhere in the same function.
/// 4. Broad use of `mock_all_auths` in test functions.
/// 5. Token transfer calls whose return value is explicitly or implicitly ignored.
/// 6. Signed `amount` parameters with no comparison against zero.
/// 7. Unchecked timestamp arithmetic operations that could overflow.
///
/// # Scan roots and exclusions
///
/// The supported scan roots are documented on `AuditArgs::path`: a
/// directory is walked recursively for `.rs` files, a single `.rs` file is
/// scanned directly, and `-` reads Rust source from stdin. Directory walks
/// prune `target/`, `node_modules/`, and every hidden directory (a name
/// beginning with `.`, such as `.git/` or `.github/`) via
/// `collect_rust_sources`, so build output and vendored/tooling trees are
/// never audited. A non-existent root returns an actionable error naming the
/// supported inputs instead of silently reporting "no findings".
///
/// This source-only scan cannot inspect code generated by procedural macros
/// or macro_rules!. Text, JSON and SARIF reports disclose that limitation.
pub fn run(args: AuditArgs) -> Result<(), CliError> {
    match &args.command {
        Some(AuditCommand::Explain { rule }) => return explain_rule(rule),
        Some(AuditCommand::Rules) => return list_rules(args.format),
        None => {}
    }

    let stdin = args.path == Path::new("-");
    let mut scan_errors = Vec::new();
    let sources = if stdin {
        Vec::new()
    } else {
        collect_rust_sources(&args.path, &mut scan_errors)?
    };
    let source_dir = if stdin {
        std::env::current_dir()?
    } else {
        let root = fs::canonicalize(&args.path).map_err(|err| {
            CliError(format!(
                "failed to resolve scan path {}: {err}",
                args.path.display()
            ))
        })?;
        if root.is_dir() {
            root
        } else {
            root.parent().unwrap_or(&root).to_path_buf()
        }
    };
    let workspace = find_workspace_root(&source_dir);
    let config = load_config(&source_dir, &workspace)?;
    let mut baseline = load_baseline(&args.baseline)?;
    for entry in &mut baseline {
        entry.file = workspace_relative_path(&workspace.join(&entry.file), &workspace)
            .to_string_lossy()
            .into_owned();
    }
    let mut findings = Vec::new();

    if stdin {
        let mut source = String::new();
        std::io::stdin()
            .read_to_string(&mut source)
            .map_err(|err| CliError(format!("failed to read stdin: {err}")))?;
        if let Err(err) = audit_source(
            Path::new("<stdin>"),
            &source,
            &mut findings,
            &config,
            &source,
        ) {
            scan_errors.push(err);
        }
    } else {
        for path in sources {
            if let Err(err) = audit_file(&path, &mut findings, &config) {
                scan_errors.push(err);
            }
        }
        for finding in &mut findings {
            finding.file = workspace_relative_path(&finding.file, &workspace);
        }
    }

    findings.retain(|f| !baseline.iter().any(|b| f.matches_baseline(b)));
    findings.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then_with(|| a.line.cmp(&b.line))
            .then_with(|| a.rule.cmp(b.rule))
            .then_with(|| a.message.cmp(&b.message))
    });

    print!("{}", render_findings(&findings, args.format)?);

    if !scan_errors.is_empty() {
        for error in &scan_errors {
            eprintln!("audit: {error}");
        }
        return Err(CliError(format!(
            "audit incomplete: {} scan error(s); findings cover only successfully parsed files; \
             fix the reported paths and re-run",
            scan_errors.len()
        )));
    }

    if args.strict && !findings.is_empty() {
        return Err(CliError(format!(
            "{} audit finding(s) (--strict)",
            findings.len()
        )));
    }
    Ok(())
}

/// Directories excluded from a workspace scan regardless of where they
/// appear. Hidden directories (names beginning with `.`) are excluded as
/// well, by `is_scannable_entry`.
///
/// These are build output and dependency/tooling trees that either generated
/// non-contract Rust or would just slow the scan down; the audit is meant to
/// run on hand-written contract source.
pub const AUDIT_EXCLUDED_DIRS: &[&str] = &["target", "node_modules"];

/// Prunes excluded directories during a walk. Returns `true` for the user's
/// chosen root (even if it happens to share a name with an excluded
/// directory) and for every non-directory entry; returns `false` for
/// excluded directories so [`WalkDir`] never descends into them.
fn is_scannable_entry(entry: &walkdir::DirEntry) -> bool {
    if !entry.file_type().is_dir() {
        return true;
    }
    // Never prune the root the user explicitly asked us to scan.
    if entry.depth() == 0 {
        return true;
    }
    let file_name = entry.file_name();
    let name = file_name.to_string_lossy();
    let name: &str = &name;
    !(name.starts_with('.') || AUDIT_EXCLUDED_DIRS.contains(&name))
}

/// Walks `root` and returns the `.rs` files that should be audited, applying
/// the exclusions documented on `AuditArgs::path`.
///
/// A root that does not exist is an error, not an empty scan: silently
/// reporting "no findings" for a typo'd path would be worse than useless in
/// CI, so the message names every supported input form.
fn collect_rust_sources(root: &Path, errors: &mut Vec<CliError>) -> Result<Vec<PathBuf>, CliError> {
    fs::metadata(root).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            CliError(format!(
                "path {} does not exist; pass a contract crate directory, a single .rs file, \
                 or `-` to read Rust source from stdin",
                root.display()
            ))
        } else {
            CliError(format!(
                "failed to access scan path {}: {err}; check path permissions and re-run",
                root.display()
            ))
        }
    })?;

    let mut sources = Vec::new();
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(is_scannable_entry)
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                errors.push(CliError(format!(
                    "failed to scan directory {}: {err}; check path permissions and re-run",
                    err.path().unwrap_or(root).display()
                )));
                continue;
            }
        };
        let is_rust = entry.path().extension().is_some_and(|ext| ext == "rs");
        if entry.file_type().is_file() && is_rust {
            sources.push(entry.path().to_path_buf());
        }
    }
    sources.sort();
    Ok(sources)
}

fn explain_rule(rule: &str) -> Result<(), CliError> {
    let explanation = match rule {
        "missing-require-auth" => {
            "missing-require-auth: Entry points that accept an Address parameter must validate the caller.\n\
             This rule detects functions with Address parameters that never call require_auth().\n\
             Fix: Call address.require_auth() to ensure the caller is authorized.\n\
             Reference: https://developers.stellar.org/docs/build/smart-contracts/guides/security"
        }
        "unchecked-i128-arithmetic" => {
            "unchecked-i128-arithmetic: Arithmetic on i128 values can silently overflow.\n\
             This rule detects + - * / operations on i128 values outside of checked/saturating/wrapping variants.\n\
             Fix: Use .checked_add(), .saturating_add(), or .wrapping_add() for safe overflow handling.\n\
             Reference: https://doc.rust-lang.org/std/primitive.i128.html"
        }
        "missing-ttl-bump" => {
            "missing-ttl-bump: Storage reads that are not followed by TTL extension can expire.\n\
             This rule detects .persistent() or .temporary() reads without .extend_ttl() in the same function.\n\
             Fix: Call .extend_ttl() after reading from storage to prevent premature expiration.\n\
             Reference: https://developers.stellar.org/docs/smart-contracts/storing-data"
        }
        "broad-mock-all-auths" => {
            "broad-mock-all-auths: Test functions using mock_all_auths() bypass auth checks entirely.\n\
             This rule detects mock_all_auths in test functions.\n\
             Fix: Replace with granular auth checks for specific callers to ensure auth is properly tested.\n\
             Reference: https://developers.stellar.org/docs/build/smart-contracts/guides/testing"
        }
        "ignored-token-transfer-result" => {
            "ignored-token-transfer-result: Token transfer operations can fail and their results must be handled.\n\
             This rule detects transfer/transfer_from/try_transfer calls whose results are not used.\n\
             Fix: Propagate the result with ?, match on it, or explicitly bind it (let _ = ...).\n\
             Reference: https://developers.stellar.org/docs/build/smart-contracts/guides/tokens"
        }
        "missing-positive-amount-validation" => {
            "missing-positive-amount-validation: Signed amount parameters should be validated.\n\
             This rule detects amount parameters (signed integers) with no comparison against zero.\n\
             Fix: Add a check like assert!(amount > 0) to validate amounts before use.\n\
             Reference: https://developers.stellar.org/docs/build/smart-contracts/guides/security"
        }
        "unchecked-timestamp-arithmetic" => {
            "unchecked-timestamp-arithmetic: Timestamp arithmetic can overflow and cause silent failures.\n\
             This rule detects + - * / operations on timestamp values without overflow protection.\n\
             Fix: Use checked arithmetic or ensure your timestamps are bounded to valid ranges.\n\
             Reference: https://developers.stellar.org/docs/build/smart-contracts/guides/time"
        }
        _ => return Err(CliError(format!("unknown rule: {}", rule))),
    };
    println!("{}", explanation);
    Ok(())
}

/// Static metadata for all audit rules.
const RULES: &[(&str, &str, &str)] = &[
    ("missing-require-auth", "warning", "Entry points that accept an Address parameter must validate the caller with require_auth()."),
    ("unchecked-i128-arithmetic", "warning", "Arithmetic on i128 values can silently overflow; use checked/saturating/wrapping variants."),
    ("missing-ttl-bump", "warning", "Storage reads without extend_ttl can expire prematurely."),
    ("broad-mock-all-auths", "warning", "Test functions using mock_all_auths() bypass auth checks entirely."),
    ("ignored-token-transfer-result", "warning", "Token transfer operations can fail; their results must be handled."),
    ("missing-positive-amount-validation", "warning", "Signed amount parameters should be validated against zero."),
    ("unchecked-timestamp-arithmetic", "warning", "Timestamp arithmetic can overflow and cause silent failures."),
];

fn list_rules(format: AuditOutputFormat) -> Result<(), CliError> {
    match format {
        AuditOutputFormat::Text => {
            println!("{:<40} {:<10} DESCRIPTION", "RULE", "SEVERITY");
            println!("{}", "-".repeat(100));
            for (rule, severity, desc) in RULES {
                println!("{rule:<40} {severity:<10} {desc}");
            }
            Ok(())
        }
        AuditOutputFormat::Json => {
            let rules: Vec<_> = RULES
                .iter()
                .map(|(id, severity, description)| {
                    serde_json::json!({
                        "id": id,
                        "severity": severity,
                        "description": description,
                    })
                })
                .collect();
            let report = serde_json::json!({
                "tool": "soroban-testkit",
                "version": 1,
                "rules": rules,
                "summary": { "rule_count": rules.len() },
            });
            println!("{}", serde_json::to_string_pretty(&report).unwrap());
            Ok(())
        }
        AuditOutputFormat::Sarif => {
            let rules: Vec<_> = RULES
                .iter()
                .map(|(id, severity, description)| {
                    serde_json::json!({
                        "id": id,
                        "shortDescription": { "text": id.replace('-', " ") },
                        "fullDescription": { "text": description },
                        "defaultConfiguration": {
                            "level": match *severity {
                                "error" => "error",
                                _ => "warning",
                            }
                        },
                    })
                })
                .collect();
            let report = serde_json::json!({
                "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
                "version": "2.1.0",
                "runs": [{
                    "tool": { "driver": { "name": "soroban-testkit", "rules": rules } },
                    "results": []
                }]
            });
            println!("{}", serde_json::to_string_pretty(&report).unwrap());
            Ok(())
        }
    }
}

/// Centralized diagnostic renderer across Text, JSON, and SARIF formats.
pub struct DiagnosticRenderer<'a> {
    findings: &'a [Finding],
}

impl<'a> DiagnosticRenderer<'a> {
    /// Create a new diagnostic renderer for the given findings.
    pub fn new(findings: &'a [Finding]) -> Self {
        Self { findings }
    }

    /// Render diagnostics according to the specified output format.
    pub fn render(&self, format: AuditOutputFormat) -> Result<String, CliError> {
        match format {
            AuditOutputFormat::Text => Ok(render_text(self.findings)),
            AuditOutputFormat::Json => render_json(self.findings),
            AuditOutputFormat::Sarif => render_sarif(self.findings),
        }
    }

    fn sorted_findings(&self) -> Vec<&Finding> {
        let mut sorted: Vec<&Finding> = self.findings.iter().collect();
        sorted.sort_by(|a, b| {
            a.file
                .cmp(&b.file)
                .then_with(|| a.line.cmp(&b.line))
                .then_with(|| a.rule.cmp(b.rule))
                .then_with(|| a.message.cmp(&b.message))
        });
        sorted
    }

    /// Render compiler-style human-readable text diagnostics.
    pub fn render_text(&self) -> String {
        if self.findings.is_empty() {
            return format!("audit: no findings\nnote: {MACRO_SOURCE_LIMITATION}\n");
        }

        let sorted = self.sorted_findings();
        let mut output = String::new();
        for finding in sorted {
            output.push_str(&format!(
                "{}:{}: [{}] {}: {}\n",
                finding.file.display(),
                finding.line,
                finding.severity,
                finding.rule,
                finding.message
            ));
        }
        output.push_str(&format!(
            "\n{} finding(s). soroban-testkit audit is a linter with seven heuristics, not a \
             security product — a missing finding is not a security guarantee.\n",
            self.findings.len()
        ));
        output.push_str(&format!("note: {MACRO_SOURCE_LIMITATION}\n"));
        output
    }

    /// Render stable, machine-readable JSON report.
    pub fn render_json(&self) -> Result<String, CliError> {
        let sorted = self.sorted_findings();
        serde_json::to_string_pretty(&serde_json::json!({
            "tool": "soroban-testkit",
            "version": 1,
            "findings": sorted,
            "summary": {
                "finding_count": sorted.len(),
                "limitations": [MACRO_SOURCE_LIMITATION]
            }
        }))
        .map(|report| format!("{report}\n"))
        .map_err(|err| CliError(format!("failed to serialize JSON audit report: {err}")))
    }

    /// Render SARIF 2.1.0 output for code scanning integration.
    pub fn render_sarif(&self) -> Result<String, CliError> {
        let sorted = self.sorted_findings();
        let mut rule_ids: Vec<_> = sorted.iter().map(|finding| finding.rule).collect();
        rule_ids.sort_unstable();
        rule_ids.dedup();
        let rules: Vec<_> = rule_ids
            .into_iter()
            .map(|rule| {
                let description = RULES
                    .iter()
                    .find(|(id, _, _)| *id == rule)
                    .map(|(_, _, desc)| *desc);
                let mut rule_obj = serde_json::json!({
                    "id": rule,
                    "shortDescription": { "text": rule.replace('-', " ") }
                });
                if let Some(desc) = description {
                    rule_obj["fullDescription"] = serde_json::json!({ "text": desc });
                }
                rule_obj
            })
            .collect();
        let results: Vec<_> = sorted
            .iter()
            .map(|finding| {
                let level = match finding.severity.as_str() {
                    "error" => "error",
                    "warning" => "warning",
                    _ => "note",
                };
                serde_json::json!({
                    "ruleId": finding.rule,
                    "level": level,
                    "message": { "text": finding.message },
                    "locations": [{
                        "physicalLocation": {
                            "artifactLocation": { "uri": finding.file.to_string_lossy() },
                            "region": { "startLine": finding.line.max(1) }
                        }
                    }]
                })
            })
            .collect();
        serde_json::to_string_pretty(&serde_json::json!({
            "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
            "version": "2.1.0",
            "runs": [{
                "tool": { "driver": { "name": "soroban-testkit", "rules": rules } },
                "results": results,
                "properties": { "limitations": [MACRO_SOURCE_LIMITATION] }
            }]
        }))
        .map(|report| format!("{report}\n"))
        .map_err(|err| CliError(format!("failed to serialize SARIF audit report: {err}")))
    }
}

pub fn render_findings(
    findings: &[Finding],
    format: AuditOutputFormat,
) -> Result<String, CliError> {
    DiagnosticRenderer::new(findings).render(format)
}

#[allow(dead_code)]
pub fn render_text(findings: &[Finding]) -> String {
    DiagnosticRenderer::new(findings).render_text()
}

#[allow(dead_code)]
pub fn render_json(findings: &[Finding]) -> Result<String, CliError> {
    DiagnosticRenderer::new(findings).render_json()
}

#[allow(dead_code)]
pub fn render_sarif(findings: &[Finding]) -> Result<String, CliError> {
    DiagnosticRenderer::new(findings).render_sarif()
}

// Prefer a Cargo workspace, then a repository root, then the nearest crate.
// Do not cross a repository boundary to discover another project's config.
fn find_workspace_root(source_dir: &Path) -> PathBuf {
    let mut crate_root = None;
    for dir in source_dir.ancestors() {
        if let Ok(manifest) = fs::read_to_string(dir.join("Cargo.toml")) {
            if crate_root.is_none() {
                crate_root = Some(dir.to_path_buf());
            }
            if let Ok(manifest) = toml::from_str::<toml::Value>(&manifest) {
                if manifest.get("workspace").is_some() {
                    return dir.to_path_buf();
                }
            }
        }
        if dir.join(".git").exists() {
            return dir.to_path_buf();
        }
    }
    crate_root.unwrap_or_else(|| source_dir.to_path_buf())
}

fn workspace_relative_path(path: &Path, workspace: &Path) -> PathBuf {
    let absolute = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    absolute
        .strip_prefix(workspace)
        .unwrap_or(&absolute)
        .components()
        .collect()
}

fn load_config(source_dir: &Path, workspace: &Path) -> Result<AuditConfig, CliError> {
    for dir in source_dir.ancestors() {
        for name in [".soroban-testkit.toml", "soroban-testkit.toml"] {
            let path = dir.join(name);
            let content = match fs::read_to_string(&path) {
                Ok(content) => content,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => {
                    return Err(CliError(format!(
                        "failed to read audit config {}: {err}; check file permissions and UTF-8 encoding",
                        path.display()
                    )));
                }
            };
            return toml::from_str(&content).map_err(|err| {
                CliError(format!(
                    "failed to parse audit config {} as TOML: {err}; fix the configuration syntax",
                    path.display()
                ))
            });
        }
        if dir == workspace {
            break;
        }
    }

    Ok(AuditConfig::default())
}

fn load_baseline(baseline_path: &Option<PathBuf>) -> Result<Vec<BaselineEntry>, CliError> {
    match baseline_path {
        Some(path) => {
            let content = fs::read_to_string(path)
                .map_err(|err| CliError(format!("failed to read baseline file {}: {err}; check the path, permissions and UTF-8 encoding", path.display())))?;
            let entries: Vec<BaselineEntry> = serde_json::from_str(&content)
                .map_err(|err| CliError(format!("failed to parse baseline JSON {}: {err}; expected an array of file, line and rule entries", path.display())))?;
            Ok(entries)
        }
        None => Ok(Vec::new()),
    }
}

fn audit_file(
    path: &Path,
    findings: &mut Vec<Finding>,
    config: &AuditConfig,
) -> Result<(), CliError> {
    let src = std::fs::read_to_string(path).map_err(|err| {
        CliError(format!(
            "failed to read Rust source {}: {err}; check file permissions and UTF-8 encoding",
            path.display()
        ))
    })?;
    audit_source(path, &src, findings, config, &src)
}

fn audit_source(
    path: &Path,
    src: &str,
    findings: &mut Vec<Finding>,
    config: &AuditConfig,
    full_src: &str,
) -> Result<(), CliError> {
    let file = syn::parse_file(src).map_err(|err| {
        let start = err.span().start();
        CliError(format!(
            "failed to parse Rust source {}:{}:{}: {err}; expected a complete Rust source file",
            path.display(),
            start.line,
            start.column + 1
        ))
    })?;

    let suppressions = extract_suppressions(full_src);

    let mut visitor = FunctionVisitor {
        path: path.to_path_buf(),
        findings,
        config,
        suppressions: &suppressions,
    };
    visitor.visit_file(&file);
    Ok(())
}

struct Suppression {
    line: usize,
    rules: Vec<String>,
    #[allow(dead_code)]
    reason: Option<String>,
}

fn find_audit_directive(line: &str) -> Option<&str> {
    let lower = line.to_lowercase();
    let prefix_pos = lower
        .find("soroban_testkit")
        .or_else(|| lower.find("soroban-testkit"))?;
    let after_prefix = &line[prefix_pos..];
    let audit_pos = after_prefix.to_lowercase().find("audit")?;
    let after_audit = &after_prefix[audit_pos + "audit".len()..];
    let open_paren = after_audit.find('(')?;
    let close_paren = after_audit.rfind(')')?;
    if close_paren > open_paren {
        Some(&after_audit[open_paren + 1..close_paren])
    } else {
        None
    }
}

fn extract_suppressions(src: &str) -> Vec<Suppression> {
    let mut suppressions = Vec::new();
    for (idx, line) in src.lines().enumerate() {
        if let Some(content) = find_audit_directive(line) {
            let mut rules = Vec::new();
            let mut reason = None;

            for part in content.split(',') {
                let part = part.trim();
                let clean = if let Some(stripped) = part.strip_prefix("reason=") {
                    reason = Some(
                        stripped
                            .trim()
                            .trim_matches('"')
                            .trim_matches('\'')
                            .to_string(),
                    );
                    continue;
                } else if let Some(stripped) = part.strip_prefix("reason =") {
                    reason = Some(
                        stripped
                            .trim()
                            .trim_matches('"')
                            .trim_matches('\'')
                            .to_string(),
                    );
                    continue;
                } else if let Some(stripped) = part.strip_prefix("rule=") {
                    stripped
                } else if let Some(stripped) = part.strip_prefix("rule =") {
                    stripped
                } else if let Some(stripped) = part.strip_prefix("rule:") {
                    stripped
                } else if let Some(stripped) = part.strip_prefix("rule :") {
                    stripped
                } else if let Some(stripped) = part.strip_prefix("skip-") {
                    stripped
                } else {
                    part
                };

                let clean = clean.trim().trim_matches('"').trim_matches('\'').trim();
                if !clean.is_empty() && clean != "skip" {
                    if clean == "all" || clean == "*" {
                        rules.push("*".to_string());
                    } else {
                        rules.push(clean.to_string());
                    }
                }
            }

            if rules.is_empty() {
                rules.push("*".to_string());
            }

            suppressions.push(Suppression {
                line: idx + 1,
                rules,
                reason,
            });
        }
    }
    suppressions
}

fn item_line_range(node: impl quote::ToTokens) -> (usize, usize) {
    let tokens = node.to_token_stream();
    let mut min_line = usize::MAX;
    let mut max_line = 0;
    for token in tokens {
        let span = token.span();
        let start = span.start().line;
        let end = span.end().line;
        if start < min_line {
            min_line = start;
        }
        if end > max_line {
            max_line = end;
        }
    }
    if min_line == usize::MAX {
        (0, 0)
    } else {
        (min_line, max_line)
    }
}

fn is_finding_suppressed(
    suppressions: &[Suppression],
    rule: &str,
    finding_line: usize,
    fn_range: Option<(usize, usize)>,
) -> bool {
    for suppression in suppressions {
        let matches_rule = suppression.rules.iter().any(|r| r == "*" || r == rule);
        if !matches_rule {
            continue;
        }

        // 1. File-level suppression (top of file before items, e.g. line <= 10)
        if suppression.line <= 10 {
            return true;
        }

        // 2. Direct line match (finding is on the same line or line immediately following suppression)
        if suppression.line == finding_line || suppression.line + 1 == finding_line {
            return true;
        }

        // 3. Function-level suppression (suppression is immediately above function or inside function)
        if let Some((start, end)) = fn_range {
            if suppression.line + 1 >= start && suppression.line <= end {
                return true;
            }
        }
    }
    false
}

fn is_require_auth_call(expr: &Expr) -> bool {
    match expr {
        Expr::MethodCall(call) => {
            let method = call.method.to_string();
            method == "require_auth" || method == "require_auth_for_args"
        }
        Expr::Call(call) => match &*call.func {
            Expr::Path(expr_path) => {
                if let Some(last_seg) = expr_path.path.segments.last() {
                    let ident = last_seg.ident.to_string();
                    ident == "require_auth" || ident == "require_auth_for_args"
                } else {
                    false
                }
            }
            _ => false,
        },
        Expr::Paren(paren) => is_require_auth_call(&paren.expr),
        Expr::Group(group) => is_require_auth_call(&group.expr),
        Expr::Try(t) => is_require_auth_call(&t.expr),
        Expr::Await(a) => is_require_auth_call(&a.base),
        _ => false,
    }
}

fn is_negated_condition(expr: &Expr) -> bool {
    match expr {
        Expr::Unary(syn::ExprUnary {
            op: UnOp::Not(_), ..
        }) => true,
        Expr::Paren(paren) => is_negated_condition(&paren.expr),
        Expr::Group(group) => is_negated_condition(&group.expr),
        Expr::Binary(syn::ExprBinary {
            left,
            op: BinOp::Eq(_),
            right,
            ..
        }) => is_lit_false(left) || is_lit_false(right),
        Expr::Binary(syn::ExprBinary {
            left,
            op: BinOp::Ne(_),
            right,
            ..
        }) => is_lit_true(left) || is_lit_true(right),
        _ => false,
    }
}

fn is_lit_false(expr: &Expr) -> bool {
    matches!(expr, Expr::Lit(syn::ExprLit { lit: syn::Lit::Bool(b), .. }) if !b.value)
}

fn is_lit_true(expr: &Expr) -> bool {
    matches!(expr, Expr::Lit(syn::ExprLit { lit: syn::Lit::Bool(b), .. }) if b.value)
}

struct AuthVisitor {
    negated_depth: usize,
    has_enforced_auth: bool,
}

impl<'ast> Visit<'ast> for AuthVisitor {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        if is_require_auth_call(expr) && self.negated_depth == 0 {
            self.has_enforced_auth = true;
        }
        visit::visit_expr(self, expr);
    }

    fn visit_expr_if(&mut self, node: &'ast syn::ExprIf) {
        let is_negated = is_negated_condition(&node.cond);
        if is_negated {
            self.negated_depth += 1;
        }
        self.visit_block(&node.then_branch);
        if is_negated {
            self.negated_depth -= 1;
        }
        if let Some((_, else_branch)) = &node.else_branch {
            self.visit_expr(else_branch);
        }
    }

    fn visit_expr_while(&mut self, node: &'ast syn::ExprWhile) {
        let is_negated = is_negated_condition(&node.cond);
        if is_negated {
            self.negated_depth += 1;
        }
        self.visit_block(&node.body);
        if is_negated {
            self.negated_depth -= 1;
        }
    }
}

fn has_enforced_require_auth(block: &syn::Block) -> bool {
    let mut visitor = AuthVisitor {
        negated_depth: 0,
        has_enforced_auth: false,
    };
    visitor.visit_block(block);
    visitor.has_enforced_auth
}

struct FunctionVisitor<'a> {
    path: PathBuf,
    findings: &'a mut Vec<Finding>,
    config: &'a AuditConfig,
    suppressions: &'a [Suppression],
}

impl<'ast> Visit<'ast> for FunctionVisitor<'_> {
    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        let line = line_of(node);
        let fn_range = item_line_range(node);
        let fn_name = node.sig.ident.to_string();
        let body_src = node.to_token_stream().to_string();
        self.audit_function(&node.sig, &node.block, body_src.clone(), line, fn_range);
        self.audit_i128_arithmetic(&node.sig, &node.block, line, fn_range);
        self.audit_ignored_transfer_results(&node.sig, &node.block, fn_range);
        self.audit_mock_all_auths(&body_src, line, &fn_name, fn_range);
        visit::visit_item_fn(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
        let line = line_of(node);
        let fn_range = item_line_range(node);
        let fn_name = node.sig.ident.to_string();
        let body_src = node.to_token_stream().to_string();
        self.audit_function(&node.sig, &node.block, body_src.clone(), line, fn_range);
        self.audit_i128_arithmetic(&node.sig, &node.block, line, fn_range);
        self.audit_ignored_transfer_results(&node.sig, &node.block, fn_range);
        self.audit_mock_all_auths(&body_src, line, &fn_name, fn_range);
        visit::visit_impl_item_fn(self, node);
    }
}

impl FunctionVisitor<'_> {
    fn audit_function(
        &mut self,
        sig: &Signature,
        block: &syn::Block,
        body_src: String,
        line: usize,
        fn_range: (usize, usize),
    ) {
        let name = sig.ident.to_string();

        // Rule 1: an Address parameter that's never checked with require_auth.
        if self.is_enabled("missing-require-auth") {
            let address_params = address_typed_params(sig);
            if !address_params.is_empty() && !has_enforced_require_auth(block) {
                self.push_with_range(
                    line,
                    "missing-require-auth",
                    "warning",
                    format!(
                        "fn {name} takes Address parameter(s) {address_params:?} but never calls \
                         require_auth in its body"
                    ),
                    Some(fn_range),
                );
            }
        }

        // Rule 3: a storage read with no extend_ttl anywhere in the function.
        if self.is_enabled("missing-ttl-bump") {
            let reads_persistent = body_src.contains(". persistent () . get (");
            let reads_temporary = body_src.contains(". temporary () . get (");
            if (reads_persistent || reads_temporary) && !body_src.contains(". extend_ttl (") {
                self.push_with_range(
                    line,
                    "missing-ttl-bump",
                    "warning",
                    format!(
                        "fn {name} reads from {} storage but never calls extend_ttl in its body",
                        if reads_persistent {
                            "persistent"
                        } else {
                            "temporary"
                        }
                    ),
                    Some(fn_range),
                );
            }
        }

        if self.is_enabled("missing-positive-amount-validation") {
            for amount_param in signed_amount_params(sig) {
                if !has_positive_amount_validation(&body_src, &amount_param) {
                    self.push_with_range(
                        line,
                        "missing-positive-amount-validation",
                        "warning",
                        format!(
                            "fn {name} takes signed amount parameter `{amount_param}` but never \
                             validates it against zero"
                        ),
                        Some(fn_range),
                    );
                }
            }
        }

        // Rule 7: unchecked timestamp arithmetic
        if self.is_enabled("unchecked-timestamp-arithmetic") {
            let has_timestamp_ops = body_src.contains("timestamp")
                && (body_src.contains("+")
                    || body_src.contains("-")
                    || body_src.contains("*")
                    || body_src.contains("/"));
            let is_checked = body_src.contains("checked_") || body_src.contains("saturating_");

            if has_timestamp_ops && !is_checked && body_src.contains("timestamp") {
                // Check if it's likely timestamp arithmetic
                if body_src.contains("timestamp()") || body_src.contains("timestamp ") {
                    self.push_with_range(
                        line,
                        "unchecked-timestamp-arithmetic",
                        "warning",
                        format!(
                            "fn {name} performs arithmetic on timestamp values; use checked arithmetic to prevent overflow"
                        ),
                        Some(fn_range),
                    );
                }
            }
        }
    }

    fn audit_mock_all_auths(
        &mut self,
        body_src: &str,
        line: usize,
        fn_name: &str,
        fn_range: (usize, usize),
    ) {
        if !self.is_enabled("broad-mock-all-auths") {
            return;
        }

        let is_test = fn_name.contains("test") || body_src.contains("#[test]");
        if !is_test {
            return;
        }

        if body_src.contains("mock_all_auths") {
            self.push_with_range(
                line,
                "broad-mock-all-auths",
                "warning",
                format!(
                    "test function {fn_name} uses mock_all_auths; prefer granular auth checks per caller"
                ),
                Some(fn_range),
            );
        }
    }

    fn is_enabled(&self, rule: &str) -> bool {
        self.config
            .rules
            .get(rule)
            .map(|r| r.enabled)
            .unwrap_or(true)
    }

    #[allow(dead_code)]
    fn push(
        &mut self,
        line: usize,
        rule: &'static str,
        default_severity: &'static str,
        message: String,
    ) {
        self.push_with_range(line, rule, default_severity, message, None);
    }

    fn push_with_range(
        &mut self,
        line: usize,
        rule: &'static str,
        default_severity: &'static str,
        message: String,
        fn_range: Option<(usize, usize)>,
    ) {
        if !self.is_enabled(rule) {
            return;
        }

        if is_finding_suppressed(self.suppressions, rule, line, fn_range) {
            return;
        }

        let severity = self
            .config
            .rules
            .get(rule)
            .and_then(|r| r.severity.as_deref())
            .unwrap_or(default_severity);

        self.findings.push(Finding {
            file: self.path.clone(),
            line,
            rule,
            severity: severity.to_string(),
            message,
        });
    }

    fn audit_i128_arithmetic(
        &mut self,
        sig: &Signature,
        body: &syn::Block,
        line: usize,
        fn_range: (usize, usize),
    ) {
        if !self.is_enabled("unchecked-i128-arithmetic") {
            return;
        }

        let mut i128_bindings: HashSet<String> = HashSet::new();
        for arg in &sig.inputs {
            if let FnArg::Typed(pat_type) = arg {
                if type_is_i128(&pat_type.ty) {
                    if let Pat::Ident(ident) = &*pat_type.pat {
                        i128_bindings.insert(ident.ident.to_string());
                    }
                }
            }
        }

        let mut visitor = I128ArithmeticVisitor {
            path: self.path.clone(),
            fn_name: sig.ident.to_string(),
            fn_line: line,
            fn_range,
            i128_bindings,
            findings: self.findings,
            config: self.config,
            suppressions: self.suppressions,
        };
        visitor.visit_block(body);
    }

    fn audit_ignored_transfer_results(
        &mut self,
        sig: &Signature,
        body: &syn::Block,
        fn_range: (usize, usize),
    ) {
        if !self.is_enabled("ignored-token-transfer-result") {
            return;
        }

        let mut visitor = IgnoredTransferVisitor {
            path: self.path.clone(),
            fn_name: sig.ident.to_string(),
            fn_range,
            findings: self.findings,
            config: self.config,
            suppressions: self.suppressions,
        };
        visitor.visit_block(body);
    }
}

fn signed_amount_params(sig: &Signature) -> Vec<String> {
    sig.inputs
        .iter()
        .filter_map(|arg| match arg {
            FnArg::Typed(pat_type) if type_is_signed_integer(&pat_type.ty) => {
                match &*pat_type.pat {
                    Pat::Ident(ident) if ident.ident.to_string().contains("amount") => {
                        Some(ident.ident.to_string())
                    }
                    _ => None,
                }
            }
            _ => None,
        })
        .collect()
}

fn type_is_signed_integer(ty: &Type) -> bool {
    match ty {
        Type::Reference(reference) => type_is_signed_integer(&reference.elem),
        Type::Path(path) => path.path.get_ident().is_some_and(|ident| {
            matches!(
                ident.to_string().as_str(),
                "i8" | "i16" | "i32" | "i64" | "i128" | "isize"
            )
        }),
        _ => false,
    }
}

fn has_positive_amount_validation(body_src: &str, amount_param: &str) -> bool {
    let compact = body_src.replace(' ', "");
    [
        format!("{amount_param}>0"),
        format!("{amount_param}>=0"),
        format!("{amount_param}<=0"),
        format!("{amount_param}<0"),
        format!("0<{amount_param}"),
        format!("0<={amount_param}"),
        format!("0>{amount_param}"),
        format!("0>={amount_param}"),
        format!("{amount_param}.is_positive()"),
    ]
    .iter()
    .any(|pattern| compact.contains(pattern))
}

fn address_typed_params(sig: &Signature) -> Vec<String> {
    sig.inputs
        .iter()
        .filter_map(|arg| match arg {
            FnArg::Typed(pat_type) if type_is_address(&pat_type.ty) => match &*pat_type.pat {
                Pat::Ident(ident) => Some(ident.ident.to_string()),
                _ => Some("<pattern>".to_string()),
            },
            _ => None,
        })
        .collect()
}

fn type_is_address(ty: &Type) -> bool {
    match ty {
        Type::Reference(r) => type_is_address(&r.elem),
        Type::Path(p) => p
            .path
            .segments
            .last()
            .is_some_and(|seg| seg.ident == "Address"),
        _ => false,
    }
}

fn line_of(node: impl quote::ToTokens) -> usize {
    node.to_token_stream()
        .into_iter()
        .next()
        .map(|t| t.span().start().line)
        .unwrap_or(0)
}

struct IgnoredTransferVisitor<'a> {
    path: PathBuf,
    fn_name: String,
    fn_range: (usize, usize),
    findings: &'a mut Vec<Finding>,
    config: &'a AuditConfig,
    suppressions: &'a [Suppression],
}

impl<'ast> Visit<'ast> for IgnoredTransferVisitor<'_> {
    fn visit_stmt(&mut self, node: &'ast syn::Stmt) {
        let ignored_call = match node {
            syn::Stmt::Expr(expr, _) if transfer_method_name(expr).is_some() => Some(expr),
            syn::Stmt::Local(local)
                if pattern_is_wildcard(&local.pat)
                    && local
                        .init
                        .as_ref()
                        .is_some_and(|init| transfer_method_name(&init.expr).is_some()) =>
            {
                local.init.as_ref().map(|init| &*init.expr)
            }
            _ => None,
        };

        if let Some(expr) = ignored_call {
            let line = line_of(expr);
            let is_suppressed = is_finding_suppressed(
                self.suppressions,
                "ignored-token-transfer-result",
                line,
                Some(self.fn_range),
            );

            if !is_suppressed {
                let severity = self
                    .config
                    .rules
                    .get("ignored-token-transfer-result")
                    .and_then(|rule| rule.severity.as_deref())
                    .unwrap_or("warning");
                let method = transfer_method_name(expr).unwrap_or("transfer");
                self.findings.push(Finding {
                    file: self.path.clone(),
                    line,
                    rule: "ignored-token-transfer-result",
                    severity: severity.to_string(),
                    message: format!(
                        "fn {} ignores the result of token `{method}`; propagate, match, or bind the result",
                        self.fn_name
                    ),
                });
            }
        }

        visit::visit_stmt(self, node);
    }
}

fn transfer_method_name(expr: &Expr) -> Option<&'static str> {
    match expr {
        Expr::MethodCall(call)
            if matches!(
                call.method.to_string().as_str(),
                "transfer" | "transfer_from" | "try_transfer" | "try_transfer_from"
            ) =>
        {
            Some(match call.method.to_string().as_str() {
                "transfer_from" => "transfer_from",
                "try_transfer" => "try_transfer",
                "try_transfer_from" => "try_transfer_from",
                _ => "transfer",
            })
        }
        Expr::Paren(paren) => transfer_method_name(&paren.expr),
        Expr::Group(group) => transfer_method_name(&group.expr),
        _ => None,
    }
}

fn pattern_is_wildcard(pattern: &Pat) -> bool {
    match pattern {
        Pat::Wild(_) => true,
        Pat::Type(typed) => pattern_is_wildcard(&typed.pat),
        _ => false,
    }
}

/// Rule 2 uses a dedicated AST walk (rather than a text search) because
/// "arithmetic on an i128" needs to know which bindings are actually
/// typed `i128`, and a plain string search over the token stream can't
/// tell an `i128` value from the substring appearing anywhere else.
struct I128ArithmeticVisitor<'a> {
    path: PathBuf,
    fn_name: String,
    fn_line: usize,
    fn_range: (usize, usize),
    i128_bindings: HashSet<String>,
    findings: &'a mut Vec<Finding>,
    config: &'a AuditConfig,
    suppressions: &'a [Suppression],
}

impl<'ast> Visit<'ast> for I128ArithmeticVisitor<'_> {
    fn visit_local(&mut self, node: &'ast Local) {
        if let Pat::Type(pat_type) = &node.pat {
            if type_is_i128(&pat_type.ty) {
                if let Pat::Ident(ident) = &*pat_type.pat {
                    self.i128_bindings.insert(ident.ident.to_string());
                }
            }
        }
        visit::visit_local(self, node);
    }

    fn visit_expr_binary(&mut self, node: &'ast syn::ExprBinary) {
        let is_arith = matches!(
            node.op,
            BinOp::Add(_) | BinOp::Sub(_) | BinOp::Mul(_) | BinOp::Div(_)
        );
        if is_arith && (self.touches_i128(&node.left) || self.touches_i128(&node.right)) {
            let expr_line = line_of(node);
            let is_suppressed = is_finding_suppressed(
                self.suppressions,
                "unchecked-i128-arithmetic",
                expr_line,
                Some(self.fn_range),
            );

            if !is_suppressed {
                let severity = self
                    .config
                    .rules
                    .get("unchecked-i128-arithmetic")
                    .and_then(|r| r.severity.as_deref())
                    .unwrap_or("warning");

                self.findings.push(Finding {
                    file: self.path.clone(),
                    line: if expr_line > 0 {
                        expr_line
                    } else {
                        self.fn_line
                    },
                    rule: "unchecked-i128-arithmetic",
                    severity: severity.to_string(),
                    message: format!(
                        "fn {} does raw arithmetic on an i128 value; prefer checked_/saturating_/ \
                         wrapping_ variants to avoid silent overflow",
                        self.fn_name
                    ),
                });
            }
        }
        visit::visit_expr_binary(self, node);
    }
}

impl I128ArithmeticVisitor<'_> {
    fn touches_i128(&self, expr: &Expr) -> bool {
        match expr {
            Expr::Path(p) => p
                .path
                .get_ident()
                .is_some_and(|ident| self.i128_bindings.contains(&ident.to_string())),
            Expr::Lit(lit) => matches!(&lit.lit, syn::Lit::Int(i) if i.suffix() == "i128"),
            Expr::Paren(p) => self.touches_i128(&p.expr),
            Expr::Group(g) => self.touches_i128(&g.expr),
            _ => false,
        }
    }
}

fn type_is_i128(ty: &Type) -> bool {
    matches!(ty, Type::Path(p) if p.path.is_ident("i128"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audit_source_for_test(source: &str) -> Vec<Finding> {
        let mut findings = Vec::new();
        audit_source(
            Path::new("<test>"),
            source,
            &mut findings,
            &AuditConfig::default(),
            source,
        )
        .unwrap();
        findings
    }

    #[test]
    fn macro_expanded_entry_point_is_scanned() {
        let findings = audit_source_for_test(
            r#"
            #[contract]
            pub struct Contract;
            #[contractimpl]
            impl Contract {
                pub fn transfer(env: Env, from: Address) {
                    env.storage().instance().set(&from, &1i128);
                }
            }
            "#,
        );
        assert!(findings
            .iter()
            .any(|finding| finding.rule == "missing-require-auth"));
    }

    #[test]
    fn authorized_macro_entry_point_has_no_auth_finding() {
        let findings = audit_source_for_test(
            r#"
            #[contractimpl]
            impl Contract {
                pub fn transfer(env: Env, from: Address) {
                    from.require_auth();
                }
            }
            "#,
        );
        assert!(!findings
            .iter()
            .any(|finding| finding.rule == "missing-require-auth"));
    }

    #[test]
    fn empty_source_is_a_clean_edge_case() {
        assert!(audit_source_for_test("").is_empty());
    }

    #[test]
    fn test_config_default_enabled() {
        let config = AuditConfig::default();
        assert!(config.rules.is_empty());
    }

    #[test]
    fn test_config_rule_disabled() {
        let mut rules = HashMap::new();
        rules.insert(
            "missing-require-auth".to_string(),
            RuleConfig {
                enabled: false,
                severity: None,
            },
        );
        let config = AuditConfig { rules };
        assert!(!config.rules["missing-require-auth"].enabled);
    }

    #[test]
    fn test_config_severity_override() {
        let mut rules = HashMap::new();
        rules.insert(
            "missing-require-auth".to_string(),
            RuleConfig {
                enabled: true,
                severity: Some("error".to_string()),
            },
        );
        let config = AuditConfig { rules };
        assert_eq!(
            config.rules["missing-require-auth"].severity,
            Some("error".to_string())
        );
    }

    #[test]
    fn test_missing_require_auth_detected() {
        let mut findings = Vec::new();
        let config = AuditConfig::default();

        let code = r#"
            pub fn withdraw(env: &Env, caller: &Address, amount: i128) {
                env.storage().persistent().set(&"amount", &amount);
            }
        "#;

        let file = syn::parse_file(code).unwrap();
        let suppressions = Vec::new();
        let mut visitor = FunctionVisitor {
            path: PathBuf::from("test.rs"),
            findings: &mut findings,
            config: &config,
            suppressions: &suppressions,
        };
        visitor.visit_file(&file);

        assert!(!findings.is_empty());
        assert_eq!(findings[0].rule, "missing-require-auth");
    }

    #[test]
    fn test_mock_all_auths_detected() {
        let mut findings = Vec::new();
        let config = AuditConfig::default();

        let code = r#"
            #[test]
            fn test_broad_auth() {
                let env = Env::default();
                env.mock_all_auths();
                env.invoke_contract(&contract, &Symbol::new(&env, "transfer"), &args);
            }
        "#;

        let file = syn::parse_file(code).unwrap();
        let suppressions = Vec::new();
        let mut visitor = FunctionVisitor {
            path: PathBuf::from("test.rs"),
            findings: &mut findings,
            config: &config,
            suppressions: &suppressions,
        };
        visitor.visit_file(&file);

        let mock_findings: Vec<_> = findings
            .iter()
            .filter(|f| f.rule == "broad-mock-all-auths")
            .collect();
        assert!(
            !mock_findings.is_empty(),
            "Expected to find broad-mock-all-auths finding"
        );
    }

    #[test]
    fn test_rule_disabled_skips_check() {
        let mut findings = Vec::new();
        let mut rules = HashMap::new();
        rules.insert(
            "missing-require-auth".to_string(),
            RuleConfig {
                enabled: false,
                severity: None,
            },
        );
        let config = AuditConfig { rules };

        let code = r#"
            pub fn withdraw(env: &Env, caller: &Address, amount: i128) {
                env.storage().persistent().set(&"amount", &amount);
            }
        "#;

        let file = syn::parse_file(code).unwrap();
        let suppressions = Vec::new();
        let mut visitor = FunctionVisitor {
            path: PathBuf::from("test.rs"),
            findings: &mut findings,
            config: &config,
            suppressions: &suppressions,
        };
        visitor.visit_file(&file);

        let require_auth_findings: Vec<_> = findings
            .iter()
            .filter(|f| f.rule == "missing-require-auth")
            .collect();
        assert!(
            require_auth_findings.is_empty(),
            "Expected no missing-require-auth findings when disabled"
        );
    }

    #[test]
    fn test_severity_override_applied() {
        let mut findings = Vec::new();
        let mut rules = HashMap::new();
        rules.insert(
            "missing-require-auth".to_string(),
            RuleConfig {
                enabled: true,
                severity: Some("error".to_string()),
            },
        );
        let config = AuditConfig { rules };

        let code = r#"
            pub fn withdraw(env: &Env, caller: &Address, amount: i128) {
                env.storage().persistent().set(&"amount", &amount);
            }
        "#;

        let file = syn::parse_file(code).unwrap();
        let suppressions = Vec::new();
        let mut visitor = FunctionVisitor {
            path: PathBuf::from("test.rs"),
            findings: &mut findings,
            config: &config,
            suppressions: &suppressions,
        };
        visitor.visit_file(&file);

        let auth_findings: Vec<_> = findings
            .iter()
            .filter(|f| f.rule == "missing-require-auth")
            .collect();
        assert!(!auth_findings.is_empty());
        assert_eq!(auth_findings[0].severity, "error");
    }

    #[test]
    fn test_i128_arithmetic_detection() {
        let mut findings = Vec::new();
        let config = AuditConfig::default();

        let code = r#"
            pub fn calculate(amount: i128) -> i128 {
                amount + 100
            }
        "#;

        let file = syn::parse_file(code).unwrap();
        let suppressions = Vec::new();
        let mut visitor = FunctionVisitor {
            path: PathBuf::from("test.rs"),
            findings: &mut findings,
            config: &config,
            suppressions: &suppressions,
        };
        visitor.visit_file(&file);

        let arith_findings: Vec<_> = findings
            .iter()
            .filter(|f| f.rule == "unchecked-i128-arithmetic")
            .collect();
        assert!(!arith_findings.is_empty());
    }

    #[test]
    fn missing_positive_amount_validation_is_reported() {
        let findings = audit_source_for_test(
            r#"
            pub fn deposit(amount: i128) {
                save(amount);
            }
            "#,
        );
        assert!(findings
            .iter()
            .any(|finding| finding.rule == "missing-positive-amount-validation"));
    }

    #[test]
    fn comparison_against_zero_satisfies_positive_amount_rule() {
        let findings = audit_source_for_test(
            r#"
            pub fn deposit(amount: i128) {
                assert!(amount > 0);
                save(amount);
            }
            "#,
        );
        assert!(!findings
            .iter()
            .any(|finding| finding.rule == "missing-positive-amount-validation"));
    }

    #[test]
    fn unsigned_amount_is_not_flagged_by_positive_amount_rule() {
        let findings = audit_source_for_test("pub fn deposit(amount: u128) { save(amount); }");
        assert!(!findings
            .iter()
            .any(|finding| finding.rule == "missing-positive-amount-validation"));
    }

    #[test]
    fn ignored_transfer_result_is_reported() {
        let findings = audit_source_for_test(
            r#"
            pub fn pay(token: TokenClient, to: Address, amount: i128) {
                assert!(amount > 0);
                token.try_transfer(&to, &amount);
            }
            "#,
        );
        assert!(findings
            .iter()
            .any(|finding| finding.rule == "ignored-token-transfer-result"));
    }

    #[test]
    fn explicitly_discarded_transfer_result_is_reported() {
        let findings =
            audit_source_for_test("pub fn pay(token: TokenClient) { let _ = token.transfer(); }");
        assert!(findings
            .iter()
            .any(|finding| finding.rule == "ignored-token-transfer-result"));
    }

    #[test]
    fn propagated_or_bound_transfer_results_are_not_reported() {
        let findings = audit_source_for_test(
            r#"
            pub fn pay(token: TokenClient) -> Result<(), Error> {
                token.try_transfer()?;
                let result = token.transfer();
                consume(result);
                Ok(())
            }
            "#,
        );
        assert!(!findings
            .iter()
            .any(|finding| finding.rule == "ignored-token-transfer-result"));
    }

    #[test]
    fn json_output_is_stable_and_machine_readable() {
        let findings = audit_source_for_test("pub fn deposit(amount: i128) { save(amount); }");
        let output = render_findings(&findings, AuditOutputFormat::Json).unwrap();
        let report: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(report["tool"], "soroban-testkit");
        assert_eq!(report["version"], 1);
        assert_eq!(report["summary"]["finding_count"], findings.len());
        assert!(report["findings"].is_array());
    }

    #[test]
    fn json_findings_escape_every_control_character_without_double_escaping() {
        let controls: String = (0..=0x1f).map(char::from).collect();
        let value = format!("{controls}\"\\雪 literal \\n and \\u0000");
        let findings = [Finding {
            file: PathBuf::from(&value),
            line: 1,
            rule: "missing-require-auth",
            severity: value.clone(),
            message: value.clone(),
        }];
        let output = render_findings(&findings, AuditOutputFormat::Json).unwrap();
        let report: serde_json::Value = serde_json::from_str(&output).unwrap();
        for field in ["file", "severity", "message"] {
            assert_eq!(report["findings"][0][field], value);
        }
        assert!(!output.chars().any(|c| c < ' ' && c != '\n'));
        assert!(output.contains("\\u0000"));
        assert!(output.contains("\\t"));
        assert!(output.contains("\\r"));
    }

    #[test]
    fn sarif_output_contains_code_scanning_location() {
        let findings = audit_source_for_test("pub fn deposit(amount: i128) { save(amount); }");
        let output = render_findings(&findings, AuditOutputFormat::Sarif).unwrap();
        let report: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(report["version"], "2.1.0");
        assert_eq!(
            report["runs"][0]["tool"]["driver"]["name"],
            "soroban-testkit"
        );
        assert_eq!(
            report["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["artifactLocation"]
                ["uri"],
            "<test>"
        );
    }

    // ---- #489: documented scan roots and exclusions ----

    #[test]
    fn collect_rust_sources_excludes_build_vendor_and_hidden_directories() {
        let root = std::env::temp_dir().join(format!("stk-audit-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        std::fs::create_dir_all(root.join(".git/objects")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub fn a() {}").unwrap();
        std::fs::write(root.join("target/debug/generated.rs"), "pub fn b() {}").unwrap();
        std::fs::write(root.join("node_modules/pkg/dep.rs"), "pub fn c() {}").unwrap();
        std::fs::write(root.join(".git/objects/blob.rs"), "pub fn d() {}").unwrap();

        let sources = collect_rust_sources(&root, &mut Vec::new()).unwrap();
        assert_eq!(sources, vec![root.join("src/lib.rs")]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn collect_rust_sources_accepts_a_single_file_root() {
        let root = std::env::temp_dir().join(format!("stk-audit-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("only.rs");
        std::fs::write(&file, "pub fn a() {}").unwrap();

        let sources = collect_rust_sources(&file, &mut Vec::new()).unwrap();
        assert_eq!(sources, vec![file]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn collect_rust_sources_reports_a_missing_root_actionably() {
        let missing = std::env::temp_dir().join("stk-audit-root-that-does-not-exist-anywhere");
        let err = collect_rust_sources(&missing, &mut Vec::new()).unwrap_err();
        assert!(err.0.contains("does not exist"), "{}", err.0);
        assert!(err.0.contains("stdin"), "{}", err.0);
    }

    // ---- #437: avoid counting suppressed findings toward strict exit status ----

    #[test]
    fn suppression_with_rule_name_skips_finding() {
        let findings = audit_source_for_test(
            r#"
            #[contractimpl]
            impl Contract {
                // soroban_testkit::audit(skip, rule = "missing-require-auth")
                pub fn withdraw(env: Env, from: Address) {
                    env.storage().instance().set(&from, &1i128);
                }
            }
            "#,
        );
        assert!(!findings.iter().any(|f| f.rule == "missing-require-auth"));
    }

    #[test]
    fn suppression_with_hyphenated_directive_and_reason_skips_finding() {
        let findings = audit_source_for_test(
            r#"
            #[contractimpl]
            impl Contract {
                // soroban-testkit::audit(skip, rule="missing-require-auth", reason="admin controlled")
                pub fn withdraw(env: Env, from: Address) {
                    env.storage().instance().set(&from, &1i128);
                }
            }
            "#,
        );
        assert!(!findings.iter().any(|f| f.rule == "missing-require-auth"));
    }

    #[test]
    fn function_level_suppression_with_attributes_and_docs_skips_finding() {
        let findings = audit_source_for_test(
            r#"
            #[contractimpl]
            impl Contract {
                // soroban_testkit::audit(skip)
                /// Documenting this function
                #[allow(dead_code)]
                pub fn withdraw(env: Env, from: Address) {
                    env.storage().instance().set(&from, &1i128);
                }
            }
            "#,
        );
        assert!(findings.is_empty());
    }

    #[test]
    fn inside_body_suppression_skips_arithmetic_finding() {
        let findings = audit_source_for_test(
            r#"
            pub fn calc(amount: i128) -> i128 {
                // soroban_testkit::audit(skip, rule = "unchecked-i128-arithmetic")
                amount + 100
            }
            "#,
        );
        assert!(!findings
            .iter()
            .any(|f| f.rule == "unchecked-i128-arithmetic"));
    }

    #[test]
    fn function_level_suppression_skips_ignored_transfer_finding() {
        let findings = audit_source_for_test(
            r#"
            // soroban_testkit::audit(skip, rule = "ignored-token-transfer-result")
            pub fn pay(token: TokenClient, to: Address, amount: i128) {
                assert!(amount > 0);
                token.try_transfer(&to, &amount);
            }
            "#,
        );
        assert!(!findings
            .iter()
            .any(|f| f.rule == "ignored-token-transfer-result"));
    }

    #[test]
    fn file_level_suppression_skips_all_findings_in_file() {
        let findings = audit_source_for_test(
            r#"
            // soroban_testkit::audit(skip)
            pub fn withdraw(env: Env, from: Address) {
                let amount: i128 = 100;
                let _ = amount + 5;
            }
            "#,
        );
        assert!(findings.is_empty());
    }

    // ---- #431: recognize qualified require_auth calls ----

    #[test]
    fn qualified_address_require_auth_call_is_recognized() {
        let findings = audit_source_for_test(
            r#"
            #[contractimpl]
            impl Contract {
                pub fn withdraw(env: Env, from: Address) {
                    Address::require_auth(&from);
                }
            }
            "#,
        );
        assert!(!findings.iter().any(|f| f.rule == "missing-require-auth"));
    }

    #[test]
    fn fully_qualified_soroban_sdk_require_auth_is_recognized() {
        let findings = audit_source_for_test(
            r#"
            #[contractimpl]
            impl Contract {
                pub fn withdraw(env: Env, from: Address) {
                    soroban_sdk::Address::require_auth(&from);
                }
            }
            "#,
        );
        assert!(!findings.iter().any(|f| f.rule == "missing-require-auth"));
    }

    #[test]
    fn qualified_auth_module_require_auth_is_recognized() {
        let findings = audit_source_for_test(
            r#"
            #[contractimpl]
            impl Contract {
                pub fn withdraw(env: Env, from: Address) {
                    auth::require_auth(&from);
                }
            }
            "#,
        );
        assert!(!findings.iter().any(|f| f.rule == "missing-require-auth"));
    }

    #[test]
    fn qualified_require_auth_for_args_is_recognized() {
        let findings = audit_source_for_test(
            r#"
            #[contractimpl]
            impl Contract {
                pub fn withdraw(env: Env, from: Address, amount: i128) {
                    assert!(amount > 0);
                    Address::require_auth_for_args(&from, (&amount,));
                }
            }
            "#,
        );
        assert!(!findings.iter().any(|f| f.rule == "missing-require-auth"));
    }

    // ---- #430: do not treat a negated auth branch as an enforced check ----

    #[test]
    fn negated_if_condition_auth_branch_is_not_treated_as_enforced() {
        let findings = audit_source_for_test(
            r#"
            #[contractimpl]
            impl Contract {
                pub fn withdraw(env: Env, from: Address) {
                    if !condition {
                        from.require_auth();
                    }
                }
            }
            "#,
        );
        assert!(findings.iter().any(|f| f.rule == "missing-require-auth"));
    }

    #[test]
    fn negated_qualified_auth_branch_is_not_treated_as_enforced() {
        let findings = audit_source_for_test(
            r#"
            #[contractimpl]
            impl Contract {
                pub fn withdraw(env: Env, from: Address) {
                    if !is_admin {
                        Address::require_auth(&from);
                    }
                }
            }
            "#,
        );
        assert!(findings.iter().any(|f| f.rule == "missing-require-auth"));
    }

    #[test]
    fn negated_boolean_comparison_branch_is_not_treated_as_enforced() {
        let findings = audit_source_for_test(
            r#"
            #[contractimpl]
            impl Contract {
                pub fn withdraw(env: Env, from: Address) {
                    if condition == false {
                        from.require_auth();
                    }
                }
            }
            "#,
        );
        assert!(findings.iter().any(|f| f.rule == "missing-require-auth"));
    }

    #[test]
    fn unconditional_auth_outside_negated_branch_is_enforced() {
        let findings = audit_source_for_test(
            r#"
            #[contractimpl]
            impl Contract {
                pub fn withdraw(env: Env, from: Address) {
                    from.require_auth();
                    if !condition {
                        let _ = 1;
                    }
                }
            }
            "#,
        );
        assert!(!findings.iter().any(|f| f.rule == "missing-require-auth"));
    }

    // ---- #429: keep repeated runs byte-for-byte deterministic ----

    #[test]
    fn collect_rust_sources_orders_files_deterministically() {
        let root = std::env::temp_dir().join(format!("stk-audit-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/z.rs"), "pub fn z() {}").unwrap();
        std::fs::write(root.join("src/a.rs"), "pub fn a() {}").unwrap();
        std::fs::write(root.join("src/m.rs"), "pub fn m() {}").unwrap();

        let sources1 = collect_rust_sources(&root, &mut Vec::new()).unwrap();
        let sources2 = collect_rust_sources(&root, &mut Vec::new()).unwrap();
        assert_eq!(sources1, sources2);
        assert_eq!(
            sources1,
            vec![
                root.join("src/a.rs"),
                root.join("src/m.rs"),
                root.join("src/z.rs"),
            ]
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn diagnostic_rendering_is_deterministic_across_repeated_calls() {
        let findings = vec![
            Finding {
                file: PathBuf::from("src/z.rs"),
                line: 50,
                rule: "missing-require-auth",
                severity: "warning".to_string(),
                message: "z issue".to_string(),
            },
            Finding {
                file: PathBuf::from("src/a.rs"),
                line: 10,
                rule: "missing-ttl-bump",
                severity: "warning".to_string(),
                message: "a issue".to_string(),
            },
            Finding {
                file: PathBuf::from("src/a.rs"),
                line: 5,
                rule: "missing-require-auth",
                severity: "warning".to_string(),
                message: "a auth".to_string(),
            },
        ];

        let text1 = render_text(&findings);
        let text2 = render_text(&findings);
        assert_eq!(text1, text2);

        let json1 = render_json(&findings).unwrap();
        let json2 = render_json(&findings).unwrap();
        assert_eq!(json1, json2);

        let sarif1 = render_sarif(&findings).unwrap();
        let sarif2 = render_sarif(&findings).unwrap();
        assert_eq!(sarif1, sarif2);

        // Verify sorted order: src/a.rs line 5 comes before line 10, which comes before src/z.rs line 50
        let a_auth_pos = text1.find("src/a.rs:5").unwrap();
        let a_ttl_pos = text1.find("src/a.rs:10").unwrap();
        let z_pos = text1.find("src/z.rs:50").unwrap();
        assert!(a_auth_pos < a_ttl_pos);
        assert!(a_ttl_pos < z_pos);
    }
}
