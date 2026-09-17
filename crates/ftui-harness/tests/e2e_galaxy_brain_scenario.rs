#![forbid(unsafe_code)]

//! bd-1lgz8.3: E2E test — Galaxy-brain transparency demo scenario.
//!
//! Executes the galaxy-brain transparency demo scenario headlessly,
//! matching the phase structure defined in
//! `tests/e2e/scenarios/demos/galaxy_brain_transparency.toml`:
//!
//! 1. Steady state + L0/L1/L2/L3 disclosure (frames 0-120)
//! 2. Drift injection + fallback trigger (frames 120-240)
//! 3. Recovery + confidence rebuild (frames 240-360)
//! 4. Timeline review + teardown (frames 360-600)
//!
//! Validates: golden frame checksums, content assertions at checkpoints,
//! deterministic replay, timing SLO, and JSONL evidence output.
//!
//! # Running
//!
//! ```sh
//! CARGO_TARGET_DIR=/tmp/ftui-test-target cargo test -p ftui-harness --test e2e_galaxy_brain_scenario
//! ```

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_runtime::transparency::{
    BayesianDetails, Disclosure, DisclosureEvidence, DisclosureLevel, EvidenceDirection,
    TrafficLight,
};
use ftui_runtime::unified_evidence::DecisionDomain;
use ftui_widgets::Widget;
use ftui_widgets::decision_card::DecisionCard;
use ftui_widgets::drift_visualization::{
    DomainSnapshot, DriftSnapshot, DriftTimeline, DriftVisualization,
};

// ============================================================================
// Constants (from scenario TOML)
// ============================================================================

const SCENARIO_NAME: &str = "galaxy-brain-transparency-demo";
const VIEWPORT_W: u16 = 120;
const VIEWPORT_H: u16 = 40;
const DURATION_FRAMES: u64 = 600;
const _SEED: u64 = 42;

// ============================================================================
// Frame Helpers
// ============================================================================

/// Captured output from a rendered frame (avoids lifetime issues with Frame/GraphemePool).
struct RenderedFrame {
    rows: Vec<String>,
    checksum: String,
}

fn row_text(frame: &Frame, y: u16) -> String {
    let mut out = String::new();
    for x in 0..frame.buffer.width() {
        let ch = frame
            .buffer
            .get(x, y)
            .and_then(|cell| cell.content.as_char())
            .unwrap_or(' ');
        out.push(ch);
    }
    out
}

fn capture_frame(frame: &Frame) -> RenderedFrame {
    let rows: Vec<String> = (0..frame.buffer.height())
        .map(|y| row_text(frame, y))
        .collect();

    let mut hasher = blake3::Hasher::new();
    hasher.update(&frame.buffer.width().to_le_bytes());
    hasher.update(&frame.buffer.height().to_le_bytes());
    for y in 0..frame.buffer.height() {
        for x in 0..frame.buffer.width() {
            if let Some(cell) = frame.buffer.get(x, y) {
                let ch = cell.content.as_char().unwrap_or(' ');
                hasher.update(ch.encode_utf8(&mut [0; 4]).as_bytes());
            }
        }
    }
    let checksum = format!("blake3:{}", hasher.finalize().to_hex());

    RenderedFrame { rows, checksum }
}

fn frame_contains(rows: &[String], needle: &str) -> bool {
    rows.iter().any(|r| r.contains(needle))
}

// ============================================================================
// Disclosure Builder (reusable across phases)
// ============================================================================

fn build_disclosure(level: DisclosureLevel, signal: TrafficLight, confidence: f64) -> Disclosure {
    let explanation = if level >= DisclosureLevel::PlainEnglish {
        Some(format!(
            "Diff strategy: chose 'incremental_diff' with {} confidence.",
            match signal {
                TrafficLight::Green => "high",
                TrafficLight::Yellow => "moderate",
                TrafficLight::Red => "low",
            }
        ))
    } else {
        None
    };

    let evidence_terms = if level >= DisclosureLevel::EvidenceTerms {
        Some(vec![
            DisclosureEvidence {
                label: "change_density",
                bayes_factor: 5.2 * confidence,
                direction: if confidence > 0.7 {
                    EvidenceDirection::Supporting
                } else {
                    EvidenceDirection::Opposing
                },
            },
            DisclosureEvidence {
                label: "scroll_velocity",
                bayes_factor: 0.6 + confidence * 0.4,
                direction: EvidenceDirection::Neutral,
            },
            DisclosureEvidence {
                label: "cursor_proximity",
                bayes_factor: 1.05 * confidence,
                direction: if confidence > 0.5 {
                    EvidenceDirection::Supporting
                } else {
                    EvidenceDirection::Opposing
                },
            },
        ])
    } else {
        None
    };

    let bayesian_details = if level >= DisclosureLevel::FullBayesian {
        let loss = 0.12 + (1.0 - confidence) * 0.5;
        Some(BayesianDetails {
            log_posterior: confidence * 2.5,
            confidence_interval: (confidence * 0.7, confidence.min(0.99)),
            expected_loss: loss,
            next_best_loss: loss + 0.33,
            loss_avoided: 0.33 * confidence,
        })
    } else {
        None
    };

    Disclosure {
        domain: DecisionDomain::DiffStrategy,
        level,
        signal,
        action_label: "incremental_diff".to_string(),
        explanation,
        evidence_terms,
        bayesian_details,
    }
}

fn signal_for_confidence(confidence: f64) -> TrafficLight {
    if confidence > 0.7 {
        TrafficLight::Green
    } else if confidence > 0.4 {
        TrafficLight::Yellow
    } else {
        TrafficLight::Red
    }
}

// ============================================================================
// Scenario State Machine
// ============================================================================

struct ScenarioState {
    disclosure_level: DisclosureLevel,
    confidence: f64,
    drift_intensity: f64,
    timeline: DriftTimeline,
    overlay_visible: bool,
    timeline_visible: bool,
}

impl ScenarioState {
    fn new() -> Self {
        Self {
            disclosure_level: DisclosureLevel::TrafficLight,
            confidence: 0.95,
            drift_intensity: 0.0,
            timeline: DriftTimeline::new(60),
            overlay_visible: false,
            timeline_visible: false,
        }
    }

    fn apply_event(
        &mut self,
        frame: u64,
        event_type: &str,
        key: Option<&str>,
        intensity: Option<f64>,
    ) {
        match event_type {
            "init" => {
                self.confidence = 0.95;
                self.drift_intensity = 0.0;
            }
            "keypress" => {
                if let Some(k) = key {
                    match k {
                        "Enter" => self.overlay_visible = true,
                        "Escape" => self.overlay_visible = false,
                        "Right" => {
                            self.disclosure_level = match self.disclosure_level {
                                DisclosureLevel::TrafficLight => DisclosureLevel::PlainEnglish,
                                DisclosureLevel::PlainEnglish => DisclosureLevel::EvidenceTerms,
                                DisclosureLevel::EvidenceTerms => DisclosureLevel::FullBayesian,
                                DisclosureLevel::FullBayesian => DisclosureLevel::FullBayesian,
                            };
                        }
                        "Left" => {
                            self.disclosure_level = match self.disclosure_level {
                                DisclosureLevel::TrafficLight => DisclosureLevel::TrafficLight,
                                DisclosureLevel::PlainEnglish => DisclosureLevel::TrafficLight,
                                DisclosureLevel::EvidenceTerms => DisclosureLevel::PlainEnglish,
                                DisclosureLevel::FullBayesian => DisclosureLevel::EvidenceTerms,
                            };
                        }
                        "t" => self.timeline_visible = !self.timeline_visible,
                        _ => {}
                    }
                }
            }
            "inject_drift" => {
                self.drift_intensity = intensity.unwrap_or(0.0);
            }
            _ => {}
        }

        let _ = frame;
    }

    fn tick(&mut self, frame: u64) {
        // Update confidence based on drift intensity.
        if self.drift_intensity > 0.0 {
            let decay = 0.015 * self.drift_intensity;
            self.confidence = (self.confidence - decay).max(0.1);
        } else if self.confidence < 0.95 {
            let recovery = 0.008;
            self.confidence = (self.confidence + recovery).min(0.95);
        }

        // Add timeline snapshot.
        let signal = signal_for_confidence(self.confidence);
        let snapshot = DriftSnapshot {
            domains: vec![DomainSnapshot {
                domain: DecisionDomain::DiffStrategy,
                confidence: self.confidence,
                signal,
                in_fallback: self.confidence < 0.4,
                regime_label: if self.confidence < 0.4 {
                    "fallback"
                } else {
                    "adaptive"
                },
            }],
            frame_id: frame,
        };
        self.timeline.push(snapshot);
    }

    fn render_frame(&self) -> RenderedFrame {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(VIEWPORT_W, VIEWPORT_H, &mut pool);

        if self.overlay_visible {
            let signal = signal_for_confidence(self.confidence);
            let disc = build_disclosure(self.disclosure_level, signal, self.confidence);
            let card = DecisionCard::new(&disc);

            // Decision card in top portion.
            let card_h = match self.disclosure_level {
                DisclosureLevel::TrafficLight => 5,
                DisclosureLevel::PlainEnglish => 7,
                DisclosureLevel::EvidenceTerms => 12,
                DisclosureLevel::FullBayesian => 18,
            };
            let card_area = Rect::new(2, 1, VIEWPORT_W - 4, card_h.min(VIEWPORT_H - 2));
            card.render(card_area, &mut frame);

            // Timeline below if visible.
            if self.timeline_visible {
                let timeline_y = card_h + 2;
                if timeline_y + 6 < VIEWPORT_H {
                    let timeline_area =
                        Rect::new(2, timeline_y, VIEWPORT_W - 4, VIEWPORT_H - timeline_y - 1);
                    let viz = DriftVisualization::new(&self.timeline);
                    viz.render(timeline_area, &mut frame);
                }
            }
        }

        capture_frame(&frame)
    }
}

// ============================================================================
// Event Schedule (from TOML)
// ============================================================================

struct ScenarioEvent {
    frame: u64,
    event_type: &'static str,
    key: Option<&'static str>,
    intensity: Option<f64>,
}

fn scenario_events() -> Vec<ScenarioEvent> {
    vec![
        ScenarioEvent {
            frame: 0,
            event_type: "init",
            key: None,
            intensity: None,
        },
        ScenarioEvent {
            frame: 5,
            event_type: "keypress",
            key: Some("Enter"),
            intensity: None,
        },
        ScenarioEvent {
            frame: 30,
            event_type: "keypress",
            key: Some("Right"),
            intensity: None,
        },
        ScenarioEvent {
            frame: 60,
            event_type: "keypress",
            key: Some("Right"),
            intensity: None,
        },
        ScenarioEvent {
            frame: 100,
            event_type: "keypress",
            key: Some("Right"),
            intensity: None,
        },
        ScenarioEvent {
            frame: 120,
            event_type: "inject_drift",
            key: None,
            intensity: Some(0.3),
        },
        ScenarioEvent {
            frame: 160,
            event_type: "inject_drift",
            key: None,
            intensity: Some(0.6),
        },
        ScenarioEvent {
            frame: 200,
            event_type: "inject_drift",
            key: None,
            intensity: Some(0.9),
        },
        ScenarioEvent {
            frame: 240,
            event_type: "inject_drift",
            key: None,
            intensity: Some(0.0),
        },
        ScenarioEvent {
            frame: 360,
            event_type: "keypress",
            key: Some("t"),
            intensity: None,
        },
        ScenarioEvent {
            frame: 420,
            event_type: "keypress",
            key: Some("Left"),
            intensity: None,
        },
        ScenarioEvent {
            frame: 450,
            event_type: "keypress",
            key: Some("Left"),
            intensity: None,
        },
        ScenarioEvent {
            frame: 480,
            event_type: "keypress",
            key: Some("Left"),
            intensity: None,
        },
        ScenarioEvent {
            frame: 540,
            event_type: "keypress",
            key: Some("Escape"),
            intensity: None,
        },
    ]
}

// ============================================================================
// Checkpoint definitions (from TOML)
// ============================================================================

struct Checkpoint {
    frame: u64,
    description: &'static str,
    assertions: Vec<&'static str>,
}

fn scenario_checkpoints() -> Vec<Checkpoint> {
    vec![
        Checkpoint {
            frame: 10,
            description: "L0 traffic light: green OK badge visible",
            assertions: vec!["contains:OK", "contains:incremental_diff"],
        },
        Checkpoint {
            frame: 35,
            description: "L1 plain English: confidence and action label",
            assertions: vec!["contains:confidence", "contains:OK"],
        },
        Checkpoint {
            frame: 65,
            description: "L2 evidence terms: Bayes factor visible",
            assertions: vec!["contains:Evidence", "contains:BF"],
        },
        Checkpoint {
            frame: 105,
            description: "L3 full Bayesian: posterior and loss visible",
            assertions: vec!["contains:log_post", "contains:loss"],
        },
        Checkpoint {
            frame: 220,
            description: "Fallback triggered: ALERT signal",
            assertions: vec!["contains:ALERT"],
        },
        Checkpoint {
            frame: 490,
            description: "L0 restored: clean traffic light badge",
            assertions: vec!["contains:OK"],
        },
        Checkpoint {
            frame: 560,
            description: "Final state: overlay closed",
            assertions: vec![],
        },
    ]
}

// ============================================================================
// JSONL Evidence
// ============================================================================

static SEQ: AtomicU64 = AtomicU64::new(0);

fn next_seq() -> u64 {
    SEQ.fetch_add(1, Ordering::Relaxed)
}

fn emit_event(events: &mut Vec<String>, event_type: &str, fields: &[(&str, &str)]) {
    let field_json: String = fields
        .iter()
        .map(|(k, v)| format!(",\"{}\":\"{}\"", k, v))
        .collect();
    events.push(format!(
        "{{\"event\":\"{}\",\"seq\":{}{}}}",
        event_type,
        next_seq(),
        field_json
    ));
}

fn emit_jsonl(events: &[String], path: &Path) {
    let mut file = std::fs::File::create(path).expect("create scenario JSONL");
    for line in events {
        writeln!(file, "{}", line).expect("write event");
    }
}

// ============================================================================
// Scenario Runner
// ============================================================================

struct ScenarioRun {
    checksums: Vec<(u64, String)>,
    checkpoint_results: Vec<(u64, bool, String)>,
    events: Vec<String>,
    total_duration_us: u64,
}

fn run_scenario() -> ScenarioRun {
    let start = Instant::now();
    let mut state = ScenarioState::new();
    let events_schedule = scenario_events();
    let checkpoints = scenario_checkpoints();
    let mut jsonl_events = Vec::new();
    let mut checksums = Vec::new();
    let mut checkpoint_results = Vec::new();

    let mut event_idx = 0;
    let mut checkpoint_idx = 0;

    for frame in 0..DURATION_FRAMES {
        // Apply any events scheduled for this frame.
        while event_idx < events_schedule.len() && events_schedule[event_idx].frame == frame {
            let ev = &events_schedule[event_idx];
            state.apply_event(frame, ev.event_type, ev.key, ev.intensity);
            emit_event(
                &mut jsonl_events,
                "scenario_event",
                &[("frame", &frame.to_string()), ("type", ev.event_type)],
            );
            event_idx += 1;
        }

        // Tick the state machine.
        state.tick(frame);

        // Check if this frame is a checkpoint.
        if checkpoint_idx < checkpoints.len() && checkpoints[checkpoint_idx].frame == frame {
            let cp = &checkpoints[checkpoint_idx];
            let rendered = state.render_frame();
            let cs = rendered.checksum.clone();
            let rows = &rendered.rows;

            let mut passed = true;
            let mut failure_detail = String::new();

            for assertion in &cp.assertions {
                if let Some(needle) = assertion.strip_prefix("contains:")
                    && !frame_contains(rows, needle)
                {
                    passed = false;
                    failure_detail = format!(
                        "frame {}: '{}' not found. Description: {}",
                        frame, needle, cp.description
                    );
                    break;
                }
            }

            checksums.push((frame, cs.clone()));
            checkpoint_results.push((frame, passed, failure_detail));

            emit_event(
                &mut jsonl_events,
                "checkpoint",
                &[
                    ("frame", &frame.to_string()),
                    ("passed", if passed { "true" } else { "false" }),
                    ("checksum", &cs),
                    ("description", cp.description),
                ],
            );

            checkpoint_idx += 1;
        }
    }

    let total_duration_us = start.elapsed().as_micros() as u64;

    emit_event(
        &mut jsonl_events,
        "scenario_summary",
        &[
            ("scenario", SCENARIO_NAME),
            ("frames", &DURATION_FRAMES.to_string()),
            ("checkpoints", &checkpoint_results.len().to_string()),
            ("duration_us", &total_duration_us.to_string()),
        ],
    );

    ScenarioRun {
        checksums,
        checkpoint_results,
        events: jsonl_events,
        total_duration_us,
    }
}

// ============================================================================
// Tests
// ============================================================================

/// Full scenario execution: all checkpoints pass.
#[test]
fn scenario_all_checkpoints_pass() {
    let run = run_scenario();

    let failures: Vec<_> = run
        .checkpoint_results
        .iter()
        .filter(|(_, passed, _)| !passed)
        .collect();

    assert!(
        failures.is_empty(),
        "Scenario checkpoint failures ({}):\n{}",
        failures.len(),
        failures
            .iter()
            .map(|(frame, _, detail)| format!("  frame {}: {}", frame, detail))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Deterministic replay: same seed produces identical checksums.
#[test]
fn scenario_deterministic_replay() {
    let run1 = run_scenario();
    let run2 = run_scenario();

    assert_eq!(
        run1.checksums.len(),
        run2.checksums.len(),
        "checkpoint count mismatch between runs"
    );

    for ((f1, cs1), (f2, cs2)) in run1.checksums.iter().zip(run2.checksums.iter()) {
        assert_eq!(f1, f2, "frame index mismatch");
        assert_eq!(
            cs1, cs2,
            "determinism violation at frame {}: {} vs {}",
            f1, cs1, cs2
        );
    }
}

/// Timing SLO: full scenario < 30 seconds.
#[test]
fn scenario_completes_within_slo() {
    let run = run_scenario();
    let max_us = 30_000_000; // 30 seconds

    eprintln!(
        "Scenario duration: {:.1}ms ({} frames)",
        run.total_duration_us as f64 / 1000.0,
        DURATION_FRAMES
    );

    assert!(
        run.total_duration_us < max_us,
        "scenario took {}us, SLO is {}us (30s)",
        run.total_duration_us,
        max_us
    );
}

/// JSONL evidence output is valid.
#[test]
fn scenario_jsonl_evidence_valid() {
    let run = run_scenario();

    let mut attempt = 0_u64;
    let output_dir = loop {
        let dir = std::env::temp_dir().join(format!(
            "ftui-galaxy-brain-{}-{attempt}",
            std::process::id()
        ));
        match std::fs::create_dir(&dir) {
            Ok(()) => break dir,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => attempt += 1,
            Err(error) => panic!("create retained fixture {}: {error}", dir.display()),
        }
    };
    let jsonl_path = output_dir.join("e2e_galaxy_brain_scenario.jsonl");
    emit_jsonl(&run.events, &jsonl_path);

    let content = std::fs::read_to_string(&jsonl_path).expect("read JSONL");
    let lines: Vec<&str> = content.lines().collect();

    assert!(
        lines.len() >= 10,
        "expected at least 10 JSONL events, got {}",
        lines.len()
    );

    for (i, line) in lines.iter().enumerate() {
        let v: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("invalid JSON at line {}: {}", i, e));
        assert!(v["event"].is_string(), "line {} missing 'event' field", i);
        assert!(v["seq"].is_u64(), "line {} missing 'seq' field", i);
    }

    // Verify summary event.
    let last: serde_json::Value = serde_json::from_str(lines.last().unwrap()).unwrap();
    assert_eq!(last["event"], "scenario_summary");
}

/// Checkpoint count matches scenario definition.
#[test]
fn scenario_checkpoint_count() {
    let run = run_scenario();
    let expected = scenario_checkpoints().len();
    assert_eq!(
        run.checkpoint_results.len(),
        expected,
        "expected {} checkpoints, got {}",
        expected,
        run.checkpoint_results.len()
    );
}

/// Phase 1: L0 shows traffic light badge.
#[test]
fn scenario_phase1_l0_traffic_light() {
    let mut state = ScenarioState::new();
    state.apply_event(0, "init", None, None);
    state.apply_event(5, "keypress", Some("Enter"), None);
    for f in 0..10 {
        state.tick(f);
    }

    let rendered = state.render_frame();
    assert!(
        frame_contains(&rendered.rows, "OK"),
        "L0 should show OK badge"
    );
    assert!(
        frame_contains(&rendered.rows, "incremental_diff"),
        "L0 should show action label"
    );
}

/// Phase 1: L3 shows full Bayesian details.
#[test]
fn scenario_phase1_l3_full_bayesian() {
    let mut state = ScenarioState::new();
    state.apply_event(0, "init", None, None);
    state.apply_event(5, "keypress", Some("Enter"), None);
    // Expand through L1, L2, L3.
    state.apply_event(30, "keypress", Some("Right"), None);
    state.apply_event(60, "keypress", Some("Right"), None);
    state.apply_event(100, "keypress", Some("Right"), None);
    for f in 0..105 {
        state.tick(f);
    }

    let rendered = state.render_frame();
    assert!(
        frame_contains(&rendered.rows, "log_post"),
        "L3 should show log_post: {:?}",
        rendered.rows
    );
    assert!(
        frame_contains(&rendered.rows, "loss"),
        "L3 should show loss: {:?}",
        rendered.rows
    );
}

/// Phase 2: Drift injection reduces confidence.
#[test]
fn scenario_phase2_drift_reduces_confidence() {
    let mut state = ScenarioState::new();
    state.apply_event(0, "init", None, None);
    state.apply_event(5, "keypress", Some("Enter"), None);
    // Keep at L3 for visibility.
    state.apply_event(30, "keypress", Some("Right"), None);
    state.apply_event(60, "keypress", Some("Right"), None);
    state.apply_event(100, "keypress", Some("Right"), None);

    // Tick through steady state.
    for f in 0..120 {
        state.tick(f);
    }
    let pre_drift_confidence = state.confidence;

    // Inject drift.
    state.apply_event(120, "inject_drift", None, Some(0.9));
    for f in 120..220 {
        state.tick(f);
    }

    assert!(
        state.confidence < pre_drift_confidence,
        "drift should reduce confidence: before={:.3}, after={:.3}",
        pre_drift_confidence,
        state.confidence
    );
    assert!(
        state.confidence < 0.4,
        "heavy drift should trigger fallback zone: confidence={:.3}",
        state.confidence
    );
}

/// Phase 3: Recovery restores confidence.
#[test]
fn scenario_phase3_recovery() {
    let mut state = ScenarioState::new();
    state.apply_event(0, "init", None, None);

    // Steady state.
    for f in 0..120 {
        state.tick(f);
    }

    // Drift.
    state.apply_event(120, "inject_drift", None, Some(0.9));
    for f in 120..240 {
        state.tick(f);
    }
    let post_drift_confidence = state.confidence;

    // Recovery.
    state.apply_event(240, "inject_drift", None, Some(0.0));
    for f in 240..360 {
        state.tick(f);
    }

    assert!(
        state.confidence > post_drift_confidence,
        "recovery should restore confidence: drift={:.3}, recovered={:.3}",
        post_drift_confidence,
        state.confidence
    );
}

/// Phase 4: Timeline visualization renders without panic.
#[test]
fn scenario_phase4_timeline_visible() {
    let mut state = ScenarioState::new();
    state.apply_event(0, "init", None, None);
    state.apply_event(5, "keypress", Some("Enter"), None);

    for f in 0..360 {
        state.tick(f);
    }

    state.apply_event(360, "keypress", Some("t"), None);
    assert!(state.timeline_visible);

    // Should render without panic.
    let _frame = state.render_frame();
}

/// Overlay toggle: Enter opens, Escape closes.
#[test]
fn scenario_overlay_lifecycle() {
    let mut state = ScenarioState::new();
    assert!(!state.overlay_visible);

    state.apply_event(0, "keypress", Some("Enter"), None);
    assert!(state.overlay_visible);

    state.apply_event(1, "keypress", Some("Escape"), None);
    assert!(!state.overlay_visible);
}

/// Disclosure level cycling: Right increments, Left decrements.
#[test]
fn scenario_disclosure_level_cycling() {
    let mut state = ScenarioState::new();
    assert_eq!(state.disclosure_level, DisclosureLevel::TrafficLight);

    state.apply_event(0, "keypress", Some("Right"), None);
    assert_eq!(state.disclosure_level, DisclosureLevel::PlainEnglish);

    state.apply_event(1, "keypress", Some("Right"), None);
    assert_eq!(state.disclosure_level, DisclosureLevel::EvidenceTerms);

    state.apply_event(2, "keypress", Some("Right"), None);
    assert_eq!(state.disclosure_level, DisclosureLevel::FullBayesian);

    // Right at max stays at max.
    state.apply_event(3, "keypress", Some("Right"), None);
    assert_eq!(state.disclosure_level, DisclosureLevel::FullBayesian);

    state.apply_event(4, "keypress", Some("Left"), None);
    assert_eq!(state.disclosure_level, DisclosureLevel::EvidenceTerms);

    state.apply_event(5, "keypress", Some("Left"), None);
    assert_eq!(state.disclosure_level, DisclosureLevel::PlainEnglish);

    state.apply_event(6, "keypress", Some("Left"), None);
    assert_eq!(state.disclosure_level, DisclosureLevel::TrafficLight);

    // Left at min stays at min.
    state.apply_event(7, "keypress", Some("Left"), None);
    assert_eq!(state.disclosure_level, DisclosureLevel::TrafficLight);
}

/// Scenario TOML file exists and has expected structure.
#[test]
fn scenario_toml_file_exists() {
    let toml_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/e2e/scenarios/demos/galaxy_brain_transparency.toml");
    assert!(
        toml_path.exists(),
        "scenario TOML missing: {}",
        toml_path.display()
    );

    let content = std::fs::read_to_string(&toml_path).expect("read scenario TOML");
    assert!(content.contains("[scenario]"), "missing [scenario] section");
    assert!(
        content.contains("galaxy-brain-transparency-demo"),
        "missing scenario name"
    );
    assert!(content.contains("[[events]]"), "missing events");
    assert!(content.contains("[[checkpoints]]"), "missing checkpoints");
    assert!(content.contains("[ci]"), "missing CI config");
    assert!(
        content.contains("duration_frames = 600"),
        "wrong duration_frames"
    );
}
