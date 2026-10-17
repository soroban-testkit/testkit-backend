use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};

use super::CliError;

/// Output format for `soroban-testkit coverage`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum Format {
    /// Per-file coverage stats printed to the terminal.
    Text,
    /// An `lcov.info` file, for IDE integrations (e.g. Coverage Gutters).
    Lcov,
    /// A browsable HTML report.
    Html,
    /// Machine-readable coverage data from cargo-llvm-cov.
    Json,
}

impl Format {
    /// All supported format names in canonical lowercase representation.
    #[allow(dead_code)]
    pub const ALL: &'static [&'static str] = &["text", "lcov", "html", "json"];

    /// Canonical lowercase string representation of the format.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Format::Text => "text",
            Format::Lcov => "lcov",
            Format::Html => "html",
            Format::Json => "json",
        }
    }
}

impl std::fmt::Display for Format {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Overall line coverage summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoverageSummary {
    pub lines_covered: usize,
    pub lines_total: usize,
    pub line_percent: f64,
}

/// Package-level line coverage summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackageSummary {
    pub package_name: String,
    pub lines_covered: usize,
    pub lines_total: usize,
    pub line_percent: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
}

/// File-level line coverage summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileSummary {
    pub filename: String,
    pub lines_covered: usize,
    pub lines_total: usize,
    pub line_percent: f64,
}

/// Generates an actionable error message when actual coverage is below the expected --fail-under threshold.
pub fn fail_under_error(actual: f64, expected: f64) -> CliError {
    CliError(format!(
        "coverage failed: actual line coverage {actual:.2}% is below the required --fail-under threshold of {expected:.2}%"
    ))
}

/// Validates that `actual` line coverage satisfies `expected` fail-under threshold.
pub fn validate_fail_under(actual: f64, expected: f64) -> Result<(), CliError> {
    if actual < expected {
        Err(fail_under_error(actual, expected))
    } else {
        Ok(())
    }
}

/// Arguments for `soroban-testkit coverage`.
#[derive(Args)]
pub struct CoverageArgs {
    /// Output format (text, lcov, html, json).
    #[arg(long, value_enum, default_value_t = Format::Text)]
    format: Format,
    /// Fail (non-zero exit) if line coverage is below this percentage.
    #[arg(long, value_name = "PCT")]
    fail_under: Option<f64>,
    /// Open the HTML report after generating it in the default browser (implies --format html).
    ///
    /// Requires platform browser opener support (e.g. macOS `open`, Linux `xdg-open`,
    /// Windows shell). On headless or CI environments without a display server or browser,
    /// omit `--open` and view the generated HTML report directly.
    ///
    /// `--open` is HTML-only; specifying it with `--format lcov` or `--format json`
    /// is rejected.
    #[arg(long)]
    open: bool,
    /// Only collect coverage for this workspace package. Repeatable.
    /// Mutually exclusive with `--exclude`.
    ///
    /// If no source files in the selected package are instrumented, coverage
    /// collection will report that no source files were covered.
    #[arg(long, value_name = "PACKAGE")]
    include: Vec<String>,
    /// Collect coverage for the whole workspace except this package.
    /// Repeatable. Mutually exclusive with `--include`.
    ///
    /// If all packages containing instrumented source files are excluded,
    /// coverage collection will report that no source files were covered.
    #[arg(long, value_name = "PACKAGE")]
    exclude: Vec<String>,
    /// Directory where coverage output files should be written.
    /// Defaults to the current directory.
    #[arg(long, value_name = "DIR")]
    output_dir: Option<std::path::PathBuf>,
    /// Write JSON to this path (defaults to coverage.json). Only valid with --format json.
    #[arg(long, value_name = "PATH")]
    output: Option<std::path::PathBuf>,
}

/// Wraps `cargo llvm-cov test`, which handles Soroban's coverage needs
/// correctly out of the box — soroban_sdk's own test harness runs contract
/// logic natively (not through the WASM VM), so ordinary LLVM
/// source-based coverage instrumentation applies without any
/// Soroban-specific flags. This command's job is knowing *that*, and
/// knowing the right output-format flags, not reimplementing coverage
/// collection.
///
/// Requires `cargo-llvm-cov` to be installed
/// (`cargo install cargo-llvm-cov`).
///
/// # Package filters
///
/// `--include <PACKAGE>` (repeatable) narrows collection to the named
/// workspace package(s), via cargo's own `-p`/`--package`. `--exclude
/// <PACKAGE>` (repeatable) collects for the whole workspace except the
/// named package(s), via `--workspace --exclude`. Specifying both is
/// rejected up front with an actionable error — cargo doesn't have a
/// sensible combined meaning for "these packages, but not these other
/// packages" as a single invocation, and guessing one would be worse
/// than asking the user to run the command twice.
///
/// # Behavior when no source files are instrumented
///
/// When `soroban-testkit coverage` runs in a workspace or package where no source
/// files are instrumented — such as when `--include` specifies a package without
/// tests or source files, `--exclude` filters out all instrumentable code, or the
/// target contracts have no native test targets — the underlying `cargo-llvm-cov`
/// tool may report zero coverage or exit with an error indicating no coverage
/// data was gathered.
///
/// In this scenario:
/// - If `cargo-llvm-cov` exits non-zero, the CLI fails with actionable diagnostic
///   guidance explaining that no instrumented source files were found or covered,
///   prompting the user to check package filters (`--include`/`--exclude`) and
///   verify that test targets exist.
/// - If the coverage run completes but generates an empty or missing output file,
///   the post-run output validator explicitly flags that no code was covered or
///   no tests were executed rather than failing silently.
///
/// # Platform support for `--open`
///
/// `--open` is HTML-only and launches the generated `index.html` report in the
/// system's default browser using platform openers (macOS `open`, Linux `xdg-open`,
/// Windows shell). In headless environments or CI containers without a display server
/// (e.g. no `$DISPLAY` or `$WAYLAND_DISPLAY`), `--open` should be omitted; users
/// should generate the report with `--format html` and inspect the output directory.
///
/// # Output file creation and validation
///
/// Output directories and parent paths are validated and created before the tool
/// executes. Output artifacts are verified after completion — empty or missing
/// files are reported with actionable diagnostic errors.
pub fn run(args: CoverageArgs) -> Result<(), CliError> {
    prepare_output_destinations(&args)?;
    let mut cmd = build_command(&args)?;

    let status = cmd.status().map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => CliError(
            "cargo-llvm-cov is not installed or not found in PATH; \
                     install it with: cargo install cargo-llvm-cov"
                .to_string(),
        ),
        _ => CliError(format!(
            "failed to run `cargo llvm-cov` ({err}); is cargo-llvm-cov installed? \
                 try `cargo install cargo-llvm-cov`"
        )),
    })?;

    if !status.success() {
        return Err(CliError(coverage_failure_message(
            status,
            args.open,
            args.fail_under,
        )));
    }

    validate_coverage_outputs(&args)?;

    if args.open {
        open_html_report(args.output_dir.as_deref())?;
    }

    Ok(())
}

/// Opens the generated HTML report in the system's default browser.
///
/// This is called **only** after [`validate_coverage_outputs`] confirms the
/// report exists, so the browser is never launched when generation failed.
pub(crate) fn open_html_report(output_dir: Option<&std::path::Path>) -> Result<(), CliError> {
    let dir = output_dir.unwrap_or(std::path::Path::new("."));
    let index_path = dir.join("index.html");

    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(target_os = "windows")]
    let opener = "cmd";
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let opener = "xdg-open";

    #[cfg(target_os = "windows")]
    let status = std::process::Command::new(opener)
        .args(["/c", "start", "", &index_path.to_string_lossy()])
        .status();
    #[cfg(not(target_os = "windows"))]
    let status = std::process::Command::new(opener).arg(&index_path).status();

    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(CliError(format!(
            "browser opener exited with {s} when trying to open '{}'; \
             open the file directly in your browser, or omit `--open` in headless/CI environments",
            index_path.display()
        ))),
        Err(err) => Err(CliError(format!(
            "failed to launch browser opener ({opener}): {err}; \
             open '{}' directly in your browser, or omit `--open` in headless/CI environments",
            index_path.display()
        ))),
    }
}

pub(crate) fn prepare_output_destinations(args: &CoverageArgs) -> Result<(), CliError> {
    if let Some(dir) = &args.output_dir {
        std::fs::create_dir_all(dir).map_err(|err| {
            CliError(format!(
                "failed to create coverage output directory '{}': {err}; \
                 verify directory permissions and path validity",
                dir.display()
            ))
        })?;
    }

    if let Some(output) = &args.output {
        if let Some(parent) = output.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|err| {
                    CliError(format!(
                        "failed to create parent directory for coverage output '{}': {err}; \
                         verify directory permissions and path validity",
                        output.display()
                    ))
                })?;
            }
        }
    }

    Ok(())
}

pub(crate) fn validate_coverage_outputs(args: &CoverageArgs) -> Result<(), CliError> {
    match args.format {
        Format::Text => {
            if args.open {
                validate_html_output(args.output_dir.as_deref())?;
            }
            Ok(())
        }
        Format::Lcov => {
            let output_path = if let Some(dir) = &args.output_dir {
                dir.join("lcov.info")
            } else {
                std::path::PathBuf::from("lcov.info")
            };

            if !output_path.exists() {
                return Err(CliError(format!(
                    "coverage LCOV output file not found at '{}'; \
                     the coverage tool may have failed to generate the output or no source files were instrumented — \
                     check the coverage run output above for errors",
                    output_path.display()
                )));
            }

            let contents = std::fs::read_to_string(&output_path).map_err(|err| {
                CliError(format!(
                    "failed to read coverage LCOV output at '{}': {err}",
                    output_path.display()
                ))
            })?;

            if contents.trim().is_empty() {
                return Err(CliError(format!(
                    "coverage LCOV output file at '{}' is empty; \
                     this usually indicates that no tests ran or no source files were instrumented",
                    output_path.display()
                )));
            }

            if let Some(expected) = args.fail_under {
                if let Some((_, _, pct)) = parse_lcov_coverage(&contents) {
                    validate_fail_under(pct, expected)?;
                }
            }

            Ok(())
        }
        Format::Html => validate_html_output(args.output_dir.as_deref()),
        Format::Json => {
            let output_path = args.output.clone().unwrap_or_else(|| {
                args.output_dir
                    .as_ref()
                    .map(|dir| dir.join("coverage.json"))
                    .unwrap_or_else(|| std::path::PathBuf::from("coverage.json"))
            });

            if !output_path.exists() {
                return Err(CliError(format!(
                    "coverage JSON output not found at '{}'; \
                     the coverage tool may have failed to generate the output or no source files were instrumented — \
                     check the coverage run output above for errors",
                    output_path.display()
                )));
            }

            let contents = std::fs::read_to_string(&output_path).map_err(|err| {
                CliError(format!(
                    "failed to read coverage JSON output at '{}': {err}",
                    output_path.display()
                ))
            })?;

            if contents.trim().is_empty() {
                return Err(CliError(format!(
                    "coverage JSON output file at '{}' is empty; \
                     this usually indicates that no tests ran or no source files were instrumented",
                    output_path.display()
                )));
            }

            let (augmented, summary) = augment_coverage_json(&contents)?;
            std::fs::write(&output_path, augmented).map_err(|err| {
                CliError(format!(
                    "failed to write augmented coverage JSON output to '{}': {err}",
                    output_path.display()
                ))
            })?;

            if let Some(expected) = args.fail_under {
                if summary.lines_total > 0 {
                    validate_fail_under(summary.line_percent, expected)?;
                }
            }

            Ok(())
        }
    }
}

pub(crate) fn parse_lcov_coverage(contents: &str) -> Option<(usize, usize, f64)> {
    let mut total_lf = 0usize;
    let mut total_lh = 0usize;
    for line in contents.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("LF:") {
            if let Ok(v) = rest.parse::<usize>() {
                total_lf += v;
            }
        } else if let Some(rest) = line.strip_prefix("LH:") {
            if let Ok(v) = rest.parse::<usize>() {
                total_lh += v;
            }
        }
    }
    if total_lf > 0 {
        let pct = (total_lh as f64 / total_lf as f64) * 100.0;
        Some((total_lh, total_lf, pct))
    } else {
        None
    }
}

/// Augments raw llvm-cov JSON export with stable `package_summaries`, `file_summaries`, and `summary` fields.
pub fn augment_coverage_json(raw_json: &str) -> Result<(String, CoverageSummary), CliError> {
    let mut root: serde_json::Value = serde_json::from_str(raw_json)
        .map_err(|err| CliError(format!("failed to parse coverage JSON output: {err}")))?;

    let mut file_summaries = Vec::new();
    let mut total_covered = 0usize;
    let mut total_lines = 0usize;

    if let Some(data_arr) = root.get("data").and_then(|d| d.as_array()) {
        for entry in data_arr {
            if let Some(files) = entry.get("files").and_then(|f| f.as_array()) {
                for file_obj in files {
                    let filename = file_obj
                        .get("filename")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_string();

                    let lines_summary = file_obj.get("summary").and_then(|s| s.get("lines"));
                    let lines_total = lines_summary
                        .and_then(|l| l.get("count"))
                        .and_then(|c| c.as_u64())
                        .unwrap_or(0) as usize;
                    let lines_covered = lines_summary
                        .and_then(|l| l.get("covered"))
                        .and_then(|c| c.as_u64())
                        .unwrap_or(0) as usize;
                    let line_percent = if lines_total > 0 {
                        lines_summary
                            .and_then(|l| l.get("percent"))
                            .and_then(|p| p.as_f64())
                            .unwrap_or_else(|| (lines_covered as f64 / lines_total as f64) * 100.0)
                    } else {
                        100.0
                    };

                    total_covered += lines_covered;
                    total_lines += lines_total;

                    file_summaries.push(FileSummary {
                        filename,
                        lines_covered,
                        lines_total,
                        line_percent,
                    });
                }
            }
        }
    }

    let mut packages_map: BTreeMap<String, (usize, usize, Vec<String>)> = BTreeMap::new();
    for file in &file_summaries {
        let pkg_name = infer_package_name(&file.filename);
        let entry = packages_map
            .entry(pkg_name)
            .or_insert_with(|| (0, 0, Vec::new()));
        entry.0 += file.lines_covered;
        entry.1 += file.lines_total;
        entry.2.push(file.filename.clone());
    }

    let package_summaries: Vec<PackageSummary> = packages_map
        .into_iter()
        .map(|(pkg, (cov, tot, files))| {
            let pct = if tot > 0 {
                (cov as f64 / tot as f64) * 100.0
            } else {
                100.0
            };
            PackageSummary {
                package_name: pkg,
                lines_covered: cov,
                lines_total: tot,
                line_percent: pct,
                files,
            }
        })
        .collect();

    let line_percent = if total_lines > 0 {
        (total_covered as f64 / total_lines as f64) * 100.0
    } else {
        0.0
    };

    let summary = CoverageSummary {
        lines_covered: total_covered,
        lines_total: total_lines,
        line_percent,
    };

    let obj = root.as_object_mut().ok_or_else(|| {
        CliError(format!(
            "coverage JSON output has unexpected structure: expected a JSON object at the root, \
             got {}; this usually means the coverage tool produced malformed or unexpected output — \
             check the coverage run output above for errors",
            match &root {
                serde_json::Value::Array(_) => "an array",
                serde_json::Value::Null => "null",
                serde_json::Value::Bool(_) => "a boolean",
                serde_json::Value::Number(_) => "a number",
                serde_json::Value::String(_) => "a string",
                serde_json::Value::Object(_) => "an object", // unreachable
            }
        ))
    })?;

    obj.insert(
        "package_summaries".to_string(),
        serde_json::to_value(&package_summaries)
            .map_err(|err| CliError(format!("failed to serialize package summaries: {err}")))?,
    );
    obj.insert(
        "file_summaries".to_string(),
        serde_json::to_value(&file_summaries)
            .map_err(|err| CliError(format!("failed to serialize file summaries: {err}")))?,
    );
    obj.insert(
        "summary".to_string(),
        serde_json::to_value(&summary)
            .map_err(|err| CliError(format!("failed to serialize coverage summary: {err}")))?,
    );

    let augmented_str = serde_json::to_string_pretty(&root)
        .map_err(|err| CliError(format!("failed to format augmented coverage JSON: {err}")))?;

    Ok((augmented_str, summary))
}

fn infer_package_name(path_str: &str) -> String {
    let path = Path::new(path_str);
    let mut components = path.components().peekable();
    while let Some(comp) = components.next() {
        let name = comp.as_os_str().to_string_lossy();
        if (name == "crates" || name == "examples") && components.peek().is_some() {
            if let Some(pkg) = components.next() {
                return pkg.as_os_str().to_string_lossy().to_string();
            }
        }
    }
    "workspace".to_string()
}

fn validate_html_output(output_dir: Option<&std::path::Path>) -> Result<(), CliError> {
    let dir = output_dir.unwrap_or(std::path::Path::new("."));
    let index_path = dir.join("index.html");

    if !index_path.exists() {
        return Err(CliError(format!(
            "coverage HTML report not found at '{}'; \
             the coverage tool may have failed to generate the report or no source files were instrumented — \
             check the coverage run output above for errors",
            index_path.display()
        )));
    }

    Ok(())
}

fn coverage_failure_message(
    status: std::process::ExitStatus,
    open_requested: bool,
    fail_under: Option<f64>,
) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;

        if let Some(signal) = status.signal() {
            return format!("coverage run terminated by signal {signal}");
        }
    }

    if let Some(expected) = fail_under {
        return format!(
            "coverage run failed (cargo llvm-cov exited with {status}); \
             line coverage failed to meet the required --fail-under threshold of {expected:.2}%"
        );
    }

    if open_requested {
        format!(
            "coverage run failed (cargo llvm-cov exited with {status}); \
             the HTML report was not generated so the browser will not be opened — \
             check the coverage run output above for errors"
        )
    } else {
        format!(
            "coverage run failed (cargo llvm-cov exited with {status}); \
             if no source files were instrumented, verify that the target package has \
             test targets and that `--include`/`--exclude` filters do not exclude all source files"
        )
    }
}

/// Builds the `cargo llvm-cov` invocation for `args`, without running it —
/// split out from [`run`] so the flag assembly (including the
/// include/exclude package filters) is unit-testable via
/// [`Command::get_args`] instead of only through a full subprocess.
fn build_command(args: &CoverageArgs) -> Result<Command, CliError> {
    if !args.include.is_empty() && !args.exclude.is_empty() {
        return Err(CliError(
            "--include and --exclude cannot be combined; run coverage twice if you need both \
             a narrowed and a widened view"
                .to_string(),
        ));
    }
    if args.output.is_some() && !matches!(args.format, Format::Json) {
        return Err(CliError(format!(
            "--output is only valid with --format json (got --format {})",
            args.format
        )));
    }
    if args.open && !matches!(args.format, Format::Html | Format::Text) {
        return Err(CliError(format!(
            "--open is only valid with --format html (got --format {})",
            args.format
        )));
    }
    if args.output_dir.is_some() && matches!(args.format, Format::Text) && !args.open {
        return Err(CliError(
            "--output-dir is not valid with --format text; \
             text output is written to the terminal — use --format html, --format lcov, or --format json to write files, \
             or add --open to generate an HTML report and open it"
                .to_string(),
        ));
    }

    let mut cmd = Command::new("cargo");
    cmd.arg("llvm-cov").arg("test");

    for package in &args.include {
        cmd.arg("--package").arg(package);
    }
    if !args.exclude.is_empty() {
        cmd.arg("--workspace");
        for package in &args.exclude {
            cmd.arg("--exclude").arg(package);
        }
    }

    match args.format {
        Format::Text => {
            if args.open {
                cmd.arg("--html");
                if let Some(dir) = &args.output_dir {
                    cmd.arg("--output-dir").arg(dir);
                }
            }
        }
        Format::Lcov => {
            cmd.arg("--lcov");
            let output_path = if let Some(dir) = &args.output_dir {
                dir.join("lcov.info")
            } else {
                std::path::PathBuf::from("lcov.info")
            };
            cmd.arg("--output-path").arg(output_path);
        }
        Format::Html => {
            cmd.arg("--html");
            if let Some(dir) = &args.output_dir {
                cmd.arg("--output-dir").arg(dir);
            }
        }
        Format::Json => {
            cmd.arg("--json");
            let output_path = args.output.clone().unwrap_or_else(|| {
                args.output_dir
                    .as_ref()
                    .map(|dir| dir.join("coverage.json"))
                    .unwrap_or_else(|| std::path::PathBuf::from("coverage.json"))
            });
            cmd.arg("--output-path").arg(output_path);
        }
    }

    if let Some(pct) = args.fail_under {
        if !pct.is_finite() || !(0.0..=100.0).contains(&pct) {
            return Err(CliError(format!(
                "--fail-under must be between 0 and 100 (got {pct})"
            )));
        }
        cmd.arg("--fail-under-lines").arg(pct.to_string());
    }

    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(include: &[&str], exclude: &[&str]) -> CoverageArgs {
        CoverageArgs {
            format: Format::Text,
            fail_under: None,
            open: false,
            include: include.iter().map(|s| s.to_string()).collect(),
            exclude: exclude.iter().map(|s| s.to_string()).collect(),
            output_dir: None,
            output: None,
        }
    }

    fn rendered_args(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect()
    }

    #[test]
    fn no_filters_by_default() {
        let cmd = build_command(&args(&[], &[])).unwrap();
        let rendered = rendered_args(&cmd);
        assert!(!rendered.contains(&"--package".to_string()));
        assert!(!rendered.contains(&"--exclude".to_string()));
        assert!(!rendered.contains(&"--workspace".to_string()));
    }

    #[test]
    fn include_adds_a_package_flag_per_entry() {
        let cmd = build_command(&args(&["core", "money"], &[])).unwrap();
        let rendered = rendered_args(&cmd);
        assert_eq!(
            rendered,
            vec![
                "llvm-cov",
                "test",
                "--package",
                "core",
                "--package",
                "money",
            ]
        );
    }

    #[test]
    fn exclude_adds_workspace_and_an_exclude_flag_per_entry() {
        let cmd = build_command(&args(&[], &["cli"])).unwrap();
        let rendered = rendered_args(&cmd);
        assert_eq!(
            rendered,
            vec!["llvm-cov", "test", "--workspace", "--exclude", "cli"]
        );
    }

    #[test]
    fn exclude_with_multiple_packages() {
        let cmd = build_command(&args(&[], &["cli", "audit"])).unwrap();
        let rendered = rendered_args(&cmd);
        assert_eq!(
            rendered,
            vec![
                "llvm-cov",
                "test",
                "--workspace",
                "--exclude",
                "cli",
                "--exclude",
                "audit",
            ]
        );
    }

    #[test]
    fn include_and_exclude_together_is_rejected() {
        let err = build_command(&args(&["core"], &["cli"])).unwrap_err();
        assert!(err.0.contains("cannot be combined"), "{}", err.0);
    }

    #[test]
    fn filters_compose_with_format_and_fail_under() {
        let mut a = args(&["core"], &[]);
        a.format = Format::Lcov;
        a.fail_under = Some(90.0);
        let cmd = build_command(&a).unwrap();
        let rendered = rendered_args(&cmd);
        assert_eq!(
            rendered,
            vec![
                "llvm-cov",
                "test",
                "--package",
                "core",
                "--lcov",
                "--output-path",
                "lcov.info",
                "--fail-under-lines",
                "90",
            ]
        );
    }

    #[test]
    fn fail_under_must_be_between_zero_and_one_hundred() {
        for pct in [-0.1, 100.1, f64::NAN, f64::INFINITY] {
            let mut a = args(&[], &[]);
            a.fail_under = Some(pct);
            let err = build_command(&a).unwrap_err();
            assert!(err.0.contains("between 0 and 100"), "{}", err.0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn signal_termination_is_reported_distinctly() {
        use std::os::unix::process::ExitStatusExt;

        let message = coverage_failure_message(std::process::ExitStatus::from_raw(9), false, None);
        assert_eq!(message, "coverage run terminated by signal 9");
    }

    #[test]
    fn output_dir_with_lcov_format() {
        let mut a = args(&[], &[]);
        a.format = Format::Lcov;
        a.output_dir = Some(std::path::PathBuf::from("./coverage"));
        let cmd = build_command(&a).unwrap();
        let rendered = rendered_args(&cmd);
        assert_eq!(
            rendered,
            vec![
                "llvm-cov",
                "test",
                "--lcov",
                "--output-path",
                "./coverage/lcov.info",
            ]
        );
    }

    #[test]
    fn output_dir_with_html_format() {
        let mut a = args(&[], &[]);
        a.format = Format::Html;
        a.output_dir = Some(std::path::PathBuf::from("./coverage"));
        let cmd = build_command(&a).unwrap();
        let rendered = rendered_args(&cmd);
        assert_eq!(
            rendered,
            vec!["llvm-cov", "test", "--html", "--output-dir", "./coverage",]
        );
    }

    #[test]
    fn output_dir_with_spaces_in_path_lcov() {
        let mut a = args(&[], &[]);
        a.format = Format::Lcov;
        a.output_dir = Some(std::path::PathBuf::from("./my coverage dir"));
        let cmd = build_command(&a).unwrap();
        let rendered = rendered_args(&cmd);
        assert_eq!(
            rendered,
            vec![
                "llvm-cov",
                "test",
                "--lcov",
                "--output-path",
                "./my coverage dir/lcov.info",
            ]
        );
    }

    #[test]
    fn output_dir_with_spaces_in_path_html() {
        let mut a = args(&[], &[]);
        a.format = Format::Html;
        a.output_dir = Some(std::path::PathBuf::from("./my coverage results"));
        let cmd = build_command(&a).unwrap();
        let rendered = rendered_args(&cmd);
        assert_eq!(
            rendered,
            vec![
                "llvm-cov",
                "test",
                "--html",
                "--output-dir",
                "./my coverage results",
            ]
        );
    }

    #[test]
    fn json_format_writes_default_file() {
        let mut a = args(&[], &[]);
        a.format = Format::Json;
        assert_eq!(
            rendered_args(&build_command(&a).unwrap()),
            vec![
                "llvm-cov",
                "test",
                "--json",
                "--output-path",
                "coverage.json"
            ]
        );
    }

    #[test]
    fn json_format_respects_output_path_and_directory() {
        let mut a = args(&[], &[]);
        a.format = Format::Json;
        a.output_dir = Some(std::path::PathBuf::from("reports"));
        assert_eq!(
            rendered_args(&build_command(&a).unwrap()),
            vec![
                "llvm-cov",
                "test",
                "--json",
                "--output-path",
                "reports/coverage.json"
            ]
        );
        a.output = Some(std::path::PathBuf::from("result.json"));
        assert_eq!(
            rendered_args(&build_command(&a).unwrap()).last().unwrap(),
            "result.json"
        );
    }

    #[test]
    fn json_output_option_is_rejected_for_other_formats() {
        let mut a = args(&[], &[]);
        a.output = Some(std::path::PathBuf::from("result.json"));
        assert!(build_command(&a)
            .unwrap_err()
            .0
            .contains("only valid with --format json"));
    }

    #[test]
    fn json_cannot_be_opened_as_html() {
        let mut a = args(&[], &[]);
        a.format = Format::Json;
        a.open = true;
        assert!(build_command(&a)
            .unwrap_err()
            .0
            .contains("only valid with --format html"));
    }

    #[test]
    fn lcov_cannot_be_opened_as_html() {
        let mut a = args(&[], &[]);
        a.format = Format::Lcov;
        a.open = true;
        assert!(build_command(&a)
            .unwrap_err()
            .0
            .contains("only valid with --format html"));
    }

    #[test]
    fn open_flag_with_default_format_implies_html() {
        let mut a = args(&[], &[]);
        a.open = true;
        let cmd = build_command(&a).unwrap();
        let rendered = rendered_args(&cmd);
        // --html is passed to the subprocess so llvm-cov generates the report
        assert!(rendered.contains(&"--html".to_string()));
        // --open is NOT forwarded to the subprocess; the CLI opens the browser
        // itself only after validate_coverage_outputs confirms the report exists
        assert!(!rendered.contains(&"--open".to_string()));
    }

    #[test]
    fn open_flag_with_explicit_html_format() {
        let mut a = args(&[], &[]);
        a.format = Format::Html;
        a.open = true;
        let cmd = build_command(&a).unwrap();
        let rendered = rendered_args(&cmd);
        // --html is passed to the subprocess
        assert!(rendered.contains(&"--html".to_string()));
        // --open is NOT forwarded to the subprocess; the CLI opens the browser
        // itself only after validate_coverage_outputs confirms the report exists
        assert!(!rendered.contains(&"--open".to_string()));
    }

    #[test]
    fn prepare_output_destinations_creates_dirs() {
        let dir = std::env::temp_dir().join(format!("stk-cov-prep-{}", std::process::id()));
        let mut a = args(&[], &[]);
        a.output_dir = Some(dir.join("reports"));
        a.format = Format::Json;
        a.output = Some(dir.join("nested/dir/cov.json"));

        assert!(prepare_output_destinations(&a).is_ok());
        assert!(dir.join("reports").exists());
        assert!(dir.join("nested/dir").exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn validate_coverage_outputs_passes_for_text_format() {
        let a = args(&[], &[]);
        assert!(validate_coverage_outputs(&a).is_ok());
    }

    #[test]
    fn validate_coverage_outputs_fails_for_missing_and_empty_lcov() {
        let dir = std::env::temp_dir().join(format!("stk-cov-val-lcov-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let mut a = args(&[], &[]);
        a.format = Format::Lcov;
        a.output_dir = Some(dir.clone());

        // Missing file
        let err = validate_coverage_outputs(&a).unwrap_err();
        assert!(err.0.contains("not found"), "{}", err.0);
        assert!(
            err.0.contains("no source files were instrumented"),
            "{}",
            err.0
        );

        // Empty file
        std::fs::write(dir.join("lcov.info"), "").unwrap();
        let err = validate_coverage_outputs(&a).unwrap_err();
        assert!(err.0.contains("empty"), "{}", err.0);
        assert!(
            err.0.contains("no source files were instrumented"),
            "{}",
            err.0
        );

        // Valid content
        std::fs::write(dir.join("lcov.info"), "TN:\nSF:src/lib.rs\nend_of_record\n").unwrap();
        assert!(validate_coverage_outputs(&a).is_ok());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn validate_coverage_outputs_fails_for_missing_html_report() {
        let dir = std::env::temp_dir().join(format!("stk-cov-val-html-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let mut a = args(&[], &[]);
        a.format = Format::Html;
        a.output_dir = Some(dir.clone());

        let err = validate_coverage_outputs(&a).unwrap_err();
        assert!(err.0.contains("HTML report not found"), "{}", err.0);

        std::fs::write(dir.join("index.html"), "<html></html>").unwrap();
        assert!(validate_coverage_outputs(&a).is_ok());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn validate_coverage_outputs_fails_for_missing_and_empty_json() {
        let dir = std::env::temp_dir().join(format!("stk-cov-val-json-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let json_path = dir.join("custom.json");

        let mut a = args(&[], &[]);
        a.format = Format::Json;
        a.output = Some(json_path.clone());

        // Missing
        let err = validate_coverage_outputs(&a).unwrap_err();
        assert!(err.0.contains("JSON output not found"), "{}", err.0);

        // Empty
        std::fs::write(&json_path, "   ").unwrap();
        let err = validate_coverage_outputs(&a).unwrap_err();
        assert!(err.0.contains("empty"), "{}", err.0);
        assert!(
            err.0.contains("no source files were instrumented"),
            "{}",
            err.0
        );

        // Valid
        std::fs::write(&json_path, r#"{"data":[]}"#).unwrap();
        assert!(validate_coverage_outputs(&a).is_ok());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn coverage_failure_message_includes_actionable_guidance() {
        #[cfg(unix)]
        use std::os::unix::process::ExitStatusExt;

        let status = std::process::ExitStatus::from_raw(256); // exit code 1
        let msg_normal = coverage_failure_message(status, false, None);
        assert!(
            msg_normal.contains("if no source files were instrumented"),
            "{msg_normal}"
        );

        let msg_open = coverage_failure_message(status, true, None);
        assert!(
            msg_open.contains("HTML report was not generated so the browser will not be opened"),
            "{msg_open}"
        );
        assert!(!msg_open.contains("desktop browser opener"), "{msg_open}");

        let msg_fail_under = coverage_failure_message(status, false, Some(85.5));
        assert!(
            msg_fail_under.contains("--fail-under threshold of 85.50%"),
            "{msg_fail_under}"
        );
    }

    #[test]
    fn format_enum_has_stable_names_and_display() {
        assert_eq!(Format::ALL, &["text", "lcov", "html", "json"]);
        assert_eq!(Format::Text.as_str(), "text");
        assert_eq!(Format::Lcov.as_str(), "lcov");
        assert_eq!(Format::Html.as_str(), "html");
        assert_eq!(Format::Json.as_str(), "json");

        assert_eq!(format!("{}", Format::Text), "text");
        assert_eq!(format!("{}", Format::Lcov), "lcov");
        assert_eq!(format!("{}", Format::Html), "html");
        assert_eq!(format!("{}", Format::Json), "json");
    }

    #[test]
    fn fail_under_error_includes_actual_and_expected_values() {
        let err = fail_under_error(78.543, 90.0);
        assert!(err.0.contains("actual line coverage 78.54%"), "{}", err.0);
        assert!(
            err.0.contains("--fail-under threshold of 90.00%"),
            "{}",
            err.0
        );

        assert!(validate_fail_under(90.0, 90.0).is_ok());
        assert!(validate_fail_under(95.5, 90.0).is_ok());
        let val_err = validate_fail_under(89.99, 90.0).unwrap_err();
        assert!(val_err.0.contains("89.99%"), "{}", val_err.0);
        assert!(val_err.0.contains("90.00%"), "{}", val_err.0);
    }

    #[test]
    fn augment_coverage_json_adds_stable_package_and_file_summaries() {
        let raw_json = r#"{
            "data": [
                {
                    "files": [
                        {
                            "filename": "crates/testkit/src/lib.rs",
                            "summary": {
                                "lines": {
                                    "count": 100,
                                    "covered": 85,
                                    "percent": 85.0
                                }
                            }
                        },
                        {
                            "filename": "crates/testkit/src/money.rs",
                            "summary": {
                                "lines": {
                                    "count": 50,
                                    "covered": 45,
                                    "percent": 90.0
                                }
                            }
                        },
                        {
                            "filename": "crates/cli/src/main.rs",
                            "summary": {
                                "lines": {
                                    "count": 200,
                                    "covered": 150,
                                    "percent": 75.0
                                }
                            }
                        }
                    ]
                }
            ]
        }"#;

        let (augmented, summary) = augment_coverage_json(raw_json).unwrap();
        assert_eq!(summary.lines_total, 350);
        assert_eq!(summary.lines_covered, 280);
        assert!((summary.line_percent - 80.0).abs() < 1e-4);

        let parsed: serde_json::Value = serde_json::from_str(&augmented).unwrap();
        assert!(parsed["package_summaries"].is_array());
        assert!(parsed["file_summaries"].is_array());
        assert!(parsed["summary"].is_object());

        let pkgs = parsed["package_summaries"].as_array().unwrap();
        assert_eq!(pkgs.len(), 2); // testkit and cli

        let files = parsed["file_summaries"].as_array().unwrap();
        assert_eq!(files.len(), 3);
        assert_eq!(files[0]["filename"], "crates/testkit/src/lib.rs");
        assert_eq!(files[0]["lines_covered"], 85);
        assert_eq!(files[0]["lines_total"], 100);
        assert_eq!(files[0]["line_percent"], 85.0);
    }

    #[test]
    fn validate_coverage_outputs_checks_fail_under_on_json() {
        let dir =
            std::env::temp_dir().join(format!("stk-cov-fail-under-json-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let json_path = dir.join("coverage.json");

        let raw_json = r#"{
            "data": [
                {
                    "files": [
                        {
                            "filename": "crates/testkit/src/lib.rs",
                            "summary": {
                                "lines": {
                                    "count": 100,
                                    "covered": 80,
                                    "percent": 80.0
                                }
                            }
                        }
                    ]
                }
            ]
        }"#;
        std::fs::write(&json_path, raw_json).unwrap();

        let mut a = args(&[], &[]);
        a.format = Format::Json;
        a.output = Some(json_path.clone());
        a.fail_under = Some(90.0);

        let err = validate_coverage_outputs(&a).unwrap_err();
        assert!(err.0.contains("actual line coverage 80.00%"), "{}", err.0);
        assert!(
            err.0.contains("--fail-under threshold of 90.00%"),
            "{}",
            err.0
        );

        // Satisfied
        a.fail_under = Some(75.0);
        assert!(validate_coverage_outputs(&a).is_ok());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn augment_coverage_json_rejects_non_object_root() {
        // JSON array root — valid JSON but wrong schema
        let err = augment_coverage_json(r#"[]"#).unwrap_err();
        assert!(
            err.0.contains("unexpected structure"),
            "expected 'unexpected structure' in: {}",
            err.0
        );
        assert!(err.0.contains("an array"), "{}", err.0);

        // JSON null root
        let err = augment_coverage_json("null").unwrap_err();
        assert!(err.0.contains("unexpected structure"), "{}", err.0);
        assert!(err.0.contains("null"), "{}", err.0);

        // JSON string root
        let err = augment_coverage_json(r#""hello""#).unwrap_err();
        assert!(err.0.contains("unexpected structure"), "{}", err.0);
        assert!(err.0.contains("a string"), "{}", err.0);

        // JSON number root
        let err = augment_coverage_json("42").unwrap_err();
        assert!(err.0.contains("unexpected structure"), "{}", err.0);
        assert!(err.0.contains("a number"), "{}", err.0);
    }

    #[test]
    fn augment_coverage_json_rejects_malformed_json() {
        // Syntactically invalid JSON
        let err = augment_coverage_json("{not valid json}").unwrap_err();
        assert!(
            err.0.contains("failed to parse coverage JSON output"),
            "{}",
            err.0
        );

        // Truncated JSON
        let err = augment_coverage_json(r#"{"data": ["#).unwrap_err();
        assert!(
            err.0.contains("failed to parse coverage JSON output"),
            "{}",
            err.0
        );
    }

    #[test]
    fn validate_coverage_outputs_fails_for_malformed_json_content() {
        let dir = std::env::temp_dir().join(format!(
            "stk-cov-val-malformed-json-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let json_path = dir.join("coverage.json");

        let mut a = args(&[], &[]);
        a.format = Format::Json;
        a.output = Some(json_path.clone());

        // Syntactically invalid JSON
        std::fs::write(&json_path, "{not valid json}").unwrap();
        let err = validate_coverage_outputs(&a).unwrap_err();
        assert!(
            err.0.contains("failed to parse coverage JSON output"),
            "{}",
            err.0
        );

        // Valid JSON but wrong root type (array instead of object)
        std::fs::write(&json_path, "[]").unwrap();
        let err = validate_coverage_outputs(&a).unwrap_err();
        assert!(
            err.0.contains("unexpected structure"),
            "expected 'unexpected structure' in: {}",
            err.0
        );

        // Valid JSON but null root
        std::fs::write(&json_path, "null").unwrap();
        let err = validate_coverage_outputs(&a).unwrap_err();
        assert!(err.0.contains("unexpected structure"), "{}", err.0);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn coverage_failure_message_never_claims_success() {
        // Verifies the failure message always contains diagnostic content,
        // not a success message.
        #[cfg(unix)]
        use std::os::unix::process::ExitStatusExt;

        let status = std::process::ExitStatus::from_raw(256); // exit code 1

        let msg = coverage_failure_message(status, false, None);
        assert!(
            msg.contains("coverage run failed") || msg.contains("terminated by signal"),
            "expected failure indication, got: {msg}"
        );
        assert!(!msg.to_lowercase().contains("success"), "{msg}");

        let msg_fail_under = coverage_failure_message(status, false, Some(80.0));
        assert!(
            msg_fail_under.contains("coverage run failed")
                || msg_fail_under.contains("terminated by signal"),
            "{msg_fail_under}"
        );
        assert!(!msg_fail_under.to_lowercase().contains("success"), "{msg_fail_under}");

        let msg_open = coverage_failure_message(status, true, None);
        assert!(
            msg_open.contains("coverage run failed") || msg_open.contains("terminated by signal"),
            "{msg_open}"
        );
        assert!(!msg_open.to_lowercase().contains("success"), "{msg_open}");
    }
}
