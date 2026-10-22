pub mod audit;
pub mod completion;
pub mod coverage;
pub mod limits;
pub mod release_verify;

use clap::{Parser, Subcommand};

/// soroban-testkit: coverage, resource-limit discovery, static audit checks,
/// and release metadata verification for Soroban contracts.
#[derive(Parser)]
#[command(name = "soroban-testkit", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Run test coverage for the WASM target via cargo-llvm-cov.
    Coverage(coverage::CoverageArgs),
    /// Empirically discover a contract function's resource ceiling.
    Limits(limits::LimitsArgs),
    /// Static checks over a contract crate (not a security product).
    Audit(audit::AuditArgs),
    /// Generate shell completions for this CLI.
    Completion(completion::CompletionArgs),
    /// Verify that workspace crate metadata is consistent and release-ready
    /// against the supplied (or workspace) version.
    ReleaseVerify(release_verify::ReleaseVerifyArgs),
    /// Hidden: runs a single limits probe in its own process. See
    /// `limits::run`'s doc comment for why.
    #[command(name = "__limits-probe", hide = true)]
    LimitsProbe(limits::ProbeArgs),
}

/// A command failure with an actionable, user-facing message. `main`
/// prints this and exits non-zero; it never lets a panic or backtrace
/// reach the user.
#[derive(Debug)]
pub struct CliError(pub String);

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for CliError {}

impl From<String> for CliError {
    fn from(value: String) -> Self {
        CliError(value)
    }
}

impl From<&str> for CliError {
    fn from(value: &str) -> Self {
        CliError(value.to_string())
    }
}

impl From<std::io::Error> for CliError {
    fn from(value: std::io::Error) -> Self {
        CliError(format!("I/O error: {value}"))
    }
}
