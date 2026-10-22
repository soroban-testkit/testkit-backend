use soroban_sdk::testutils::Events as _;
use soroban_sdk::{xdr, Address, Env, IntoVal, TryFromVal, Val};

use crate::core::{TestEnv, TestkitError};

/// A single contract event, captured from the environment.
///
/// `topics` and `data` are the **raw** `Val`s exactly as published — the same
/// values `soroban_sdk` captured for the event — held as `Val` so they can be
/// compared and decoded on demand (for example with
/// [`EventLog::decode_one`]). They are not eagerly converted into other types.
///
/// [`EventLog::diagnostics`] prints those raw values in their decoded form
/// (`contract=... topics=[Symbol(mint)] data=i128(1)`), which is exactly what
/// assertion-failure diagnostics include, so the diagnostic a failed
/// assertion shows is always aligned with the event that was captured.
#[derive(Debug, Clone)]
pub struct CapturedEvent {
    /// The contract that published the event.
    pub contract: Address,
    /// The event's topics, in publish order.
    pub topics: Vec<Val>,
    /// The event's data payload.
    pub data: Val,
}

impl CapturedEvent {
    /// Attempt to decode the topic at `index` into type `T`.
    ///
    /// Returns `Err(TestkitError::Misuse)` if `index` is out of bounds,
    /// or `Err(TestkitError::DecodeFailed)` with actionable diagnostic context
    /// if decoding fails, without panicking.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    /// use soroban_sdk::Symbol;
    ///
    /// let env = TestEnv::new();
    /// assert!(env.events().is_empty());
    /// ```
    pub fn decode_topic<T: TryFromVal<Env, Val>>(
        &self,
        env: &Env,
        index: usize,
    ) -> Result<T, TestkitError> {
        let topic = self.topics.get(index).ok_or_else(|| {
            TestkitError::Misuse(format!(
                "topic index {index} out of bounds: event has {} topic(s)",
                self.topics.len()
            ))
        })?;
        T::try_from_val(env, topic).map_err(|_| {
            let diag = decode_topic_diagnostic(env, topic);
            TestkitError::DecodeFailed(format!(
                "failed to decode topic at index {index} as requested type; topic value: {diag}"
            ))
        })
    }

    /// Render all topics as human-readable diagnostic strings without panicking.
    pub fn diagnostic_topics(&self, env: &Env) -> Vec<String> {
        self.topics
            .iter()
            .map(|t| decode_topic_diagnostic(env, t))
            .collect()
    }

    /// Structured debug formatting for this event's captured values.
    ///
    /// Returns a multi-line string with indented sections for the contract,
    /// topics, and data — each rendered with its type tag and decoded value.
    /// Useful for logging, test failure output, and inspecting event
    /// contents in CI logs.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let log = env.events();
    /// assert!(log.is_empty());
    /// ```
    pub fn debug_format(&self, env: &Env) -> String {
        let mut out = String::new();
        out.push_str("CapturedEvent {\n");
        out.push_str(&format!("  contract: {:?},\n", self.contract));
        out.push_str(&format!("  topics: [{}],\n", if self.topics.is_empty() {
            String::new()
        } else {
            let rendered: Vec<String> = self
                .topics
                .iter()
                .map(|t| format!("\n    {}", decode_topic_diagnostic(env, t)))
                .collect();
            rendered.join(",")
        }));
        out.push_str(&format!("  data: {},\n", decode_val_diagnostic(env, &self.data)));
        out.push('}');
        out
    }
}

/// A filterable, assertable log of captured contract events.
///
/// Obtained from [`TestEnv::events`] or [`TestEnv::events_during`]. Filters
/// return a new `EventLog`, so they chain:
/// `env.events().from(&contract).with_topic(symbol_short!("transfer"))`.
///
/// # A note on scope and nested invocations
///
/// `soroban_sdk::testutils::Events::all` — which this is built on — returns
/// events from the most recent top-level contract invocation, not a full
/// history accumulated since the `TestEnv` was created (verified against
/// soroban-sdk 27.0.6; its own doc comment says as much). Call
/// [`TestEnv::events`] or [`TestEnv::events_during`] right after the
/// invocation you want to inspect.
///
/// This is what makes the log cover **nested** calls correctly:
///
/// - Events published by a sub-contract call made from inside the invoked
///   entry point are included, no matter how deep the nesting goes. Each event
///   is attributed to the contract that actually published it (its
///   [`CapturedEvent::contract`]), not to the top-level entry point.
/// - All nested calls made during one top-level invocation accumulate into
///   the same log; a depth-2 call is not a separate capture window.
/// - The window is bounded by the **top-level** invocation: starting the next
///   top-level call (for example the next client method call from the test)
///   resets the captured log, so events from the previous top-level call are
///   no longer returned.
///
/// In short: one log per top-level call, spanning every nested sub-call of
/// that call.
#[derive(Clone)]
pub struct EventLog {
    pub(crate) env: Env,
    pub(crate) events: Vec<CapturedEvent>,
}

impl EventLog {
    /// Create an `EventLog` directly from an environment and captured events.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    /// use soroban_testkit::events::EventLog;
    ///
    /// let env = TestEnv::new();
    /// let log = EventLog::from_events(env.env(), vec![]);
    /// assert!(log.is_empty());
    /// ```
    pub fn from_events(env: &Env, events: Vec<CapturedEvent>) -> Self {
        Self {
            env: env.clone(),
            events,
        }
    }

    pub(crate) fn capture(env: &Env) -> Self {
        let raw = env.events().all();
        let events = raw
            .events()
            .iter()
            .filter_map(|event| {
                let contract_id = event.contract_id.clone()?;
                let xdr::ContractEventBody::V0(body) = &event.body;
                let contract =
                    Address::try_from_val(env, &xdr::ScAddress::Contract(contract_id)).ok()?;
                let topics = body
                    .topics
                    .iter()
                    .filter_map(|topic| Val::try_from_val(env, topic).ok())
                    .collect();
                let data = Val::try_from_val(env, &body.data).ok()?;
                Some(CapturedEvent {
                    contract,
                    topics,
                    data,
                })
            })
            .collect();
        Self {
            env: env.clone(),
            events,
        }
    }

    /// The number of captured events.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether no events were captured.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Keep only events published by `contract`.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let contract = env.address();
    /// let log = env.events().from(&contract);
    /// assert!(log.is_empty());
    /// ```
    pub fn from(&self, contract: &Address) -> EventLog {
        EventLog {
            env: self.env.clone(),
            events: self
                .events
                .iter()
                .filter(|e| &e.contract == contract)
                .cloned()
                .collect(),
        }
    }

    /// Keep only events with a topic equal to `topic`, once converted into
    /// this environment's `Val` representation.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    /// use soroban_sdk::symbol_short;
    ///
    /// let env = TestEnv::new();
    /// let log = env.events().with_topic(symbol_short!("transfer"));
    /// assert!(log.is_empty());
    /// ```
    pub fn with_topic<T: IntoVal<Env, Val>>(&self, topic: T) -> EventLog {
        let query = topic.into_val(&self.env);
        EventLog {
            env: self.env.clone(),
            events: self
                .events
                .iter()
                .filter(|e| e.topics.iter().any(|t| vals_equal(&self.env, t, &query)))
                .cloned()
                .collect(),
        }
    }

    /// Return the diagnostic rendering of all captured events in this log.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let log = env.events();
    /// assert_eq!(log.diagnostics(), "captured events: (none)");
    /// ```
    pub fn diagnostics(&self) -> String {
        self.render()
    }

    /// Structured debug formatting for all captured events in this log.
    ///
    /// Returns a multi-line string with one formatted event per block.
    /// Empty logs render as a single line.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let log = env.events();
    /// let formatted = log.debug_format();
    /// assert!(formatted.contains("EventLog"));
    /// ```
    pub fn debug_format(&self) -> String {
        if self.events.is_empty() {
            return "EventLog {\n  events: (none),\n}".to_string();
        }
        let mut out = String::from("EventLog {\n  events: [\n");
        for (i, event) in self.events.iter().enumerate() {
            let rendered = event.debug_format(&self.env);
            for line in rendered.lines() {
                out.push_str(&format!("    {line}\n"));
            }
            if i < self.events.len() - 1 {
                out.push_str(",\n");
            }
        }
        out.push_str("  ],\n}");
        out
    }
}

/// Decode a topic `Val` into a human-readable diagnostic string without panicking.
///
/// If the topic is a recognized type (Symbol, Address, integers, bool, string, etc.),
/// it is formatted with its type tag. If decoding fails or the topic value is an
/// unknown or arbitrary `Val`, returns a safe diagnostic fallback `<unknown topic: ...>`
/// without panicking.
///
/// # Example
///
/// ```
/// use soroban_testkit::core::TestEnv;
/// use soroban_testkit::events::decode_topic_diagnostic;
/// use soroban_sdk::{symbol_short, IntoVal};
///
/// let env = TestEnv::new();
/// let val = symbol_short!("test").into_val(env.env());
/// let rendered = decode_topic_diagnostic(env.env(), &val);
/// assert!(rendered.contains("test"));
/// ```
pub fn decode_topic_diagnostic(env: &Env, val: &Val) -> String {
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if let Ok(sym) = soroban_sdk::Symbol::try_from_val(env, val) {
            return format!("Symbol({sym:?})");
        }
        if let Ok(addr) = soroban_sdk::Address::try_from_val(env, val) {
            return format!("Address({addr:?})");
        }
        if let Ok(num) = i128::try_from_val(env, val) {
            return format!("i128({num})");
        }
        if let Ok(num) = u64::try_from_val(env, val) {
            return format!("u64({num})");
        }
        if let Ok(num) = u32::try_from_val(env, val) {
            return format!("u32({num})");
        }
        if let Ok(num) = i32::try_from_val(env, val) {
            return format!("i32({num})");
        }
        if let Ok(b) = bool::try_from_val(env, val) {
            return format!("bool({b})");
        }
        if let Ok(sc_val) = xdr::ScVal::try_from_val(env, val) {
            return format!("{sc_val:?}");
        }
        format!("<unknown topic: {val:?}>")
    }));

    res.unwrap_or_else(|_| format!("<unknown topic: unparseable {val:?}>"))
}

/// Decode an event data payload `Val` into a diagnostic string without panicking.
///
/// # Example
///
/// ```
/// use soroban_testkit::core::TestEnv;
/// use soroban_testkit::events::decode_val_diagnostic;
/// use soroban_sdk::IntoVal;
///
/// let env = TestEnv::new();
/// let val = 42_i128.into_val(env.env());
/// let rendered = decode_val_diagnostic(env.env(), &val);
/// assert!(rendered.contains("42"));
/// ```
pub fn decode_val_diagnostic(env: &Env, val: &Val) -> String {
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if let Ok(sym) = soroban_sdk::Symbol::try_from_val(env, val) {
            return format!("Symbol({sym:?})");
        }
        if let Ok(num) = i128::try_from_val(env, val) {
            return format!("i128({num})");
        }
        if let Ok(sc_val) = xdr::ScVal::try_from_val(env, val) {
            return format!("{sc_val:?}");
        }
        format!("<unknown data: {val:?}>")
    }));

    res.unwrap_or_else(|_| format!("<unknown data: unparseable {val:?}>"))
}

/// Compares two `Val`s for structural equality by round-tripping both
/// through XDR (`ScVal`), which compares by value rather than by object
/// handle identity. A conversion failure is treated as "not equal" rather
/// than propagated, since this is used only for filtering.
pub(crate) fn vals_equal(env: &Env, a: &Val, b: &Val) -> bool {
    let a = xdr::ScVal::try_from_val(env, a);
    let b = xdr::ScVal::try_from_val(env, b);
    matches!((a, b), (Ok(a), Ok(b)) if a == b)
}

impl TestEnv {
    /// The events captured so far — see [`EventLog`]'s note on scope: this
    /// reflects the most recent top-level contract invocation, per the
    /// underlying SDK's `testutils::Events::all`.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// assert!(env.events().is_empty());
    /// ```
    pub fn events(&self) -> EventLog {
        EventLog::capture(self.env())
    }

    /// Run `f`, then return its result alongside the events published
    /// while it ran.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let (doubled, log) = env.events_during(|| 21 * 2);
    /// assert_eq!(doubled, 42);
    /// assert!(log.is_empty());
    /// ```
    pub fn events_during<T>(&self, f: impl FnOnce() -> T) -> (T, EventLog) {
        let result = f();
        (result, self.events())
    }
}

#[cfg(test)]
mod capture_tests {
    use super::*;

    #[test]
    fn empty_log_reports_empty() {
        let env = TestEnv::new();
        let log = env.events();
        assert!(log.is_empty());
        assert_eq!(log.len(), 0);
    }

    #[test]
    fn from_and_with_topic_return_new_empty_logs_on_no_match() {
        let env = TestEnv::new();
        let contract = env.address();
        assert!(env.events().from(&contract).is_empty());
        assert!(env
            .events()
            .with_topic(soroban_sdk::symbol_short!("nope"))
            .is_empty());
    }

    #[test]
    fn debug_format_empty_log_renders_none() {
        let env = TestEnv::new();
        let log = env.events();
        let formatted = log.debug_format();
        assert!(formatted.contains("EventLog"));
        assert!(formatted.contains("(none)"));
    }

    #[test]
    fn debug_format_contains_event_structure() {
        let env = TestEnv::new();
        let log = env.events();
        let formatted = log.debug_format();
        assert!(formatted.contains("EventLog"));
        assert!(formatted.contains("events:"));
    }
}
