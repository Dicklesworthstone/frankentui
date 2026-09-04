#![cfg(feature = "experimental")]
#![forbid(unsafe_code)]

//! bd-382tk.4: Unit tests for policy-as-data controllers.
//!
//! Validates:
//! 1. PolicyConfig deserialization from TOML with all fields
//! 2. PolicyConfig validation rejects invalid values
//! 3. PolicyRegistry hot-swap under concurrent reads
//! 4. Default policy matches hardcoded constants
//! 5. Evidence ledger captures policy switch events
//!
//! Run:
//!   cargo test -p ftui-runtime --test policy_controller_tests --features policy-config

use std::sync::Arc;

use ftui_runtime::policy_config::*;
use ftui_runtime::policy_registry::{PolicyRegistry, PolicyRegistryError, STANDARD_POLICY};

// ============================================================================
// 1. TOML Deserialization
// ============================================================================

#[cfg(feature = "policy-config")]
mod toml_tests {
    use super::*;

    #[test]
    fn deserialize_all_fields_from_toml() {
        let toml = r#"
[conformal]
alpha = 0.01
min_samples = 50
window_size = 512
q_default = 5000.0

[frame_guard]
fallback_budget_us = 8000.0
time_series_window = 256
nonconformity_window = 128

[cascade]
recovery_threshold = 20
max_degradation = "skip_frame"
min_trigger_level = "simple_borders"
degradation_floor = "simple_borders"

[pid]
kp = 0.8
ki = 0.02
kd = 0.3
integral_max = 10.0

[eprocess_budget]
lambda = 0.3
alpha = 0.01
beta = 0.4
sigma_ema_decay = 0.95
sigma_floor_ms = 0.5
warmup_frames = 5

[bocpd]
mu_steady_ms = 100.0
mu_burst_ms = 10.0
hazard_lambda = 100.0
max_run_length = 200
steady_threshold = 0.4
burst_threshold = 0.6
burst_prior = 0.3
min_observation_ms = 0.5
max_observation_ms = 5000.0
enable_logging = true

[eprocess_throttle]
alpha = 0.01
mu_0 = 0.2
initial_lambda = 0.3
grapa_eta = 0.05
hard_deadline_ms = 1000
min_observations_between = 4
rate_window_size = 128
enable_logging = false

[voi]
alpha = 0.01
prior_alpha = 2.0
prior_beta = 2.0
mu_0 = 0.1
lambda = 0.3
value_scale = 2.0
boundary_weight = 1.5
sample_cost = 0.02
min_interval_ms = 10
max_interval_ms = 500
min_interval_events = 1
max_interval_events = 50
enable_logging = true
max_log_entries = 4096

[evidence]
ledger_capacity = 2048
sink_enabled = true
sink_file = "/tmp/evidence.jsonl"
flush_on_write = false
"#;

        let config = PolicyConfig::from_toml_str(toml).unwrap();

        // Conformal
        assert!((config.conformal.alpha - 0.01).abs() < f64::EPSILON);
        assert_eq!(config.conformal.min_samples, 50);
        assert_eq!(config.conformal.window_size, 512);
        assert!((config.conformal.q_default - 5000.0).abs() < f64::EPSILON);

        // Frame guard
        assert!((config.frame_guard.fallback_budget_us - 8000.0).abs() < f64::EPSILON);
        assert_eq!(config.frame_guard.time_series_window, 256);

        // PID
        assert!((config.pid.kp - 0.8).abs() < f64::EPSILON);
        assert!((config.pid.ki - 0.02).abs() < f64::EPSILON);
        assert!((config.pid.kd - 0.3).abs() < f64::EPSILON);

        // Cascade
        assert_eq!(config.cascade.recovery_threshold, 20);

        // BOCPD
        assert!((config.bocpd.mu_steady_ms - 100.0).abs() < f64::EPSILON);
        assert_eq!(config.bocpd.max_run_length, 200);
        assert!(config.bocpd.enable_logging);

        // E-process budget
        assert!((config.eprocess_budget.lambda - 0.3).abs() < f64::EPSILON);
        assert_eq!(config.eprocess_budget.warmup_frames, 5);

        // E-process throttle
        assert_eq!(config.eprocess_throttle.hard_deadline_ms, 1000);
        assert_eq!(config.eprocess_throttle.rate_window_size, 128);

        // VOI
        assert!((config.voi.alpha - 0.01).abs() < f64::EPSILON);
        assert_eq!(config.voi.max_log_entries, 4096);
        assert!(config.voi.enable_logging);

        // Evidence
        assert_eq!(config.evidence.ledger_capacity, 2048);
        assert!(config.evidence.sink_enabled);
        assert_eq!(
            config.evidence.sink_file.as_deref(),
            Some("/tmp/evidence.jsonl")
        );
        assert!(!config.evidence.flush_on_write);
    }

    #[test]
    fn partial_toml_uses_defaults() {
        let toml = r#"
[conformal]
alpha = 0.01
"#;
        let config = PolicyConfig::from_toml_str(toml).unwrap();

        // Overridden
        assert!((config.conformal.alpha - 0.01).abs() < f64::EPSILON);

        // Defaults preserved
        assert_eq!(config.conformal.min_samples, 20);
        assert_eq!(config.conformal.window_size, 256);
        assert!((config.pid.kp - 0.5).abs() < f64::EPSILON);
        assert_eq!(config.evidence.ledger_capacity, 1024);
    }

    #[test]
    fn empty_toml_produces_default() {
        let config = PolicyConfig::from_toml_str("").unwrap();
        let default = PolicyConfig::default();
        assert!((config.conformal.alpha - default.conformal.alpha).abs() < f64::EPSILON);
        assert_eq!(config.conformal.min_samples, default.conformal.min_samples);
        assert!((config.pid.kp - default.pid.kp).abs() < f64::EPSILON);
    }

    #[test]
    fn toml_validation_rejects_invalid_alpha() {
        let toml = r#"
[conformal]
alpha = 0.0
"#;
        let err = PolicyConfig::from_toml_str(toml).unwrap_err();
        assert!(err.to_string().contains("conformal.alpha"));
    }

    #[test]
    fn json_round_trip() {
        let config = PolicyConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let parsed = PolicyConfig::from_json_str(&json).unwrap();
        assert!((parsed.conformal.alpha - config.conformal.alpha).abs() < f64::EPSILON);
        assert_eq!(parsed.conformal.min_samples, config.conformal.min_samples);
    }

    #[test]
    fn cargo_toml_metadata_extraction() {
        let cargo = r#"
[package]
name = "test"
version = "0.1.0"

[package.metadata.ftui]

[package.metadata.ftui.conformal]
alpha = 0.02
"#;
        let config = PolicyConfig::from_cargo_toml_str(cargo).unwrap();
        assert!((config.conformal.alpha - 0.02).abs() < f64::EPSILON);
    }
}

// ============================================================================
// 2. Validation Rejects Invalid Values
// ============================================================================

#[test]
fn validation_rejects_negative_alpha() {
    let mut config = PolicyConfig::default();
    config.conformal.alpha = -0.5;
    let errors = config.validate();
    assert!(!errors.is_empty());
    assert!(errors.iter().any(|e| e.contains("conformal.alpha")));
}

#[test]
fn validation_rejects_alpha_at_boundary() {
    let mut config = PolicyConfig::default();
    config.conformal.alpha = 0.0;
    let errors = config.validate();
    assert!(errors.iter().any(|e| e.contains("conformal.alpha")));

    config.conformal.alpha = 1.0;
    let errors = config.validate();
    assert!(errors.iter().any(|e| e.contains("conformal.alpha")));
}

#[test]
fn validation_rejects_zero_min_samples() {
    let mut config = PolicyConfig::default();
    config.conformal.min_samples = 0;
    let errors = config.validate();
    assert!(errors.iter().any(|e| e.contains("conformal.min_samples")));
}

#[test]
fn validation_rejects_zero_window_size() {
    let mut config = PolicyConfig::default();
    config.conformal.window_size = 0;
    let errors = config.validate();
    assert!(errors.iter().any(|e| e.contains("conformal.window_size")));
}

#[test]
fn validation_rejects_negative_pid_kp() {
    let mut config = PolicyConfig::default();
    config.pid.kp = -1.0;
    let errors = config.validate();
    assert!(errors.iter().any(|e| e.contains("pid.kp")));
}

#[test]
fn validation_rejects_zero_integral_max() {
    let mut config = PolicyConfig::default();
    config.pid.integral_max = 0.0;
    let errors = config.validate();
    assert!(errors.iter().any(|e| e.contains("pid.integral_max")));
}

#[test]
fn validation_rejects_nan_values() {
    let mut config = PolicyConfig::default();
    config.conformal.alpha = f64::NAN;
    let errors = config.validate();
    assert!(
        errors
            .iter()
            .any(|e| e.contains("conformal.alpha") && e.contains("finite"))
    );
}

#[test]
fn validation_rejects_inf_values() {
    let mut config = PolicyConfig::default();
    config.pid.kp = f64::INFINITY;
    let errors = config.validate();
    assert!(
        errors
            .iter()
            .any(|e| e.contains("pid.kp") && e.contains("finite"))
    );
}

#[test]
fn validation_rejects_zero_ledger_capacity() {
    let mut config = PolicyConfig::default();
    config.evidence.ledger_capacity = 0;
    let errors = config.validate();
    assert!(
        errors
            .iter()
            .any(|e| e.contains("evidence.ledger_capacity"))
    );
}

#[test]
fn validation_rejects_zero_bocpd_max_run_length() {
    let mut config = PolicyConfig::default();
    config.bocpd.max_run_length = 0;
    let errors = config.validate();
    assert!(errors.iter().any(|e| e.contains("bocpd.max_run_length")));
}

#[test]
fn validation_rejects_negative_frame_guard_budget() {
    let mut config = PolicyConfig::default();
    config.frame_guard.fallback_budget_us = -1.0;
    let errors = config.validate();
    assert!(
        errors
            .iter()
            .any(|e| e.contains("frame_guard.fallback_budget_us"))
    );
}

#[test]
fn validation_rejects_eprocess_budget_alpha_out_of_range() {
    let mut config = PolicyConfig::default();
    config.eprocess_budget.alpha = 1.5;
    let errors = config.validate();
    assert!(errors.iter().any(|e| e.contains("eprocess_budget.alpha")));
}

#[test]
fn validation_rejects_eprocess_throttle_alpha_out_of_range() {
    let mut config = PolicyConfig::default();
    config.eprocess_throttle.alpha = 0.0;
    let errors = config.validate();
    assert!(errors.iter().any(|e| e.contains("eprocess_throttle.alpha")));
}

#[test]
fn validation_rejects_voi_alpha_out_of_range() {
    let mut config = PolicyConfig::default();
    config.voi.alpha = -0.1;
    let errors = config.validate();
    assert!(errors.iter().any(|e| e.contains("voi.alpha")));
}

#[test]
fn validation_rejects_negative_sample_cost() {
    let mut config = PolicyConfig::default();
    config.voi.sample_cost = -0.1;
    let errors = config.validate();
    assert!(errors.iter().any(|e| e.contains("voi.sample_cost")));
}

#[test]
fn validation_rejects_negative_bocpd_hazard_lambda() {
    let mut config = PolicyConfig::default();
    config.bocpd.hazard_lambda = 0.0;
    let errors = config.validate();
    assert!(errors.iter().any(|e| e.contains("bocpd.hazard_lambda")));
}

#[test]
fn validation_multiple_errors() {
    let mut config = PolicyConfig::default();
    config.conformal.alpha = -1.0;
    config.pid.kp = -1.0;
    config.evidence.ledger_capacity = 0;
    let errors = config.validate();
    assert!(
        errors.len() >= 3,
        "should collect multiple errors: {errors:?}"
    );
}

#[test]
fn default_config_is_valid() {
    let config = PolicyConfig::default();
    let errors = config.validate();
    assert!(
        errors.is_empty(),
        "default config should be valid: {errors:?}"
    );
}

// ============================================================================
// 3. PolicyRegistry Hot-Swap Under Concurrent Reads
// ============================================================================

#[test]
fn registry_concurrent_reads_during_rapid_switches() {
    let reg = Arc::new(PolicyRegistry::new());

    // Register several named policies
    for i in 0..5 {
        let mut p = PolicyConfig::default();
        p.conformal.alpha = 0.01 + (i as f64) * 0.01;
        reg.register(&format!("policy_{i}"), p).unwrap();
    }

    std::thread::scope(|s| {
        // 8 reader threads
        for _ in 0..8 {
            let reg = Arc::clone(&reg);
            s.spawn(move || {
                for _ in 0..500 {
                    let name = reg.active_name();
                    let config = reg.active_config();
                    // Must always get a valid config; never panic
                    assert!(!name.is_empty());
                    assert!(config.conformal.alpha > 0.0);
                }
            });
        }

        // 2 writer threads cycling through policies
        for thread_id in 0..2 {
            let reg = Arc::clone(&reg);
            s.spawn(move || {
                let names = [
                    "standard", "policy_0", "policy_1", "policy_2", "policy_3", "policy_4",
                ];
                for i in 0..200 {
                    let name = names[(i + thread_id) % names.len()];
                    let _ = reg.set_active(name);
                }
            });
        }
    });

    // No panics → lock-free reads are safe under concurrent writes
    assert!(reg.switch_count() > 0);
}

#[test]
fn registry_concurrent_register_and_switch() {
    let reg = Arc::new(PolicyRegistry::new());

    std::thread::scope(|s| {
        // Thread A: registers new policies
        {
            let reg = Arc::clone(&reg);
            s.spawn(move || {
                for i in 0..50 {
                    let mut p = PolicyConfig::default();
                    p.conformal.alpha = 0.02 + (i as f64) * 0.001;
                    let _ = reg.register(&format!("dyn_{i}"), p);
                }
            });
        }

        // Thread B: tries to switch (some may fail for not-yet-registered)
        {
            let reg = Arc::clone(&reg);
            s.spawn(move || {
                for i in 0..50 {
                    let _ = reg.set_active(&format!("dyn_{i}"));
                    let _ = reg.set_active(STANDARD_POLICY);
                }
            });
        }

        // Thread C: concurrent reads
        {
            let reg = Arc::clone(&reg);
            s.spawn(move || {
                for _ in 0..200 {
                    let _ = reg.active_name();
                    let _ = reg.active_config();
                    let _ = reg.list();
                }
            });
        }
    });

    assert!(reg.list().len() >= 2); // at least standard + some dynamic
}

// ============================================================================
// 4. Default Policy Matches Hardcoded Constants
// ============================================================================

#[test]
fn default_conformal_config_matches_hardcoded() {
    use ftui_runtime::conformal_predictor::ConformalConfig;

    let policy = PolicyConfig::default();
    let from_policy = policy.to_conformal_config();
    let hardcoded = ConformalConfig::default();

    assert!((from_policy.alpha - hardcoded.alpha).abs() < f64::EPSILON);
    assert_eq!(from_policy.min_samples, hardcoded.min_samples);
    assert_eq!(from_policy.window_size, hardcoded.window_size);
    assert!((from_policy.q_default - hardcoded.q_default).abs() < f64::EPSILON);
}

#[test]
fn default_frame_guard_config_matches_hardcoded() {
    use ftui_runtime::conformal_frame_guard::ConformalFrameGuardConfig;

    let policy = PolicyConfig::default();
    let from_policy = policy.to_frame_guard_config();
    let hardcoded = ConformalFrameGuardConfig::default();

    assert!((from_policy.fallback_budget_us - hardcoded.fallback_budget_us).abs() < f64::EPSILON);
    assert_eq!(from_policy.time_series_window, hardcoded.time_series_window);
    assert_eq!(
        from_policy.nonconformity_window,
        hardcoded.nonconformity_window
    );
}

#[test]
fn default_pid_gains_matches_hardcoded() {
    use ftui_render::budget::PidGains;

    let policy = PolicyConfig::default();
    let from_policy = policy.to_pid_gains();
    let hardcoded = PidGains::default();

    assert!((from_policy.kp - hardcoded.kp).abs() < f64::EPSILON);
    assert!((from_policy.ki - hardcoded.ki).abs() < f64::EPSILON);
    assert!((from_policy.kd - hardcoded.kd).abs() < f64::EPSILON);
    assert!((from_policy.integral_max - hardcoded.integral_max).abs() < f64::EPSILON);
}

#[test]
fn default_bocpd_config_matches_hardcoded() {
    use ftui_runtime::bocpd::BocpdConfig;

    let policy = PolicyConfig::default();
    let from_policy = policy.to_bocpd_config();
    let hardcoded = BocpdConfig::default();

    assert!((from_policy.mu_steady_ms - hardcoded.mu_steady_ms).abs() < f64::EPSILON);
    assert!((from_policy.mu_burst_ms - hardcoded.mu_burst_ms).abs() < f64::EPSILON);
    assert!((from_policy.hazard_lambda - hardcoded.hazard_lambda).abs() < f64::EPSILON);
    assert_eq!(from_policy.max_run_length, hardcoded.max_run_length);
}

#[test]
fn default_voi_config_matches_hardcoded() {
    use ftui_runtime::voi_sampling::VoiConfig;

    let policy = PolicyConfig::default();
    let from_policy = policy.to_voi_config();
    let hardcoded = VoiConfig::default();

    assert!((from_policy.alpha - hardcoded.alpha).abs() < f64::EPSILON);
    assert!((from_policy.prior_alpha - hardcoded.prior_alpha).abs() < f64::EPSILON);
    assert!((from_policy.sample_cost - hardcoded.sample_cost).abs() < f64::EPSILON);
    assert_eq!(from_policy.min_interval_ms, hardcoded.min_interval_ms);
    assert_eq!(from_policy.max_interval_ms, hardcoded.max_interval_ms);
}

#[test]
fn default_eprocess_budget_config_matches_hardcoded() {
    use ftui_render::budget::EProcessConfig;

    let policy = PolicyConfig::default();
    let from_policy = policy.to_eprocess_budget_config();
    let hardcoded = EProcessConfig::default();

    assert!((from_policy.lambda - hardcoded.lambda).abs() < f64::EPSILON);
    assert!((from_policy.alpha - hardcoded.alpha).abs() < f64::EPSILON);
    assert!((from_policy.beta - hardcoded.beta).abs() < f64::EPSILON);
    assert_eq!(from_policy.warmup_frames, hardcoded.warmup_frames);
}

#[test]
fn default_throttle_config_matches_hardcoded() {
    use ftui_runtime::eprocess_throttle::ThrottleConfig;

    let policy = PolicyConfig::default();
    let from_policy = policy.to_throttle_config();
    let hardcoded = ThrottleConfig::default();

    assert!((from_policy.alpha - hardcoded.alpha).abs() < f64::EPSILON);
    assert!((from_policy.mu_0 - hardcoded.mu_0).abs() < f64::EPSILON);
    assert_eq!(from_policy.hard_deadline_ms, hardcoded.hard_deadline_ms);
    assert_eq!(
        from_policy.min_observations_between,
        hardcoded.min_observations_between
    );
}

#[test]
fn default_cascade_config_matches_hardcoded() {
    use ftui_runtime::degradation_cascade::CascadeConfig;

    let policy = PolicyConfig::default();
    let from_policy = policy.to_cascade_config();
    let hardcoded = CascadeConfig::default();

    assert_eq!(from_policy.recovery_threshold, hardcoded.recovery_threshold);
    assert_eq!(from_policy.max_degradation, hardcoded.max_degradation);
    assert_eq!(from_policy.min_trigger_level, hardcoded.min_trigger_level);
}

#[test]
fn default_evidence_sink_config_matches_hardcoded() {
    use ftui_runtime::evidence_sink::EvidenceSinkConfig;

    let policy = PolicyConfig::default();
    let from_policy = policy.to_evidence_sink_config();
    let hardcoded = EvidenceSinkConfig::default();

    assert_eq!(from_policy.enabled, hardcoded.enabled);
    assert_eq!(from_policy.flush_on_write, hardcoded.flush_on_write);
}

#[test]
fn registry_default_policy_matches_default_config() {
    let reg = PolicyRegistry::new();
    let active = reg.active_config();
    let default = PolicyConfig::default();

    assert!((active.conformal.alpha - default.conformal.alpha).abs() < f64::EPSILON);
    assert_eq!(active.conformal.min_samples, default.conformal.min_samples);
    assert!((active.pid.kp - default.pid.kp).abs() < f64::EPSILON);
    assert_eq!(
        active.evidence.ledger_capacity,
        default.evidence.ledger_capacity
    );
}

// ============================================================================
// 5. Evidence Ledger Captures Policy Switch Events
// ============================================================================

#[test]
fn switch_event_contains_old_and_new() {
    let reg = PolicyRegistry::new();
    reg.register("custom", PolicyConfig::default()).unwrap();

    let event = reg.set_active("custom").unwrap();
    assert_eq!(event.old_name, STANDARD_POLICY);
    assert_eq!(event.new_name, "custom");
}

#[test]
fn switch_event_increments_id() {
    let reg = PolicyRegistry::new();
    reg.register("a", PolicyConfig::default()).unwrap();
    reg.register("b", PolicyConfig::default()).unwrap();

    let e1 = reg.set_active("a").unwrap();
    let e2 = reg.set_active("b").unwrap();
    let e3 = reg.set_active(STANDARD_POLICY).unwrap();

    assert_eq!(e1.switch_id, 0);
    assert_eq!(e2.switch_id, 1);
    assert_eq!(e3.switch_id, 2);
}

#[test]
fn switch_event_jsonl_format() {
    let reg = PolicyRegistry::new();
    reg.register("test_policy", PolicyConfig::default())
        .unwrap();

    let event = reg.set_active("test_policy").unwrap();
    let jsonl = event.to_jsonl();

    assert!(jsonl.contains("policy-switch-v1"));
    assert!(jsonl.contains("\"old\":\"standard\""));
    assert!(jsonl.contains("\"new\":\"test_policy\""));
    assert!(jsonl.contains("\"switch_id\":0"));

    // Must be valid JSON
    let parsed: serde_json::Value = serde_json::from_str(&jsonl).unwrap();
    assert_eq!(parsed["schema"], "policy-switch-v1");
    assert_eq!(parsed["old"], "standard");
    assert_eq!(parsed["new"], "test_policy");
}

#[test]
fn config_jsonl_format() {
    let config = PolicyConfig::default();
    let jsonl = config.to_jsonl();

    assert!(jsonl.contains("policy-config-v1"));
    assert!(jsonl.contains("conformal_alpha"));
    assert!(jsonl.contains("pid_kp"));

    // Must be valid JSON
    let parsed: serde_json::Value = serde_json::from_str(&jsonl).unwrap();
    assert_eq!(parsed["schema"], "policy-config-v1");
    assert!((parsed["conformal_alpha"].as_f64().unwrap() - 0.05).abs() < f64::EPSILON);
}

#[test]
fn switch_count_tracks_total() {
    let reg = PolicyRegistry::new();
    reg.register("x", PolicyConfig::default()).unwrap();

    assert_eq!(reg.switch_count(), 0);

    reg.set_active("x").unwrap();
    assert_eq!(reg.switch_count(), 1);

    reg.set_active(STANDARD_POLICY).unwrap();
    assert_eq!(reg.switch_count(), 2);
}

#[test]
fn failed_switch_does_not_increment_count() {
    let reg = PolicyRegistry::new();
    let before = reg.switch_count();
    let _ = reg.set_active("nonexistent");
    assert_eq!(reg.switch_count(), before);
}

#[test]
fn validation_prevents_bad_policy_registration() {
    let reg = PolicyRegistry::new();
    let mut bad = PolicyConfig::default();
    bad.conformal.alpha = 0.0;

    let err = reg.register("bad", bad).unwrap_err();
    assert!(matches!(err, PolicyRegistryError::ValidationFailed(_)));

    // Must not appear in list
    assert!(!reg.list().contains(&"bad".to_string()));
}

#[test]
fn switch_back_preserves_config() {
    let reg = PolicyRegistry::new();
    let mut aggressive = PolicyConfig::default();
    aggressive.conformal.alpha = 0.01;
    reg.register("aggressive", aggressive).unwrap();

    reg.set_active("aggressive").unwrap();
    assert!((reg.active_config().conformal.alpha - 0.01).abs() < f64::EPSILON);

    reg.set_active(STANDARD_POLICY).unwrap();
    assert!((reg.active_config().conformal.alpha - 0.05).abs() < f64::EPSILON);

    reg.set_active("aggressive").unwrap();
    assert!((reg.active_config().conformal.alpha - 0.01).abs() < f64::EPSILON);
}
