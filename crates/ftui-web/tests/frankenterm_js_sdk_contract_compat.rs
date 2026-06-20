//! FrankenTermJS SDK contract compatibility harness — typed event/error model
//! + TypeScript-definition lockstep (bd-2vr05.9.2).
//!
//! Drives the **production** typed SDK model (`ftui_web::sdk_event_model`) — the
//! durable in-tree source of truth for the host-observable event taxonomy, the
//! typed error codes, and the bounded host-queue policy documented in
//! `docs/spec/frankenterm-web-api.md`. It proves the runtime model and the
//! shipped TypeScript definitions (`crates/ftui-web/sdk/frankenterm-js-events.d.ts`)
//! stay in lockstep, so a host integrator's `.d.ts` never drifts from the
//! runtime payloads.
//!
//! Every cell prints one structured `FTUI_SDK_CONTRACT_COMPAT ...` JSONL line so
//! the aggregator (`scripts/frankenterm_js_sdk_contract_compat.sh`) folds it into
//! a single compatibility manifest with a subsystem tag, a correlation id, and a
//! `failure_injection` flag.
//!
//! Determinism: the model and its generated `.d.ts` are pure functions of the
//! crate version, so every cell is byte-for-byte reproducible.
//!
//! Regenerate the committed `.d.ts` after an intentional model change with:
//!   FTUI_SDK_DTS_BLESS=1 cargo test -p ftui-web --test frankenterm_js_sdk_contract_compat
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use ftui_web::sdk_event_model::{
    EVENT_SCHEMA_VERSION, EventBufferPolicy, HostEventClass, SdkErrorKind, typescript_definitions,
};

const PREFIX: &str = "FTUI_SDK_CONTRACT_COMPAT";

fn golden_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("sdk/frankenterm-js-events.d.ts")
}

fn emit(
    subsystem: &str,
    scenario: &str,
    case: &str,
    correlation_id: &str,
    passed: bool,
    failure_injection: bool,
    extra: &str,
) {
    println!(
        "{PREFIX} {{\"subsystem\":\"{subsystem}\",\"scenario\":\"{scenario}\",\
\"case\":\"{case}\",\"correlation_id\":\"{correlation_id}\",\"passed\":{passed},\
\"failure_injection\":{failure_injection},{extra}}}"
    );
}

/// Pure validator used both by the live contract checks and by the deliberate
/// fault-injection case below: a contract list must be unique and sorted.
fn validate_unique_sorted(values: &[&str]) -> Result<(), String> {
    for pair in values.windows(2) {
        if pair[0] == pair[1] {
            return Err(format!("duplicate entry: {}", pair[0]));
        }
        if pair[0] > pair[1] {
            return Err(format!("out of order: {} before {}", pair[0], pair[1]));
        }
    }
    Ok(())
}

#[test]
fn event_taxonomy_contract() {
    let wires: Vec<&str> = HostEventClass::ALL.iter().map(|c| c.as_str()).collect();
    let valid = validate_unique_sorted(&wires).is_ok();
    let round_trips = HostEventClass::ALL
        .iter()
        .all(|&c| HostEventClass::from_wire(c.as_str()) == Some(c));
    let count_ok = wires.len() == 15;
    let schema_ok = EVENT_SCHEMA_VERSION == "1.0.0";

    let passed = valid && round_trips && count_ok && schema_ok;
    assert!(
        passed,
        "host event taxonomy must be unique, sorted, and round-trip"
    );
    emit(
        "event_taxonomy",
        "unique_sorted_round_trip",
        "host_event_class",
        "sdk-taxonomy",
        passed,
        false,
        &format!(
            "\"count\":{},\"schema_version\":\"{EVENT_SCHEMA_VERSION}\",\"round_trips\":{round_trips}",
            wires.len()
        ),
    );
}

#[test]
fn error_model_contract() {
    let codes: Vec<&str> = SdkErrorKind::ALL.iter().map(|k| k.code()).collect();
    let valid = validate_unique_sorted(&codes).is_ok();
    let round_trips = SdkErrorKind::ALL
        .iter()
        .all(|&k| SdkErrorKind::from_code(k.code()) == Some(k));
    let summaries_ok = SdkErrorKind::ALL.iter().all(|k| !k.summary().is_empty());

    let passed = valid && round_trips && summaries_ok && codes.len() == 8;
    assert!(
        passed,
        "error taxonomy must be unique, sorted, round-trip, and described"
    );
    emit(
        "error_model",
        "unique_sorted_round_trip",
        "sdk_error_kind",
        "sdk-errors",
        passed,
        false,
        &format!("\"count\":{},\"round_trips\":{round_trips}", codes.len()),
    );
}

#[test]
fn buffer_policy_contract() {
    let policy = EventBufferPolicy::DEFAULT;
    // Spot-check the documented bounds and the invariant that the configurable
    // ceiling is not below the default.
    let documented = policy.encoded_inputs_queue_max == 4096
        && policy.ime_trace_queue_max == 2048
        && policy.accessibility_announcement_queue_max == 64
        && policy.attach_transition_queue_max == 512
        && policy.event_subscription_queue_default_max == 512
        && policy.event_subscription_registry_max == 256;
    let ceiling_ok = policy.event_subscription_queue_configurable_max
        >= policy.event_subscription_queue_default_max;
    let all_bounded = policy.entries().iter().all(|&(_, v)| v > 0);

    let passed = documented && ceiling_ok && all_bounded;
    assert!(
        passed,
        "bounded-buffer policy must match the documented contract"
    );
    emit(
        "buffer_policy",
        "documented_bounds",
        "event_buffer_policy_default",
        "sdk-buffer-policy",
        passed,
        false,
        &format!(
            "\"entries\":{},\"default_sub_max\":{},\"configurable_max\":{}",
            policy.entries().len(),
            policy.event_subscription_queue_default_max,
            policy.event_subscription_queue_configurable_max
        ),
    );
}

#[test]
fn determinism_contract() {
    let a = typescript_definitions();
    let b = typescript_definitions();
    let stable = a == b;
    // Every Rust variant must surface in the generated definitions.
    let covers_events = HostEventClass::ALL
        .iter()
        .all(|c| a.contains(&format!("\"{}\"", c.as_str())));
    let covers_errors = SdkErrorKind::ALL
        .iter()
        .all(|k| a.contains(&format!("\"{}\"", k.code())));

    let passed = stable && covers_events && covers_errors;
    assert!(
        passed,
        "generated definitions must be deterministic and complete"
    );
    emit(
        "determinism",
        "generator_stable_and_complete",
        "typescript_definitions",
        "sdk-determinism",
        passed,
        false,
        &format!(
            "\"bytes\":{},\"covers_events\":{covers_events},\"covers_errors\":{covers_errors}",
            a.len()
        ),
    );
}

#[test]
fn typescript_lockstep() {
    let generated = typescript_definitions();
    let path = golden_path();

    if std::env::var_os("FTUI_SDK_DTS_PRINT").is_some() {
        // Bootstrap aid: stream the canonical definitions so the committed
        // golden can be (re)materialised from CI/rch stdout.
        println!("<<<FTUI_SDK_DTS_BEGIN>>>");
        print!("{generated}");
        println!("<<<FTUI_SDK_DTS_END>>>");
    }

    if std::env::var_os("FTUI_SDK_DTS_BLESS").is_some() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create sdk dir");
        }
        std::fs::write(&path, &generated).expect("write golden .d.ts");
    }

    let committed = std::fs::read_to_string(&path);
    let passed = matches!(&committed, Ok(c) if *c == generated);
    assert!(
        passed,
        "committed sdk/frankenterm-js-events.d.ts is out of sync with the Rust model; \
         regenerate with FTUI_SDK_DTS_BLESS=1"
    );
    emit(
        "ts_lockstep",
        "committed_matches_generated",
        "frankenterm_js_events_d_ts",
        "sdk-lockstep",
        passed,
        false,
        &format!("\"generated_bytes\":{}", generated.len()),
    );
}

/// FAILURE INJECTION: feed the contract validator a deliberately corrupted
/// taxonomy (a duplicate, then an out-of-order pair). The validator must reject
/// both, while accepting the real taxonomy — proving the lockstep/uniqueness
/// guard that protects the contract is real, not a no-op.
#[test]
fn corrupted_taxonomy_is_rejected() {
    let real: Vec<&str> = HostEventClass::ALL.iter().map(|c| c.as_str()).collect();
    let real_ok = validate_unique_sorted(&real).is_ok();

    let duplicated = ["input.key", "input.key"];
    let dup_rejected = validate_unique_sorted(&duplicated).is_err();

    let unsorted = ["ui.link_click", "attach.transition"];
    let order_rejected = validate_unique_sorted(&unsorted).is_err();

    let passed = real_ok && dup_rejected && order_rejected;
    assert!(
        passed,
        "the contract validator must reject corrupted taxonomies"
    );
    emit(
        "event_taxonomy",
        "corrupted_rejected",
        "duplicate_and_out_of_order",
        "sdk-taxonomy-fault",
        passed,
        true,
        &format!(
            "\"real_ok\":{real_ok},\"dup_rejected\":{dup_rejected},\"order_rejected\":{order_rejected}"
        ),
    );
}
