//! `soroban-testkit release-verify`: verify crate metadata before a release.
//!
//! The release checklist in `RELEASING.md` requires both published crates
//! (`soroban-testkit` and `soroban-testkit-cli`) to stay version-locked and
//! released together from one git tag. The shell script
//! `.github/scripts/verify-release.sh` already enforces this at publish time;
//! this subcommand exposes the same invariants as a first-class CLI command,
//! so a contributor can run `cargo run -p soroban-testkit-cli -- release-verify`
//! locally before pushing a tag, rather than discovering a mismatch from a
//! failed workflow run.
//!
//! The command checks, in order:
//!
//! 1. The workspace `Cargo.toml` has a `[workspace.package] version`.
//! 2. Every published crate (`crates/testkit`, `crates/cli`) either inherits
//!    that version with `version.workspace = true` or declares a literal
//!    version that matches it exactly.
//! 3. Each published crate inherits `repository.workspace = true` and the
//!    workspace `repository` URL is set (crates.io shows this link for both
//!    crates; a forked or blank URL here ships a broken link).
//! 4. Each published crate inherits `license.workspace = true` and the
//!    workspace `license` is set.
//! 5. `CHANGELOG.md` has a `## [<version>]` section, so a tag can never ship
//!    without release notes.
//! 6. `COMPATIBILITY.md` mentions the pinned `soroban-sdk` version, so the
//!    compatibility matrix cannot silently drift from the actual pin.
//!
//! Every failure produces an actionable message: it names the file, the
//! invariant that failed, and the fix. The command never panics; `main`
//! prints the [`CliError`] and exits non-zero.

use std::path::{Path, PathBuf};

use clap::Args;

use super::CliError;

/// The published workspace crates, in release order (library first, then
/// the CLI — see `RELEASING.md`). Both must agree with the release tag.
const PUBLISHED_CRATES: &[&str] = &["crates/testkit", "crates/cli"];

/// Arguments for `soroban-testkit release-verify`.
#[derive(Args, Debug)]
pub struct ReleaseVerifyArgs {
    /// The version being released (e.g. `0.1.0`). A leading `v` is stripped.
    /// If omitted, the workspace version from `Cargo.toml` is used, so the
    /// command can verify the *current* tree without a tag.
    #[arg(long, value_name = "VERSION")]
    version: Option<String>,
    /// Workspace root to verify. Defaults to the current directory. Pass
    /// this only when invoking the command from outside the workspace.
    #[arg(long, value_name = "PATH", default_value = ".")]
    workspace: PathBuf,
}

/// Run `release-verify`: check that the workspace's crate metadata is
/// consistent and release-ready against the supplied (or workspace) version.
///
/// All failures are collected and reported together, so a contributor
/// fixing one mismatch sees the next on the next run rather than
/// discovering them one at a time across CI runs.
pub fn run(args: ReleaseVerifyArgs) -> Result<(), CliError> {
    let workspace = canonicalize_workspace(&args.workspace)?;
    let workspace_manifest = workspace.join("Cargo.toml");
    let workspace_text = read_file(&workspace_manifest, "Cargo.toml")?;

    let workspace_version = match args.version.as_deref() {
        Some(v) => strip_leading_v(v).to_owned(),
        None => extract_workspace_version(&workspace_text)?,
    };

    let mut findings = Vec::new();

    // 1. Workspace version is present (only checked when the caller didn't
    //    supply one explicitly, since extract_workspace_version already
    //    errors on absence).
    if args.version.is_none() {
        findings.extend(check_workspace_version_set(&workspace_text));
    }

    // 2. Each published crate matches the workspace version.
    for crate_rel in PUBLISHED_CRATES {
        let manifest_path = workspace.join(crate_rel).join("Cargo.toml");
        let manifest_text = read_file(&manifest_path, crate_rel)?;
        findings.extend(check_crate_version(crate_rel, &manifest_text, &workspace_version));
        findings.extend(check_crate_inherits(crate_rel, &manifest_text, "repository"));
        findings.extend(check_crate_inherits(crate_rel, &manifest_text, "license"));
    }

    // 3. Workspace repository and license are set.
    findings.extend(check_workspace_field_set(&workspace_text, "repository"));
    findings.extend(check_workspace_field_set(&workspace_text, "license"));

    // 4. CHANGELOG has a section for this version.
    let changelog_path = workspace.join("CHANGELOG.md");
    let changelog_text = read_file(&changelog_path, "CHANGELOG.md")?;
    findings.extend(check_changelog_has_version(&changelog_text, &workspace_version));

    // 5. COMPATIBILITY.md mentions the pinned soroban-sdk version.
    let sdk_pin = extract_sdk_pin(&workspace_text)?;
    let compat_path = workspace.join("COMPATIBILITY.md");
    let compat_text = read_file(&compat_path, "COMPATIBILITY.md")?;
    findings.extend(check_compatibility_lists_sdk(&compat_text, &sdk_pin));

    if findings.is_empty() {
        println!(
            "release-verify: OK — workspace and both published crates are consistent at v{workspace_version}."
        );
        println!(
            "  workspace version: {workspace_version}\n  published crates: {}",
            PUBLISHED_CRATES.join(", ")
        );
        return Ok(());
    }

    for finding in &findings {
        eprintln!("release-verify: {finding}");
    }
    Err(CliError(format!(
        "release-verify found {} problem(s) at v{workspace_version}; fix the issues above before tagging.",
        findings.len()
    )))
}

/// A single metadata invariant violation, formatted for stderr.
type Finding = String;

/// Strip a leading `v` from a version string (e.g. `v0.1.0` -> `0.1.0`).
fn strip_leading_v(version: &str) -> &str {
    version.strip_prefix('v').unwrap_or(version)
}

/// Resolve the workspace root, failing with an actionable error if the path
/// does not exist or has no `Cargo.toml`.
fn canonicalize_workspace(path: &Path) -> Result<PathBuf, CliError> {
    let absolute = path
        .canonicalize()
        .map_err(|err| CliError(format!("workspace path {} cannot be read: {err}; pass --workspace <path> pointing at the workspace root", path.display())))?;
    if !absolute.join("Cargo.toml").is_file() {
        return Err(CliError(format!(
            "no Cargo.toml found under {}; pass --workspace <path> pointing at the workspace root",
            absolute.display()
        )));
    }
    Ok(absolute)
}

/// Read a file, with an actionable error naming the file and the underlying
/// cause. A missing release-critical file is a real failure, not a skip.
fn read_file(path: &Path, label: &str) -> Result<String, CliError> {
    std::fs::read_to_string(path).map_err(|err| {
        CliError(format!(
            "cannot read {label} at {}: {err}; this file is required for a release",
            path.display()
        ))
    })
}

/// Extract `version = "..."` from the `[workspace.package]` section of the
/// root manifest. Returns an error if it is missing — every other check
/// depends on it.
fn extract_workspace_version(manifest: &str) -> Result<String, CliError> {
    let version = first_matching_line_value(manifest, "version");
    match version {
        Some(v) => Ok(v),
        None => Err(CliError(
            "Cargo.toml has no [workspace.package] version; set it before releasing.".into(),
        )),
    }
}

/// Extract the pinned `soroban-sdk` version from the workspace manifest's
/// `[workspace.dependencies]` section. Returns an error if absent — the
/// compatibility matrix check depends on it.
fn extract_sdk_pin(manifest: &str) -> Result<String, CliError> {
    // Look for a line like `soroban-sdk = "27.0.6"` or
    // `soroban-sdk = { version = "27.0.6", ... }` under
    // [workspace.dependencies]. The first match is the pin.
    for line in manifest.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("soroban-sdk") {
            // Skip the `=` and surrounding whitespace.
            let rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '=');
            // Plain string form: soroban-sdk = "27.0.6"
            if let Some(v) = rest.strip_prefix('"').and_then(|s| s.split('"').next()) {
                if !v.is_empty() {
                    return Ok(v.to_owned());
                }
            }
            // Table form: soroban-sdk = { version = "27.0.6", ... }
            if let Some(table) = rest.strip_prefix('{') {
                if let Some(end) = table.find('}') {
                    let inner = &table[..end];
                    if let Some(v) = first_matching_line_value(inner, "version") {
                        return Ok(v);
                    }
                }
            }
        }
    }
    Err(CliError(
        "Cargo.toml has no soroban-sdk pin under [workspace.dependencies]; the compatibility matrix check requires it.".into(),
    ))
}

/// Find the first `key = "value"` line in `text` and return the value.
fn first_matching_line_value(text: &str, key: &str) -> Option<String> {
    let prefix = format!("{key} = \"");
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(&prefix) {
            if let Some(end) = rest.find('"') {
                return Some(rest[..end].to_owned());
            }
        }
    }
    None
}

/// Check that `[workspace.package] version` is literally set in the root
/// manifest (used only when the caller did not pass `--version`).
fn check_workspace_version_set(manifest: &str) -> Vec<Finding> {
    if first_matching_line_value(manifest, "version").is_some() {
        return Vec::new();
    }
    vec![format!(
        "Cargo.toml: [workspace.package] version is not set; add `version = \"<x.y.z>\"` under [workspace.package]."
    )]
}

/// Check that a published crate's manifest matches the workspace version,
/// either by inheriting it (`version.workspace = true`) or by declaring the
/// same literal version.
fn check_crate_version(crate_rel: &str, manifest: &str, expected: &str) -> Vec<Finding> {
    if manifest.contains("version.workspace = true") || manifest.contains("version.workspace=true")
    {
        return Vec::new();
    }
    match first_matching_line_value(manifest, "version") {
        Some(v) if v == expected => Vec::new(),
        Some(v) => vec![format!(
            "{crate_rel}/Cargo.toml: version is \"{v}\" but workspace version is \"{expected}\"; set `version.workspace = true` or align the literal."
        )],
        None => vec![format!(
            "{crate_rel}/Cargo.toml: no version declared; set `version.workspace = true` to inherit the workspace version ({expected})."
        )],
    }
}

/// Check that a published crate inherits a given field from the workspace
/// (e.g. `repository.workspace = true`, `license.workspace = true`). A
/// hardcoded value here would drift from the workspace on the next edit.
fn check_crate_inherits(crate_rel: &str, manifest: &str, field: &str) -> Vec<Finding> {
    let inherits = format!("{field}.workspace = true");
    if manifest.contains(&inherits) || manifest.contains(&inherits.replace(' ', "")) {
        return Vec::new();
    }
    vec![format!(
        "{crate_rel}/Cargo.toml: missing `{inherits}`; a hardcoded or missing {field} would drift from the workspace on the next edit."
    )]
}

/// Check that the workspace manifest has a literal value for `field` under
/// `[workspace.package]`.
fn check_workspace_field_set(manifest: &str, field: &str) -> Vec<Finding> {
    if first_matching_line_value(manifest, field).is_some() {
        return Vec::new();
    }
    vec![format!(
        "Cargo.toml: [workspace.package] {field} is not set; add `{field} = \"<value>\"` so both published crates inherit it."
    )]
}

/// Check that `CHANGELOG.md` has a `## [<version>]` heading.
fn check_changelog_has_version(changelog: &str, version: &str) -> Vec<Finding> {
    let heading = format!("## [{version}]");
    if changelog.lines().any(|line| line.trim() == heading || line.trim().starts_with(&heading)) {
        return Vec::new();
    }
    vec![format!(
        "CHANGELOG.md: no \"## [{version}]\" section; move the [Unreleased] content into a dated entry before tagging v{version}."
    )]
}

/// Check that `COMPATIBILITY.md` mentions the pinned `soroban-sdk` version.
fn check_compatibility_lists_sdk(compatibility: &str, sdk_pin: &str) -> Vec<Finding> {
    if compatibility.contains(sdk_pin) {
        return Vec::new();
    }
    vec![format!(
        "COMPATIBILITY.md: does not mention the pinned soroban-sdk version ({sdk_pin}); update the compatibility matrix before releasing."
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACE_MANIFEST: &str = "\
[workspace]
resolver = \"2\"
members = [\"crates/*\", \"examples/vault\", \"examples/quickstart\"]

[workspace.package]
version = \"0.1.0\"
edition = \"2021\"
license = \"Apache-2.0\"
repository = \"https://github.com/soroban-testkit/soroban-testkit\"

[workspace.dependencies]
soroban-sdk = \"27.0.6\"
thiserror = \"2\"
";

    const TESTKIT_MANIFEST: &str = "\
[package]
name = \"soroban-testkit\"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true
";

    const CLI_MANIFEST_WRONG: &str = "\
[package]
name = \"soroban-testkit-cli\"
version = \"0.0.9\"
edition.workspace = true
";

    const CHANGELOG: &str = "\
# Changelog

## [Unreleased]

## [0.1.0] - 2024-01-01

- initial release
";

    const COMPATIBILITY: &str = "\
# Compatibility

| soroban-sdk | protocol |
| 27.0.6      | 23      |
";

    #[test]
    fn extracts_workspace_version() {
        let v = extract_workspace_version(WORKSPACE_MANIFEST).unwrap();
        assert_eq!(v, "0.1.0");
    }

    #[test]
    fn extracts_sdk_pin_from_plain_string_form() {
        let v = extract_sdk_pin(WORKSPACE_MANIFEST).unwrap();
        assert_eq!(v, "27.0.6");
    }

    #[test]
    fn extracts_sdk_pin_from_table_form() {
        let manifest = "\
[workspace.dependencies]
soroban-sdk = { version = \"27.0.6\", features = [\"testutils\"] }
";
        let v = extract_sdk_pin(manifest).unwrap();
        assert_eq!(v, "27.0.6");
    }

    #[test]
    fn strip_leading_v_handles_both_forms() {
        assert_eq!(strip_leading_v("v0.1.0"), "0.1.0");
        assert_eq!(strip_leading_v("0.1.0"), "0.1.0");
    }

    #[test]
    fn crate_version_passes_when_inheriting() {
        let findings = check_crate_version("crates/testkit", TESTKIT_MANIFEST, "0.1.0");
        assert!(findings.is_empty());
    }

    #[test]
    fn crate_version_fails_on_mismatched_literal() {
        let findings = check_crate_version("crates/cli", CLI_MANIFEST_WRONG, "0.1.0");
        assert_eq!(findings.len(), 1);
        assert!(findings[0].contains("0.0.9"));
        assert!(findings[0].contains("0.1.0"));
    }

    #[test]
    fn crate_inherits_finds_missing_field() {
        let findings = check_crate_inherits("crates/cli", CLI_MANIFEST_WRONG, "repository");
        assert_eq!(findings.len(), 1);
        assert!(findings[0].contains("repository.workspace = true"));
    }

    #[test]
    fn workspace_field_set_passes_when_present() {
        let findings = check_workspace_field_set(WORKSPACE_MANIFEST, "repository");
        assert!(findings.is_empty());
    }

    #[test]
    fn workspace_field_set_fails_when_absent() {
        let findings = check_workspace_field_set(
            "[workspace.package]\nversion = \"0.1.0\"\n",
            "repository",
        );
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn changelog_check_finds_existing_version() {
        let findings = check_changelog_has_version(CHANGELOG, "0.1.0");
        assert!(findings.is_empty());
    }

    #[test]
    fn changelog_check_fails_on_missing_version() {
        let findings = check_changelog_has_version(CHANGELOG, "9.9.9");
        assert_eq!(findings.len(), 1);
        assert!(findings[0].contains("9.9.9"));
    }

    #[test]
    fn compatibility_check_passes_when_sdk_pin_mentioned() {
        let findings = check_compatibility_lists_sdk(COMPATIBILITY, "27.0.6");
        assert!(findings.is_empty());
    }

    #[test]
    fn compatibility_check_fails_when_sdk_pin_missing() {
        let findings = check_compatibility_lists_sdk(COMPATIBILITY, "99.0.0");
        assert_eq!(findings.len(), 1);
        assert!(findings[0].contains("99.0.0"));
    }

    #[test]
    fn extract_workspace_version_errors_when_absent() {
        let manifest = "[workspace]\nmembers = []\n";
        let err = extract_workspace_version(manifest).unwrap_err();
        assert!(err.0.contains("[workspace.package] version"));
    }
}
