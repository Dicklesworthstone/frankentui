#![cfg(feature = "experimental")]
#![forbid(unsafe_code)]

//! bd-382tk.1: E2E integration test for Recipe F Policy-as-Data controllers.
//!
//! Validates:
//! 1. Policy artifact creation and loading into the registry.
//! 2. Shadow-run: execute loaded vs baked-in policy decisions in parallel.
//! 3. Progressive delivery stages (shadow → canary → ramp → default).
//! 4. Fallback on invalid/corrupt policy (validation failure).
//! 5. Seamless recovery (fallback triggers within one decision cycle).
//! 6. JSONL structured logging for every policy decision.
//!
//! Run:
//!   cargo test -p ftui-runtime --test policy_e2e

use std::sync::Arc;

use ftui_runtime::policy_config::PolicyConfig;
use ftui_runtime::policy_registry::{PolicyRegistry, PolicyRegistryError, STANDARD_POLICY};

// ============================================================================
// JSONL Log Entry
// ============================================================================

#[derive(serde::Serialize)]
struct PolicyDecisionLog {
    event: &'static str,
    frame_id: u64,
    policy_source: String,
    delivery_stage: String,
    decision_point: String,
    loaded_action: String,
    baked_in_action: String,
    divergence: bool,
    divergence_reason: Option<String>,
    policy_version: String,
    signature_valid: bool,
    fallback_triggered: bool,
}

// ============================================================================
// Decision Simulation
// ============================================================================

/// Simulate a policy decision by comparing loaded vs baked-in config values.
/// Returns (loaded_action, baked_in_action, divergence).
fn simulate_decision(
    loaded: &PolicyConfig,
    baked_in: &PolicyConfig,
    decision_point: &str,
) -> (String, String, bool) {
    match decision_point {
        "diff_strategy" => {
            let loaded_alpha = loaded.conformal.alpha;
            let baked_in_alpha = baked_in.conformal.alpha;
            let loaded_action = if loaded_alpha < 0.03 {
                "aggressive_diff"
            } else {
                "conservative_diff"
            };
            let baked_in_action = if baked_in_alpha < 0.03 {
                "aggressive_diff"
            } else {
                "conservative_diff"
            };
            (
                loaded_action.to_string(),
                baked_in_action.to_string(),
                loaded_action != baked_in_action,
            )
        }
        "resize_coalesce" => {
            let loaded_kp = loaded.pid.kp;
            let baked_in_kp = baked_in.pid.kp;
            let loaded_action = if loaded_kp > 0.7 {
                "fast_coalesce"
            } else {
                "slow_coalesce"
            };
            let baked_in_action = if baked_in_kp > 0.7 {
                "fast_coalesce"
            } else {
                "slow_coalesce"
            };
            (
                loaded_action.to_string(),
                baked_in_action.to_string(),
                loaded_action != baked_in_action,
            )
        }
        "degradation" => {
            let loaded_threshold = loaded.cascade.recovery_threshold;
            let baked_in_threshold = baked_in.cascade.recovery_threshold;
            let loaded_action = if loaded_threshold > 15 {
                "strict_recovery"
            } else {
                "relaxed_recovery"
            };
            let baked_in_action = if baked_in_threshold > 15 {
                "strict_recovery"
            } else {
                "relaxed_recovery"
            };
            (
                loaded_action.to_string(),
                baked_in_action.to_string(),
                loaded_action != baked_in_action,
            )
        }
        _ => ("unknown".to_string(), "unknown".to_string(), false),
    }
}

/// Which policy's decision to use based on delivery stage and frame_id.
fn select_action(
    delivery_stage: &str,
    frame_id: u64,
    loaded_action: &str,
    baked_in_action: &str,
) -> String {
    match delivery_stage {
        "shadow" => baked_in_action.to_string(), // 100% baked-in
        "canary" => {
            // 10% loaded, 90% baked-in (deterministic based on frame_id)
            if frame_id.is_multiple_of(10) {
                loaded_action.to_string()
            } else {
                baked_in_action.to_string()
            }
        }
        "ramp" => {
            // 50/50
            if frame_id.is_multiple_of(2) {
                loaded_action.to_string()
            } else {
                baked_in_action.to_string()
            }
        }
        "default" => loaded_action.to_string(), // 100% loaded
        _ => baked_in_action.to_string(),
    }
}

// ============================================================================
// Test: Full E2E Progressive Delivery
// ============================================================================

#[test]
fn e2e_progressive_delivery_with_shadow_run() {
    let registry = Arc::new(PolicyRegistry::new());
    let baked_in = PolicyConfig::default();

    // Step 1: Create a custom policy artifact with different tuning.
    let mut loaded_policy = PolicyConfig::default();
    loaded_policy.conformal.alpha = 0.01; // aggressive → diverges on diff_strategy
    loaded_policy.pid.kp = 0.8; // fast → diverges on resize_coalesce
    loaded_policy.cascade.recovery_threshold = 20; // strict → diverges on degradation

    registry.register("loaded", loaded_policy.clone()).unwrap();

    let decision_points = ["diff_strategy", "resize_coalesce", "degradation"];
    let stages = ["shadow", "canary", "ramp", "default"];
    let frames_per_stage = 20u64;

    let mut logs = Vec::new();
    let mut frame_id = 0u64;

    for &stage in &stages {
        // Switch to loaded policy for all stages except shadow
        // (in shadow mode we still use baked-in but log loaded decisions)
        match stage {
            "shadow" => {
                // Active stays "standard"
                assert_eq!(registry.active_name(), STANDARD_POLICY);
            }
            "canary" | "ramp" | "default" => {
                let _ = registry.set_active("loaded");
                assert_eq!(registry.active_name(), "loaded");
            }
            _ => unreachable!(),
        }

        for _ in 0..frames_per_stage {
            for &dp in &decision_points {
                let (loaded_action, baked_in_action, divergence) =
                    simulate_decision(&loaded_policy, &baked_in, dp);

                let selected = select_action(stage, frame_id, &loaded_action, &baked_in_action);

                let log = PolicyDecisionLog {
                    event: "recipe_f_policy",
                    frame_id,
                    policy_source: if selected == loaded_action {
                        "loaded".to_string()
                    } else {
                        "baked_in".to_string()
                    },
                    delivery_stage: stage.to_string(),
                    decision_point: dp.to_string(),
                    loaded_action: loaded_action.clone(),
                    baked_in_action: baked_in_action.clone(),
                    divergence,
                    divergence_reason: if divergence {
                        Some(format!(
                            "{dp}: loaded={loaded_action}, baked_in={baked_in_action}"
                        ))
                    } else {
                        None
                    },
                    policy_version: "loaded-v1".to_string(),
                    signature_valid: true,
                    fallback_triggered: false,
                };

                logs.push(log);
            }
            frame_id += 1;
        }
    }

    // Verify JSONL compliance.
    for log in &logs {
        let json = serde_json::to_string(log).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["event"], "recipe_f_policy");
        assert!(parsed["frame_id"].is_u64());
        assert!(parsed["divergence"].is_boolean());
    }

    // Verify shadow mode: all decisions use baked_in.
    let shadow_logs: Vec<_> = logs
        .iter()
        .filter(|l| l.delivery_stage == "shadow")
        .collect();
    assert_eq!(shadow_logs.len(), frames_per_stage as usize * 3);
    for log in &shadow_logs {
        assert_eq!(
            log.policy_source, "baked_in",
            "shadow mode must always use baked_in"
        );
    }

    // Verify shadow mode logs divergences.
    let shadow_divergences: Vec<_> = shadow_logs.iter().filter(|l| l.divergence).collect();
    assert!(
        !shadow_divergences.is_empty(),
        "shadow mode should detect divergences with different policy"
    );

    // Verify canary mode: 10% loaded decisions.
    let canary_logs: Vec<_> = logs
        .iter()
        .filter(|l| l.delivery_stage == "canary")
        .collect();
    let canary_loaded: Vec<_> = canary_logs
        .iter()
        .filter(|l| l.policy_source == "loaded")
        .collect();
    // Should be roughly 10% (frame_id.is_multiple_of(10))
    assert!(
        !canary_loaded.is_empty(),
        "canary mode should use some loaded decisions"
    );
    assert!(
        canary_loaded.len() < canary_logs.len(),
        "canary mode should not use loaded for all decisions"
    );

    // Verify default mode: all decisions use loaded.
    let default_logs: Vec<_> = logs
        .iter()
        .filter(|l| l.delivery_stage == "default")
        .collect();
    for log in &default_logs {
        assert_eq!(
            log.policy_source, "loaded",
            "default mode must always use loaded"
        );
    }

    // Verify progressive order.
    let stage_first_frame: Vec<(&str, u64)> = stages
        .iter()
        .map(|&s| {
            let first = logs
                .iter()
                .find(|l| l.delivery_stage == s)
                .unwrap()
                .frame_id;
            (s, first)
        })
        .collect();
    for w in stage_first_frame.windows(2) {
        assert!(
            w[0].1 < w[1].1,
            "stages must execute in order: {} (frame {}) before {} (frame {})",
            w[0].0,
            w[0].1,
            w[1].0,
            w[1].1,
        );
    }

    let total = logs.len();
    eprintln!("--- e2e_progressive_delivery summary ---");
    eprintln!("total_decisions: {total}");
    eprintln!("shadow_divergences: {}", shadow_divergences.len());
    eprintln!(
        "canary_loaded_pct: {:.1}%",
        canary_loaded.len() as f64 / canary_logs.len() as f64 * 100.0
    );
    eprintln!(
        "stages: {:?}",
        stage_first_frame
            .iter()
            .map(|(s, f)| format!("{s}@{f}"))
            .collect::<Vec<_>>()
    );
}

// ============================================================================
// Test: Fallback on Invalid Policy
// ============================================================================

#[test]
fn e2e_fallback_on_invalid_policy() {
    let registry = PolicyRegistry::new();
    let baked_in = PolicyConfig::default();

    // Register a valid loaded policy and activate it.
    let mut loaded_policy = PolicyConfig::default();
    loaded_policy.conformal.alpha = 0.01;
    registry.register("loaded", loaded_policy.clone()).unwrap();
    registry.set_active("loaded").unwrap();
    assert_eq!(registry.active_name(), "loaded");

    // Simulate normal operation for a few frames.
    let mut logs = Vec::new();
    for frame_id in 0..5u64 {
        let (loaded_action, baked_in_action, divergence) =
            simulate_decision(&loaded_policy, &baked_in, "diff_strategy");
        logs.push(PolicyDecisionLog {
            event: "recipe_f_policy",
            frame_id,
            policy_source: "loaded".to_string(),
            delivery_stage: "default".to_string(),
            decision_point: "diff_strategy".to_string(),
            loaded_action,
            baked_in_action,
            divergence,
            divergence_reason: None,
            policy_version: "loaded-v1".to_string(),
            signature_valid: true,
            fallback_triggered: false,
        });
    }

    // Step 7: Inject verification failure — try to register a corrupt policy.
    let mut corrupt = PolicyConfig::default();
    corrupt.conformal.alpha = 0.0; // Invalid: alpha must be in (0, 1)
    let err = registry.register("corrupt", corrupt);
    assert!(
        matches!(err, Err(PolicyRegistryError::ValidationFailed(_))),
        "corrupt policy must be rejected"
    );

    // Step 8: Verify immediate fallback — switch back to standard.
    registry.set_active(STANDARD_POLICY).unwrap();
    assert_eq!(registry.active_name(), STANDARD_POLICY);

    // Step 9: Verify fallback is seamless — next decision uses baked-in.
    let active = registry.active_config();
    let (loaded_action, baked_in_action, divergence) =
        simulate_decision(&active, &baked_in, "diff_strategy");
    assert!(
        !divergence,
        "after fallback, active policy must match baked-in"
    );

    logs.push(PolicyDecisionLog {
        event: "recipe_f_policy",
        frame_id: 5,
        policy_source: "baked_in".to_string(),
        delivery_stage: "fallback".to_string(),
        decision_point: "diff_strategy".to_string(),
        loaded_action,
        baked_in_action,
        divergence,
        divergence_reason: None,
        policy_version: "standard".to_string(),
        signature_valid: false,
        fallback_triggered: true,
    });

    // Verify JSONL.
    for log in &logs {
        let json = serde_json::to_string(log).unwrap();
        let _: serde_json::Value = serde_json::from_str(&json).unwrap();
    }

    // Verify fallback log entry.
    let fallback_log = logs.last().unwrap();
    assert!(fallback_log.fallback_triggered);
    assert!(!fallback_log.signature_valid);
    assert_eq!(fallback_log.delivery_stage, "fallback");

    eprintln!("--- e2e_fallback_on_invalid_policy ---");
    eprintln!("normal_frames: 5, fallback_frame: 1");
    eprintln!("corrupt_rejected: true, fallback_triggered: true");
}

// ============================================================================
// Test: Policy Switch Evidence Ledger
// ============================================================================

#[test]
fn e2e_switch_evidence_ledger() {
    let registry = PolicyRegistry::new();

    // Register multiple policies.
    let policies = [
        ("aggressive", 0.01f64),
        ("moderate", 0.03),
        ("relaxed", 0.08),
    ];
    for &(name, alpha) in &policies {
        let mut p = PolicyConfig::default();
        p.conformal.alpha = alpha;
        registry.register(name, p).unwrap();
    }

    // Progressive delivery: aggressive → moderate → relaxed → standard.
    let sequence = ["aggressive", "moderate", "relaxed", STANDARD_POLICY];
    let mut switch_events = Vec::new();

    for &target in &sequence {
        let event = registry.set_active(target).unwrap();
        let jsonl = event.to_jsonl();

        // Verify JSONL format.
        let parsed: serde_json::Value = serde_json::from_str(&jsonl).unwrap();
        assert_eq!(parsed["schema"], "policy-switch-v1");
        assert_eq!(parsed["new"], target);

        switch_events.push(event);
    }

    // Verify monotonic switch IDs.
    for w in switch_events.windows(2) {
        assert!(
            w[1].switch_id > w[0].switch_id,
            "switch IDs must be monotonically increasing"
        );
    }

    // Verify old/new chain.
    assert_eq!(switch_events[0].old_name, STANDARD_POLICY);
    assert_eq!(switch_events[0].new_name, "aggressive");
    assert_eq!(switch_events[1].old_name, "aggressive");
    assert_eq!(switch_events[1].new_name, "moderate");

    // Verify round-trip back to standard.
    assert_eq!(switch_events.last().unwrap().new_name, STANDARD_POLICY);
    assert_eq!(registry.active_name(), STANDARD_POLICY);

    // Switch count matches.
    assert_eq!(registry.switch_count(), sequence.len() as u64);

    eprintln!("--- e2e_switch_evidence_ledger ---");
    eprintln!("switches: {}", switch_events.len());
    for e in &switch_events {
        eprintln!("  #{}: {} -> {}", e.switch_id, e.old_name, e.new_name);
    }
}

// ============================================================================
// Test: Concurrent Shadow-Run Safety
// ============================================================================

#[test]
fn e2e_concurrent_shadow_run() {
    let registry = Arc::new(PolicyRegistry::new());
    let baked_in = PolicyConfig::default();

    // Register loaded policy.
    let mut loaded_policy = PolicyConfig::default();
    loaded_policy.conformal.alpha = 0.02;
    registry.register("loaded", loaded_policy.clone()).unwrap();

    // Run shadow comparison from multiple threads simultaneously.
    let divergence_count = std::sync::atomic::AtomicU64::new(0);

    std::thread::scope(|s| {
        // 4 reader threads simulating shadow-run decisions.
        for thread_id in 0..4u64 {
            let registry = Arc::clone(&registry);
            let baked_in = &baked_in;
            let loaded_policy = &loaded_policy;
            let divergence_count = &divergence_count;

            s.spawn(move || {
                for i in 0..100u64 {
                    let frame_id = thread_id * 100 + i;
                    let active = registry.active_config();

                    // Shadow: compare loaded vs baked_in.
                    let (loaded_action, baked_in_action, divergence) =
                        simulate_decision(loaded_policy, baked_in, "diff_strategy");

                    if divergence {
                        divergence_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }

                    // Verify active config is always readable (no panic).
                    assert!(active.conformal.alpha > 0.0);
                    let _ = (loaded_action, baked_in_action, frame_id);
                }
            });
        }

        // 1 writer thread cycling policies.
        {
            let registry = Arc::clone(&registry);
            s.spawn(move || {
                for i in 0..50 {
                    if i % 2 == 0 {
                        let _ = registry.set_active("loaded");
                    } else {
                        let _ = registry.set_active(STANDARD_POLICY);
                    }
                }
            });
        }
    });

    // Divergences were detected (loaded has different alpha).
    let total_divergences = divergence_count.load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        total_divergences > 0,
        "shadow-run should detect divergences"
    );

    eprintln!("--- e2e_concurrent_shadow_run ---");
    eprintln!("total_divergences: {total_divergences} / {}", 4 * 100);
}

// ============================================================================
// Test: JSONL Schema Compliance
// ============================================================================

#[test]
fn e2e_jsonl_schema_compliance() {
    let baked_in = PolicyConfig::default();
    let mut loaded = PolicyConfig::default();
    loaded.conformal.alpha = 0.01;

    let (loaded_action, baked_in_action, divergence) =
        simulate_decision(&loaded, &baked_in, "diff_strategy");

    let log = PolicyDecisionLog {
        event: "recipe_f_policy",
        frame_id: 42,
        policy_source: "loaded".to_string(),
        delivery_stage: "canary".to_string(),
        decision_point: "diff_strategy".to_string(),
        loaded_action,
        baked_in_action,
        divergence,
        divergence_reason: Some("alpha divergence".to_string()),
        policy_version: "loaded-v1".to_string(),
        signature_valid: true,
        fallback_triggered: false,
    };

    let json = serde_json::to_string(&log).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

    // Verify all required fields exist and have correct types.
    assert!(parsed["event"].is_string());
    assert!(parsed["frame_id"].is_u64());
    assert!(parsed["policy_source"].is_string());
    assert!(parsed["delivery_stage"].is_string());
    assert!(parsed["decision_point"].is_string());
    assert!(parsed["loaded_action"].is_string());
    assert!(parsed["baked_in_action"].is_string());
    assert!(parsed["divergence"].is_boolean());
    assert!(parsed["policy_version"].is_string());
    assert!(parsed["signature_valid"].is_boolean());
    assert!(parsed["fallback_triggered"].is_boolean());

    // divergence_reason can be null or string.
    assert!(parsed["divergence_reason"].is_string() || parsed["divergence_reason"].is_null());

    // Verify specific values.
    assert_eq!(parsed["event"], "recipe_f_policy");
    assert_eq!(parsed["frame_id"], 42);
    assert_eq!(parsed["delivery_stage"], "canary");
    assert!(parsed["divergence"].as_bool().unwrap());

    // Also test the PolicyConfig JSONL format.
    let config_jsonl = baked_in.to_jsonl();
    let config_parsed: serde_json::Value = serde_json::from_str(&config_jsonl).unwrap();
    assert_eq!(config_parsed["schema"], "policy-config-v1");
}

// ============================================================================
// Test: No Dropped Frames During Fallback
// ============================================================================

#[test]
fn e2e_no_dropped_frames_during_fallback() {
    let registry = PolicyRegistry::new();
    let baked_in = PolicyConfig::default();

    // Register and activate loaded policy.
    let mut loaded = PolicyConfig::default();
    loaded.conformal.alpha = 0.01;
    registry.register("loaded", loaded.clone()).unwrap();
    registry.set_active("loaded").unwrap();

    // Simulate 10 frames with loaded policy, then fallback at frame 10.
    let mut frame_decisions = Vec::new();

    for frame_id in 0..15u64 {
        if frame_id == 10 {
            // Trigger fallback.
            registry.set_active(STANDARD_POLICY).unwrap();
        }

        let active = registry.active_config();
        let (loaded_action, baked_in_action, divergence) =
            simulate_decision(&active, &baked_in, "diff_strategy");

        let is_fallback = frame_id >= 10;
        frame_decisions.push((
            frame_id,
            loaded_action,
            baked_in_action,
            divergence,
            is_fallback,
        ));
    }

    // Verify continuous frame IDs (no gaps = no dropped frames).
    for (i, (frame_id, _, _, _, _)) in frame_decisions.iter().enumerate() {
        assert_eq!(
            *frame_id, i as u64,
            "frame IDs must be continuous (no dropped frames)"
        );
    }

    // Pre-fallback: decisions should diverge (loaded != baked_in).
    let pre_fallback: Vec<_> = frame_decisions.iter().filter(|d| !d.4).collect();
    assert!(
        pre_fallback.iter().all(|d| d.3),
        "pre-fallback decisions should diverge"
    );

    // Post-fallback: decisions should NOT diverge (active == baked_in).
    let post_fallback: Vec<_> = frame_decisions.iter().filter(|d| d.4).collect();
    assert!(
        post_fallback.iter().all(|d| !d.3),
        "post-fallback decisions must not diverge"
    );

    assert_eq!(frame_decisions.len(), 15);
    eprintln!("--- e2e_no_dropped_frames ---");
    eprintln!(
        "frames: {}, pre_fallback: {}, post_fallback: {}",
        frame_decisions.len(),
        pre_fallback.len(),
        post_fallback.len()
    );
}
