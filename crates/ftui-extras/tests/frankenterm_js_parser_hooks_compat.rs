//! Advanced host API compatibility harness — parser-hook subsystem (bd-2vr05.13.6).
//!
//! This is the parser-hook arm of the cross-subsystem advanced-API compatibility
//! harness. It drives the **production** CSI/OSC/DCS/ESC parser-hook API
//! (`ftui_extras::terminal::parser`, the in-tree home of bd-2vr05.13.3) through
//! its full contract — ordered dispatch, consumption, deregistration,
//! capability gating, per-parse quotas, and panic isolation — plus deliberate
//! fault injection.
//!
//! Every cell prints one structured `FTUI_ADVANCED_API_COMPAT ...` JSONL line
//! (subsystem-tagged, with a correlation id and a hook-trace summary) so the
//! aggregator (`scripts/frankenterm_js_advanced_api_compat.sh`) can fold it into
//! a single compatibility manifest alongside the runtime-option and IME/event
//! arms. Determinism: the parser assigns monotonic correlation ids and there is
//! no wall-clock or randomness in the assertions, so the trace shape is
//! reproducible for a fixed input.
//!
//! Requires the `terminal` feature (the VT parser + hook pipeline).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use ftui_extras::terminal::parser::{
    AnsiHandler, AnsiParser, CsiHookEvent, DcsHookEvent, EscHookEvent, HookCapabilities, HookClass,
    HookDisposition, HookPolicy, HookRejectReason, HookTraceEvent, HookTraceStage, OscHookEvent,
};

// ── Minimal fallback handler (records what reached the terminal, not the hook) ─

/// Records what reached the terminal handler (i.e. fell through the hook
/// pipeline). Only the fields the assertions read are tracked.
#[derive(Default)]
struct Recorder {
    csi: Vec<(Vec<i64>, char)>,
    printed: String,
}

impl AnsiHandler for Recorder {
    fn print(&mut self, c: char) {
        self.printed.push(c);
    }
    fn execute(&mut self, _byte: u8) {}
    fn csi_dispatch(&mut self, params: &[i64], _intermediates: &[u8], c: char) {
        self.csi.push((params.to_vec(), c));
    }
    fn osc_dispatch(&mut self, _params: &[&[u8]]) {}
    fn esc_dispatch(&mut self, _intermediates: &[u8], _c: char) {}
}

// ── Trace token serialisation (the enums are not `Display`) ──────────────────

fn class_token(c: HookClass) -> &'static str {
    match c {
        HookClass::Csi => "csi",
        HookClass::Osc => "osc",
        HookClass::Esc => "esc",
        HookClass::Dcs => "dcs",
    }
}

fn reject_token(r: HookRejectReason) -> &'static str {
    match r {
        HookRejectReason::CapabilityDisabled => "capability_disabled",
        HookRejectReason::QuotaExceeded => "quota_exceeded",
        HookRejectReason::TimeoutExceeded => "timeout_exceeded",
        HookRejectReason::HookPanicked => "hook_panicked",
        HookRejectReason::HookRejected => "hook_rejected",
    }
}

/// Counts of each trace stage plus the distinct reject reasons present.
struct TraceSummary {
    invoked: usize,
    consumed: usize,
    fallback: usize,
    rejected: usize,
    reject_reasons: Vec<&'static str>,
}

fn summarise(trace: &[HookTraceEvent]) -> TraceSummary {
    let mut s = TraceSummary {
        invoked: 0,
        consumed: 0,
        fallback: 0,
        rejected: 0,
        reject_reasons: Vec::new(),
    };
    // Correlation ids must be monotonically non-decreasing (deterministic order).
    let mut last_cid = 0u64;
    for ev in trace {
        assert!(
            ev.correlation_id >= last_cid,
            "hook trace correlation ids must be monotonic"
        );
        last_cid = ev.correlation_id;
        match ev.stage {
            HookTraceStage::HookInvoked => s.invoked += 1,
            HookTraceStage::HookConsumed => s.consumed += 1,
            HookTraceStage::FallbackDispatched => s.fallback += 1,
            HookTraceStage::PolicyRejected => {
                s.rejected += 1;
                if let Some(reason) = ev.reject_reason {
                    let token = reject_token(reason);
                    if !s.reject_reasons.contains(&token) {
                        s.reject_reasons.push(token);
                    }
                }
            }
        }
    }
    s
}

/// Emit one structured compatibility-harness cell. The aggregator greps the
/// `FTUI_ADVANCED_API_COMPAT ` prefix and strips it to build the manifest.
fn emit(scenario: &str, case: &str, correlation_id: &str, passed: bool, trace: &[HookTraceEvent]) {
    let s = summarise(trace);
    let reasons = s
        .reject_reasons
        .iter()
        .map(|r| format!("\"{r}\""))
        .collect::<Vec<_>>()
        .join(",");
    let classes: Vec<&str> = {
        let mut v: Vec<&str> = Vec::new();
        for ev in trace {
            let t = class_token(ev.class);
            if !v.contains(&t) {
                v.push(t);
            }
        }
        v
    };
    let classes_json = classes
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(",");
    println!(
        "FTUI_ADVANCED_API_COMPAT {{\"subsystem\":\"parser_hooks\",\"scenario\":\"{scenario}\",\
\"case\":\"{case}\",\"correlation_id\":\"{correlation_id}\",\"passed\":{passed},\
\"invoked\":{},\"consumed\":{},\"fallback\":{},\"rejected\":{},\
\"reject_reasons\":[{reasons}],\"classes\":[{classes_json}]}}",
        s.invoked, s.consumed, s.fallback, s.rejected,
    );
}

// ── Contract scenarios ───────────────────────────────────────────────────────

/// Hooks dispatch in registration order; a `Consume` disposition suppresses the
/// fallback handler and is recorded as `HookConsumed`.
#[test]
fn ordered_dispatch_and_consume() {
    let mut parser = AnsiParser::new();
    let mut handler = Recorder::default();
    let order: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));

    let first = Rc::clone(&order);
    parser.register_csi_hook(move |_e: &CsiHookEvent| {
        first.borrow_mut().push("first");
        HookDisposition::Continue
    });
    let second = Rc::clone(&order);
    parser.register_csi_hook(move |_e: &CsiHookEvent| {
        second.borrow_mut().push("second");
        HookDisposition::Consume
    });

    parser.parse(b"\x1b[5A", &mut handler);

    assert_eq!(
        *order.borrow(),
        vec!["first", "second"],
        "registration order"
    );
    assert!(handler.csi.is_empty(), "consume must suppress fallback");
    let trace = parser.drain_hook_trace();
    let s = summarise(&trace);
    assert!(s.consumed >= 1, "consumption must be traced");
    emit("ordered_consume", "csi_two_hooks", "ph-order", true, &trace);
}

/// After deregistration the class falls back to the handler.
#[test]
fn deregister_restores_fallback() {
    let mut parser = AnsiParser::new();
    let mut handler = Recorder::default();
    let id = parser.register_csi_hook(|_e: &CsiHookEvent| HookDisposition::Consume);

    parser.parse(b"\x1b[1m", &mut handler);
    assert!(handler.csi.is_empty(), "hook consumed first sequence");

    assert!(parser.deregister_hook(id), "deregister returns true");
    parser.drain_hook_trace();
    parser.parse(b"\x1b[2m", &mut handler);
    assert_eq!(handler.csi.len(), 1, "fallback after deregister");

    let trace = parser.drain_hook_trace();
    let s = summarise(&trace);
    assert!(s.fallback >= 1, "fallback dispatch must be traced");
    emit("deregister", "fallback_restored", "ph-dereg", true, &trace);
}

/// A disabled capability class skips hooks and records a `PolicyRejected`
/// (`CapabilityDisabled`) trace, falling back to the handler.
#[test]
fn capability_gating_records_rejection() {
    let mut parser = AnsiParser::new();
    let mut handler = Recorder::default();
    let calls = Rc::new(RefCell::new(0u32));
    let calls_hook = Rc::clone(&calls);
    parser.register_csi_hook(move |_e: &CsiHookEvent| {
        *calls_hook.borrow_mut() += 1;
        HookDisposition::Continue
    });

    parser.set_hook_capabilities(HookCapabilities {
        csi: false,
        ..HookCapabilities::default()
    });
    parser.parse(b"\x1b[3m", &mut handler);

    assert_eq!(
        *calls.borrow(),
        0,
        "disabled class must not invoke the hook"
    );
    assert_eq!(handler.csi.len(), 1, "gated class falls back to handler");
    let trace = parser.drain_hook_trace();
    let s = summarise(&trace);
    assert!(s.rejected >= 1, "rejection must be traced");
    assert!(
        s.reject_reasons.contains(&"capability_disabled"),
        "reason must be capability_disabled, got {:?}",
        s.reject_reasons
    );
    emit("capability_gating", "csi_disabled", "ph-cap", true, &trace);
}

/// Per-`parse()` quotas bound hook dispatch; the over-quota sequence is rejected
/// with `QuotaExceeded` and falls back.
#[test]
fn quota_enforced_per_parse() {
    let mut parser = AnsiParser::new();
    let mut handler = Recorder::default();
    parser.register_csi_hook(|_e: &CsiHookEvent| HookDisposition::Consume);
    parser.set_hook_policy(HookPolicy {
        max_csi_invocations_per_parse: 1,
        ..HookPolicy::default()
    });

    // Two CSI sequences in one parse call: the second exceeds the quota.
    parser.parse(b"\x1b[1m\x1b[2m", &mut handler);

    let trace = parser.drain_hook_trace();
    let s = summarise(&trace);
    assert!(
        s.reject_reasons.contains(&"quota_exceeded"),
        "quota must be enforced, reasons={:?}",
        s.reject_reasons
    );
    // The over-quota sequence falls back to the handler.
    assert_eq!(handler.csi.len(), 1, "over-quota sequence falls back");
    emit("quota", "csi_max_1_per_parse", "ph-quota", true, &trace);
}

/// Every hook class (CSI/OSC/ESC/DCS) registers and dispatches.
#[test]
fn all_classes_dispatch() {
    let mut parser = AnsiParser::new();
    let mut handler = Recorder::default();
    let hits = Rc::new(RefCell::new((0u32, 0u32, 0u32, 0u32)));

    let h_csi = Rc::clone(&hits);
    parser.register_csi_hook(move |_e: &CsiHookEvent| {
        h_csi.borrow_mut().0 += 1;
        HookDisposition::Continue
    });
    let h_osc = Rc::clone(&hits);
    parser.register_osc_hook(move |_e: &OscHookEvent| {
        h_osc.borrow_mut().1 += 1;
        HookDisposition::Continue
    });
    let h_esc = Rc::clone(&hits);
    parser.register_esc_hook(move |_e: &EscHookEvent| {
        h_esc.borrow_mut().2 += 1;
        HookDisposition::Continue
    });
    let h_dcs = Rc::clone(&hits);
    parser.register_dcs_hook(move |_e: &DcsHookEvent| {
        h_dcs.borrow_mut().3 += 1;
        HookDisposition::Continue
    });

    // CSI (SGR), OSC (set title), ESC (RIS-style single final), DCS.
    parser.parse(b"\x1b[31m", &mut handler);
    parser.parse(b"\x1b]0;hi\x07", &mut handler);
    parser.parse(b"\x1bD", &mut handler);
    parser.parse(b"\x1bP1;2q\x1b\\", &mut handler);

    let (c, o, e, d) = *hits.borrow();
    assert!(c >= 1, "csi hook fired");
    assert!(o >= 1, "osc hook fired");
    assert!(e >= 1, "esc hook fired");
    assert!(d >= 1, "dcs hook fired");
    let trace = parser.drain_hook_trace();
    emit("all_classes", "csi_osc_esc_dcs", "ph-classes", true, &trace);
}

/// Fault injection: a hook that panics is isolated (caught), recorded as
/// `HookPanicked`, and the parser falls back to the handler rather than
/// aborting.
#[test]
fn fault_injection_hook_panic_isolated() {
    let mut parser = AnsiParser::new();
    let mut handler = Recorder::default();
    parser.register_csi_hook(|_e: &CsiHookEvent| panic!("synthetic hook panic"));

    // The parser isolates the hook panic via `catch_unwind`; the panic's default
    // backtrace lands on stderr (harmless — the aggregator greps stdout only).
    // We deliberately do NOT install a process-global panic hook here: these
    // integration tests run in parallel threads, and muting the global hook
    // could swallow a *concurrent* test's panic message and hinder triage.
    parser.parse(b"\x1b[7m", &mut handler);

    assert_eq!(handler.csi.len(), 1, "panicking hook falls back to handler");
    let trace = parser.drain_hook_trace();
    let s = summarise(&trace);
    assert!(
        s.reject_reasons.contains(&"hook_panicked"),
        "panic must be isolated and traced, reasons={:?}",
        s.reject_reasons
    );
    emit("fault_injection", "hook_panic", "ph-panic", true, &trace);
}

/// Timeout policy: a slow hook is rejected with `TimeoutExceeded` and falls back.
#[test]
fn slow_hook_times_out() {
    let mut parser = AnsiParser::new();
    let mut handler = Recorder::default();
    parser.register_csi_hook(|_e: &CsiHookEvent| {
        // Busy-spin past the configured 1µs budget.
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_millis(2) {
            std::hint::spin_loop();
        }
        HookDisposition::Consume
    });
    parser.set_hook_policy(HookPolicy {
        max_hook_runtime: Duration::from_micros(1),
        ..HookPolicy::default()
    });

    parser.parse(b"\x1b[4m", &mut handler);

    let trace = parser.drain_hook_trace();
    let s = summarise(&trace);
    assert!(
        s.reject_reasons.contains(&"timeout_exceeded"),
        "slow hook must time out, reasons={:?}",
        s.reject_reasons
    );
    assert_eq!(handler.csi.len(), 1, "timed-out hook falls back");
    emit("timeout", "slow_csi_hook", "ph-timeout", true, &trace);
}

/// Determinism: the same input through a fresh parser yields an identical
/// trace-stage profile and identical handler state.
#[test]
fn trace_is_deterministic() {
    let run = || {
        let mut parser = AnsiParser::new();
        let mut handler = Recorder::default();
        parser.register_csi_hook(|_e: &CsiHookEvent| HookDisposition::Continue);
        parser.parse(b"\x1b[1m\x1b[2mhi", &mut handler);
        let trace = parser.drain_hook_trace();
        let s = summarise(&trace);
        (
            s.invoked,
            s.consumed,
            s.fallback,
            s.rejected,
            handler.printed.clone(),
        )
    };
    assert_eq!(
        run(),
        run(),
        "parser-hook trace profile must be deterministic"
    );
}
