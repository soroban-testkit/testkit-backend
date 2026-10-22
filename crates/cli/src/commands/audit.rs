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

/// How to read every finding this scanner emits. Each rule is a syntactic
/// heuristic: a finding means the source matched a pattern, which is a
/// signal for manual review — not a definitive defect report — and a
/// missing finding is never a security guarantee.
pub const FINDING_NATURE: &str =
    "heuristic: each finding is a pattern-match signal for manual review, possibly a false positive; it is not a definitive defect report and a clean scan is not a security guarantee";

/// How to read a parse warning as distinct from a finding. A **finding** is
/// a heuristic pattern match over source that parsed successfully: it is
/// rendered in the report, counted in `summary.finding_count`, and is the
/// only thing `--strict` fails on. A **parse warning** means an input could
/// not be read or parsed, so it was never scanned at all: warnings go to
/// stderr only, never count as findings, and make the audit incomplete
/// rather than a strict-mode failure.
pub const PARSE_WARNING_NATURE: &str =
    "parse warnings are not findings: an input that could not be read or parsed was skipped without being scanned; warnings are reported on stderr only, never counted in finding_count, and never trigger --strict; they make the audit incomplete instead";

/// Single mapping from a rule severity to a SARIF reporting level. Both the
/// `rules` metadata and the `results` entries (and the `audit rules`
/// machine-readable listing) route through this one function, so the
/// severity shown in JSON and text can never disagree with the level SARIF
/// consumers see.
fn sarif_level(severity: &str) -> &'static str {
    match severity {
        "error" => "error",
        "warning" => "warning",
        _ => "note",
    }
}

/// Rewrites a source line into a safe snippet: ANSI terminal escapes
/// (e.g. `ESC[31m`) are removed and control characters collapse to a
/// single space, so copying an arbitrary source line into a report cannot
/// inject output or mangle the surrounding text.
fn sanitize_snippet(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // Skip the rest of a CSI-style escape sequence (ESC [ params
            // final-byte). Other ESC forms simply drop the escape byte.
            if chars.peek() == Some(&'[') {
                for skipped in chars.by_ref() {
                    if skipped.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        if c.is_control() {
            if !out.ends_with(' ') {
                out.push(' ');
            }
        } else {
            out.push(c);
        }
    }
    out.trim_end().to_string()
}

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
    /// Findings and baseline entries use workspace-relative paths, and
    /// findings are reported in one canonical order: file, then line, then
    /// rule, then message. Config is discovered from the source directory
    /// up to the workspace root (or from the current directory for stdin).
    /// Files that cannot be read or parsed produce a *parse warning* on
    /// stderr — an input skipped, never scanned — not a finding: warnings
    /// never appear in the report, never count toward `finding_count`, and
    /// never trigger `--strict`. Other files are scanned, then the command
    /// exits non-zero to indicate an incomplete audit.
    ///
    /// This static scan does not expand procedural macros or macro_rules!
    /// invocations. Inspect generated code separately (e.g. cargo expand).
    ///
    /// Every path the command compares or prints is normalized through one
    /// canonical routine first (symlinks and `.`/`..` components resolved),
    /// so one file always reports under a single workspace-relative
    /// spelling no matter how it was spelled on the command line.
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
    /// The offending source line, filled from the file/stdin text when the
    /// finding is created. Terminal escape sequences are stripped and
    /// control characters collapsed at render time, so this field is
    /// stored raw. `None` when no source text was available.
    pub source_snippet: Option<String>,
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
///
/// Every finding is a syntactic pattern match, not proof of a defect
/// ([`FINDING_NATURE`]). The heuristic nature is spelled out wherever users
/// read reports: the closing `note` of text diagnostics, the `limitations`
/// section of JSON and SARIF reports, the `audit rules` listing, and
/// `audit explain <rule>`. Rule severities map to SARIF levels through one
/// shared function (`sarif_level`), so text, JSON and SARIF can never
/// disagree about the same finding.
pub fn run(args: AuditArgs) -> Result<(), CliError> {
    match &args.command {
        Some(AuditCommand::Explain { rule }) => return explain_rule(rule),
        Some(AuditCommand::Rules) => return list_rules(args.format),
        None => {}
    }

    let stdin = args.path == Path::new("-");
    let mut parse_warnings = Vec::new();
    let sources = if stdin {
        Vec::new()
    } else {
        collect_rust_sources(&args.path, &mut parse_warnings)?
    };
    let source_dir = if stdin {
        canonical_path(&std::env::current_dir()?)?
    } else {
        let root = canonical_path(&args.path)?;
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
            parse_warnings.push(err);
        }
    } else {
        for path in sources {
            if let Err(err) = audit_file(&path, &mut findings, &config) {
                parse_warnings.push(err);
            }
        }
        for finding in &mut findings {
            finding.file = workspace_relative_path(&finding.file, &workspace);
        }
    }

    findings.retain(|f| !baseline.iter().any(|b| f.matches_baseline(b)));
    // Paths are final now, so this is the ordering every rendered report
    // is guaranteed to follow.
    normalize_findings(&mut findings);

    print!("{}", render_findings(&findings, args.format)?);

    // Findings (heuristic pattern matches over parsed source) were just
    // rendered above; these are parse warnings (inputs that were never
    // scanned), and the two are reported through different channels on
    // purpose so a skipped file can never be mistaken for a finding.
    if !parse_warnings.is_empty() {
        for warning in &parse_warnings {
            eprintln!("audit: warning: {warning}");
        }
        eprintln!("audit: note: {PARSE_WARNING_NATURE}");
        return Err(CliError(format!(
            "audit incomplete: {} parse warning(s); findings cover only successfully parsed files; \
             fix the reported inputs and re-run",
            parse_warnings.len()
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
fn collect_rust_sources(
    root: &Path,
    warnings: &mut Vec<CliError>,
) -> Result<Vec<PathBuf>, CliError> {
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
                warnings.push(CliError(format!(
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
    // Every message above describes a syntactic pattern, not a proven
    // defect: state the heuristic nature right next to the rule so the
    // distinction between "flagged" and "broken" is never left to guesswork.
    println!("{explanation}\n\nnote: {FINDING_NATURE}");
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
            println!("note: {FINDING_NATURE}");
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
                "summary": {
                    "rule_count": rules.len(),
                    "limitations": [FINDING_NATURE]
                },
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
                            "level": sarif_level(severity)
                        },
                    })
                })
                .collect();
            let report = serde_json::json!({
                "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
                "version": "2.1.0",
                "runs": [{
                    "tool": { "driver": { "name": "soroban-testkit", "rules": rules } },
                    "results": [],
                    "properties": { "limitations": [FINDING_NATURE] }
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

/// Present non-UTF-8 names unambiguously rather than replacing bytes with U+FFFD.
/// The standard Path::display() uses a lossy conversion for such names.
fn display_audit_path(path: &Path) -> String {
    path.to_str()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{path:?}"))
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
    ///
    /// Every finding carries the offending source line (`-->`), with ANSI
    /// escape sequences stripped and control characters collapsed so a
    /// copied snippet can never inject terminal output. Each finding is a
    /// heuristic signal, spelled out in the closing note.
    pub fn render_text(&self) -> String {
        if self.findings.is_empty() {
            return format!(
                "audit: no findings\nnote: {MACRO_SOURCE_LIMITATION}\nnote: {FINDING_NATURE}\n"
            );
        }

        let mut output = String::new();
        for finding in self.findings {
            output.push_str(&format!(
                "{}:{}: [{}] {}: {}\n",
                display_audit_path(&finding.file),
                finding.line,
                finding.severity,
                finding.rule,
                finding.message
            ));
            if let Some(snippet) = &finding.source_snippet {
                output.push_str(&format!("    --> {}\n", sanitize_snippet(snippet)));
            }
        }
        output.push_str(&format!(
            "\n{} finding(s). soroban-testkit audit is a linter with seven heuristics, not a \
             security product — a missing finding is not a security guarantee.\n",
            self.findings.len()
        ));
        output.push_str(&format!("note: {FINDING_NATURE}\n"));
        output.push_str(&format!("note: {MACRO_SOURCE_LIMITATION}\n"));
        output
    }

    /// Render stable, machine-readable JSON report. Source snippets are
    /// sanitized before serialization so no terminal control sequence
    /// survives transport.
    pub fn render_json(&self) -> Result<String, CliError> {
        let findings: Vec<_> = self
            .findings
            .iter()
            .map(|finding| {
                let mut value = serde_json::to_value(finding)
                    .expect("finding serializes despite sanitized snippet");
                if let Some(snippet) = &finding.source_snippet {
                    value["source_snippet"] = serde_json::Value::String(sanitize_snippet(snippet));
                }
                value
            })
            .collect();
        serde_json::to_string_pretty(&serde_json::json!({
            "tool": "soroban-testkit",
            "version": 1,
            "findings": findings,
            "summary": {
                "finding_count": self.findings.len(),
                "limitations": [MACRO_SOURCE_LIMITATION, FINDING_NATURE]
            }
        }))
        .map(|report| format!("{report}\n"))
        .map_err(|err| CliError(format!("failed to serialize JSON audit report: {err}")))
    }

    /// Render SARIF 2.1.0 output for code scanning integration.
    ///
    /// Rule metadata and result levels both map through [`sarif_level`], so
    /// a severity shown in the `rules` section can never disagree with the
    /// `level` on its `results`. Region snippets are sanitized before
    /// embedding.
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
                // The metadata level mirrors the level this scan gives the
                // rule's findings: a config override takes effect, and with
                // nothing configured the `RULES` default applies — both walk
                // through the shared `sarif_level` mapping, so the metadata
                // level always matches how `results` display the same rule.
                let effective_severity = self
                    .findings
                    .iter()
                    .find(|finding| finding.rule == rule)
                    .map(|finding| finding.severity.as_str())
                    .or_else(|| {
                        RULES
                            .iter()
                            .find(|(id, _, _)| *id == rule)
                            .map(|(_, severity, _)| *severity)
                    });
                let mut rule_obj = serde_json::json!({
                    "id": rule,
                    "shortDescription": { "text": rule.replace('-', " ") }
                });
                if let Some(desc) = description {
                    rule_obj["fullDescription"] = serde_json::json!({ "text": desc });
                }
                if let Some(severity) = effective_severity {
                    rule_obj["defaultConfiguration"] =
                        serde_json::json!({ "level": sarif_level(severity) });
                }
                rule_obj
            })
            .collect();
        let results: Vec<_> = self
            .findings
            .iter()
            .map(|finding| {
                let region = match &finding.source_snippet {
                    Some(snippet) => serde_json::json!({
                        "startLine": finding.line.max(1),
                        "snippet": { "text": sanitize_snippet(snippet) }
                    }),
                    None => serde_json::json!({ "startLine": finding.line.max(1) }),
                };
                serde_json::json!({
                    "ruleId": finding.rule,
                    "level": sarif_level(&finding.severity),
                    "message": { "text": finding.message },
                    "locations": [{
                        "physicalLocation": {
                            "artifactLocation": { "uri": finding.file.to_string_lossy() },
                            "region": region
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
                "properties": { "limitations": [MACRO_SOURCE_LIMITATION, FINDING_NATURE] }
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

/// The one canonical path normalization routine for `audit`. The path is
/// resolved through the operating system (symlinks, `.` and `..` components,
/// redundant separators) and rebuilt from its components, so every spelling
/// of one file normalizes to a single value. The scan root, the stdin start
/// directory, finding paths, and baseline entries all normalize here, and a
/// failure carries actionable context for the caller to surface.
fn canonical_path(path: &Path) -> Result<PathBuf, CliError> {
    fs::canonicalize(path)
        .map(|resolved| resolved.components().collect())
        .map_err(|err| {
            CliError(format!(
                "failed to resolve path {}: {err}; verify the path exists and is readable",
                path.display()
            ))
        })
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
    let absolute = canonical_path(path).unwrap_or_else(|_| path.to_path_buf());
    absolute
        .strip_prefix(workspace)
        .unwrap_or(&absolute)
        .components()
        .collect()
}

/// Extract the source line and TOML table/key from a deserialization error.
/// If the parser cannot identify a key (e.g. an unterminated table), the
/// source line is still helpful alongside the path and parser diagnostic.
fn config_error_context(content: &str, err: &toml::de::Error) -> String {
    let Some(span) = err.span() else {
        return String::new();
    };
    let position = span.start.min(content.len());
    let line_index = content
        .get(..position)
        .unwrap_or("")
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count();
    let line = content.lines().nth(line_index).unwrap_or("");
    let key = line
        .split_once('=')
        .map(|(key, _)| key.trim())
        .filter(|key| !key.is_empty());
    let section = content
        .lines()
        .take(line_index + 1)
        .filter_map(|line| {
            line.trim()
                .strip_prefix('[')
                .and_then(|section| section.strip_suffix(']'))
        })
        .last()
        .unwrap_or("");
    match key {
        Some(key) if !section.is_empty() => {
            format!(" at line {} (key `{section}.{key}`)", line_index + 1)
        }
        Some(key) => format!(" at line {} (key `{key}`)", line_index + 1),
        None => format!(" at line {}", line_index + 1),
    }
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
                    "failed to parse audit config {}{} as TOML: {err}; fix the configuration syntax",
                    path.display(),
                    config_error_context(&content, &err)
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

/// The one canonical ordering for findings: file, then line, then rule,
/// then message. Exact duplicates collapse. Every
/// report path funnels through this routine, in `audit_source` per parsed
/// file and again in `run` after paths are rewritten workspace-relative, so
/// text, JSON and SARIF always list a finding in the same place regardless
/// of scan order.
fn normalize_findings(findings: &mut Vec<Finding>) {
    findings.sort_by(|left, right| {
        left.file
            .cmp(&right.file)
            .then(left.line.cmp(&right.line))
            .then(left.rule.cmp(right.rule))
            .then(left.message.cmp(&right.message))
    });
    findings.dedup_by(|left, right| {
        left.file == right.file
            && left.line == right.line
            && left.rule == right.rule
            && left.message == right.message
    });
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
    let source_lines = SourceLines::new(full_src);

    let mut visitor = FunctionVisitor {
        path: path.to_path_buf(),
        findings,
        config,
        suppressions: &suppressions,
        test_context: path
            .components()
            .any(|part| part.as_os_str() == "tests" || part.as_os_str() == "benches"),
        source: &source_lines,
    };
    visitor.visit_file(&file);
    normalize_findings(findings);
    Ok(())
}

/// Line-indexed view of the scanned source, used to attach the offending
/// line to each finding. Lines are stored exactly as written off disk or
/// stdin; sanitizing (ANSI/control-character stripping) happens at render
/// time so the stored form can still round-trip in tests of the renderer.
pub struct SourceLines(Vec<String>);

impl SourceLines {
    fn new(src: &str) -> Self {
        Self(src.lines().map(str::to_string).collect())
    }

    /// 1-based accessor matching compiler-style line numbers. Returns
    /// `None` for line 0 (when token spans are unavailable) or any line
    /// past the end of the file.
    pub fn get(&self, line: usize) -> Option<&String> {
        self.0.get(line.checked_sub(1)?)
    }
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
    (close_paren > open_paren).then_some(&after_audit[open_paren + 1..close_paren])
}

fn extract_suppressions(src: &str) -> Vec<Suppression> {
    let mut suppressions = Vec::new();
    for (idx, line) in src.lines().enumerate() {
        if let Some(content) = find_audit_directive(line) {
            let mut rules = Vec::new();
            let mut reason = None;

            for part in content.split(',') {
                let part = part.trim();
                let clean = if let Some(stripped) = part
                    .strip_prefix("reason=")
                    .or_else(|| part.strip_prefix("reason ="))
                {
                    reason = Some(
                        stripped
                            .trim()
                            .trim_matches('"')
                            .trim_matches('\'')
                            .to_string(),
                    );
                    continue;
                } else if let Some(stripped) = part
                    .strip_prefix("rule=")
                    .or_else(|| part.strip_prefix("rule ="))
                    .or_else(|| part.strip_prefix("rule:"))
                    .or_else(|| part.strip_prefix("rule :"))
                    .or_else(|| part.strip_prefix("skip-"))
                {
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
    let mut min_line = usize::MAX;
    let mut max_line = 0;
    for token in node.to_token_stream() {
        let span = token.span();
        min_line = min_line.min(span.start().line);
        max_line = max_line.max(span.end().line);
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
    suppressions.iter().any(|suppression| {
        let matches_rule = suppression
            .rules
            .iter()
            .any(|item| item == "*" || item == rule);
        matches_rule
            && (suppression.line <= 10
                || suppression.line == finding_line
                || suppression.line + 1 == finding_line
                || fn_range.is_some_and(|(start, end)| {
                    suppression.line + 1 >= start && suppression.line <= end
                }))
    })
}

fn is_require_auth_call(expr: &Expr) -> bool {
    match expr {
        Expr::MethodCall(call) => matches!(
            call.method.unraw().to_string().as_str(),
            "require_auth" | "require_auth_for_args"
        ),
        Expr::Call(call) => match &*call.func {
            Expr::Path(path) => path.path.segments.last().is_some_and(|segment| {
                matches!(
                    segment.ident.unraw().to_string().as_str(),
                    "require_auth" | "require_auth_for_args"
                )
            }),
            _ => false,
        },
        Expr::Paren(paren) => is_require_auth_call(&paren.expr),
        Expr::Group(group) => is_require_auth_call(&group.expr),
        Expr::Try(expr_try) => is_require_auth_call(&expr_try.expr),
        Expr::Await(expr_await) => is_require_auth_call(&expr_await.base),
        _ => false,
    }
}

fn is_negated_condition(expr: &Expr) -> bool {
    match expr {
        Expr::Unary(syn::ExprUnary {
            op: syn::UnOp::Not(_),
            ..
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
    matches!(expr, Expr::Lit(syn::ExprLit { lit: syn::Lit::Bool(value), .. }) if !value.value)
}

fn is_lit_true(expr: &Expr) -> bool {
    matches!(expr, Expr::Lit(syn::ExprLit { lit: syn::Lit::Bool(value), .. }) if value.value)
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
        self.negated_depth += usize::from(is_negated);
        self.visit_block(&node.then_branch);
        self.negated_depth -= usize::from(is_negated);
        if let Some((_, else_branch)) = &node.else_branch {
            self.visit_expr(else_branch);
        }
    }

    fn visit_expr_while(&mut self, node: &'ast syn::ExprWhile) {
        let is_negated = is_negated_condition(&node.cond);
        self.negated_depth += usize::from(is_negated);
        self.visit_block(&node.body);
        self.negated_depth -= usize::from(is_negated);
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

/// Returns `true` when the function body contains a call to a helper whose
/// name conventionally signals that it handles authentication or
/// authorization on behalf of the caller (e.g. `check_auth`, `verify_auth`,
/// `assert_authorized`, `require_caller`).  These are heuristic patterns:
/// the check avoids false positives for the common idiom of extracting the
/// `require_auth` call into a shared helper so it cannot be silently omitted
/// at new call sites.
///
/// The rule is intentionally conservative: only function/method calls whose
/// callee name *contains* one of the recognised stems (case-insensitive) are
/// treated as delegated auth.  A call to `check_balance` would not qualify;
/// only calls whose name overlaps with the auth-semantics vocabulary do.
fn calls_auth_helper(block: &syn::Block) -> bool {
    struct AuthHelperVisitor {
        found: bool,
    }

    impl<'ast> Visit<'ast> for AuthHelperVisitor {
        fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
            let name = node.method.unraw().to_string();
            if callee_looks_like_auth_helper(&name) {
                self.found = true;
            }
            visit::visit_expr_method_call(self, node);
        }

        fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
            if let Expr::Path(path) = &*node.func {
                if let Some(last) = path.path.segments.last() {
                    let name = last.ident.unraw().to_string();
                    if callee_looks_like_auth_helper(&name) {
                        self.found = true;
                    }
                }
            }
            visit::visit_expr_call(self, node);
        }
    }

    let mut visitor = AuthHelperVisitor { found: false };
    visitor.visit_block(block);
    visitor.found
}

/// Stems that, when present in a callee name, indicate the callee is
/// responsible for verifying the caller's identity or permission.
const AUTH_HELPER_STEMS: &[&str] = &[
    "require_auth",
    "check_auth",
    "verify_auth",
    "assert_auth",
    "ensure_auth",
    "validate_auth",
    "assert_authorized",
    "check_authorized",
    "verify_caller",
    "require_caller",
    "assert_caller",
    "check_caller",
];

fn callee_looks_like_auth_helper(name: &str) -> bool {
    let lower = name.to_lowercase();
    AUTH_HELPER_STEMS.iter().any(|stem| lower.contains(stem))
}

/// True only for cfg predicates that *require* `test` to be enabled.
/// `cfg(any(test, feature = "x"))` is not test-only: it can ship in production.
fn cfg_requires_test(meta: &syn::Meta) -> bool {
    match meta {
        syn::Meta::Path(path) => path.is_ident("test"),
        syn::Meta::List(list) if list.path.is_ident("all") || list.path.is_ident("any") => {
            let Ok(parts) = list.parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            ) else {
                return false;
            };
            if list.path.is_ident("all") {
                parts.iter().any(cfg_requires_test)
            } else {
                !parts.is_empty() && parts.iter().all(cfg_requires_test)
            }
        }
        _ => false,
    }
}

fn is_test_only(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("test")
            || (attr.path().is_ident("cfg")
                && attr
                    .parse_args::<syn::Meta>()
                    .is_ok_and(|meta| cfg_requires_test(&meta)))
    })
}

struct FunctionVisitor<'a> {
    path: PathBuf,
    findings: &'a mut Vec<Finding>,
    config: &'a AuditConfig,
    suppressions: &'a [Suppression],
    test_context: bool,
    source: &'a SourceLines,
}

impl<'ast> Visit<'ast> for FunctionVisitor<'_> {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        let previous = self.test_context;
        self.test_context |= is_test_only(&node.attrs);
        visit::visit_item_mod(self, node);
        self.test_context = previous;
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        let previous = self.test_context;
        self.test_context |= is_test_only(&node.attrs);
        visit::visit_item_impl(self, node);
        self.test_context = previous;
    }

    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        let line = line_of(node);
        let fn_range = item_line_range(node);
        let fn_name = node.sig.ident.to_string();
        let body_src = normalized_tokens(node.to_token_stream()).to_string();
        let previous = self.test_context;
        self.test_context |= is_test_only(&node.attrs);
        if !self.test_context {
            self.audit_function(&node.sig, &node.block, body_src.clone(), line, fn_range);
            self.audit_i128_arithmetic(&node.sig, &node.block, line, fn_range);
            self.audit_ignored_transfer_results(&node.sig, &node.block, fn_range);
        }
        self.audit_mock_all_auths(&body_src, line, &fn_name, fn_range);
        visit::visit_item_fn(self, node);
        self.test_context = previous;
    }

    fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
        let line = line_of(node);
        let fn_range = item_line_range(node);
        let fn_name = node.sig.ident.to_string();
        let body_src = normalized_tokens(node.to_token_stream()).to_string();
        let previous = self.test_context;
        self.test_context |= is_test_only(&node.attrs);
        if !self.test_context {
            self.audit_function(&node.sig, &node.block, body_src.clone(), line, fn_range);
            self.audit_i128_arithmetic(&node.sig, &node.block, line, fn_range);
            self.audit_ignored_transfer_results(&node.sig, &node.block, fn_range);
        }
        self.audit_mock_all_auths(&body_src, line, &fn_name, fn_range);
        visit::visit_impl_item_fn(self, node);
        self.test_context = previous;
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
        fn_range: (usize, usize),
    ) {
        let name = sig.ident.to_string();

        // Rule 1: an Address parameter that's never checked with require_auth.
        //
        // Two sources of evidence are accepted to avoid false positives:
        //   (a) a direct `require_auth` / `require_auth_for_args` call anywhere
        //       in the function body (possibly via `Address::require_auth(&addr)`),
        //   (b) a call to a helper function whose name contains a recognised
        //       auth-semantics stem (e.g. `check_auth`, `verify_caller`) — the
        //       common pattern of extracting auth into a shared helper.
        if self.is_enabled("missing-require-auth") {
            let address_params = address_typed_params(sig);
            if !address_params.is_empty()
                && !has_enforced_require_auth(block)
                && !calls_auth_helper(block)
            {
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

        // Attach the offending source line before any render step, so every
        // output format gets the same verified context. Lines that carry
        // ANSI codes or control characters stay in this raw form on the
        // struct; sanitizing happens in the renderer.
        let source_snippet = self.source.get(line).cloned();

        self.findings.push(Finding {
            file: self.path.clone(),
            line,
            rule,
            severity: severity.to_string(),
            message,
            source_snippet,
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
                        i128_bindings.insert(ident.ident.unraw().to_string());
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
            source: self.source,
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

fn line_of(node: impl quote::ToTokens) -> usize {
    node.to_token_stream()
        .into_iter()
        .next()
        .map(|token| token.span().start().line)
        .unwrap_or(0)
}

struct IgnoredTransferVisitor<'a> {
    path: PathBuf,
    fn_name: String,
    fn_range: (usize, usize),
    findings: &'a mut Vec<Finding>,
    config: &'a AuditConfig,
    suppressions: &'a [Suppression],
    source: &'a SourceLines,
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
                    source_snippet: self.source.get(line).cloned(),
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
    fn_range: (usize, usize),
    i128_bindings: HashSet<String>,
    findings: &'a mut Vec<Finding>,
    config: &'a AuditConfig,
    suppressions: &'a [Suppression],
    source: &'a SourceLines,
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
            let is_suppressed = is_finding_suppressed(
                self.suppressions,
                "unchecked-i128-arithmetic",
                self.fn_line,
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
                    line: self.fn_line,
                    rule: "unchecked-i128-arithmetic",
                    severity: severity.to_string(),
                    message: format!(
                        "fn {} does raw arithmetic on an i128 value; prefer checked_/saturating_/ \
                         wrapping_ variants to avoid silent overflow",
                        self.fn_name
                    ),
                    source_snippet: self.source.get(self.fn_line).cloned(),
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

    /// Runs the [`FunctionVisitor`] over `code` once; the handful of
    /// visitor-level tests share it so a visitor change cannot silently
    /// update five different call sites.
    fn unit_findings(code: &str, config: &AuditConfig) -> Vec<Finding> {
        let mut findings = Vec::new();
        let file = syn::parse_file(code).unwrap();
        let suppressions = Vec::new();
        let source_lines = SourceLines::new(code);
        let mut visitor = FunctionVisitor {
            path: PathBuf::from("test.rs"),
            findings: &mut findings,
            config,
            suppressions: &suppressions,
            source: &source_lines,
            test_context: false,
        };
        visitor.visit_file(&file);
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
    fn helper_function_performing_auth_suppresses_false_positive() {
        // The entry point delegates require_auth to a helper — a very common
        // pattern for sharing auth logic across entry points.  The scanner
        // must not flag the entry point as missing auth.
        let findings = audit_source_for_test(
            r#"
            fn check_auth(caller: &Address) {
                caller.require_auth();
            }

            #[contractimpl]
            impl Contract {
                pub fn withdraw(env: Env, from: Address) {
                    check_auth(&from);
                    let balance: i128 = env.storage().instance().get(&"bal").unwrap_or(0);
                    env.storage().instance().set(&"bal", &(balance - 1));
                }
            }
            "#,
        );
        assert!(
            !findings.iter().any(|f| f.rule == "missing-require-auth"),
            "false positive: entry point delegates to check_auth helper but was still flagged; \
             findings: {findings:?}"
        );
    }

    #[test]
    fn auth_helper_stems_are_recognised_for_various_naming_conventions() {
        // Covers verify_auth, assert_authorized, require_caller, etc.
        for helper_call in &[
            "verify_auth(&from);",
            "assert_authorized(&from);",
            "require_caller(&from);",
            "self.verify_caller(&from);",
            "ctx.check_auth(&from);",
        ] {
            let source = format!(
                r#"
                #[contractimpl]
                impl Contract {{
                    pub fn transfer(env: Env, from: Address) {{
                        {helper_call}
                    }}
                }}
                "#
            );
            let findings = audit_source_for_test(&source);
            assert!(
                !findings.iter().any(|f| f.rule == "missing-require-auth"),
                "false positive for helper call `{helper_call}`; findings: {findings:?}"
            );
        }
    }

    #[test]
    fn unrelated_helper_call_does_not_suppress_missing_auth_finding() {
        // Calling a helper named `process_payment` must NOT suppress the
        // missing-require-auth finding; only auth-semantics helpers qualify.
        let findings = audit_source_for_test(
            r#"
            #[contractimpl]
            impl Contract {
                pub fn withdraw(env: Env, from: Address) {
                    process_payment(&from);
                }
            }
            "#,
        );
        assert!(
            findings.iter().any(|f| f.rule == "missing-require-auth"),
            "missing-require-auth should still fire when only an unrelated helper is called; \
             findings: {findings:?}"
        );
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
        let findings = unit_findings(
            r#"
            pub fn withdraw(env: &Env, caller: &Address, amount: i128) {
                env.storage().persistent().set(&"amount", &amount);
            }
        "#,
            &AuditConfig::default(),
        );
        assert!(!findings.is_empty());
        assert_eq!(findings[0].rule, "missing-require-auth");
    }

    #[test]
    fn test_mock_all_auths_detected() {
        let findings = unit_findings(
            r#"
            #[test]
            fn test_broad_auth() {
                let env = Env::default();
                env.mock_all_auths();
                env.invoke_contract(&contract, &Symbol::new(&env, "transfer"), &args);
            }
        "#,
            &AuditConfig::default(),
        );
        assert!(
            findings.iter().any(|f| f.rule == "broad-mock-all-auths"),
            "Expected to find broad-mock-all-auths finding"
        );
    }

    #[test]
    fn test_rule_disabled_skips_check() {
        let mut rules = HashMap::new();
        rules.insert(
            "missing-require-auth".to_string(),
            RuleConfig {
                enabled: false,
                severity: None,
            },
        );
        let config = AuditConfig { rules };
        let findings = unit_findings(
            r#"
            pub fn withdraw(env: &Env, caller: &Address, amount: i128) {
                env.storage().persistent().set(&"amount", &amount);
            }
        "#,
            &config,
        );
        assert!(
            findings.iter().all(|f| f.rule != "missing-require-auth"),
            "Expected no missing-require-auth findings when disabled"
        );
    }

    #[test]
    fn test_severity_override_applied() {
        let mut rules = HashMap::new();
        rules.insert(
            "missing-require-auth".to_string(),
            RuleConfig {
                enabled: true,
                severity: Some("error".to_string()),
            },
        );
        let config = AuditConfig { rules };
        let findings = unit_findings(
            r#"
            pub fn withdraw(env: &Env, caller: &Address, amount: i128) {
                env.storage().persistent().set(&"amount", &amount);
            }
        "#,
            &config,
        );
        let auth_findings: Vec<_> = findings
            .iter()
            .filter(|f| f.rule == "missing-require-auth")
            .collect();
        assert!(!auth_findings.is_empty());
        assert_eq!(auth_findings[0].severity, "error");
    }

    #[test]
    fn test_i128_arithmetic_detection() {
        let findings = unit_findings(
            r#"
            pub fn calculate(amount: i128) -> i128 {
                amount + 100
            }
        "#,
            &AuditConfig::default(),
        );
        assert!(
            findings
                .iter()
                .any(|f| f.rule == "unchecked-i128-arithmetic"),
            "Expected to find unchecked-i128-arithmetic finding"
        );
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
            source_snippet: None,
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

    // ---- #482: findings disclose when they are heuristic, not definitive ----

    #[test]
    fn rendered_outputs_document_the_heuristic_nature_of_findings() {
        let findings = audit_source_for_test("pub fn deposit(amount: i128) { save(amount); }");
        let text = render_findings(&findings, AuditOutputFormat::Text).unwrap();
        assert!(text.contains("note: heuristic"), "{text}");

        let json = render_findings(&findings, AuditOutputFormat::Json).unwrap();
        let report: serde_json::Value = serde_json::from_str(&json).unwrap();
        let limitations = report["summary"]["limitations"].as_array().unwrap();
        assert!(
            limitations
                .iter()
                .any(|l| l.as_str().unwrap().contains("heuristic")),
            "{limitations:?}"
        );

        let sarif = render_findings(&findings, AuditOutputFormat::Sarif).unwrap();
        let report: serde_json::Value = serde_json::from_str(&sarif).unwrap();
        let limitations = report["runs"][0]["properties"]["limitations"]
            .as_array()
            .unwrap();
        assert!(
            limitations
                .iter()
                .any(|l| l.as_str().unwrap().contains("heuristic")),
            "{limitations:?}"
        );
    }

    #[test]
    fn clean_reports_still_disclose_the_heuristic_limitation() {
        let text = render_text(&[]);
        assert!(text.contains("audit: no findings"), "{text}");
        assert!(text.contains("macro_rules!"), "{text}");
        assert!(text.contains("heuristic"), "{text}");
    }

    // ---- #483: severity mapping is identical across output formats ----

    #[test]
    fn sarif_level_mapping_is_total_and_shared() {
        assert_eq!(sarif_level("error"), "error");
        assert_eq!(sarif_level("warning"), "warning");
        assert_eq!(sarif_level("note"), "note");
        assert_eq!(sarif_level(""), "note");
        assert_eq!(sarif_level("critical"), "note");
    }

    #[test]
    fn rule_metadata_and_results_agree_for_unconfigured_rules() {
        let findings = audit_source_for_test("pub fn deposit(amount: i128) { save(amount); }");
        for severity in findings.iter().map(|f| f.severity.as_str()) {
            assert_eq!(severity, "warning");
            assert_eq!(sarif_level(severity), "warning");
        }
    }

    // ---- #484: source snippets without terminal control sequences ----

    #[test]
    fn text_and_json_render_snippets_that_stripped_terminal_escapes() {
        let source = "pub fn deposit(env: &Env, from: &Address, amount: i128) { \
                      save(from, amount); } // \u{1b}[31mbold\u{1b}[0m";
        let findings = audit_source_for_test(source);

        let text = render_findings(&findings, AuditOutputFormat::Text).unwrap();
        assert!(
            text.contains("    --> pub fn deposit(env: &Env, from: &Address, amount: i128)"),
            "{text}"
        );
        assert!(text.contains("// bold"), "{text}");
        assert!(!text.contains('\u{1b}'), "{text}");

        let json = render_findings(&findings, AuditOutputFormat::Json).unwrap();
        let report: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            report["findings"][0]["source_snippet"],
            "pub fn deposit(env: &Env, from: &Address, amount: i128) { save(from, amount); } // bold"
        );
    }

    #[test]
    fn snippets_sanitize_without_losing_surrounding_text() {
        // Tab, BEL and DEL collapse; CSI sequences disappear entirely.
        let dirty = "let x = 1;\t\u{7}let y = 2;\u{7f}";
        let clean = sanitize_snippet(dirty);
        assert_eq!(clean, "let x = 1; let y = 2;");
        assert!(!clean.chars().any(char::is_control));
    }

    #[test]
    fn sarif_region_snippets_carry_only_printable_text() {
        let findings = audit_source_for_test("pub fn deposit(amount: i128) { save(amount); }");
        let sarif = render_findings(&findings, AuditOutputFormat::Sarif).unwrap();
        let report: serde_json::Value = serde_json::from_str(&sarif).unwrap();
        let snippet = report["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["region"]
            ["snippet"]["text"]
            .as_str()
            .unwrap();
        assert_eq!(snippet, "pub fn deposit(amount: i128) { save(amount); }");
        assert!(!snippet.chars().any(char::is_control));
    }

    #[test]
    fn snippets_match_the_reported_line_across_rules() {
        let source =
            "pub fn tokenbridge(token: TokenClient) { token.transfer(); token.transfer_from(); }";
        let findings = audit_source_for_test(source);
        for finding in findings
            .iter()
            .filter(|f| f.rule == "ignored-token-transfer-result")
        {
            assert_eq!(finding.source_snippet.as_deref(), Some(source));
        }
    }

    // ---- #488: one canonical path normalization routine ----

    #[test]
    fn canonical_path_maps_equivalent_spellings_to_one_value() {
        let root = std::env::temp_dir().join(format!("stk-audit-canonical-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "").unwrap();

        let expected = fs::canonicalize(root.join("src/lib.rs")).unwrap();
        assert!(expected.is_absolute());
        assert_eq!(canonical_path(&root.join("src/lib.rs")).unwrap(), expected);
        assert_eq!(
            canonical_path(&root.join("src/../src/lib.rs")).unwrap(),
            expected
        );
        assert_eq!(
            canonical_path(&root.join("./src/./lib.rs")).unwrap(),
            expected
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn canonical_path_follows_symlinks_before_comparing() {
        let root = std::env::temp_dir().join(format!("stk-audit-symlink-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("real")).unwrap();
        fs::write(root.join("real/lib.rs"), "").unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();

        assert_eq!(
            canonical_path(&root.join("link/lib.rs")).unwrap(),
            canonical_path(&root.join("real/lib.rs")).unwrap()
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn canonical_path_failure_names_the_path_and_the_check_to_make() {
        let missing = std::env::temp_dir()
            .join(format!(
                "stk-audit-canonical-missing-{}",
                std::process::id()
            ))
            .join("nope.rs");
        let message = canonical_path(&missing).unwrap_err().0;
        assert!(message.contains("failed to resolve path"), "{message}");
        assert!(message.contains("nope.rs"), "{message}");
        assert!(message.contains("verify the path exists"), "{message}");
    }

    #[test]
    fn workspace_relative_path_normalizes_before_stripping_the_root() {
        let root =
            std::env::temp_dir().join(format!("stk-audit-ws-relative-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "").unwrap();
        let workspace = fs::canonicalize(&root).unwrap();

        assert_eq!(
            workspace_relative_path(&workspace.join("src/../src/lib.rs"), &workspace),
            PathBuf::from("src/lib.rs")
        );
        assert_eq!(
            workspace_relative_path(&workspace.join("src/lib.rs"), &workspace),
            PathBuf::from("src/lib.rs")
        );

        // A file outside the workspace keeps its full canonical path instead
        // of a misleading stripped one.
        let outside =
            std::env::temp_dir().join(format!("stk-audit-outside-{}", std::process::id()));
        fs::write(&outside, "").unwrap();
        assert_eq!(
            workspace_relative_path(&outside, &workspace),
            fs::canonicalize(&outside).unwrap()
        );

        let _ = fs::remove_file(&outside);
        let _ = fs::remove_dir_all(&root);
    }

    // ---- #485: findings sort by file, line, rule, and message ----

    fn sample_finding(file: &str, line: usize, rule: &'static str, message: &str) -> Finding {
        Finding {
            file: PathBuf::from(file),
            line,
            rule,
            severity: "warning".to_string(),
            message: message.to_string(),
            source_snippet: None,
        }
    }

    #[test]
    fn normalize_findings_orders_by_file_line_rule_and_message() {
        let mut findings = vec![
            sample_finding("b.rs", 9, "missing-ttl-bump", "later file"),
            sample_finding("a.rs", 9, "missing-require-auth", "second message"),
            sample_finding("a.rs", 2, "missing-require-auth", "first line"),
            sample_finding("a.rs", 9, "missing-require-auth", "another message"),
            // The rule sorts before the message does, so a lexicographically
            // later message still lands first when its rule is smaller.
            sample_finding("a.rs", 9, "missing-positive-amount-validation", "z message"),
        ];

        normalize_findings(&mut findings);

        let ordered: Vec<_> = findings
            .iter()
            .map(|f| (f.file.to_str().unwrap(), f.line, f.message.as_str()))
            .collect();
        assert_eq!(
            ordered,
            vec![
                ("a.rs", 2, "first line"),
                ("a.rs", 9, "z message"),
                ("a.rs", 9, "another message"),
                ("a.rs", 9, "second message"),
                ("b.rs", 9, "later file"),
            ]
        );
    }

    #[test]
    fn normalize_findings_collapses_only_exact_duplicates() {
        let mut findings = vec![
            sample_finding(
                "a.rs",
                1,
                "missing-positive-amount-validation",
                "fn f takes `amount1`",
            ),
            sample_finding(
                "a.rs",
                1,
                "missing-positive-amount-validation",
                "fn f takes `amount2`",
            ),
            sample_finding(
                "a.rs",
                1,
                "missing-positive-amount-validation",
                "fn f takes `amount1`",
            ),
        ];

        normalize_findings(&mut findings);

        let messages: Vec<_> = findings.iter().map(|f| f.message.as_str()).collect();
        assert_eq!(
            messages,
            vec!["fn f takes `amount1`", "fn f takes `amount2`"]
        );
    }
}
