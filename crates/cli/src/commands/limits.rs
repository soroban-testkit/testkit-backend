//! Resource-ceiling discovery for compiled Soroban contracts.
//!
//! This module owns the `limits` command and the hidden `__limits-probe`
//! child entry point it spawns. The two halves cooperate through process
//! isolation, whose full lifecycle is documented on [`run`]; each probe's
//! result is classified with [`ProbeFailureCategory`] and every report —
//! human-readable or machine-readable — is rendered from a single
//! [`LimitsSummary`] so the formats cannot disagree.

use std::collections::BTreeMap;
use std::panic;
use std::path::Path;
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

use clap::Args;
use soroban_sdk::testutils::{Address as _, MockAuth, MockAuthInvoke};
use soroban_sdk::token::StellarAssetClient;
use soroban_sdk::xdr::{ScSpecEntry, ScSpecFunctionInputV0, ScSpecFunctionV0, ScSpecTypeDef};
use soroban_sdk::{Address, Env, IntoVal, Symbol, TryFromVal, Val, Vec as SVec};

use super::CliError;

/// Measurement limitations warning reported across stdout and machine-readable outputs.
pub const MEASUREMENT_LIMITATIONS: &str =
    "ledger reads/writes and transaction size are not measured by this command; see --help";

/// Built-in upper bound for the doubling search. If the contract still
/// succeeds here the run stops and reports that the ceiling may be higher,
/// rather than ramping forever.
pub const SEARCH_UPPER_BOUND: u32 = 1 << 24;

/// The standardized reason a single probe did not succeed.
///
/// Every failure category is reported verbatim in the human-readable summary,
/// the refinement audit, and the JSON export, so a failure can be triaged the
/// same way regardless of which output format a CI job happens to consume.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum ProbeFailureCategory {
    /// The child process ran to completion and exited non-zero: the contract
    /// either rejected the ramp value with its own validation error or
    /// exhausted a host resource budget.
    ExitFailure,
    /// The probe exceeded `--probe-timeout` and was killed by the parent
    /// before it could finish.
    TimedOut,
    /// The child process was terminated by a signal — the host abort that
    /// real WASM execution can cause rather than a normal exit. On such a
    /// run the failure is real, but the ramp value reported may reflect the
    /// abort rather than a clean resource boundary.
    Abort,
    /// The isolated probe process could not be spawned at all; this is a
    /// host/tooling problem, not a statement about the contract.
    SpawnFailure,
}

impl ProbeFailureCategory {
    /// The stable, machine-readable category name (also the JSON key).
    pub fn label(self) -> &'static str {
        match self {
            ProbeFailureCategory::ExitFailure => "exit-failure",
            ProbeFailureCategory::TimedOut => "timed-out",
            ProbeFailureCategory::Abort => "abort",
            ProbeFailureCategory::SpawnFailure => "spawn-failure",
        }
    }

    /// A one-line, actionable explanation of what the category implies for
    /// the discovered ceiling.
    pub fn guidance(self) -> &'static str {
        match self {
            ProbeFailureCategory::ExitFailure => {
                "the contract rejected this value or exhausted a resource budget; this is the normal ceiling signal"
            }
            ProbeFailureCategory::TimedOut => {
                "raise --probe-timeout to confirm whether the ceiling reflects a hang or a real resource limit"
            }
            ProbeFailureCategory::Abort => {
                "the host aborted during WASM execution; re-run to confirm the boundary is stable"
            }
            ProbeFailureCategory::SpawnFailure => {
                "relocate/reinstall the soroban-testkit binary because probes could not be spawned"
            }
        }
    }
}

impl std::fmt::Display for ProbeFailureCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

/// The outcome of a single isolated probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// The probe exited zero: the ramp value is supported.
    Succeeded,
    /// The probe failed, classified for the report.
    Failed(ProbeFailureCategory),
}

impl ProbeOutcome {
    /// Whether the probe succeeded, for the binary search's yes/no decision.
    pub fn succeeded(self) -> bool {
        matches!(self, ProbeOutcome::Succeeded)
    }
}

/// `serde(skip_serializing_if)` helper: omitted for an empty failure map.
fn map_is_empty<K, V>(map: &BTreeMap<K, V>) -> bool {
    map.is_empty()
}

/// The single source of truth for one `limits` run.
///
/// The terminal summary ([`LimitsSummary::render_text`]) and the JSON export
/// ([`export_results`]) are both rendered from this value, so the two formats
/// can never disagree about the discovered boundary, the failing value, or
/// why probes failed.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LimitsSummary {
    /// Sanitized contract path (see [`sanitize_report_path`]).
    pub contract: String,
    pub function: String,
    pub ramp: String,
    /// The highest ramp value that succeeded.
    pub best: u32,
    /// CPU instructions consumed at `best`.
    pub instructions: u64,
    /// Memory bytes consumed at `best`.
    pub memory_bytes: u64,
    /// The first ramp value that failed, if one was found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_failed: Option<u32>,
    /// Why that first failure happened, standardized for every report.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_failure_category: Option<ProbeFailureCategory>,
    /// Stderr captured from the first failing probe, providing
    /// actionable context for why the ceiling was hit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_failed_probe_stderr: Option<String>,
    /// How many probes failed in each category over the whole run.
    #[serde(skip_serializing_if = "map_is_empty")]
    pub probe_failure_counts: BTreeMap<ProbeFailureCategory, usize>,
    /// Whether the search stopped at [`SEARCH_UPPER_BOUND`] without finding a
    /// failure.
    pub hit_upper_bound: bool,
    /// Per-probe timeout the run used, for the timeout guidance line.
    pub probe_timeout_secs: u64,
    /// Boundary-refinement audit trail, when a failure was bracketed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audit: Option<RefinementAudit>,
    pub measurement_limitations: String,
    /// Actionable instructions for reproducing the reported maximum locally.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reproduction: Option<String>,
}

impl LimitsSummary {
    /// Renders the human-readable summary. This is the only place the text
    /// output is produced, keeping it in lockstep with the JSON export.
    pub fn render_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "last successful {} for {:?}: {}\n",
            self.ramp, self.function, self.best
        ));
        out.push_str(&format!("  instructions: {}\n", self.instructions));
        out.push_str(&format!("  memory bytes: {}\n", self.memory_bytes));
        if let Some(failed) = self.first_failed {
            out.push_str(&format!("  first failed {} value: {failed}", self.ramp));
            if let Some(category) = self.first_failure_category {
                out.push_str(&format!(" [{category}]"));
            }
            out.push('\n');
        }
        if !self.probe_failure_counts.is_empty() {
            let counts = self
                .probe_failure_counts
                .iter()
                .map(|(category, count)| format!("{category}: {count}"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("  probe failures: {counts}\n"));
        }
        if let Some(stderr) = &self.first_failed_probe_stderr {
            let trimmed = stderr.trim();
            if !trimmed.is_empty() {
                out.push_str(&format!("  first failing probe stderr: {trimmed}\n"));
            }
        }
        if let Some(audit) = &self.audit {
            out.push_str(&format!(
                "  boundary refinement: probed {} values between {} and {} (final boundary: [{}, {}])\n",
                audit.steps.len(),
                audit.initial_low,
                audit.initial_high,
                audit.final_success,
                audit.final_failure,
            ));
        }
        out.push_str(&format!("  ({})\n", self.measurement_limitations));
        if let Some(repro) = &self.reproduction {
            out.push_str(&format!("  reproduce locally: {repro}\n"));
        }
        if self.hit_upper_bound {
            out.push_str(&format!(
                "  note: search stopped at the built-in upper bound ({SEARCH_UPPER_BOUND}); the \
                 contract may support higher ramp values\n"
            ));
        }
        if self
            .probe_failure_counts
            .contains_key(&ProbeFailureCategory::TimedOut)
        {
            out.push_str(&format!(
                "  note: at least one probe was killed for exceeding --probe-timeout ({}s); the \
                 discovered ceiling may reflect a hang rather than a real resource limit — rerun \
                 with a larger --probe-timeout to check\n",
                self.probe_timeout_secs
            ));
        }
        out
    }
}

/// A single step in a numeric boundary refinement search.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RefinementStep {
    pub step: usize,
    pub probe_value: u32,
    pub succeeded: bool,
    /// Why the probe failed, when it did. Uses the standardized
    /// [`ProbeFailureCategory`] vocabulary so every report agrees.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_category: Option<ProbeFailureCategory>,
    pub window_low: u32,
    pub window_high: u32,
}

/// Structured audit trail documenting boundary refinement progress and final window.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RefinementAudit {
    pub initial_low: u32,
    pub initial_high: u32,
    pub steps: Vec<RefinementStep>,
    pub final_success: u32,
    pub final_failure: u32,
}

/// Arguments for `soroban-testkit limits`.
#[derive(Args)]
pub struct LimitsArgs {
    /// Path to an existing compiled contract .wasm file. Build successfully before
    /// running limits; this command does not rebuild contracts.
    #[arg(long, value_name = "PATH")]
    contract: std::path::PathBuf,
    /// The contract function to ramp.
    #[arg(long = "fn", value_name = "NAME")]
    function: String,
    /// The parameter to increase on each attempt. Must exactly match an input
    /// name in the selected function's contract ABI; checked before probing.
    #[arg(long, value_name = "PARAM")]
    ramp: String,
    /// Explicit value for a non-ramped parameter, as NAME=VALUE. Repeatable.
    /// Scalars use their Soroban type; Bytes uses 0x-prefixed hexadecimal.
    #[arg(long = "arg", value_name = "NAME=VALUE")]
    arg_values: Vec<String>,
    /// Kill and treat as a failure any single probe that runs longer than
    /// this many seconds. Guards against a ramp value that hangs the host
    /// (rather than erroring or aborting) turning `limits` into an
    /// infinite wait.
    #[arg(long, value_name = "SECONDS", default_value_t = 30)]
    probe_timeout: u64,
    /// Compare this run's result against a baseline previously written by
    /// `--save-baseline`. Exits non-zero if the discovered ceiling
    /// regressed (a lower ramp value, or higher instructions/memory) by
    /// more than `--baseline-tolerance-pct`.
    #[arg(long, value_name = "PATH")]
    baseline: Option<std::path::PathBuf>,
    /// How much instructions/memory are allowed to grow (as a percentage
    /// of the baseline value) before `--baseline` reports a regression.
    /// The ramp ceiling itself allows no tolerance: any decrease is a
    /// regression, since it means the contract now handles *fewer*
    /// recipients (or whatever the ramp parameter represents) than before.
    #[arg(long, value_name = "PCT", default_value_t = 5.0)]
    baseline_tolerance_pct: f64,
    /// Write this run's result to PATH as a new baseline for future
    /// `--baseline` comparisons. Combinable with `--baseline` itself, to
    /// compare against the old baseline and then update it in one run.
    #[arg(long, value_name = "PATH")]
    save_baseline: Option<std::path::PathBuf>,
    /// Export this run's result to PATH as a JSON file for CI artifacts.
    /// Contains the discovered ceiling, instructions, and memory usage.
    #[arg(long, value_name = "PATH")]
    export: Option<std::path::PathBuf>,
}

/// Hidden: runs exactly one probe (a single ramp value) and reports its
/// outcome via exit code. See [`run`]'s doc comment for why this exists
/// as a separate process rather than an in-process call.
#[derive(Args)]
pub struct ProbeArgs {
    #[arg(long)]
    contract: std::path::PathBuf,
    #[arg(long = "fn")]
    function: String,
    #[arg(long)]
    ramp: String,
    #[arg(long)]
    value: u32,
    #[arg(long = "arg", value_name = "NAME=VALUE")]
    arg_values: Vec<String>,
}

/// Empirically discovers a resource ceiling by invoking `--fn` with an
/// increasing `--ramp` parameter — natively, with no network access —
/// until it fails, then reports the last successful value plus the CPU
/// instructions and memory consumed at that point.
///
/// # Subprocess isolation lifecycle
///
/// Ramping deliberately drives a real, compiled `.wasm` contract past its
/// resource limits. Verified empirically: when that failure happens
/// *during actual WASM execution* (as opposed to a native, non-WASM
/// `#[contract]` registration), the host can abort the whole process
/// (`thread caused non-unwinding panic. aborting.`) rather than return an
/// error or unwind — neither `catch_unwind` nor `try_invoke_contract`
/// (the SDK's own no-panic call path) prevents this for the WASM target.
/// A probe is therefore never invoked in-process. The full lifecycle is:
///
/// 1. **Validate first, in-process.** [`load`] and [`build_args`] check the
///    function spec, the ramp parameter, and every other parameter's type
///    before any child is spawned. This is type/spec checking, not WASM
///    execution, so it carries none of the abort risk above and a typo'd
///    `--fn`/`--ramp` reports its real cause instead of a generic probe
///    failure.
/// 2. **Spawn one child per probe.** The parent re-executes itself as
///    `soroban-testkit __limits-probe` (hidden from `--help`) with the
///    ramp value passed through `--value`. See [`run_probe`] for the child
///    half. The command creates no temporary Cargo projects and launches
///    no shell; the child's in-memory `Env` is disposable. `ProbeChild`
///    kills and reaps on ordinary interruption, error and early return.
/// 3. **Bound the probe.** The parent waits up to `--probe-timeout` in
///    [`wait_with_timeout`]. If the child finishes, its exit status decides
///    the outcome; if it overruns, the parent kills it and records a
///    [`ProbeFailureCategory::TimedOut`]. A child terminated by a signal
///    (the host abort above) is recorded as [`ProbeFailureCategory::Abort`].
/// 4. **Classify and search.** Every outcome is classified with
///    [`ProbeFailureCategory`]; the parent doubles to bracket the first
///    failure and then binary-searches with
///    [`refine_upper_bound_audited`], recording each probe's category.
/// 5. **Report once.** The collected results are assembled into a single
///    [`LimitsSummary`] that renders both the terminal output and the JSON
///    export, so the two can never disagree.
///
/// If the parent cannot spawn a child at all, that is surfaced as a
/// [`ProbeFailureCategory::SpawnFailure`] with guidance to reinstall the
/// binary, rather than being mistaken for a contract ceiling.
///
/// # Scope
///
/// The ramp parameter must be a numeric type (`u32`/`i32`/`u64`/`i64`/
/// `u128`/`i128`, ramped as the value itself) or `Vec<T>` for a supported
/// element type `T` (ramped as the element *count*, each element filled
/// with a fixed value — see [`vec_element_val`]) — this covers the common
/// "maximum recipients in a batch operation" question directly, for
/// `Vec<Address>` and beyond. Every other parameter is filled with a fixed
/// default (an address, `0`, an empty collection, ...). A parameter type
/// this command doesn't know how to default (`Map`, `Tuple`, `Option`,
/// `Result`, a user-defined type, ...) is reported as an error rather than
/// guessed at.
///
/// Ledger read/write counts and transaction size are **not** reported:
/// they come from a transaction's simulated resource footprint, which
/// this command's native invocation does not produce (that requires the
/// network-dependent `stellar contract invoke` simulation path, which
/// this crate deliberately avoids — see its "zero network access"
/// constraint). Only CPU instructions and memory, available locally via
/// the host's budget, are reported.
pub fn run(args: LimitsArgs) -> Result<(), CliError> {
    validate_percentage("--baseline-tolerance-pct", args.baseline_tolerance_pct)?;

    // Validate the configuration (function exists, ramp parameter exists,
    // every parameter's type is one this command knows how to default or
    // ramp) up front, in-process, before spawning any probes — this is
    // just type/spec checking, not invocation, so it carries none of the
    // abort risk documented above, and it means a typo'd --fn or --ramp
    // reports its actual cause instead of the generic "failed at value 1"
    // a swallowed child-process error would otherwise produce.
    let (_, env, function, ramp_index) = load(&args.contract, &args.function, &args.ramp)?;
    build_args(&env, &function, ramp_index, 1, &args.arg_values)?;
    drop(env);

    let self_exe = std::env::current_exe()
        .map_err(|err| CliError(format!("failed to locate this binary: {err}")))?;

    let probe_timeout = Duration::from_secs(args.probe_timeout);
    let mut probe_failure_counts: BTreeMap<ProbeFailureCategory, usize> = BTreeMap::new();
    let mut last_failed_probe: Option<u32> = None;
    let mut last_failure_category: Option<ProbeFailureCategory> = None;
    let mut first_failed_probe_stderr: Option<String> = None;
    let mut hit_upper_bound = false;

    let mut probe = |value: u32| -> Result<ProbeOutcome, CliError> {
        let child = Command::new(&self_exe)
            .arg("__limits-probe")
            .arg("--contract")
            .arg(&args.contract)
            .arg("--fn")
            .arg(&args.function)
            .arg("--ramp")
            .arg(&args.ramp)
            .arg("--value")
            .arg(value.to_string())
            .args(
                args.arg_values
                    .iter()
                    .flat_map(|value| ["--arg", value.as_str()]),
            )
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|err| {
                CliError(format!(
                    "failed to spawn the isolated probe process for ramp value {value}: {err}; \
                     verify the soroban-testkit binary is intact and re-run \
                     ({})",
                    ProbeFailureCategory::SpawnFailure.guidance()
                ))
            })?;
        // Own every probe so early returns or errors cannot leave it running.
        let mut child = ProbeChild { child };

        let outcome = match wait_with_timeout(&mut child.child, probe_timeout)? {
            Some(status) if status.success() => ProbeOutcome::Succeeded,
            // A signal-terminated child has no exit code; that is the host
            // abort this command exists to survive, not a normal rejection.
            Some(status) if status.code().is_none() => {
                ProbeOutcome::Failed(ProbeFailureCategory::Abort)
            }
            Some(_) => ProbeOutcome::Failed(ProbeFailureCategory::ExitFailure),
            None => ProbeOutcome::Failed(ProbeFailureCategory::TimedOut),
        };
        if let ProbeOutcome::Failed(category) = outcome {
            *probe_failure_counts.entry(category).or_insert(0) += 1;
            // Capture the child's stderr the first time a probe fails, so the
            // report carries actionable context about *why* the ceiling was
            // hit rather than silently discarding it. Only the first failure
            // is retained: later failures in the same run typically have the
            // same root cause, and keeping one keeps the report concise.
            if first_failed_probe_stderr.is_none() {
                if let Ok(stderr) = child
                    .child
                    .stderr
                    .take()
                    .map_or(Ok(String::new()), |mut s| {
                        use std::io::Read;
                        let mut buf = String::new();
                        s.read_to_string(&mut buf).map(|_| buf)
                    })
                {
                    if !stderr.trim().is_empty() {
                        first_failed_probe_stderr = Some(stderr);
                    }
                }
            }
        }
        Ok(outcome)
    };

    // Find a failing upper bound by doubling.
    let mut last_ok: Option<u32> = None;
    let mut low = 1u32;
    let mut high = None;
    loop {
        match probe(low)? {
            ProbeOutcome::Succeeded => {
                last_ok = Some(low);
                match next_ramp_value(low, SEARCH_UPPER_BOUND) {
                    Some(next) => low = next,
                    None => {
                        hit_upper_bound = true;
                        break;
                    }
                }
            }
            ProbeOutcome::Failed(category) => {
                last_failed_probe = Some(low);
                last_failure_category = Some(category);
                high = Some(low);
                break;
            }
        }
    }

    let Some(last_ok) = last_ok else {
        let category_note = match last_failure_category {
            Some(category) => format!(
                "; probe failure category: {category} ({})",
                category.guidance()
            ),
            None => String::new(),
        };
        let stderr_note = first_failed_probe_stderr
            .as_deref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| format!("; probe stderr: {s}"))
            .unwrap_or_default();
        return Err(CliError(format!(
            "{:?} failed even at the smallest ramp value (1) for parameter {:?}{category_note}\
             {stderr_note}; \
             this usually indicates a validation error in the contract rather than a \
             resource limit — check the contract's parameter validation logic and retry",
            args.function, args.ramp
        )));
    };

    // Binary search between the last success and the first failure for a
    // tighter bound, if we found one.
    let mut best = last_ok;
    let mut first_failed = last_failed_probe;
    let mut first_failure_category = last_failure_category;
    let mut refinement_audit = None;
    if let Some(hi_start) = high {
        let (last_success, first_failure, audit) =
            refine_upper_bound_audited(last_ok, hi_start, &mut probe)?;
        best = last_success;
        first_failed = Some(first_failure);
        // The value that set the final upper bound is the last failing step,
        // so its category is the standardized reason for the boundary. Fall
        // back to the doubling probe's category if refinement took no steps.
        first_failure_category = audit
            .steps
            .iter()
            .rev()
            .find(|step| !step.succeeded)
            .and_then(|step| step.failure_category)
            .or(last_failure_category);
        refinement_audit = Some(audit);
    }

    // Measure resources at the best-known-good value, in-process this
    // time (a known-successful call is safe to make directly).
    let (instructions, memory_bytes) = measure(&args, best)?;

    let reproduction = Some(build_reproduction_guidance(
        &sanitize_report_path(&args.contract),
        &args.function,
        &args.ramp,
        best,
        &args.arg_values,
    ));

    let summary = LimitsSummary {
        contract: sanitize_report_path(&args.contract),
        function: args.function.clone(),
        ramp: args.ramp.clone(),
        best,
        instructions,
        memory_bytes,
        first_failed,
        first_failure_category,
        first_failed_probe_stderr,
        probe_failure_counts,
        hit_upper_bound,
        probe_timeout_secs: args.probe_timeout,
        audit: refinement_audit,
        measurement_limitations: MEASUREMENT_LIMITATIONS.to_string(),
        reproduction,
    };
    print!("{}", summary.render_text());

    if let Some(baseline_path) = &args.baseline {
        compare_to_baseline(
            baseline_path,
            &args.ramp,
            &args.function,
            best,
            instructions,
            memory_bytes,
            args.baseline_tolerance_pct,
        )?;
    }
    if let Some(save_path) = &args.save_baseline {
        write_baseline(
            save_path,
            &args.contract,
            &args.ramp,
            &args.function,
            best,
            instructions,
            memory_bytes,
        )?;
        println!("saved baseline to {}", save_path.display());
    }
    if let Some(export_path) = &args.export {
        export_results(export_path, &summary)?;
        println!("exported results to {}", export_path.display());
    }

    Ok(())
}

/// Shell-quote a single argument for copyable POSIX-shell guidance.
/// Actual probes use Command::arg directly and never launch a shell or Cargo.
fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_./:=+".contains(&byte))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

/// Builds shell-safe instructions for reproducing the discovered ceiling.
/// Spaces and single quotes in the WASM path remain one argument.
pub fn build_reproduction_guidance(
    contract: &str,
    function: &str,
    ramp: &str,
    best: u32,
    arg_values: &[String],
) -> String {
    let mut cmd = format!(
        "soroban-testkit limits --contract {} --fn {} --ramp {}",
        shell_quote(contract),
        shell_quote(function),
        shell_quote(ramp)
    );
    for arg in arg_values {
        cmd.push_str(&format!(" --arg {}", shell_quote(arg)));
    }
    format!(
        "invoke `{function}` with `{ramp}` = {best} in a test environment; to re-verify the boundary via CLI run: `{cmd}`"
    )
}

fn validate_percentage(name: &str, pct: f64) -> Result<(), CliError> {
    if !pct.is_finite() || !(0.0..=100.0).contains(&pct) {
        return Err(CliError(format!(
            "{name} must be between 0 and 100 (got {pct})"
        )));
    }
    Ok(())
}

// Stop at the cap and clamp the last step, including when doubling would overflow.
fn next_ramp_value(current: u32, upper_bound: u32) -> Option<u32> {
    if current >= upper_bound {
        None
    } else {
        Some(current.saturating_mul(2).min(upper_bound))
    }
}

/// Refines the boundary between a known success (`low`) and a known failure (`high`)
/// using binary search, recording every intermediate step in a [`RefinementAudit`]
/// — including the standardized [`ProbeFailureCategory`] of each failing step.
/// Re-probes both endpoints before refinement and rejects a contradictory bracket.
/// The contract must succeed monotonically up to its ceiling: these checks detect
/// observed inconsistencies, but cannot prove monotonicity at unprobed values.
pub fn refine_upper_bound_audited<F>(
    mut low: u32,
    mut high: u32,
    probe: &mut F,
) -> Result<(u32, u32, RefinementAudit), CliError>
where
    F: FnMut(u32) -> Result<ProbeOutcome, CliError>,
{
    if low >= high {
        return Err(CliError(format!(
            "refine_upper_bound requires lower bound ({low}) < upper bound ({high})"
        )));
    }

    if !probe(low)?.succeeded() {
        return Err(CliError(format!(
            "non-monotonic probe results: lower bound {low} no longer succeeds; \
             binary refinement requires a stable successful lower bound and failing upper bound; \
             check contract validation and probe timeouts, then re-run"
        )));
    }
    if probe(high)?.succeeded() {
        return Err(CliError(format!(
            "non-monotonic probe results: upper bound {high} now succeeds; \
             binary refinement requires a stable successful lower bound and failing upper bound; \
             check contract validation and probe timeouts, then re-run"
        )));
    }

    let initial_low = low;
    let initial_high = high;
    let mut steps = Vec::new();
    let mut step_num = 1;

    while low.saturating_add(1) < high {
        let middle = low + (high - low) / 2;
        let outcome = probe(middle)?;
        let succeeded = outcome.succeeded();
        let failure_category = match outcome {
            ProbeOutcome::Succeeded => None,
            ProbeOutcome::Failed(category) => Some(category),
        };
        if succeeded {
            low = middle;
        } else {
            high = middle;
        }
        steps.push(RefinementStep {
            step: step_num,
            probe_value: middle,
            succeeded,
            failure_category,
            window_low: low,
            window_high: high,
        });
        step_num += 1;
    }

    let audit = RefinementAudit {
        initial_low,
        initial_high,
        steps,
        final_success: low,
        final_failure: high,
    };

    Ok((low, high, audit))
}

/// Bool-returning convenience wrapper around [`refine_upper_bound_audited`],
/// retained for callers (and tests) that only care about success/failure.
/// A `false` probe is classified as [`ProbeFailureCategory::ExitFailure`],
/// the category the isolated probe reports for a clean non-zero exit.
#[allow(dead_code)]
pub fn refine_upper_bound<F>(low: u32, high: u32, probe: &mut F) -> Result<(u32, u32), CliError>
where
    F: FnMut(u32) -> Result<bool, CliError>,
{
    let (success, failure, _) = refine_upper_bound_audited(low, high, &mut |value| {
        Ok(if probe(value)? {
            ProbeOutcome::Succeeded
        } else {
            ProbeOutcome::Failed(ProbeFailureCategory::ExitFailure)
        })
    })?;
    Ok((success, failure))
}

/// The hidden child half of the isolation lifecycle documented on [`run`]:
/// performs exactly one invocation in a throwaway [`Env`] registered from
/// the `.wasm` and returns `Ok(())` (exit 0) or `Err` (exit 1) — no
/// measurement, no output. Because the child dies after a single call, the
/// parent can classify an abort by signal without losing its own run.
///
/// The child only ever reports success/failure through its exit status; the
/// parent turns that status into a [`ProbeFailureCategory`]. Failures that
/// happen *before* invocation (an unreadable contract, a bad ramp value)
/// come back as an actionable [`CliError`] message, but the parent's
/// pre-validation normally catches those first.
pub fn run_probe(args: ProbeArgs) -> Result<(), CliError> {
    let _quiet = QuietPanics::install();
    let (contract_id, env, function, ramp_index) =
        load(&args.contract, &args.function, &args.ramp)?;
    let args_vec = build_args(&env, &function, ramp_index, args.value, &args.arg_values)?;
    let func = Symbol::new(&env, &args.function);
    if invoke_succeeds(&env, &contract_id, &func, args_vec) {
        Ok(())
    } else {
        Err(CliError("probe failed".to_string()))
    }
}

/// Owns one isolated probe. On ordinary error, early return or unwinding,
/// terminate and reap any child still running. No temporary Cargo project or
/// source directory is created by this command. An uncatchable OS termination
/// of the parent cannot invoke Drop.
struct ProbeChild {
    child: Child,
}

impl Drop for ProbeChild {
    fn drop(&mut self) {
        if let Ok(Some(_)) = self.child.try_wait() {
            return;
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Poll one child until it exits or exceeds its deadline. A timeout kills
/// and reaps before returning None. Errors name the process rather than
/// silently claiming a resource ceiling. ProbeChild handles early errors.
fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Result<Option<ExitStatus>, CliError> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| CliError("probe timeout is too large for the host clock".to_string()))?;
    loop {
        if let Some(status) = child.try_wait().map_err(|err| {
            CliError(format!(
                "failed to poll isolated probe process {}: {err}",
                child.id()
            ))
        })? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            // A child can exit between polling and kill. In that race,
            // try_wait reaps it; otherwise report the failed termination.
            match child.kill() {
                Ok(()) => {
                    child.wait().map_err(|err| {
                        CliError(format!(
                            "failed to reap timed-out probe process {}: {err}",
                            child.id()
                        ))
                    })?;
                }
                Err(kill_err) => match child.try_wait() {
                    Ok(Some(_)) => {} // Already exited and reaped.
                    Ok(None) => {
                        return Err(CliError(format!(
                            "failed to terminate timed-out probe process {}: {kill_err}",
                            child.id()
                        )));
                    }
                    Err(poll_err) => {
                        return Err(CliError(format!(
                            "failed to terminate or poll timed-out probe process {}: \
                             kill: {kill_err}; poll: {poll_err}",
                            child.id()
                        )));
                    }
                },
            }
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Sanitizes a filesystem path for machine-readable reports, ensuring host-specific
/// or process-specific temporary directory prefixes do not leak into reproducible outputs.
pub fn sanitize_report_path(path: &Path) -> String {
    let temp_dir = std::env::temp_dir();
    let is_temp = path.starts_with(&temp_dir)
        || temp_dir
            .canonicalize()
            .map(|t| path.starts_with(t))
            .unwrap_or(false)
        || path.to_string_lossy().starts_with("/tmp")
        || path.to_string_lossy().starts_with("/var/tmp");

    if is_temp {
        return path
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_else(|| "contract.wasm".to_string());
    }

    if let Ok(cwd) = std::env::current_dir() {
        if let Ok(rel) = path.strip_prefix(&cwd) {
            return rel.to_string_lossy().to_string();
        }
        if let Ok(canonical_cwd) = cwd.canonicalize() {
            if let Ok(rel) = path.strip_prefix(&canonical_cwd) {
                return rel.to_string_lossy().to_string();
            }
        }
    }

    path.to_string_lossy().to_string()
}

/// A previously-saved `limits` result, for `--baseline` regression checks.
/// Stored as a flat `key=value` file recording ceiling and resource metrics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Baseline {
    pub contract: Option<String>,
    pub function: String,
    pub ramp: String,
    pub best: u32,
    pub instructions: u64,
    pub memory_bytes: u64,
    pub measurement_limitations: Option<String>,
}

impl Baseline {
    pub fn to_file_contents(&self) -> String {
        let mut out = String::new();
        if let Some(c) = &self.contract {
            out.push_str(&format!("contract={c}\n"));
        }
        out.push_str(&format!(
            "function={}\nramp={}\nbest={}\ninstructions={}\nmemory_bytes={}\nmeasurement_limitations={}\n",
            self.function,
            self.ramp,
            self.best,
            self.instructions,
            self.memory_bytes,
            self.measurement_limitations
                .as_deref()
                .unwrap_or(MEASUREMENT_LIMITATIONS),
        ));
        out
    }

    pub fn parse(contents: &str) -> Result<Self, CliError> {
        let mut contract = None;
        let mut function = None;
        let mut ramp = None;
        let mut best = None;
        let mut instructions = None;
        let mut memory_bytes = None;
        let mut measurement_limitations = None;

        for (line_no, line) in contents.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(CliError(format!(
                    "malformed baseline file at line {}: expected key=value, got {line:?}",
                    line_no + 1
                )));
            };
            match key {
                "contract" => contract = Some(value.to_string()),
                "function" => function = Some(value.to_string()),
                "ramp" => ramp = Some(value.to_string()),
                "best" => {
                    best = Some(value.parse::<u32>().map_err(|err| {
                        CliError(format!("malformed baseline `best` value {value:?}: {err}"))
                    })?)
                }
                "instructions" => {
                    instructions = Some(value.parse::<u64>().map_err(|err| {
                        CliError(format!(
                            "malformed baseline `instructions` value {value:?}: {err}"
                        ))
                    })?)
                }
                "memory_bytes" => {
                    memory_bytes = Some(value.parse::<u64>().map_err(|err| {
                        CliError(format!(
                            "malformed baseline `memory_bytes` value {value:?}: {err}"
                        ))
                    })?)
                }
                "measurement_limitations" => measurement_limitations = Some(value.to_string()),
                other => {
                    return Err(CliError(format!(
                        "malformed baseline file at line {}: unknown key {other:?}",
                        line_no + 1
                    )))
                }
            }
        }

        Ok(Baseline {
            contract,
            function: function
                .ok_or_else(|| CliError("baseline file is missing `function`".to_string()))?,
            ramp: ramp.ok_or_else(|| CliError("baseline file is missing `ramp`".to_string()))?,
            best: best.ok_or_else(|| CliError("baseline file is missing `best`".to_string()))?,
            instructions: instructions
                .ok_or_else(|| CliError("baseline file is missing `instructions`".to_string()))?,
            memory_bytes: memory_bytes
                .ok_or_else(|| CliError("baseline file is missing `memory_bytes`".to_string()))?,
            measurement_limitations: measurement_limitations
                .or_else(|| Some(MEASUREMENT_LIMITATIONS.to_string())),
        })
    }
}

pub fn write_baseline(
    path: &Path,
    contract: &Path,
    ramp: &str,
    function: &str,
    best: u32,
    instructions: u64,
    memory_bytes: u64,
) -> Result<(), CliError> {
    let sanitized_contract = sanitize_report_path(contract);
    let baseline = Baseline {
        contract: Some(sanitized_contract),
        function: function.to_string(),
        ramp: ramp.to_string(),
        best,
        instructions,
        memory_bytes,
        measurement_limitations: Some(MEASUREMENT_LIMITATIONS.to_string()),
    };
    std::fs::write(path, baseline.to_file_contents()).map_err(|err| {
        CliError(format!(
            "failed to write baseline to {}: {err}",
            path.display()
        ))
    })
}

/// Compares this run's result against the baseline saved at `path`, printing
/// a before/after summary and returning an error (which `main` turns into a
/// non-zero exit) if it regressed beyond `--baseline-tolerance-pct`.
fn compare_to_baseline(
    path: &std::path::Path,
    ramp: &str,
    function: &str,
    best: u32,
    instructions: u64,
    memory_bytes: u64,
    tolerance_pct: f64,
) -> Result<(), CliError> {
    let contents = std::fs::read_to_string(path).map_err(|err| {
        CliError(format!(
            "failed to read baseline at {}: {err}",
            path.display()
        ))
    })?;
    let baseline = Baseline::parse(&contents)?;

    if baseline.function != function || baseline.ramp != ramp {
        return Err(CliError(format!(
            "baseline at {} was recorded for {:?}/{:?}, not {function:?}/{ramp:?}; comparing \
             across different functions or ramp parameters isn't meaningful",
            path.display(),
            baseline.function,
            baseline.ramp
        )));
    }

    println!("baseline comparison ({}):", path.display());
    println!("  {ramp}: {} -> {best}", baseline.best);
    println!(
        "  instructions: {} -> {instructions}",
        baseline.instructions
    );
    println!(
        "  memory bytes: {} -> {memory_bytes}",
        baseline.memory_bytes
    );

    let mut regressions = Vec::new();
    if best < baseline.best {
        regressions.push(format!(
            "{ramp} ceiling dropped from {} to {best}",
            baseline.best
        ));
    }
    check_metric_regression(
        "instructions",
        baseline.instructions,
        instructions,
        tolerance_pct,
        &mut regressions,
    );
    check_metric_regression(
        "memory bytes",
        baseline.memory_bytes,
        memory_bytes,
        tolerance_pct,
        &mut regressions,
    );

    fn check_metric_regression(
        name: &str,
        old: u64,
        new: u64,
        tolerance_pct: f64,
        out: &mut Vec<String>,
    ) {
        if new <= old {
            return;
        }
        let growth_pct = ((new - old) as f64 / old.max(1) as f64) * 100.0;
        if growth_pct <= tolerance_pct {
            return;
        }
        out.push(format!(
            "{name} grew from {old} to {new} ({growth_pct:.1}%, over the {tolerance_pct:.1}% tolerance)"
        ));
    }

    if regressions.is_empty() {
        println!("  no regression");
        return Ok(());
    }

    // Instruction/memory growth within tolerance is reported above but not
    // treated as a regression; only the ramp-ceiling check and
    // over-tolerance growth fail the run.
    Err(CliError(format!(
        "baseline regression detected: {}",
        regressions.join("; ")
    )))
}

/// Writes `summary` to `path` in the format implied by its extension:
/// pretty JSON for `.json`, otherwise the flat `key=value` baseline format.
///
/// Both branches are rendered from the same [`LimitsSummary`] the terminal
/// output uses, so a boundary or failure category can never differ between
/// what a human sees and what CI reads back.
pub fn export_results(path: &Path, summary: &LimitsSummary) -> Result<(), CliError> {
    if path.extension().is_some_and(|ext| ext == "json") {
        let json_str = serde_json::to_string_pretty(summary)
            .map_err(|err| CliError(format!("failed to format JSON export: {err}")))?;
        std::fs::write(path, json_str).map_err(|err| {
            CliError(format!(
                "failed to export results to {}: {err}",
                path.display()
            ))
        })
    } else {
        let baseline = Baseline {
            contract: Some(summary.contract.clone()),
            function: summary.function.clone(),
            ramp: summary.ramp.clone(),
            best: summary.best,
            instructions: summary.instructions,
            memory_bytes: summary.memory_bytes,
            measurement_limitations: Some(summary.measurement_limitations.clone()),
        };
        std::fs::write(path, baseline.to_file_contents()).map_err(|err| {
            CliError(format!(
                "failed to export results to {}: {err}",
                path.display()
            ))
        })
    }
}

fn measure(args: &LimitsArgs, value: u32) -> Result<(u64, u64), CliError> {
    let (contract_id, env, function, ramp_index) =
        load(&args.contract, &args.function, &args.ramp)?;
    let func = Symbol::new(&env, &args.function);
    let mut budget = env.cost_estimate().budget();
    budget.reset_default();
    let args_vec = build_args(&env, &function, ramp_index, value, &args.arg_values)?;
    if !invoke_succeeds(&env, &contract_id, &func, args_vec) {
        return Err(CliError(format!(
            "internal error: {:?} reported ramp value {value} as successful during probing \
             but failed on in-process re-measurement for parameter {:?}; re-run to confirm the \
             boundary is stable, or lower --probe-timeout if the run is racing a hang",
            args.function, args.ramp
        )));
    }
    Ok((budget.cpu_instruction_cost(), budget.memory_bytes_cost()))
}

/// Silences the default panic hook for the duration it's held, restoring
/// the previous hook on drop, so a probe's failure doesn't dump a panic
/// message and the host's diagnostic event log to stderr.
type PanicHook = Box<dyn Fn(&panic::PanicHookInfo<'_>) + Sync + Send>;

struct QuietPanics {
    previous: Option<PanicHook>,
}

impl QuietPanics {
    fn install() -> Self {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(|_| {}));
        Self {
            previous: Some(previous),
        }
    }
}

impl Drop for QuietPanics {
    fn drop(&mut self) {
        if let Some(hook) = self.previous.take() {
            panic::set_hook(hook);
        }
    }
}

fn load(
    contract: &std::path::Path,
    function: &str,
    ramp: &str,
) -> Result<(Address, Env, ScSpecFunctionV0, usize), CliError> {
    let wasm = std::fs::read(contract).map_err(|err| {
        let guidance = if err.kind() == std::io::ErrorKind::NotFound {
            "contract file not found; check --contract (relative paths use the current directory) \
             and build the contract successfully before retrying"
        } else {
            "check that --contract points to a readable .wasm file"
        };
        CliError(format!(
            "failed to read {}: {err}; {guidance}",
            contract.display()
        ))
    })?;

    let entries = soroban_spec::read::from_wasm(&wasm)
        .map_err(|err| CliError(format!("failed to read contract spec: {err}")))?;

    if entries.is_empty() {
        return Err(CliError(format!(
            "{}: contract has no exported functions in its spec; \
             did you include #[contract] and #[contractimpl] on your contract?",
            contract.display()
        )));
    }

    let available_functions: Vec<String> = entries
        .iter()
        .filter_map(|entry| match entry {
            ScSpecEntry::FunctionV0(f) => Some(f.name.to_string()),
            _ => None,
        })
        .collect();

    let func_spec = entries
        .into_iter()
        .find_map(|entry| match entry {
            ScSpecEntry::FunctionV0(f) if f.name.to_string() == function => Some(f),
            _ => None,
        })
        .ok_or_else(|| {
            if available_functions.is_empty() {
                CliError(format!(
                    "{}: contract has no exported functions in its spec; \
                     did you include #[contract] and #[contractimpl] on your contract?",
                    contract.display()
                ))
            } else {
                CliError(format!(
                    "no function named {function:?} in {}'s spec; \
                     available functions: {}",
                    contract.display(),
                    available_functions.join(", ")
                ))
            }
        })?;

    let ramp_index = func_spec
        .inputs
        .iter()
        .position(|input| input.name.to_utf8_string_lossy() == ramp)
        .ok_or_else(|| {
            let param_names: Vec<String> = func_spec
                .inputs
                .iter()
                .map(|p| p.name.to_utf8_string_lossy().to_string())
                .collect();
            let (available, guidance) = if param_names.is_empty() {
                (
                    "(none)".to_string(),
                    "choose a function with a numeric, Bytes, or Vec parameter using --fn",
                )
            } else {
                (
                    param_names.join(", "),
                    "set --ramp to an input name from the selected function's contract ABI",
                )
            };
            CliError(format!(
                "{function:?} has no parameter named {ramp:?}; \
                 available parameters: {available}; {guidance}"
            ))
        })?;

    let env = Env::new_with_config(soroban_sdk::testutils::EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    // Auth is not what this command measures — it exists to find a
    // resource ceiling, not to prove who may call the function — and each
    // probe runs in its own throwaway process/Env, so blanket-approving
    // auth here carries none of the AuthMatrix-correctness risk that a
    // shared, longer-lived TestEnv would (see TestToken's mint, which
    // deliberately does NOT do this for that reason).
    env.mock_all_auths_allowing_non_root_auth();
    let contract_id = env.register(wasm.as_slice(), ());

    Ok((contract_id, env, func_spec, ramp_index))
}

fn build_args(
    env: &Env,
    function: &ScSpecFunctionV0,
    ramp_index: usize,
    ramp_value: u32,
    explicit: &[String],
) -> Result<SVec<Val>, CliError> {
    let mut overrides = std::collections::HashMap::new();
    for item in explicit {
        let (name, value) = item
            .split_once('=')
            .ok_or_else(|| CliError(format!("invalid --arg {item:?}; expected NAME=VALUE")))?;
        if name.is_empty()
            || overrides
                .insert(name.to_string(), value.to_string())
                .is_some()
        {
            return Err(CliError(format!(
                "empty or duplicate --arg parameter {name:?}"
            )));
        }
    }
    let admin = Address::generate(env);
    let mut vals = SVec::new(env);
    let ramp_name = function.inputs[ramp_index].name.to_utf8_string_lossy();
    for name in overrides.keys() {
        if name == &ramp_name {
            return Err(CliError(format!(
                "--arg cannot override ramp parameter {name:?}"
            )));
        }
        if !function
            .inputs
            .iter()
            .any(|input| input.name.to_utf8_string_lossy() == *name)
        {
            return Err(CliError(format!("--arg names unknown parameter {name:?}")));
        }
    }
    for (i, input) in function.inputs.iter().enumerate() {
        let input_name = input.name.to_utf8_string_lossy();
        let val = if i == ramp_index {
            ramp_val(env, &input.type_, ramp_value)?
        } else if let Some(value) = overrides.get(input_name.as_str()) {
            explicit_val(env, &input.type_, value)?
        } else {
            default_val(env, &admin, &input.type_, input)?
        };
        vals.push_back(val);
    }
    Ok(vals)
}

fn explicit_val(env: &Env, type_: &ScSpecTypeDef, value: &str) -> Result<Val, CliError> {
    macro_rules! parse {
        ($ty:ty) => {
            value
                .parse::<$ty>()
                .map_err(|err| CliError(format!("invalid value {value:?}: {err}")))?
                .into_val(env)
        };
    }
    match type_ {
        ScSpecTypeDef::Bool => value
            .parse::<bool>()
            .map(|v| v.into_val(env))
            .map_err(|_| CliError(format!("invalid bool value {value:?}; use true or false"))),
        ScSpecTypeDef::U32 => Ok(parse!(u32)),
        ScSpecTypeDef::I32 => Ok(parse!(i32)),
        ScSpecTypeDef::U64 => Ok(parse!(u64)),
        ScSpecTypeDef::I64 => Ok(parse!(i64)),
        ScSpecTypeDef::U128 => Ok(parse!(u128)),
        ScSpecTypeDef::I128 => Ok(parse!(i128)),
        ScSpecTypeDef::Symbol => Symbol::try_from_val(env, &value)
            .map(|symbol| symbol.into_val(env))
            .map_err(|err| CliError(format!("invalid Symbol value {value:?}: {err:?}"))),
        ScSpecTypeDef::String => Ok(soroban_sdk::String::from_str(env, value).into_val(env)),
        ScSpecTypeDef::Bytes => {
            let hex = value
                .strip_prefix("0x")
                .ok_or_else(|| CliError("Bytes --arg values must start with 0x".to_string()))?;
            let bytes = decode_hex(hex)?;
            // An empty override (0x with no digits) must produce the same
            // empty `Bytes` that `default_val` and `vec_element_val` use
            // (`Bytes::new`), not `Bytes::from_slice(&[])`, which serializes
            // to invalid XDR for the zero-length case.
            if bytes.is_empty() {
                Ok(soroban_sdk::Bytes::new(env).into_val(env))
            } else {
                Ok(soroban_sdk::Bytes::from_slice(env, &bytes).into_val(env))
            }
        }
        ScSpecTypeDef::Address => {
            let strkey = value
                .parse::<stellar_strkey::Strkey>()
                .map_err(|err| CliError(format!("invalid Stellar address {value:?}: {err}")))?;
            if !matches!(
                strkey,
                stellar_strkey::Strkey::PublicKeyEd25519(_) | stellar_strkey::Strkey::Contract(_)
            ) {
                return Err(CliError(format!(
                    "{value:?} is a valid strkey but not an account or contract address"
                )));
            }
            Ok(Address::from_str(env, value).into_val(env))
        }
        other => Err(CliError(format!(
            "--arg does not support explicit values for {other:?}"
        ))),
    }
}

fn decode_hex(hex: &str) -> Result<Vec<u8>, CliError> {
    if !hex.len().is_multiple_of(2) {
        return Err(CliError(
            "Bytes --arg hex must contain pairs of digits".to_string(),
        ));
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&hex[i..i + 2], 16)
                .map_err(|_| CliError(format!("invalid hexadecimal Bytes value {hex:?}")))
        })
        .collect()
}

fn invoke_succeeds(env: &Env, contract_id: &Address, func: &Symbol, args: SVec<Val>) -> bool {
    // try_invoke_contract is the SDK's own no-panic call path, used here
    // so a probe that succeeds doesn't need catch_unwind at all — only a
    // *failing* real-WASM call risks the process abort documented above,
    // and that only ever happens in the isolated child process.
    matches!(
        env.try_invoke_contract::<Val, soroban_sdk::Error>(contract_id, func, args),
        Ok(Ok(_))
    )
}

fn ramp_val(env: &Env, type_: &ScSpecTypeDef, ramp_value: u32) -> Result<Val, CliError> {
    match type_ {
        ScSpecTypeDef::U32 => Ok(ramp_value.into_val(env)),
        ScSpecTypeDef::I32 => Ok((ramp_value as i32).into_val(env)),
        ScSpecTypeDef::U64 => Ok((ramp_value as u64).into_val(env)),
        ScSpecTypeDef::I64 => Ok((ramp_value as i64).into_val(env)),
        ScSpecTypeDef::U128 => Ok((ramp_value as u128).into_val(env)),
        ScSpecTypeDef::I128 => Ok((ramp_value as i128).into_val(env)),
        ScSpecTypeDef::Bytes => {
            let bytes = vec![0u8; ramp_value as usize];
            Ok(soroban_sdk::Bytes::from_slice(env, &bytes).into_val(env))
        }
        // Any Vec<T> is ramped as its element *count*, filled with a fixed
        // per-element value from `vec_element_val` — the same "maximum
        // recipients" shape as the original Vec<Address>-only support, now
        // generalized to any element type this command already knows how
        // to default. The element value itself never varies across the
        // ramp; only the count does.
        ScSpecTypeDef::Vec(inner) => {
            let element = vec_element_val(env, &inner.element_type)?;
            let mut items = SVec::new(env);
            for _ in 0..ramp_value {
                items.push_back(element);
            }
            Ok(items.into_val(env))
        }
        other => Err(CliError(format!(
            "the ramp parameter's type ({other:?}) isn't supported yet; supported ramp types \
            are u32/i32/u64/i64/u128/i128, Bytes, and Vec<T> (for the element types listed in \
             the Vec<T> element-type error, if T itself isn't supported)"
        ))),
    }
}

/// A fixed filler value for one element of a ramped `Vec<T>`. Deliberately
/// narrower than [`default_val`]: it has no `admin`/`input` context (an
/// element has no parameter name to apply the `token`-heuristic to, and
/// nothing meaningfully "ramps" as a nested Vec-of-Vec's inner count), so
/// unsupported element types are reported explicitly rather than silently
/// reusing a heuristic that wouldn't make sense at this level.
fn vec_element_val(env: &Env, type_: &ScSpecTypeDef) -> Result<Val, CliError> {
    match type_ {
        ScSpecTypeDef::Address => Ok(Address::generate(env).into_val(env)),
        ScSpecTypeDef::Bool => Ok(false.into_val(env)),
        ScSpecTypeDef::U32 => Ok(1u32.into_val(env)),
        ScSpecTypeDef::I32 => Ok(1i32.into_val(env)),
        ScSpecTypeDef::U64 => Ok(1u64.into_val(env)),
        ScSpecTypeDef::I64 => Ok(1i64.into_val(env)),
        ScSpecTypeDef::U128 => Ok(1u128.into_val(env)),
        ScSpecTypeDef::I128 => Ok(1i128.into_val(env)),
        ScSpecTypeDef::Symbol => Ok(Symbol::new(env, "x").into_val(env)),
        ScSpecTypeDef::String => Ok(soroban_sdk::String::from_str(env, "").into_val(env)),
        ScSpecTypeDef::Bytes => Ok(soroban_sdk::Bytes::new(env).into_val(env)),
        other => Err(CliError(format!(
            "Vec<{other:?}> isn't supported as a ramp parameter yet; supported Vec<T> element \
             types are Address/Bool/u32/i32/u64/i64/u128/i128/Symbol/String/Bytes"
        ))),
    }
}

fn default_val(
    env: &Env,
    admin: &Address,
    type_: &ScSpecTypeDef,
    input: &ScSpecFunctionInputV0,
) -> Result<Val, CliError> {
    match type_ {
        // A parameter literally named `token` is, by overwhelming Soroban
        // convention, a token contract address — and a batch/payout-style
        // function will call transfer() on it and needs `admin` to hold a
        // real balance, not just a syntactically valid address. Deploying
        // a funded Stellar Asset Contract for this one case is a
        // name-based heuristic, not a semantic guarantee, but it's what
        // makes `limits` usable against a real payout contract at all
        // (verified against sororail-contracts' batch_payout, whose
        // execute_equal(funder, token, recipients, amount_each) is
        // exactly this shape).
        ScSpecTypeDef::Address if input.name.to_utf8_string_lossy() == "token" => {
            let issuer = Address::generate(env);
            let sac = env.register_stellar_asset_contract_v2(issuer.clone());
            let token_address = sac.address();
            let amount = i128::MAX / 2;
            StellarAssetClient::new(env, &token_address)
                .mock_auths(&[MockAuth {
                    address: &issuer,
                    invoke: &MockAuthInvoke {
                        contract: &token_address,
                        fn_name: "mint",
                        args: (admin.clone(), amount).into_val(env),
                        sub_invokes: &[],
                    },
                }])
                .mint(admin, &amount);
            Ok(token_address.into_val(env))
        }
        ScSpecTypeDef::Address => Ok(admin.into_val(env)),
        ScSpecTypeDef::Bool => Ok(false.into_val(env)),
        // 1, not 0: verified against sororail-contracts' batch_payout,
        // whose execute_equal(..., amount_each: i128) requires a positive
        // amount and rejects 0 with its own InvalidAmount error before
        // any resource-limit-relevant work happens. There's no default
        // that's safe for every contract's validation rules, but 1 is
        // the smaller mistake: far more contracts reject a non-positive
        // amount than reject exactly 1.
        ScSpecTypeDef::U32 => Ok(1u32.into_val(env)),
        ScSpecTypeDef::I32 => Ok(1i32.into_val(env)),
        ScSpecTypeDef::U64 => Ok(1u64.into_val(env)),
        ScSpecTypeDef::I64 => Ok(1i64.into_val(env)),
        ScSpecTypeDef::U128 => Ok(1u128.into_val(env)),
        ScSpecTypeDef::I128 => Ok(1i128.into_val(env)),
        ScSpecTypeDef::Symbol => Ok(Symbol::new(env, "x").into_val(env)),
        ScSpecTypeDef::String => Ok(soroban_sdk::String::from_str(env, "").into_val(env)),
        ScSpecTypeDef::Bytes => Ok(soroban_sdk::Bytes::new(env).into_val(env)),
        ScSpecTypeDef::Vec(_) => Ok(SVec::<Val>::new(env).into_val(env)),
        other => Err(CliError(format!(
            "parameter {:?} has type {other:?}, which this command doesn't know how to \
             default yet; only the --ramp parameter needs a type this command understands \
             today, every other parameter needs a supported default type",
            input.name.to_utf8_string_lossy()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::xdr::ScSpecTypeVec;
    use std::collections::BTreeMap;

    // ---- #239: Vec<T> ramp support ----

    #[test]
    fn ramp_val_vec_address_produces_the_requested_count() {
        let env = Env::default();
        let type_ = ScSpecTypeDef::Vec(Box::new(ScSpecTypeVec {
            element_type: Box::new(ScSpecTypeDef::Address),
        }));
        let val = ramp_val(&env, &type_, 5).unwrap();
        let vec: SVec<Address> = val.into_val(&env);
        assert_eq!(vec.len(), 5);
    }

    #[test]
    fn ramp_val_vec_u32_produces_the_requested_count() {
        let env = Env::default();
        let type_ = ScSpecTypeDef::Vec(Box::new(ScSpecTypeVec {
            element_type: Box::new(ScSpecTypeDef::U32),
        }));
        let val = ramp_val(&env, &type_, 7).unwrap();
        let vec: SVec<u32> = val.into_val(&env);
        assert_eq!(vec.len(), 7);
        assert_eq!(vec.get(0), Some(1u32));
    }

    #[test]
    fn ramp_val_vec_of_zero_is_an_empty_vec() {
        let env = Env::default();
        let type_ = ScSpecTypeDef::Vec(Box::new(ScSpecTypeVec {
            element_type: Box::new(ScSpecTypeDef::Symbol),
        }));
        let val = ramp_val(&env, &type_, 0).unwrap();
        let vec: SVec<Symbol> = val.into_val(&env);
        assert!(vec.is_empty());
    }

    #[test]
    fn ramp_val_rejects_vec_of_unsupported_element_type() {
        let env = Env::default();
        let type_ = ScSpecTypeDef::Vec(Box::new(ScSpecTypeVec {
            element_type: Box::new(ScSpecTypeDef::Vec(Box::new(ScSpecTypeVec {
                element_type: Box::new(ScSpecTypeDef::U32),
            }))),
        }));
        let err = ramp_val(&env, &type_, 3).unwrap_err();
        assert!(err.0.contains("isn't supported"), "{}", err.0);
    }

    #[test]
    fn ramp_val_rejects_an_unsupported_scalar_type() {
        let env = Env::default();
        let err = ramp_val(&env, &ScSpecTypeDef::Void, 1).unwrap_err();
        assert!(err.0.contains("isn't supported yet"), "{}", err.0);
    }

    #[test]
    fn ramp_val_bytes_uses_the_requested_length() {
        let env = Env::default();
        let val = ramp_val(&env, &ScSpecTypeDef::Bytes, 4).unwrap();
        let bytes: soroban_sdk::Bytes = val.into_val(&env);
        assert_eq!(bytes.len(), 4);
        assert_eq!(bytes.get(0), Some(0));
    }

    #[test]
    fn explicit_values_parse_typed_scalars_and_bytes() {
        let env = Env::default();
        let value: u32 = explicit_val(&env, &ScSpecTypeDef::U32, "42")
            .unwrap()
            .into_val(&env);
        assert_eq!(value, 42);
        let bytes: soroban_sdk::Bytes = explicit_val(&env, &ScSpecTypeDef::Bytes, "0x00a1")
            .unwrap()
            .into_val(&env);
        assert_eq!(bytes.len(), 2);
        assert_eq!(
            decode_hex("abc").unwrap_err().0,
            "Bytes --arg hex must contain pairs of digits"
        );
    }

    #[test]
    fn ramp_steps_stop_at_the_cap_without_overflow() {
        assert_eq!(next_ramp_value(1, SEARCH_UPPER_BOUND), Some(2));
        assert_eq!(
            next_ramp_value(SEARCH_UPPER_BOUND / 2, SEARCH_UPPER_BOUND),
            Some(SEARCH_UPPER_BOUND)
        );
        assert_eq!(
            next_ramp_value(SEARCH_UPPER_BOUND, SEARCH_UPPER_BOUND),
            None
        );
        assert_eq!(next_ramp_value(8, 13), Some(13));
        assert_eq!(next_ramp_value(u32::MAX / 2 + 1, u32::MAX), Some(u32::MAX));
        assert_eq!(next_ramp_value(u32::MAX, u32::MAX), None);
        assert_eq!(next_ramp_value(u32::MAX, SEARCH_UPPER_BOUND), None);
    }

    #[test]
    fn refinement_rejects_changed_endpoints_before_probing_interior() {
        for (lower_succeeds, upper_succeeds, expected, calls) in [
            (false, false, "lower bound 8 no longer succeeds", vec![8]),
            (true, true, "upper bound 32 now succeeds", vec![8, 32]),
            (false, true, "lower bound 8 no longer succeeds", vec![8]),
        ] {
            let mut probed = Vec::new();
            let err = refine_upper_bound(8, 32, &mut |value| {
                probed.push(value);
                Ok(if value == 8 {
                    lower_succeeds
                } else {
                    upper_succeeds
                })
            })
            .unwrap_err();
            assert!(err.0.contains("non-monotonic probe results"), "{}", err.0);
            assert!(err.0.contains(expected), "{}", err.0);
            assert_eq!(probed, calls);
        }
    }

    #[test]
    fn refinement_validates_adjacent_bounds_and_handles_u32_max() {
        let (low, high, audit) = refine_upper_bound_audited(u32::MAX - 1, u32::MAX, &mut |value| {
            Ok(if value < u32::MAX {
                ProbeOutcome::Succeeded
            } else {
                ProbeOutcome::Failed(ProbeFailureCategory::ExitFailure)
            })
        })
        .unwrap();
        assert_eq!((low, high), (u32::MAX - 1, u32::MAX));
        assert!(audit.steps.is_empty());
        let result = refine_upper_bound(1, u32::MAX, &mut |v| Ok(v < u32::MAX)).unwrap();
        assert_eq!(result, (u32::MAX - 1, u32::MAX));
    }

    #[test]
    fn refinement_preserves_probe_errors() {
        let err = refine_upper_bound(1, 4, &mut |_| Err(CliError("cannot spawn probe".into())))
            .unwrap_err();
        assert_eq!(err.0, "cannot spawn probe");
    }

    #[test]
    fn binary_search_refines_to_adjacent_success_and_failure() {
        let mut probed = Vec::new();
        let (success, failure) = refine_upper_bound(8, 32, &mut |value| {
            probed.push(value);
            Ok(value <= 19)
        })
        .unwrap();
        assert_eq!((success, failure), (19, 20));
        assert_eq!(&probed[..2], &[8, 32]);
        assert!(probed.len() <= 7);
    }

    // ---- #245: baseline comparison ----

    fn sample_baseline() -> Baseline {
        Baseline {
            contract: Some("vault.wasm".to_string()),
            function: "batch_payout".to_string(),
            ramp: "recipients".to_string(),
            best: 100,
            instructions: 1_000_000,
            memory_bytes: 50_000,
            measurement_limitations: Some(MEASUREMENT_LIMITATIONS.to_string()),
        }
    }

    fn sample_summary() -> LimitsSummary {
        LimitsSummary {
            contract: "vault.wasm".to_string(),
            function: "batch_payout".to_string(),
            ramp: "recipients".to_string(),
            best: 100,
            instructions: 1_000_000,
            memory_bytes: 50_000,
            first_failed: Some(101),
            first_failure_category: Some(ProbeFailureCategory::ExitFailure),
            first_failed_probe_stderr: None,
            probe_failure_counts: BTreeMap::from([(ProbeFailureCategory::ExitFailure, 1)]),
            hit_upper_bound: false,
            probe_timeout_secs: 30,
            audit: None,
            measurement_limitations: MEASUREMENT_LIMITATIONS.to_string(),
            reproduction: Some(
                "invoke `batch_payout` with `recipients` = 100 in a test environment; to re-verify the boundary via CLI run: `soroban-testkit limits --contract vault.wasm --fn batch_payout --ramp recipients`".to_string(),
            ),
        }
    }

    /// A summary whose contract field went through [`sanitize_report_path`],
    /// mirroring what `run` stores before exporting.
    fn summary_for_contract(contract: &Path) -> LimitsSummary {
        LimitsSummary {
            contract: sanitize_report_path(contract),
            ..sample_summary()
        }
    }

    #[test]
    fn baseline_round_trips_through_its_file_format() {
        let baseline = sample_baseline();
        let parsed = Baseline::parse(&baseline.to_file_contents()).unwrap();
        assert_eq!(parsed.contract, baseline.contract);
        assert_eq!(parsed.function, baseline.function);
        assert_eq!(parsed.ramp, baseline.ramp);
        assert_eq!(parsed.best, baseline.best);
        assert_eq!(parsed.instructions, baseline.instructions);
        assert_eq!(parsed.memory_bytes, baseline.memory_bytes);
        assert_eq!(
            parsed.measurement_limitations,
            baseline.measurement_limitations
        );
    }

    #[test]
    fn baseline_parse_rejects_a_missing_field() {
        let err = Baseline::parse("function=f\nramp=r\nbest=1\n").unwrap_err();
        assert!(err.0.contains("missing `instructions`"), "{}", err.0);
    }

    #[test]
    fn baseline_parse_rejects_an_unknown_key() {
        let err = Baseline::parse("function=f\nramp=r\nbest=1\nbogus=2\n").unwrap_err();
        assert!(err.0.contains("unknown key"), "{}", err.0);
    }

    #[test]
    fn baseline_parse_rejects_a_malformed_line() {
        let err = Baseline::parse("not-a-key-value-line\n").unwrap_err();
        assert!(err.0.contains("malformed baseline file"), "{}", err.0);
    }

    #[test]
    fn compare_to_baseline_passes_when_nothing_regressed() {
        let dir = std::env::temp_dir().join(format!("stk-baseline-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pass.baseline");
        std::fs::write(&path, sample_baseline().to_file_contents()).unwrap();

        let result = compare_to_baseline(
            &path,
            "recipients",
            "batch_payout",
            100,
            1_000_000,
            50_000,
            5.0,
        );
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[test]
    fn compare_to_baseline_fails_when_the_ramp_ceiling_drops() {
        let dir = std::env::temp_dir().join(format!("stk-baseline-{}-2", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ceiling-drop.baseline");
        std::fs::write(&path, sample_baseline().to_file_contents()).unwrap();

        let err = compare_to_baseline(
            &path,
            "recipients",
            "batch_payout",
            90,
            1_000_000,
            50_000,
            5.0,
        )
        .unwrap_err();
        assert!(
            err.0.contains("ceiling dropped from 100 to 90"),
            "{}",
            err.0
        );
    }

    #[test]
    fn compare_to_baseline_fails_when_instructions_grow_past_tolerance() {
        let dir = std::env::temp_dir().join(format!("stk-baseline-{}-3", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("instr-grow.baseline");
        std::fs::write(&path, sample_baseline().to_file_contents()).unwrap();

        // +10% instructions, tolerance is 5%.
        let err = compare_to_baseline(
            &path,
            "recipients",
            "batch_payout",
            100,
            1_100_000,
            50_000,
            5.0,
        )
        .unwrap_err();
        assert!(err.0.contains("instructions grew"), "{}", err.0);
    }

    #[test]
    fn compare_to_baseline_allows_growth_within_tolerance() {
        let dir = std::env::temp_dir().join(format!("stk-baseline-{}-4", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("within-tolerance.baseline");
        std::fs::write(&path, sample_baseline().to_file_contents()).unwrap();

        // +2% instructions, tolerance is 5%.
        let result = compare_to_baseline(
            &path,
            "recipients",
            "batch_payout",
            100,
            1_020_000,
            50_000,
            5.0,
        );
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[test]
    fn compare_to_baseline_rejects_a_mismatched_function_or_ramp() {
        let dir = std::env::temp_dir().join(format!("stk-baseline-{}-5", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mismatch.baseline");
        std::fs::write(&path, sample_baseline().to_file_contents()).unwrap();

        let err = compare_to_baseline(&path, "amount", "batch_payout", 100, 1_000_000, 50_000, 5.0)
            .unwrap_err();
        assert!(err.0.contains("was recorded for"), "{}", err.0);
    }

    #[test]
    fn compare_to_baseline_reports_a_missing_file_actionably() {
        let missing = std::env::temp_dir().join("this-baseline-does-not-exist.baseline");
        let err = compare_to_baseline(
            &missing,
            "recipients",
            "batch_payout",
            100,
            1_000_000,
            50_000,
            5.0,
        )
        .unwrap_err();
        assert!(err.0.contains("failed to read baseline"), "{}", err.0);
    }

    // ---- #243: per-probe timeout ----

    #[test]
    fn wait_with_timeout_returns_the_exit_status_for_a_fast_process() {
        let mut child = Command::new("true").spawn().unwrap();
        let status = wait_with_timeout(&mut child, Duration::from_secs(5))
            .unwrap()
            .expect("should not have timed out");
        assert!(status.success());
    }

    #[test]
    fn baseline_tolerance_must_be_between_zero_and_one_hundred() {
        for pct in [-0.1, 100.1, f64::NAN, f64::INFINITY] {
            let err = validate_percentage("--baseline-tolerance-pct", pct).unwrap_err();
            assert!(err.0.contains("between 0 and 100"), "{}", err.0);
        }
    }

    #[test]
    fn wait_with_timeout_kills_and_returns_none_for_a_slow_process() {
        let mut child = Command::new("sleep").arg("5").spawn().unwrap();
        let start = Instant::now();
        let result = wait_with_timeout(&mut child, Duration::from_millis(200)).unwrap();
        assert!(result.is_none());
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "should have killed the child near the timeout, not waited for it to finish"
        );
    }

    #[test]
    fn shell_quote_protects_paths_with_spaces_and_single_quotes() {
        assert_eq!(shell_quote("vault.wasm"), "vault.wasm");
        assert_eq!(shell_quote("my dir/vault.wasm"), "'my dir/vault.wasm'");
        assert_eq!(shell_quote("can't run.wasm"), "'can'\\''t run.wasm'");
        assert_eq!(shell_quote("$(touch bad)"), "'$(touch bad)'");
        let guide = build_reproduction_guidance(
            "my dir/can't run.wasm",
            "batch_payout",
            "recipients",
            3,
            &["memo=hello world".into()],
        );
        assert!(guide.contains("--contract 'my dir/can'\\''t run.wasm'"));
        assert!(guide.contains("--arg 'memo=hello world'"));
    }

    #[test]
    fn completed_probe_retains_actual_nonzero_exit_status() {
        let mut child = Command::new("sh").args(["-c", "exit 17"]).spawn().unwrap();
        let status = wait_with_timeout(&mut child, Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert_eq!(status.code(), Some(17));
        assert_eq!(child.try_wait().unwrap().unwrap().code(), Some(17));
    }

    #[test]
    fn timed_out_probe_is_killed_and_reaped() {
        let mut child = Command::new("sleep").arg("10").spawn().unwrap();
        assert!(wait_with_timeout(&mut child, Duration::from_millis(40))
            .unwrap()
            .is_none());
        assert!(child.try_wait().unwrap().is_some(), "child must be reaped");
    }

    #[test]
    fn abandoned_probe_guard_prevents_late_artifacts() {
        let root = std::env::temp_dir().join(format!("stk-interrupted-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let marker = root.join("late-artifact");
        let child = Command::new("sh")
            .args(["-c", "sleep 1; touch \"$1\"", "_"])
            .arg(&marker)
            .current_dir(&root)
            .spawn()
            .unwrap();
        {
            // Simulates an early return/error: the guard kills the child.
            let _probe = ProbeChild { child };
        }
        std::thread::sleep(Duration::from_millis(1100));
        assert!(!marker.exists(), "an abandoned probe wrote after Drop");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        std::fs::remove_dir(&root).unwrap();
    }

    #[test]
    fn oversized_timeout_is_actionable_and_guard_still_reaps() {
        let child = Command::new("sleep").arg("10").spawn().unwrap();
        let mut guard = ProbeChild { child };
        let err = wait_with_timeout(&mut guard.child, Duration::MAX).unwrap_err();
        assert!(err.0.contains("too large for the host clock"), "{}", err.0);
        drop(guard);
    }

    // ---- #244: export results ----

    #[test]
    fn export_results_writes_a_file_with_the_correct_format() {
        let dir = std::env::temp_dir().join(format!("stk-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("export.results");

        export_results(&path, &sample_summary()).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("contract=vault.wasm"));
        assert!(contents.contains("function=batch_payout"));
        assert!(contents.contains("ramp=recipients"));
        assert!(contents.contains("best=100"));
        assert!(contents.contains("instructions=1000000"));
        assert!(contents.contains("memory_bytes=50000"));
        assert!(contents.contains(&format!(
            "measurement_limitations={MEASUREMENT_LIMITATIONS}"
        )));
    }

    #[test]
    fn export_results_can_be_parsed_as_a_baseline() {
        let dir = std::env::temp_dir().join(format!("stk-export-{}-2", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("export.results");

        export_results(&path, &sample_summary()).unwrap();

        let baseline = Baseline::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(baseline.contract, Some("vault.wasm".to_string()));
        assert_eq!(baseline.function, "batch_payout");
        assert_eq!(baseline.ramp, "recipients");
        assert_eq!(baseline.best, 100);
        assert_eq!(baseline.instructions, 1_000_000);
        assert_eq!(baseline.memory_bytes, 50_000);
    }

    // ---- #233: paths with spaces ----

    #[test]
    fn write_baseline_handles_paths_with_spaces() {
        let dir = std::env::temp_dir().join(format!("stk baseline test {}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("my baseline.txt");

        write_baseline(
            &path,
            Path::new("vault.wasm"),
            "recipients",
            "batch_payout",
            100,
            1_000_000,
            50_000,
        )
        .unwrap();

        let baseline = Baseline::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(baseline.contract, Some("vault.wasm".to_string()));
        assert_eq!(baseline.function, "batch_payout");
        assert_eq!(baseline.ramp, "recipients");
        assert_eq!(baseline.best, 100);
    }

    #[test]
    fn export_results_handles_paths_with_spaces() {
        let dir = std::env::temp_dir().join(format!("stk export test {}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("my export results.txt");

        export_results(&path, &summary_for_contract(Path::new("vault.wasm"))).unwrap();

        let baseline = Baseline::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(baseline.contract, Some("vault.wasm".to_string()));
        assert_eq!(baseline.function, "batch_payout");
        assert_eq!(baseline.ramp, "recipients");
        assert_eq!(baseline.best, 100);
    }

    #[test]
    fn compare_to_baseline_handles_paths_with_spaces() {
        let dir = std::env::temp_dir().join(format!("stk compare test {}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("my baseline file.txt");

        std::fs::write(&path, sample_baseline().to_file_contents()).unwrap();

        let result = compare_to_baseline(
            &path,
            "recipients",
            "batch_payout",
            100,
            1_000_000,
            50_000,
            5.0,
        );
        assert!(result.is_ok(), "{:?}", result.err());
    }

    // ---- #227: handle missing WASM exports ----

    #[test]
    fn load_rejects_missing_function_with_available_list() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("stk-load-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let wasm_path = dir.join("test.wasm");
        let mut file = std::fs::File::create(&wasm_path).unwrap();
        file.write_all(b"mock").unwrap();

        let err_msg = match load(&wasm_path, "nonexistent", "count") {
            Err(e) => e.0,
            Ok(_) => panic!("expected error"),
        };
        assert!(
            err_msg.contains("failed to read contract spec"),
            "expected spec error, got: {}",
            err_msg
        );
    }

    #[test]
    fn load_provides_helpful_param_error() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("stk-param-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let wasm_path = dir.join("test.wasm");
        let mut file = std::fs::File::create(&wasm_path).unwrap();
        file.write_all(b"mock").unwrap();

        let err_msg = match load(&wasm_path, "test_func", "wrong_param") {
            Err(e) => e.0,
            Ok(_) => panic!("expected error"),
        };
        assert!(
            err_msg.contains("failed to read contract spec"),
            "expected spec error early, got: {}",
            err_msg
        );
    }

    // ---- #228: distinguish resource-limit failures from validation ----

    #[test]
    fn run_distinguishes_validation_failures_from_limits() {
        let err_result = LimitsArgs {
            contract: std::path::PathBuf::from("nonexistent.wasm"),
            function: "test".to_string(),
            ramp: "count".to_string(),
            probe_timeout: 30,
            baseline: None,
            baseline_tolerance_pct: 5.0,
            save_baseline: None,
            export: None,
            arg_values: Vec::new(),
        };

        let result = run(err_result);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().0;
        assert!(
            err_msg.contains("failed to read"),
            "expected file error, got: {}",
            err_msg
        );
    }

    // ---- #493: numeric ramp boundary refinement audit ----

    #[test]
    fn refine_upper_bound_audited_records_all_steps_and_boundaries() {
        let mut probed = Vec::new();
        let (success, failure, audit) = refine_upper_bound_audited(10, 30, &mut |value| {
            probed.push(value);
            Ok(if value <= 23 {
                ProbeOutcome::Succeeded
            } else {
                ProbeOutcome::Failed(ProbeFailureCategory::ExitFailure)
            })
        })
        .unwrap();

        assert_eq!((success, failure), (23, 24));
        assert_eq!(audit.initial_low, 10);
        assert_eq!(audit.initial_high, 30);
        assert_eq!(audit.final_success, 23);
        assert_eq!(audit.final_failure, 24);
        assert_eq!(&probed[..2], &[10, 30]);
        assert_eq!(audit.steps.len(), probed.len() - 2);

        for (i, step) in audit.steps.iter().enumerate() {
            assert_eq!(step.step, i + 1);
            assert_eq!(step.probe_value, probed[i + 2]);
            assert_eq!(step.succeeded, step.probe_value <= 23);
            // Every failing step carries the standardized category; every
            // succeeding step carries none.
            assert_eq!(
                step.failure_category,
                (!step.succeeded).then_some(ProbeFailureCategory::ExitFailure)
            );
        }
    }

    // ---- #492: standardized probe failure categories ----

    #[test]
    fn probe_failure_categories_have_stable_labels_and_guidance() {
        let expected = [
            (ProbeFailureCategory::ExitFailure, "exit-failure"),
            (ProbeFailureCategory::TimedOut, "timed-out"),
            (ProbeFailureCategory::Abort, "abort"),
            (ProbeFailureCategory::SpawnFailure, "spawn-failure"),
        ];
        for (category, label) in expected {
            assert_eq!(category.label(), label);
            assert_eq!(category.to_string(), label);
            assert!(
                !category.guidance().is_empty(),
                "category {label} must carry actionable guidance"
            );
        }
    }

    #[test]
    fn probe_failure_categories_serialize_as_stable_kebab_case_strings() {
        let json = serde_json::to_value(ProbeFailureCategory::TimedOut).unwrap();
        assert_eq!(json, serde_json::json!("timed-out"));
        let parsed: ProbeFailureCategory = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, ProbeFailureCategory::TimedOut);
    }

    #[test]
    fn probe_outcome_reports_success_only_for_succeeded() {
        assert!(ProbeOutcome::Succeeded.succeeded());
        assert!(!ProbeOutcome::Failed(ProbeFailureCategory::Abort).succeeded());
    }

    #[test]
    fn refine_upper_bound_rejects_inverted_or_equal_bounds() {
        let err_inverted = refine_upper_bound(30, 20, &mut |_| Ok(true)).unwrap_err();
        assert!(
            err_inverted
                .0
                .contains("requires lower bound (30) < upper bound (20)"),
            "{}",
            err_inverted.0
        );

        let err_equal = refine_upper_bound(20, 20, &mut |_| Ok(true)).unwrap_err();
        assert!(
            err_equal
                .0
                .contains("requires lower bound (20) < upper bound (20)"),
            "{}",
            err_equal.0
        );
    }

    // ---- #494: sanitize temporary paths from machine-readable reports ----

    #[test]
    fn sanitize_report_path_strips_temp_directory_prefixes() {
        let temp = std::env::temp_dir().join("some_run_1234/contract.wasm");
        let sanitized = sanitize_report_path(&temp);
        assert_eq!(sanitized, "contract.wasm");

        let tmp_path = Path::new("/tmp/some_dir/mock.wasm");
        assert_eq!(sanitize_report_path(tmp_path), "mock.wasm");

        let rel_path = Path::new("relative/path/contract.wasm");
        assert_eq!(
            sanitize_report_path(rel_path),
            "relative/path/contract.wasm"
        );
    }

    #[test]
    fn write_baseline_sanitizes_temporary_contract_path() {
        let dir = std::env::temp_dir().join(format!("stk-sanitize-base-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clean.baseline");
        let temp_contract = dir.join("ephemeral/vault.wasm");

        write_baseline(
            &path,
            &temp_contract,
            "recipients",
            "batch_payout",
            100,
            1_000_000,
            50_000,
        )
        .unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(
            contents.contains("contract=vault.wasm"),
            "contents: {contents}"
        );
        assert!(
            !contents.contains("stk-sanitize-base"),
            "leaked temp dir: {contents}"
        );
    }

    #[test]
    fn export_results_json_sanitizes_temporary_contract_path() {
        let dir = std::env::temp_dir().join(format!("stk-sanitize-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("report.json");
        let temp_contract = dir.join("run-pid-999/vault.wasm");

        export_results(&path, &summary_for_contract(&temp_contract)).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(parsed["contract"], "vault.wasm");
        assert!(!contents.contains("run-pid-999"));
    }

    // ---- #495: regression coverage for each supported ramp type ----

    #[test]
    fn ramp_val_supports_all_scalar_numeric_types() {
        let env = Env::default();

        // U32
        let val = ramp_val(&env, &ScSpecTypeDef::U32, 42).unwrap();
        let num: u32 = val.into_val(&env);
        assert_eq!(num, 42);

        // I32
        let val = ramp_val(&env, &ScSpecTypeDef::I32, 42).unwrap();
        let num: i32 = val.into_val(&env);
        assert_eq!(num, 42);

        // U64
        let val = ramp_val(&env, &ScSpecTypeDef::U64, 42).unwrap();
        let num: u64 = val.into_val(&env);
        assert_eq!(num, 42);

        // I64
        let val = ramp_val(&env, &ScSpecTypeDef::I64, 42).unwrap();
        let num: i64 = val.into_val(&env);
        assert_eq!(num, 42);

        // U128
        let val = ramp_val(&env, &ScSpecTypeDef::U128, 42).unwrap();
        let num: u128 = val.into_val(&env);
        assert_eq!(num, 42);

        // I128
        let val = ramp_val(&env, &ScSpecTypeDef::I128, 42).unwrap();
        let num: i128 = val.into_val(&env);
        assert_eq!(num, 42);
    }

    #[test]
    fn ramp_val_supports_all_vec_element_types() {
        let env = Env::default();

        let element_types = [
            ScSpecTypeDef::Address,
            ScSpecTypeDef::Bool,
            ScSpecTypeDef::U32,
            ScSpecTypeDef::I32,
            ScSpecTypeDef::U64,
            ScSpecTypeDef::I64,
            ScSpecTypeDef::U128,
            ScSpecTypeDef::I128,
            ScSpecTypeDef::Symbol,
            ScSpecTypeDef::String,
            ScSpecTypeDef::Bytes,
        ];

        for element_type in element_types {
            let type_ = ScSpecTypeDef::Vec(Box::new(ScSpecTypeVec {
                element_type: Box::new(element_type),
            }));

            // Test 0 count
            let val_zero = ramp_val(&env, &type_, 0).unwrap();
            let vec_zero: SVec<Val> = val_zero.into_val(&env);
            assert_eq!(vec_zero.len(), 0);

            // Test non-zero count
            let val = ramp_val(&env, &type_, 3).unwrap();
            let vec: SVec<Val> = val.into_val(&env);
            assert_eq!(vec.len(), 3);
        }
    }

    #[test]
    fn ramp_val_rejects_unsupported_scalar_and_composite_types() {
        let env = Env::default();

        let unsupported = [
            ScSpecTypeDef::Void,
            ScSpecTypeDef::Timepoint,
            ScSpecTypeDef::Duration,
        ];
        for ty in unsupported {
            let err = ramp_val(&env, &ty, 5).unwrap_err();
            assert!(err.0.contains("isn't supported yet"), "{}", err.0);
        }

        // Unsupported Vec element type
        let unsupported_vec = ScSpecTypeDef::Vec(Box::new(ScSpecTypeVec {
            element_type: Box::new(ScSpecTypeDef::Void),
        }));
        let err = ramp_val(&env, &unsupported_vec, 3).unwrap_err();
        assert!(
            err.0.contains("isn't supported as a ramp parameter yet"),
            "{}",
            err.0
        );
    }

    // ---- #497: measurement limitations in every output format ----

    #[test]
    fn export_results_json_includes_measurement_limitations_and_audit() {
        let dir = std::env::temp_dir().join(format!("stk-export-json-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("report.json");

        let audit = RefinementAudit {
            initial_low: 10,
            initial_high: 20,
            steps: vec![RefinementStep {
                step: 1,
                probe_value: 15,
                succeeded: true,
                failure_category: None,
                window_low: 15,
                window_high: 20,
            }],
            final_success: 15,
            final_failure: 16,
        };

        let summary = LimitsSummary {
            best: 15,
            instructions: 500_000,
            memory_bytes: 25_000,
            first_failed: Some(16),
            first_failure_category: Some(ProbeFailureCategory::ExitFailure),
            first_failed_probe_stderr: None,
            probe_failure_counts: BTreeMap::from([(ProbeFailureCategory::ExitFailure, 1)]),
            audit: Some(audit),
            ..sample_summary()
        };

        export_results(&path, &summary).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(parsed["contract"], "vault.wasm");
        assert_eq!(parsed["function"], "batch_payout");
        assert_eq!(parsed["ramp"], "recipients");
        assert_eq!(parsed["best"], 15);
        assert_eq!(parsed["instructions"], 500_000);
        assert_eq!(parsed["memory_bytes"], 25_000);
        assert_eq!(parsed["measurement_limitations"], MEASUREMENT_LIMITATIONS);
        assert_eq!(parsed["audit"]["steps"].as_array().unwrap().len(), 1);
        // The standardized failure categories appear in the JSON export.
        assert_eq!(parsed["first_failed"], 16);
        assert_eq!(parsed["first_failure_category"], "exit-failure");
        assert_eq!(parsed["probe_failure_counts"]["exit-failure"], 1);
    }

    #[test]
    fn render_text_reports_boundary_categories_and_unifies_with_json() {
        let summary = LimitsSummary {
            best: 15,
            instructions: 500_000,
            memory_bytes: 25_000,
            first_failed: Some(16),
            first_failure_category: Some(ProbeFailureCategory::TimedOut),
            first_failed_probe_stderr: None,
            probe_failure_counts: BTreeMap::from([
                (ProbeFailureCategory::ExitFailure, 2),
                (ProbeFailureCategory::TimedOut, 1),
            ]),
            audit: Some(RefinementAudit {
                initial_low: 10,
                initial_high: 20,
                steps: vec![RefinementStep {
                    step: 1,
                    probe_value: 15,
                    succeeded: false,
                    failure_category: Some(ProbeFailureCategory::TimedOut),
                    window_low: 10,
                    window_high: 15,
                }],
                final_success: 14,
                final_failure: 15,
            }),
            ..sample_summary()
        };

        let text = summary.render_text();
        assert!(text.contains("last successful recipients"), "{text}");
        assert!(
            text.contains("first failed recipients value: 16 [timed-out]"),
            "{text}"
        );
        assert!(
            text.contains("probe failures: exit-failure: 2, timed-out: 1"),
            "{text}"
        );
        assert!(
            text.contains("boundary refinement: probed 1 values"),
            "{text}"
        );
        assert!(text.contains("--probe-timeout (30s)"), "{text}");
        assert!(text.contains("reproduce locally:"), "{text}");

        // The same summary serializes the same boundary into JSON, so the
        // two formats cannot drift apart.
        let json: serde_json::Value = serde_json::to_value(&summary).unwrap();
        assert_eq!(json["best"], 15);
        assert_eq!(json["first_failure_category"], "timed-out");
        assert_eq!(json["probe_failure_counts"]["exit-failure"], 2);
        assert!(json["reproduction"]
            .as_str()
            .unwrap()
            .contains("batch_payout"));
    }

    #[test]
    fn build_reproduction_guidance_includes_args_and_clarity() {
        let guidance = build_reproduction_guidance(
            "vault.wasm",
            "batch_payout",
            "recipients",
            42,
            &["token=0x1234".to_string(), "mode=1".to_string()],
        );
        assert!(guidance.contains("invoke `batch_payout` with `recipients` = 42"));
        assert!(guidance.contains("--contract vault.wasm"));
        assert!(guidance.contains("--fn batch_payout"));
        assert!(guidance.contains("--ramp recipients"));
        assert!(guidance.contains("--arg token=0x1234 --arg mode=1"));
    }

    #[test]
    fn baseline_file_format_includes_measurement_limitations() {
        let dir = std::env::temp_dir().join(format!("stk-limitations-base-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.baseline");

        write_baseline(
            &path,
            Path::new("vault.wasm"),
            "recipients",
            "batch_payout",
            100,
            1_000_000,
            50_000,
        )
        .unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains(&format!(
            "measurement_limitations={MEASUREMENT_LIMITATIONS}"
        )));

        let parsed = Baseline::parse(&contents).unwrap();
        assert_eq!(
            parsed.measurement_limitations.as_deref(),
            Some(MEASUREMENT_LIMITATIONS)
        );
    }

    // ---- Bug: handle empty Bytes overrides without producing invalid XDR ----

    #[test]
    fn explicit_val_empty_bytes_override_produces_valid_empty_bytes() {
        let env = Env::default();
        // 0x with no hex digits — the regression input. Must produce the
        // same empty Bytes that `Bytes::new` does, not invalid XDR.
        let val = explicit_val(&env, &ScSpecTypeDef::Bytes, "0x").unwrap();
        let bytes: soroban_sdk::Bytes = val.into_val(&env);
        assert_eq!(bytes.len(), 0);
    }

    #[test]
    fn explicit_val_nonempty_bytes_override_still_works() {
        let env = Env::default();
        let val = explicit_val(&env, &ScSpecTypeDef::Bytes, "0xdeadbeef").unwrap();
        let bytes: soroban_sdk::Bytes = val.into_val(&env);
        assert_eq!(bytes.len(), 4);
    }

    #[test]
    fn explicit_val_single_byte_bytes_override() {
        // Edge case: a one-byte (two hex digit) value.
        let env = Env::default();
        let val = explicit_val(&env, &ScSpecTypeDef::Bytes, "0x00").unwrap();
        let bytes: soroban_sdk::Bytes = val.into_val(&env);
        assert_eq!(bytes.len(), 1);
    }

    #[test]
    fn explicit_val_bytes_override_rejects_missing_prefix() {
        let env = Env::default();
        let err = explicit_val(&env, &ScSpecTypeDef::Bytes, "deadbeef").unwrap_err();
        assert!(err.0.contains("must start with 0x"), "{}", err.0);
    }

    #[test]
    fn explicit_val_empty_bytes_matches_default_val() {
        // The empty override should produce the same empty Bytes that the
        // default path (which uses `Bytes::new`) produces.
        let env = Env::default();
        let override_val: soroban_sdk::Bytes = explicit_val(&env, &ScSpecTypeDef::Bytes, "0x")
            .unwrap()
            .into_val(&env);
        let default_val: soroban_sdk::Bytes = soroban_sdk::Bytes::new(&env).into_val(&env);
        assert_eq!(default_val.len(), 0);
        assert_eq!(override_val.len(), 0);
    }

    // ---- Bug: retain stderr context from a failed wasm probe ----

    #[test]
    fn limits_summary_renders_first_failed_probe_stderr() {
        let summary = LimitsSummary {
            first_failed: Some(10),
            first_failure_category: Some(ProbeFailureCategory::ExitFailure),
            first_failed_probe_stderr: Some("contract panicked: overflow".to_string()),
            ..sample_summary()
        };
        let text = summary.render_text();
        assert!(
            text.contains("first failing probe stderr: contract panicked: overflow"),
            "{text}"
        );
    }

    #[test]
    fn limits_summary_omits_stderr_line_when_empty() {
        let summary = LimitsSummary {
            first_failed_probe_stderr: Some("   \n  ".to_string()),
            ..sample_summary()
        };
        let text = summary.render_text();
        assert!(
            !text.contains("first failing probe stderr"),
            "whitespace-only stderr should not render: {text}"
        );
    }

    #[test]
    fn limits_summary_omits_stderr_line_when_none() {
        let summary = LimitsSummary {
            first_failed_probe_stderr: None,
            ..sample_summary()
        };
        let text = summary.render_text();
        assert!(!text.contains("first failing probe stderr"), "{text}");
    }

    #[test]
    fn limits_summary_includes_failed_probe_stderr_in_json() {
        let summary = LimitsSummary {
            first_failed_probe_stderr: Some("host abort".to_string()),
            ..sample_summary()
        };
        let json: serde_json::Value = serde_json::to_value(&summary).unwrap();
        assert_eq!(json["first_failed_probe_stderr"], "host abort");
    }

    #[test]
    fn limits_summary_omits_stderr_from_json_when_none() {
        let summary = LimitsSummary {
            first_failed_probe_stderr: None,
            ..sample_summary()
        };
        let json: serde_json::Value = serde_json::to_value(&summary).unwrap();
        assert!(
            json.get("first_failed_probe_stderr").is_none()
                || json["first_failed_probe_stderr"].is_null(),
            "stderr should be skipped when None: {json}"
        );
    }
}
