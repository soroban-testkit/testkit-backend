use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand, ValueEnum};
use proc_macro2::{Group, TokenStream, TokenTree};
use quote::ToTokens;
use serde::{Deserialize, Serialize};
use syn::ext::IdentExt;
use syn::visit::{self, Visit};
use syn::{BinOp, Expr, FnArg, ImplItemFn, ItemFn, Local, Pat, Signature, Type};
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
    /// descended into): `target/`, `vendor/`, `node_modules/`, and any hidden
    /// directory (its name begins with `.`, e.g. `.git/`, `.github/`, `.cargo/`). A
    /// missing path is reported as an actionable error rather than an empty
    /// "no findings" result. Explicitly selecting an excluded directory or
    /// one of its files still scans that input. Empty Rust files are valid
    /// and produce no findings.
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
    pub column: usize,
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
        self.file.to_string_lossy() == entry.file
            && self.line == entry.line
            && self.rule == entry.rule
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
/// prune `target/`, `vendor/`, `node_modules/`, and every hidden directory
/// (a name beginning with `.`, such as `.git/` or `.github/`) via
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
pub const AUDIT_EXCLUDED_DIRS: &[&str] = &["target", "vendor", "node_modules"];

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

    /// Render compiler-style human-readable text diagnostics.
    pub fn render_text(&self) -> String {
        if self.findings.is_empty() {
            return format!("audit: no findings\nnote: {MACRO_SOURCE_LIMITATION}\n");
        }

        let mut output = String::new();
        for finding in self.findings {
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
        serde_json::to_string_pretty(&serde_json::json!({
            "tool": "soroban-testkit",
            "version": 1,
            "findings": self.findings,
            "summary": {
                "finding_count": self.findings.len(),
                "limitations": [MACRO_SOURCE_LIMITATION]
            }
        }))
        .map(|report| format!("{report}\n"))
        .map_err(|err| CliError(format!("failed to serialize JSON audit report: {err}")))
    }

    /// Render SARIF 2.1.0 output for code scanning integration.
    pub fn render_sarif(&self) -> Result<String, CliError> {
        let mut rule_ids: Vec<_> = self.findings.iter().map(|finding| finding.rule).collect();
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
        let results: Vec<_> = self
            .findings
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
                            "region": { "startLine": finding.line.max(1), "startColumn": finding.column.max(1) }
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

pub fn render_text(findings: &[Finding]) -> String {
    DiagnosticRenderer::new(findings).render_text()
}

pub fn render_json(findings: &[Finding]) -> Result<String, CliError> {
    DiagnosticRenderer::new(findings).render_json()
}

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
        source: full_src,
        current_column: 1,
    };
    visitor.visit_file(&file);
    findings.sort_by(|left, right| {
        left.file
            .cmp(&right.file)
            .then(left.line.cmp(&right.line))
            .then(left.column.cmp(&right.column))
            .then(left.rule.cmp(right.rule))
    });
    findings.dedup_by(|left, right| {
        left.file == right.file
            && left.line == right.line
            && left.column == right.column
            && left.rule == right.rule
    });
    Ok(())
}

struct Suppression {
    line: usize,
    rules: Vec<String>,
    #[allow(dead_code)]
    reason: Option<String>,
}

fn extract_suppressions(src: &str) -> Vec<Suppression> {
    let mut suppressions = Vec::new();
    for (idx, line) in src.lines().enumerate() {
        if let Some(comment_start) = line.find("soroban_testkit::audit(skip") {
            if let Some(comment_text) = line[comment_start..].split_once('(') {
                if let Some(end_paren) = comment_text.1.rfind(')') {
                    let content = &comment_text.1[..end_paren];
                    let mut rules = Vec::new();
                    let mut reason = None;

                    for part in content.split(',') {
                        let part = part.trim();
                        if let Some(stripped) = part.strip_prefix("reason=") {
                            reason = Some(stripped.trim_matches('"').to_string());
                        } else if !part.is_empty() && part != "skip" {
                            rules.push(part.to_string());
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
        }
    }
    suppressions
}

struct FunctionVisitor<'a> {
    path: PathBuf,
    findings: &'a mut Vec<Finding>,
    config: &'a AuditConfig,
    suppressions: &'a [Suppression],
    source: &'a str,
    current_column: usize,
}

impl<'ast> Visit<'ast> for FunctionVisitor<'_> {
    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        let (line, column) = position_of(node, self.source);
        self.current_column = column;
        let fn_name = node.sig.ident.to_string();
        let body_src = node.to_token_stream().to_string();
        self.audit_function(&node.sig, &node.block, body_src.clone(), line);
        let body_src = normalized_tokens(node.to_token_stream()).to_string();
        self.audit_function(&node.sig, body_src.clone(), line);
        self.audit_i128_arithmetic(&node.sig, &node.block, line);
        self.audit_ignored_transfer_results(&node.sig, &node.block);
        self.audit_mock_all_auths(&node.block, line, &fn_name);
        visit::visit_item_fn(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
        let (line, column) = position_of(node, self.source);
        self.current_column = column;
        let fn_name = node.sig.ident.to_string();
        let body_src = node.to_token_stream().to_string();
        self.audit_function(&node.sig, &node.block, body_src.clone(), line);
        let body_src = normalized_tokens(node.to_token_stream()).to_string();
        self.audit_function(&node.sig, body_src.clone(), line);
        self.audit_i128_arithmetic(&node.sig, &node.block, line);
        self.audit_ignored_transfer_results(&node.sig, &node.block);
        self.audit_mock_all_auths(&node.block, line, &fn_name);
        visit::visit_impl_item_fn(self, node);
    }
}

// Raw spelling does not change an identifier's meaning. Normalize identifiers
// for the token-based heuristics without altering literals or source spans.
fn normalized_tokens(tokens: TokenStream) -> TokenStream {
    tokens
        .into_iter()
        .map(|token| match token {
            TokenTree::Ident(ident) => TokenTree::Ident(ident.unraw()),
            TokenTree::Group(group) => {
                let mut normalized =
                    Group::new(group.delimiter(), normalized_tokens(group.stream()));
                normalized.set_span(group.span());
                TokenTree::Group(normalized)
            }
            other => other,
        })
        .collect()
}

impl FunctionVisitor<'_> {
    fn audit_function(
        &mut self,
        sig: &Signature,
        block: &syn::Block,
        body_src: String,
        line: usize,
    ) {
        let name = sig.ident.to_string();

        // Rule 1: an Address parameter that's never checked with require_auth.
        if self.is_enabled("missing-require-auth") {
            let address_params = address_typed_params(sig);
            if !address_params.is_empty() && !contains_method_call(block, "require_auth") {
                self.push(
                    line,
                    "missing-require-auth",
                    "warning",
                    format!(
                        "fn {name} takes Address parameter(s) {address_params:?} but never calls \
                         require_auth in its body"
                    ),
                );
            }
        }

        // Rule 3: a storage read with no extend_ttl anywhere in the function.
        if self.is_enabled("missing-ttl-bump") {
            let reads_persistent = body_src.contains(". persistent () . get (");
            let reads_temporary = body_src.contains(". temporary () . get (");
            if (reads_persistent || reads_temporary) && !body_src.contains(". extend_ttl (") {
                self.push(
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
                );
            }
        }

        if self.is_enabled("missing-positive-amount-validation") {
            for amount_param in signed_amount_params(sig) {
                if !has_positive_amount_validation(&body_src, &amount_param) {
                    self.push(
                        line,
                        "missing-positive-amount-validation",
                        "warning",
                        format!(
                            "fn {name} takes signed amount parameter `{amount_param}` but never \
                             validates it against zero"
                        ),
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
                    self.push(
                        line,
                        "unchecked-timestamp-arithmetic",
                        "warning",
                        format!(
                            "fn {name} performs arithmetic on timestamp values; use checked arithmetic to prevent overflow"
                        ),
                    );
                }
            }
        }
    }

    fn audit_mock_all_auths(
        &mut self,
        block: &syn::Block,
        line: usize,
        fn_name: &str,
    ) {
        if !self.is_enabled("broad-mock-all-auths") {
            return;
        }

        let is_test = fn_name.contains("test");
        if !is_test {
            return;
        }

        if contains_method_call(block, "mock_all_auths") {
            self.push(
                line,
                "broad-mock-all-auths",
                "warning",
                format!(
                    "test function {fn_name} uses mock_all_auths; prefer granular auth checks per caller"
                ),
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

    fn push(
        &mut self,
        line: usize,
        rule: &'static str,
        default_severity: &'static str,
        message: String,
    ) {
        if !self.is_enabled(rule) {
            return;
        }

        // Check if this finding is suppressed
        for suppression in self.suppressions {
            if (suppression.line == line || suppression.line == line - 1)
                && (suppression.rules.contains(&"*".to_string())
                    || suppression.rules.iter().any(|r| r == rule))
            {
                return;
            }
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
            column: self.current_column,
            rule,
            severity: severity.to_string(),
            message,
        });
    }

    fn audit_i128_arithmetic(&mut self, sig: &Signature, body: &syn::Block, line: usize) {
        if !self.is_enabled("unchecked-i128-arithmetic") {
            return;
        }

        let mut i128_bindings: HashSet<String> = HashSet::new();
        for arg in &sig.inputs {
            if let FnArg::Typed(pat_type) = arg {
                if type_is_i128(&pat_type.ty) {
                    if let Pat::Ident(ident) = &*pat_type.pat {
                        i128_bindings.insert(ident.ident.unraw().to_string());
                    }
                }
            }
        }

        let mut visitor = I128ArithmeticVisitor {
            path: self.path.clone(),
            fn_name: sig.ident.to_string(),
            fn_line: line,
            fn_column: self.current_column,
            i128_bindings,
            findings: self.findings,
            config: self.config,
            suppressions: self.suppressions,
            source: self.source,
        };
        visitor.visit_block(body);
    }

    fn audit_ignored_transfer_results(&mut self, sig: &Signature, body: &syn::Block) {
        if !self.is_enabled("ignored-token-transfer-result") {
            return;
        }

        let mut visitor = IgnoredTransferVisitor {
            path: self.path.clone(),
            fn_name: sig.ident.to_string(),
            findings: self.findings,
            config: self.config,
            suppressions: self.suppressions,
            source: self.source,
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
                        Some(ident.ident.unraw().to_string())
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
                ident.unraw().to_string().as_str(),
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
            .is_some_and(|seg| seg.ident.unraw() == "Address"),
        _ => false,
    }
}

fn position_of(node: impl quote::ToTokens, source: &str) -> (usize, usize) {
    node.to_token_stream()
        .into_iter()
        .next()
        .map(|token| {
            let start = token.span().start();
            (start.line, source.lines().nth(start.line.saturating_sub(1)).map(|line| line[..start.column.min(line.len())].chars().count() + 1).unwrap_or(1))
        })
        .unwrap_or((0, 1))
}

fn contains_method_call(block: &syn::Block, method: &str) -> bool {
    struct MethodCallVisitor<'a> {
        method: &'a str,
        found: bool,
    }

    impl<'ast> Visit<'ast> for MethodCallVisitor<'_> {
        fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
            if node.method == self.method {
                self.found = true;
            }
            visit::visit_expr_method_call(self, node);
        }
    }

    let mut visitor = MethodCallVisitor {
        method,
        found: false,
    };
    visitor.visit_block(block);
    visitor.found
}

struct IgnoredTransferVisitor<'a> {
    path: PathBuf,
    fn_name: String,
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
            let (line, column) = position_of(expr, self.source);
            let is_suppressed = self.suppressions.iter().any(|s| {
                (s.line == line || s.line == line - 1)
                    && (s.rules.contains(&"*".to_string())
                        || s.rules
                            .contains(&"ignored-token-transfer-result".to_string()))
            });

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
                    column,
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
                call.method.unraw().to_string().as_str(),
                "transfer" | "transfer_from" | "try_transfer" | "try_transfer_from"
            ) =>
        {
            Some(match call.method.unraw().to_string().as_str() {
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
    fn_column: usize,
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
                    self.i128_bindings.insert(ident.ident.unraw().to_string());
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
            let is_suppressed = self.suppressions.iter().any(|s| {
                (s.line == self.fn_line || s.line == self.fn_line - 1)
                    && (s.rules.contains(&"*".to_string())
                        || s.rules.contains(&"unchecked-i128-arithmetic".to_string()))
            });

            if !is_suppressed {
                let severity = self
                    .config
                    .rules
                    .get("unchecked-i128-arithmetic")
                    .and_then(|r| r.severity.as_deref())
                    .unwrap_or("warning");

                self.findings.push(Finding {
                    file: self.path.clone(),
                    line: self.fn_line,
                    column: self.fn_column,
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
                .is_some_and(|ident| self.i128_bindings.contains(&ident.unraw().to_string())),
            Expr::Lit(lit) => matches!(&lit.lit, syn::Lit::Int(i) if i.suffix() == "i128"),
            Expr::Paren(p) => self.touches_i128(&p.expr),
            Expr::Group(g) => self.touches_i128(&g.expr),
            _ => false,
        }
    }
}

fn type_is_i128(ty: &Type) -> bool {
    matches!(ty, Type::Path(p) if p.path.get_ident().is_some_and(|ident| ident.unraw() == "i128"))
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
            source: code,
            current_column: 1,
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
            source: code,
            current_column: 1,
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
            source: code,
            current_column: 1,
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
            source: code,
            current_column: 1,
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
            source: code,
            current_column: 1,
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
            column: 1,
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
}
