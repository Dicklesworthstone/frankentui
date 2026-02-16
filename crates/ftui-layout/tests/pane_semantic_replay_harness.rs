//! Deterministic replay harness over versioned pane semantic traces.
//!
//! Fixtures live in `tests/fixtures/pane_semantic_replay/*.json` and encode:
//! - a versioned `PaneSemanticInputTrace`
//! - golden replay outputs (checksum, transition/frame hashes, final state)
//!
//! The harness replays each fixture twice and asserts exact output parity.

use std::fs;
use std::path::{Path, PathBuf};

use ftui_layout::{PaneDragResizeMachine, PaneDragResizeState, PaneSemanticInputTrace};
use serde::Deserialize;
use serde::Serialize;

const FIXTURE_SCHEMA_VERSION: u16 = 1;
const FIXTURE_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/pane_semantic_replay"
);

#[derive(Debug, Deserialize)]
struct ReplayHarnessFixture {
    schema_version: u16,
    fixture_id: String,
    description: String,
    trace: PaneSemanticInputTrace,
    expected: ReplayExpected,
}

#[derive(Debug, Deserialize)]
struct ReplayExpected {
    trace_checksum: u64,
    transition_count: usize,
    final_state: PaneDragResizeState,
    transition_hash: u64,
    frame_hash: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReplayRun {
    trace_checksum: u64,
    transition_count: usize,
    final_state: PaneDragResizeState,
    transition_hash: u64,
    frame_hash: u64,
}

fn fixture_paths() -> Vec<PathBuf> {
    let mut paths = fs::read_dir(FIXTURE_DIR)
        .unwrap_or_else(|err| panic!("failed to read fixture directory {FIXTURE_DIR}: {err}"))
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn load_fixture(path: &Path) -> ReplayHarnessFixture {
    let raw = fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read fixture {}: {err}", path.display()));
    let fixture: ReplayHarnessFixture = serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("failed to parse fixture {}: {err}", path.display()));

    assert_eq!(
        fixture.schema_version,
        FIXTURE_SCHEMA_VERSION,
        "fixture {} has unsupported schema version {}",
        path.display(),
        fixture.schema_version
    );
    fixture.trace.validate().unwrap_or_else(|err| {
        panic!(
            "fixture {} trace validation failed for {}: {err}",
            path.display(),
            fixture.fixture_id
        )
    });
    fixture
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0001_0000_01b3;

    let mut hash = OFFSET_BASIS;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

fn stable_hash<T: Serialize>(value: &T) -> u64 {
    let bytes = serde_json::to_vec(value).expect("serialization for stable hash should succeed");
    fnv1a64(&bytes)
}

fn run_fixture(trace: &PaneSemanticInputTrace) -> ReplayRun {
    let mut machine = PaneDragResizeMachine::default();
    let outcome = trace
        .replay(&mut machine)
        .expect("fixture replay should execute without runtime errors");

    let transition_hash = stable_hash(&outcome.transitions);
    let frame_payload = serde_json::json!({
        "trace_checksum": outcome.trace_checksum,
        "transition_count": outcome.transitions.len(),
        "final_state": &outcome.final_state,
        "transitions": &outcome.transitions,
    });
    let frame_hash = stable_hash(&frame_payload);

    ReplayRun {
        trace_checksum: outcome.trace_checksum,
        transition_count: outcome.transitions.len(),
        final_state: outcome.final_state,
        transition_hash,
        frame_hash,
    }
}

fn assert_expected(path: &Path, fixture: &ReplayHarnessFixture, run: &ReplayRun) {
    let expected = &fixture.expected;
    let fixture_desc = fixture.description.as_str();

    assert_eq!(
        run.trace_checksum,
        expected.trace_checksum,
        "fixture {} ({}) [{}] trace checksum mismatch: expected={} actual={}",
        path.display(),
        fixture.fixture_id,
        fixture_desc,
        expected.trace_checksum,
        run.trace_checksum
    );
    assert_eq!(
        run.transition_count,
        expected.transition_count,
        "fixture {} ({}) [{}] transition count mismatch: expected={} actual={}",
        path.display(),
        fixture.fixture_id,
        fixture_desc,
        expected.transition_count,
        run.transition_count
    );
    assert_eq!(
        run.final_state,
        expected.final_state,
        "fixture {} ({}) [{}] final state mismatch",
        path.display(),
        fixture.fixture_id,
        fixture_desc
    );
    assert_eq!(
        run.transition_hash,
        expected.transition_hash,
        "fixture {} ({}) [{}] transition hash mismatch: expected={} actual={}",
        path.display(),
        fixture.fixture_id,
        fixture_desc,
        expected.transition_hash,
        run.transition_hash
    );
    assert_eq!(
        run.frame_hash,
        expected.frame_hash,
        "fixture {} ({}) [{}] frame hash mismatch: expected={} actual={}",
        path.display(),
        fixture.fixture_id,
        fixture_desc,
        expected.frame_hash,
        run.frame_hash
    );
}

#[test]
fn semantic_replay_fixtures_match_golden_hashes() {
    let paths = fixture_paths();
    assert!(
        !paths.is_empty(),
        "no replay fixtures found in {FIXTURE_DIR}"
    );

    for path in paths {
        let fixture = load_fixture(&path);
        let first = run_fixture(&fixture.trace);
        assert_expected(&path, &fixture, &first);

        let second = run_fixture(&fixture.trace);
        assert_eq!(
            first,
            second,
            "fixture {} ({}) produced non-deterministic replay outputs",
            path.display(),
            fixture.fixture_id
        );
    }
}

#[test]
fn semantic_replay_fixtures_round_trip_trace_payloads() {
    for path in fixture_paths() {
        let fixture = load_fixture(&path);
        let encoded = serde_json::to_string(&fixture.trace)
            .unwrap_or_else(|err| panic!("failed to encode trace for {}: {err}", path.display()));
        let reparsed: PaneSemanticInputTrace = serde_json::from_str(&encoded)
            .unwrap_or_else(|err| panic!("failed to decode trace for {}: {err}", path.display()));
        assert_eq!(
            reparsed,
            fixture.trace,
            "trace JSON round-trip changed payload for {}",
            path.display()
        );
    }
}
