#![forbid(unsafe_code)]

//! Policy-as-data configuration for FrankenTUI decision controllers.
//!
//! Captures all tunable parameters across the decision stack as a single
//! [`PolicyConfig`] that can be loaded from TOML or JSON at startup, removing
//! the need for compile-time constant changes.
//!
//! # Loading
//!
//! ```toml
//! # ftui-policy.toml
//! [conformal]
//! alpha = 0.05
//! min_samples = 20
//!
//! [cascade]
//! recovery_threshold = 10
//! ```
//!
//! ```rust,ignore
//! let policy = PolicyConfig::from_toml_file("ftui-policy.toml")?;
//! let policy = PolicyConfig::from_json_str(json)?;
//! ```
//!
//! # Defaults
//!
//! Every field has a default that exactly matches the current hardcoded values
//! in each decision component, so `PolicyConfig::default()` produces the same
//! behavior as the existing code.

#[cfg(feature = "policy-config")]
use std::path::Path;

#[cfg(feature = "policy-config")]
use serde::{Deserialize, Serialize};

use crate::bocpd::BocpdConfig;
use crate::conformal_frame_guard::ConformalFrameGuardConfig;
use crate::conformal_predictor::ConformalConfig;
use crate::degradation_cascade::CascadeConfig;
use crate::eprocess_throttle::ThrottleConfig;
use crate::evidence_sink::{EvidenceSinkConfig, EvidenceSinkDestination};
use crate::voi_sampling::VoiConfig;
use ftui_render::budget::{DegradationLevel, EProcessConfig, PidGains};

#[cfg(feature = "policy-config")]
const STANDALONE_POLICY_TOML: &str = "ftui-policy.toml";
#[cfg(feature = "policy-config")]
const STANDALONE_POLICY_JSON: &str = "ftui-policy.json";
#[cfg(feature = "policy-config")]
const CARGO_MANIFEST_NAME: &str = "Cargo.toml";

// ---------------------------------------------------------------------------
// Top-level PolicyConfig
// ---------------------------------------------------------------------------

/// Top-level policy configuration for the FrankenTUI decision stack.
///
/// Groups every tunable parameter into a single struct that can be
/// loaded from TOML or JSON. All fields default to the values currently
/// hardcoded in the individual config structs.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "policy-config", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "policy-config", serde(default))]
pub struct PolicyConfig {
    /// Conformal predictor parameters.
    pub conformal: ConformalPolicyConfig,

    /// Conformal frame guard parameters.
    pub frame_guard: FrameGuardPolicyConfig,

    /// Degradation cascade parameters.
    pub cascade: CascadePolicyConfig,

    /// PID controller gains for budget control.
    pub pid: PidPolicyConfig,

    /// E-process sequential test parameters (budget controller).
    pub eprocess_budget: EProcessBudgetPolicyConfig,

    /// BOCPD changepoint detection parameters.
    pub bocpd: BocpdPolicyConfig,

    /// E-process throttle parameters (recomputation gating).
    pub eprocess_throttle: EProcessThrottlePolicyConfig,

    /// Value-of-information sampling parameters.
    pub voi: VoiPolicyConfig,

    /// Evidence logging parameters.
    pub evidence: EvidencePolicyConfig,
}

impl PolicyConfig {
    /// Load from a TOML string.
    #[cfg(feature = "policy-config")]
    pub fn from_toml_str(s: &str) -> Result<Self, PolicyConfigError> {
        let policy: Self = toml::from_str(s).map_err(PolicyConfigError::Toml)?;
        policy.validate_or_err()
    }

    /// Load from a TOML file on disk.
    #[cfg(feature = "policy-config")]
    pub fn from_toml_file(path: impl AsRef<Path>) -> Result<Self, PolicyConfigError> {
        let content = std::fs::read_to_string(path.as_ref()).map_err(PolicyConfigError::Io)?;
        Self::from_toml_str(&content)
    }

    /// Load from a JSON string.
    #[cfg(feature = "policy-config")]
    pub fn from_json_str(s: &str) -> Result<Self, PolicyConfigError> {
        let policy: Self = serde_json::from_str(s).map_err(PolicyConfigError::Json)?;
        policy.validate_or_err()
    }

    /// Load from a JSON file on disk.
    #[cfg(feature = "policy-config")]
    pub fn from_json_file(path: impl AsRef<Path>) -> Result<Self, PolicyConfigError> {
        let content = std::fs::read_to_string(path.as_ref()).map_err(PolicyConfigError::Io)?;
        Self::from_json_str(&content)
    }

    /// Load from a Cargo manifest that embeds policy config under
    /// `[package.metadata.ftui]`.
    #[cfg(feature = "policy-config")]
    pub fn from_cargo_toml_str(s: &str) -> Result<Self, PolicyConfigError> {
        let manifest: CargoManifestPolicyConfig =
            toml::from_str(s).map_err(PolicyConfigError::Toml)?;
        let policy = manifest
            .package
            .and_then(|package| package.metadata)
            .and_then(|metadata| metadata.ftui)
            .ok_or(PolicyConfigError::MissingMetadataSection(
                "[package.metadata.ftui]",
            ))?;
        policy.validate_or_err()
    }

    /// Load from a Cargo manifest file that embeds policy config under
    /// `[package.metadata.ftui]`.
    #[cfg(feature = "policy-config")]
    pub fn from_cargo_toml_file(path: impl AsRef<Path>) -> Result<Self, PolicyConfigError> {
        let content = std::fs::read_to_string(path.as_ref()).map_err(PolicyConfigError::Io)?;
        Self::from_cargo_toml_str(&content)
    }

    /// Discover policy config from a project directory.
    ///
    /// Precedence is:
    /// 1. `ftui-policy.toml`
    /// 2. `ftui-policy.json`
    /// 3. `Cargo.toml` `[package.metadata.ftui]`
    #[cfg(feature = "policy-config")]
    pub fn discover_in_dir(dir: impl AsRef<Path>) -> Result<Self, PolicyConfigError> {
        let dir = dir.as_ref();

        let standalone_toml = dir.join(STANDALONE_POLICY_TOML);
        if standalone_toml.is_file() {
            return Self::from_toml_file(standalone_toml);
        }

        let standalone_json = dir.join(STANDALONE_POLICY_JSON);
        if standalone_json.is_file() {
            return Self::from_json_file(standalone_json);
        }

        let cargo_manifest = dir.join(CARGO_MANIFEST_NAME);
        if cargo_manifest.is_file() {
            return Self::from_cargo_toml_file(cargo_manifest);
        }

        Err(PolicyConfigError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "no policy config found in {} (expected {}, {}, or {})",
                dir.display(),
                STANDALONE_POLICY_TOML,
                STANDALONE_POLICY_JSON,
                CARGO_MANIFEST_NAME
            ),
        )))
    }

    #[cfg(feature = "policy-config")]
    fn validate_or_err(self) -> Result<Self, PolicyConfigError> {
        let errors = self.validate();
        if errors.is_empty() {
            Ok(self)
        } else {
            Err(PolicyConfigError::Validation(errors))
        }
    }

    /// Validate all parameters are within acceptable ranges.
    ///
    /// Returns a list of validation errors. An empty list means the config
    /// is valid.
    #[must_use]
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();

        let conformal_alpha_finite =
            validate_finite_f64(&mut errors, "conformal.alpha", self.conformal.alpha);
        let _ = validate_finite_f64(&mut errors, "conformal.q_default", self.conformal.q_default);
        let frame_guard_fallback_budget_finite = validate_finite_f64(
            &mut errors,
            "frame_guard.fallback_budget_us",
            self.frame_guard.fallback_budget_us,
        );
        let pid_kp_finite = validate_finite_f64(&mut errors, "pid.kp", self.pid.kp);
        let _ = validate_finite_f64(&mut errors, "pid.ki", self.pid.ki);
        let _ = validate_finite_f64(&mut errors, "pid.kd", self.pid.kd);
        let pid_integral_max_finite =
            validate_finite_f64(&mut errors, "pid.integral_max", self.pid.integral_max);
        let _ = validate_finite_f64(
            &mut errors,
            "eprocess_budget.lambda",
            self.eprocess_budget.lambda,
        );
        let eprocess_budget_alpha_finite = validate_finite_f64(
            &mut errors,
            "eprocess_budget.alpha",
            self.eprocess_budget.alpha,
        );
        let _ = validate_finite_f64(
            &mut errors,
            "eprocess_budget.beta",
            self.eprocess_budget.beta,
        );
        let _ = validate_finite_f64(
            &mut errors,
            "eprocess_budget.sigma_ema_decay",
            self.eprocess_budget.sigma_ema_decay,
        );
        let _ = validate_finite_f64(
            &mut errors,
            "eprocess_budget.sigma_floor_ms",
            self.eprocess_budget.sigma_floor_ms,
        );
        let _ = validate_finite_f64(&mut errors, "bocpd.mu_steady_ms", self.bocpd.mu_steady_ms);
        let _ = validate_finite_f64(&mut errors, "bocpd.mu_burst_ms", self.bocpd.mu_burst_ms);
        let bocpd_hazard_lambda_finite =
            validate_finite_f64(&mut errors, "bocpd.hazard_lambda", self.bocpd.hazard_lambda);
        let _ = validate_finite_f64(
            &mut errors,
            "bocpd.steady_threshold",
            self.bocpd.steady_threshold,
        );
        let _ = validate_finite_f64(
            &mut errors,
            "bocpd.burst_threshold",
            self.bocpd.burst_threshold,
        );
        let _ = validate_finite_f64(&mut errors, "bocpd.burst_prior", self.bocpd.burst_prior);
        let _ = validate_finite_f64(
            &mut errors,
            "bocpd.min_observation_ms",
            self.bocpd.min_observation_ms,
        );
        let _ = validate_finite_f64(
            &mut errors,
            "bocpd.max_observation_ms",
            self.bocpd.max_observation_ms,
        );
        let eprocess_throttle_alpha_finite = validate_finite_f64(
            &mut errors,
            "eprocess_throttle.alpha",
            self.eprocess_throttle.alpha,
        );
        let _ = validate_finite_f64(
            &mut errors,
            "eprocess_throttle.mu_0",
            self.eprocess_throttle.mu_0,
        );
        let _ = validate_finite_f64(
            &mut errors,
            "eprocess_throttle.initial_lambda",
            self.eprocess_throttle.initial_lambda,
        );
        let _ = validate_finite_f64(
            &mut errors,
            "eprocess_throttle.grapa_eta",
            self.eprocess_throttle.grapa_eta,
        );
        let voi_alpha_finite = validate_finite_f64(&mut errors, "voi.alpha", self.voi.alpha);
        let _ = validate_finite_f64(&mut errors, "voi.prior_alpha", self.voi.prior_alpha);
        let _ = validate_finite_f64(&mut errors, "voi.prior_beta", self.voi.prior_beta);
        let _ = validate_finite_f64(&mut errors, "voi.mu_0", self.voi.mu_0);
        let _ = validate_finite_f64(&mut errors, "voi.lambda", self.voi.lambda);
        let _ = validate_finite_f64(&mut errors, "voi.value_scale", self.voi.value_scale);
        let _ = validate_finite_f64(&mut errors, "voi.boundary_weight", self.voi.boundary_weight);
        let voi_sample_cost_finite =
            validate_finite_f64(&mut errors, "voi.sample_cost", self.voi.sample_cost);

        // Conformal alpha must be in (0, 1)
        if conformal_alpha_finite && (self.conformal.alpha <= 0.0 || self.conformal.alpha >= 1.0) {
            errors.push(format!(
                "conformal.alpha must be in (0, 1), got {}",
                self.conformal.alpha
            ));
        }

        if self.conformal.min_samples == 0 {
            errors.push("conformal.min_samples must be > 0".into());
        }

        if self.cascade.min_trigger_level > self.cascade.max_degradation {
            errors.push(format!(
                "cascade.min_trigger_level ({:?}) cannot be strictly greater than cascade.max_degradation ({:?})",
                self.cascade.min_trigger_level, self.cascade.max_degradation
            ));
        }

        if self.conformal.window_size == 0 {
            errors.push("conformal.window_size must be > 0".into());
        }

        // Frame guard budget must be positive
        if frame_guard_fallback_budget_finite && self.frame_guard.fallback_budget_us <= 0.0 {
            errors.push(format!(
                "frame_guard.fallback_budget_us must be > 0, got {}",
                self.frame_guard.fallback_budget_us
            ));
        }

        // PID gains: kp must be non-negative
        if pid_kp_finite && self.pid.kp < 0.0 {
            errors.push(format!("pid.kp must be >= 0, got {}", self.pid.kp));
        }
        if pid_integral_max_finite && self.pid.integral_max <= 0.0 {
            errors.push(format!(
                "pid.integral_max must be > 0, got {}",
                self.pid.integral_max
            ));
        }

        // E-process alpha in (0, 1)
        if eprocess_budget_alpha_finite
            && (self.eprocess_budget.alpha <= 0.0 || self.eprocess_budget.alpha >= 1.0)
        {
            errors.push(format!(
                "eprocess_budget.alpha must be in (0, 1), got {}",
                self.eprocess_budget.alpha
            ));
        }

        // BOCPD hazard lambda must be positive
        if bocpd_hazard_lambda_finite && self.bocpd.hazard_lambda <= 0.0 {
            errors.push(format!(
                "bocpd.hazard_lambda must be > 0, got {}",
                self.bocpd.hazard_lambda
            ));
        }
        if self.bocpd.max_run_length == 0 {
            errors.push("bocpd.max_run_length must be > 0".into());
        }

        // E-process throttle alpha in (0, 1)
        if eprocess_throttle_alpha_finite
            && (self.eprocess_throttle.alpha <= 0.0 || self.eprocess_throttle.alpha >= 1.0)
        {
            errors.push(format!(
                "eprocess_throttle.alpha must be in (0, 1), got {}",
                self.eprocess_throttle.alpha
            ));
        }

        // VOI alpha in (0, 1)
        if voi_alpha_finite && (self.voi.alpha <= 0.0 || self.voi.alpha >= 1.0) {
            errors.push(format!(
                "voi.alpha must be in (0, 1), got {}",
                self.voi.alpha
            ));
        }

        if voi_sample_cost_finite && self.voi.sample_cost < 0.0 {
            errors.push(format!(
                "voi.sample_cost must be >= 0, got {}",
                self.voi.sample_cost
            ));
        }

        // Evidence ledger capacity must be positive
        if self.evidence.ledger_capacity == 0 {
            errors.push("evidence.ledger_capacity must be > 0".into());
        }

        errors
    }

    /// Build a [`ConformalConfig`] from this policy.
    #[must_use]
    pub fn to_conformal_config(&self) -> ConformalConfig {
        ConformalConfig {
            alpha: self.conformal.alpha,
            min_samples: self.conformal.min_samples,
            window_size: self.conformal.window_size,
            q_default: self.conformal.q_default,
        }
    }

    /// Build a [`ConformalFrameGuardConfig`] from this policy.
    #[must_use]
    pub fn to_frame_guard_config(&self) -> ConformalFrameGuardConfig {
        ConformalFrameGuardConfig {
            conformal: self.to_conformal_config(),
            fallback_budget_us: self.frame_guard.fallback_budget_us,
            time_series_window: self.frame_guard.time_series_window,
            nonconformity_window: self.frame_guard.nonconformity_window,
        }
    }

    /// Build a [`CascadeConfig`] from this policy.
    #[must_use]
    pub fn to_cascade_config(&self) -> CascadeConfig {
        CascadeConfig {
            guard: self.to_frame_guard_config(),
            recovery_threshold: self.cascade.recovery_threshold,
            max_degradation: self.cascade.max_degradation,
            min_trigger_level: self.cascade.min_trigger_level,
            degradation_floor: self.cascade.degradation_floor,
        }
    }

    /// Build [`PidGains`] from this policy.
    #[must_use]
    pub fn to_pid_gains(&self) -> PidGains {
        PidGains {
            kp: self.pid.kp,
            ki: self.pid.ki,
            kd: self.pid.kd,
            integral_max: self.pid.integral_max,
        }
    }

    /// Build an [`EProcessConfig`] (budget controller) from this policy.
    #[must_use]
    pub fn to_eprocess_budget_config(&self) -> EProcessConfig {
        EProcessConfig {
            lambda: self.eprocess_budget.lambda,
            alpha: self.eprocess_budget.alpha,
            beta: self.eprocess_budget.beta,
            sigma_ema_decay: self.eprocess_budget.sigma_ema_decay,
            sigma_floor_ms: self.eprocess_budget.sigma_floor_ms,
            warmup_frames: self.eprocess_budget.warmup_frames,
        }
    }

    /// Build a [`BocpdConfig`] from this policy.
    #[must_use]
    pub fn to_bocpd_config(&self) -> BocpdConfig {
        BocpdConfig {
            mu_steady_ms: self.bocpd.mu_steady_ms,
            mu_burst_ms: self.bocpd.mu_burst_ms,
            hazard_lambda: self.bocpd.hazard_lambda,
            max_run_length: self.bocpd.max_run_length,
            steady_threshold: self.bocpd.steady_threshold,
            burst_threshold: self.bocpd.burst_threshold,
            burst_prior: self.bocpd.burst_prior,
            min_observation_ms: self.bocpd.min_observation_ms,
            max_observation_ms: self.bocpd.max_observation_ms,
            enable_logging: self.bocpd.enable_logging,
        }
    }

    /// Build a [`ThrottleConfig`] from this policy.
    #[must_use]
    pub fn to_throttle_config(&self) -> ThrottleConfig {
        ThrottleConfig {
            alpha: self.eprocess_throttle.alpha,
            mu_0: self.eprocess_throttle.mu_0,
            initial_lambda: self.eprocess_throttle.initial_lambda,
            grapa_eta: self.eprocess_throttle.grapa_eta,
            hard_deadline_ms: self.eprocess_throttle.hard_deadline_ms,
            min_observations_between: self.eprocess_throttle.min_observations_between,
            rate_window_size: self.eprocess_throttle.rate_window_size,
            enable_logging: self.eprocess_throttle.enable_logging,
        }
    }

    /// Build a [`VoiConfig`] from this policy.
    #[must_use]
    pub fn to_voi_config(&self) -> VoiConfig {
        VoiConfig {
            alpha: self.voi.alpha,
            prior_alpha: self.voi.prior_alpha,
            prior_beta: self.voi.prior_beta,
            mu_0: self.voi.mu_0,
            lambda: self.voi.lambda,
            value_scale: self.voi.value_scale,
            boundary_weight: self.voi.boundary_weight,
            sample_cost: self.voi.sample_cost,
            min_interval_ms: self.voi.min_interval_ms,
            max_interval_ms: self.voi.max_interval_ms,
            min_interval_events: self.voi.min_interval_events,
            max_interval_events: self.voi.max_interval_events,
            enable_logging: self.voi.enable_logging,
            max_log_entries: self.voi.max_log_entries,
        }
    }

    /// Build an [`EvidenceSinkConfig`] from this policy.
    #[must_use]
    pub fn to_evidence_sink_config(&self) -> EvidenceSinkConfig {
        EvidenceSinkConfig {
            enabled: self.evidence.sink_enabled,
            destination: if let Some(ref path) = self.evidence.sink_file {
                EvidenceSinkDestination::File(path.into())
            } else {
                EvidenceSinkDestination::Stdout
            },
            flush_on_write: self.evidence.flush_on_write,
            max_bytes: crate::evidence_sink::DEFAULT_MAX_EVIDENCE_BYTES,
        }
    }

    /// Format as a JSONL line for structured logging.
    #[must_use]
    pub fn to_jsonl(&self) -> String {
        format!(
            r#"{{"schema":"policy-config-v1","conformal_alpha":{},"conformal_min_samples":{},"cascade_recovery_threshold":{},"pid_kp":{},"bocpd_hazard_lambda":{},"voi_alpha":{},"evidence_ledger_capacity":{}}}"#,
            self.conformal.alpha,
            self.conformal.min_samples,
            self.cascade.recovery_threshold,
            self.pid.kp,
            self.bocpd.hazard_lambda,
            self.voi.alpha,
            self.evidence.ledger_capacity,
        )
    }
}

fn validate_finite_f64(errors: &mut Vec<String>, field: &str, value: f64) -> bool {
    if value.is_finite() {
        true
    } else {
        errors.push(format!("{field} must be finite, got {value}"));
        false
    }
}

// ---------------------------------------------------------------------------
// Sub-configs (flat, serde-friendly)
// ---------------------------------------------------------------------------

/// Conformal predictor policy parameters.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "policy-config", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "policy-config", serde(default))]
pub struct ConformalPolicyConfig {
    /// Significance level for conformal prediction. Default: 0.05 (95% coverage).
    pub alpha: f64,
    /// Minimum calibration samples before using conformal intervals. Default: 20.
    pub min_samples: usize,
    /// Sliding window size for calibration data. Default: 256.
    pub window_size: usize,
    /// Default quantile fallback (µs) when bucket has no data. Default: 10 000.
    pub q_default: f64,
}

impl Default for ConformalPolicyConfig {
    fn default() -> Self {
        Self {
            alpha: 0.05,
            min_samples: 20,
            window_size: 256,
            q_default: 10_000.0,
        }
    }
}

/// Conformal frame guard policy parameters.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "policy-config", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "policy-config", serde(default))]
pub struct FrameGuardPolicyConfig {
    /// Fixed fallback budget threshold (µs) during warmup. Default: 16 000.
    pub fallback_budget_us: f64,
    /// Rolling window size for frame time tracking. Default: 512.
    pub time_series_window: usize,
    /// Rolling window size for nonconformity scores. Default: 256.
    pub nonconformity_window: usize,
}

impl Default for FrameGuardPolicyConfig {
    fn default() -> Self {
        Self {
            fallback_budget_us: 16_000.0,
            time_series_window: 512,
            nonconformity_window: 256,
        }
    }
}

/// Degradation cascade policy parameters.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "policy-config", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "policy-config", serde(default))]
pub struct CascadePolicyConfig {
    /// Consecutive within-budget frames before upgrading one level. Default: 10.
    pub recovery_threshold: u32,
    /// Maximum degradation level allowed. Default: SkipFrame.
    #[cfg_attr(
        feature = "policy-config",
        serde(
            serialize_with = "serialize_degradation_level",
            deserialize_with = "deserialize_degradation_level"
        )
    )]
    pub max_degradation: DegradationLevel,
    /// Minimum degradation level when first triggered. Default: SimpleBorders.
    #[cfg_attr(
        feature = "policy-config",
        serde(
            serialize_with = "serialize_degradation_level",
            deserialize_with = "deserialize_degradation_level"
        )
    )]
    pub min_trigger_level: DegradationLevel,
    /// Minimum quality floor: the cascade will never degrade past this level.
    ///
    /// Default: `SimpleBorders` — preserves readable text content, preventing
    /// escalation to `EssentialOnly`, `Skeleton`, or `SkipFrame` after
    /// transient focus/resize spikes.
    #[cfg_attr(
        feature = "policy-config",
        serde(
            serialize_with = "serialize_degradation_level",
            deserialize_with = "deserialize_degradation_level"
        )
    )]
    pub degradation_floor: DegradationLevel,
}

impl Default for CascadePolicyConfig {
    fn default() -> Self {
        Self {
            recovery_threshold: 10,
            max_degradation: DegradationLevel::SkipFrame,
            min_trigger_level: DegradationLevel::SimpleBorders,
            degradation_floor: DegradationLevel::SimpleBorders,
        }
    }
}

/// PID controller gains policy parameters.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "policy-config", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "policy-config", serde(default))]
pub struct PidPolicyConfig {
    /// Proportional gain. Default: 0.5.
    pub kp: f64,
    /// Integral gain. Default: 0.05.
    pub ki: f64,
    /// Derivative gain. Default: 0.2.
    pub kd: f64,
    /// Maximum integral accumulator. Default: 5.0.
    pub integral_max: f64,
}

impl Default for PidPolicyConfig {
    fn default() -> Self {
        Self {
            kp: 0.5,
            ki: 0.05,
            kd: 0.2,
            integral_max: 5.0,
        }
    }
}

/// E-process budget controller policy parameters.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "policy-config", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "policy-config", serde(default))]
pub struct EProcessBudgetPolicyConfig {
    /// Likelihood ratio scale. Default: 0.5.
    pub lambda: f64,
    /// Type-I error rate. Default: 0.05.
    pub alpha: f64,
    /// Wealth decay parameter. Default: 0.5.
    pub beta: f64,
    /// EMA decay for sigma estimation. Default: 0.9.
    pub sigma_ema_decay: f64,
    /// Floor for sigma estimation (ms). Default: 1.0.
    pub sigma_floor_ms: f64,
    /// Warmup frames before e-process is active. Default: 10.
    pub warmup_frames: u32,
}

impl Default for EProcessBudgetPolicyConfig {
    fn default() -> Self {
        Self {
            lambda: 0.5,
            alpha: 0.05,
            beta: 0.5,
            sigma_ema_decay: 0.9,
            sigma_floor_ms: 1.0,
            warmup_frames: 10,
        }
    }
}

/// BOCPD changepoint detection policy parameters.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "policy-config", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "policy-config", serde(default))]
pub struct BocpdPolicyConfig {
    /// Expected inter-event time during steady regime (ms). Default: 200.
    pub mu_steady_ms: f64,
    /// Expected inter-event time during burst regime (ms). Default: 20.
    pub mu_burst_ms: f64,
    /// Hazard rate (1/expected run length). Default: 50.
    pub hazard_lambda: f64,
    /// Maximum run length tracked. Default: 100.
    pub max_run_length: usize,
    /// Posterior threshold for steady regime. Default: 0.3.
    pub steady_threshold: f64,
    /// Posterior threshold for burst regime. Default: 0.7.
    pub burst_threshold: f64,
    /// Prior probability of burst regime. Default: 0.2.
    pub burst_prior: f64,
    /// Minimum observation value (ms). Default: 1.0.
    pub min_observation_ms: f64,
    /// Maximum observation value (ms). Default: 10 000.
    pub max_observation_ms: f64,
    /// Enable debug logging. Default: false.
    pub enable_logging: bool,
}

impl Default for BocpdPolicyConfig {
    fn default() -> Self {
        Self {
            mu_steady_ms: 200.0,
            mu_burst_ms: 20.0,
            hazard_lambda: 50.0,
            max_run_length: 100,
            steady_threshold: 0.3,
            burst_threshold: 0.7,
            burst_prior: 0.2,
            min_observation_ms: 1.0,
            max_observation_ms: 10_000.0,
            enable_logging: false,
        }
    }
}

/// E-process throttle (recomputation gating) policy parameters.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "policy-config", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "policy-config", serde(default))]
pub struct EProcessThrottlePolicyConfig {
    /// Type-I error rate. Default: 0.05.
    pub alpha: f64,
    /// Null hypothesis rate. Default: 0.1.
    pub mu_0: f64,
    /// Initial likelihood ratio scale. Default: 0.5.
    pub initial_lambda: f64,
    /// GraPa learning rate. Default: 0.1.
    pub grapa_eta: f64,
    /// Hard deadline between recomputations (ms). Default: 500.
    pub hard_deadline_ms: u64,
    /// Minimum observations between recomputations. Default: 8.
    pub min_observations_between: u64,
    /// Sliding window size for rate estimation. Default: 64.
    pub rate_window_size: usize,
    /// Enable debug logging. Default: false.
    pub enable_logging: bool,
}

impl Default for EProcessThrottlePolicyConfig {
    fn default() -> Self {
        Self {
            alpha: 0.05,
            mu_0: 0.1,
            initial_lambda: 0.5,
            grapa_eta: 0.1,
            hard_deadline_ms: 500,
            min_observations_between: 8,
            rate_window_size: 64,
            enable_logging: false,
        }
    }
}

/// VOI (value-of-information) sampling policy parameters.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "policy-config", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "policy-config", serde(default))]
pub struct VoiPolicyConfig {
    /// Significance level. Default: 0.05.
    pub alpha: f64,
    /// Beta prior alpha. Default: 1.0.
    pub prior_alpha: f64,
    /// Beta prior beta. Default: 1.0.
    pub prior_beta: f64,
    /// Null hypothesis mean. Default: 0.05.
    pub mu_0: f64,
    /// Likelihood ratio scale. Default: 0.5.
    pub lambda: f64,
    /// Value scaling factor. Default: 1.0.
    pub value_scale: f64,
    /// Weight for decision boundary proximity. Default: 1.0.
    pub boundary_weight: f64,
    /// Cost per sample. Default: 0.01.
    pub sample_cost: f64,
    /// Minimum interval between samples (ms). Default: 0.
    pub min_interval_ms: u64,
    /// Maximum interval between samples (ms). Default: 250.
    pub max_interval_ms: u64,
    /// Minimum interval between samples (events). Default: 0.
    pub min_interval_events: u64,
    /// Maximum interval between samples (events). Default: 20.
    pub max_interval_events: u64,
    /// Enable VOI debug logging. Default: false.
    pub enable_logging: bool,
    /// Maximum VOI log entries. Default: 2048.
    pub max_log_entries: usize,
}

impl Default for VoiPolicyConfig {
    fn default() -> Self {
        Self {
            alpha: 0.05,
            prior_alpha: 1.0,
            prior_beta: 1.0,
            mu_0: 0.05,
            lambda: 0.5,
            value_scale: 1.0,
            boundary_weight: 1.0,
            sample_cost: 0.01,
            min_interval_ms: 0,
            max_interval_ms: 250,
            min_interval_events: 0,
            max_interval_events: 20,
            enable_logging: false,
            max_log_entries: 2048,
        }
    }
}

/// Evidence logging policy parameters.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "policy-config", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "policy-config", serde(default))]
pub struct EvidencePolicyConfig {
    /// Capacity of the unified evidence ledger (ring buffer). Default: 1024.
    pub ledger_capacity: usize,
    /// Whether the evidence sink is enabled. Default: false.
    pub sink_enabled: bool,
    /// File path for evidence output; None → stdout. Default: None.
    pub sink_file: Option<String>,
    /// Flush after every write. Default: true.
    pub flush_on_write: bool,
}

impl Default for EvidencePolicyConfig {
    fn default() -> Self {
        Self {
            ledger_capacity: 1024,
            sink_enabled: false,
            sink_file: None,
            flush_on_write: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur when loading a policy configuration.
#[derive(Debug)]
pub enum PolicyConfigError {
    /// I/O error reading a file.
    Io(std::io::Error),
    /// Expected `[package.metadata.ftui]` section was not present in Cargo.toml.
    MissingMetadataSection(&'static str),
    /// TOML parse error.
    #[cfg(feature = "policy-config")]
    Toml(toml::de::Error),
    /// JSON parse error.
    #[cfg(feature = "policy-config")]
    Json(serde_json::Error),
    /// Validation errors.
    Validation(Vec<String>),
}

impl std::fmt::Display for PolicyConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::MissingMetadataSection(path) => {
                write!(f, "missing metadata section: {path}")
            }
            #[cfg(feature = "policy-config")]
            Self::Toml(e) => write!(f, "TOML parse error: {e}"),
            #[cfg(feature = "policy-config")]
            Self::Json(e) => write!(f, "JSON parse error: {e}"),
            Self::Validation(errors) => {
                write!(f, "validation errors: {}", errors.join("; "))
            }
        }
    }
}

impl std::error::Error for PolicyConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::MissingMetadataSection(_) => None,
            #[cfg(feature = "policy-config")]
            Self::Toml(e) => Some(e),
            #[cfg(feature = "policy-config")]
            Self::Json(e) => Some(e),
            Self::Validation(_) => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Serde helpers for DegradationLevel
// ---------------------------------------------------------------------------

#[cfg(feature = "policy-config")]
fn serialize_degradation_level<S>(
    level: &DegradationLevel,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let s = match level {
        DegradationLevel::Full => "full",
        DegradationLevel::SimpleBorders => "simple_borders",
        DegradationLevel::NoStyling => "no_styling",
        DegradationLevel::EssentialOnly => "essential_only",
        DegradationLevel::Skeleton => "skeleton",
        DegradationLevel::SkipFrame => "skip_frame",
    };
    serializer.serialize_str(s)
}

#[cfg(feature = "policy-config")]
fn deserialize_degradation_level<'de, D>(deserializer: D) -> Result<DegradationLevel, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    match s.as_str() {
        "full" | "Full" => Ok(DegradationLevel::Full),
        "simple_borders" | "SimpleBorders" => Ok(DegradationLevel::SimpleBorders),
        "no_styling" | "NoStyling" => Ok(DegradationLevel::NoStyling),
        "essential_only" | "EssentialOnly" => Ok(DegradationLevel::EssentialOnly),
        "skeleton" | "Skeleton" => Ok(DegradationLevel::Skeleton),
        "skip_frame" | "SkipFrame" => Ok(DegradationLevel::SkipFrame),
        other => Err(serde::de::Error::custom(format!(
            "unknown degradation level: {other}"
        ))),
    }
}

#[cfg(feature = "policy-config")]
#[derive(Debug, Deserialize)]
struct CargoManifestPolicyConfig {
    package: Option<CargoPackagePolicyConfig>,
}

#[cfg(feature = "policy-config")]
#[derive(Debug, Deserialize)]
struct CargoPackagePolicyConfig {
    metadata: Option<CargoMetadataPolicyConfig>,
}

#[cfg(feature = "policy-config")]
#[derive(Debug, Deserialize)]
struct CargoMetadataPolicyConfig {
    ftui: Option<PolicyConfig>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "policy-config")]
    fn tempdir() -> std::io::Result<tempfile::TempDir> {
        tempfile::Builder::new()
            .prefix("ftui-policy-")
            .disable_cleanup(true)
            .tempdir()
    }

    #[test]
    fn default_matches_component_defaults() {
        let policy = PolicyConfig::default();

        // Conformal
        let conformal = policy.to_conformal_config();
        let expected = ConformalConfig::default();
        assert_eq!(conformal.alpha, expected.alpha);
        assert_eq!(conformal.min_samples, expected.min_samples);
        assert_eq!(conformal.window_size, expected.window_size);
        assert_eq!(conformal.q_default, expected.q_default);

        // Frame guard
        let fg = policy.to_frame_guard_config();
        let expected_fg = ConformalFrameGuardConfig::default();
        assert_eq!(fg.fallback_budget_us, expected_fg.fallback_budget_us);
        assert_eq!(fg.time_series_window, expected_fg.time_series_window);
        assert_eq!(fg.nonconformity_window, expected_fg.nonconformity_window);

        // Cascade
        let cascade = policy.to_cascade_config();
        let expected_cc = CascadeConfig::default();
        assert_eq!(cascade.recovery_threshold, expected_cc.recovery_threshold);
        assert_eq!(cascade.max_degradation, expected_cc.max_degradation);
        assert_eq!(cascade.min_trigger_level, expected_cc.min_trigger_level);
        assert_eq!(cascade.degradation_floor, expected_cc.degradation_floor);

        // PID
        let pid = policy.to_pid_gains();
        let expected_pid = PidGains::default();
        assert_eq!(pid.kp, expected_pid.kp);
        assert_eq!(pid.ki, expected_pid.ki);
        assert_eq!(pid.kd, expected_pid.kd);
        assert_eq!(pid.integral_max, expected_pid.integral_max);

        // E-process budget
        let ep = policy.to_eprocess_budget_config();
        let expected_ep = EProcessConfig::default();
        assert_eq!(ep.lambda, expected_ep.lambda);
        assert_eq!(ep.alpha, expected_ep.alpha);
        assert_eq!(ep.warmup_frames, expected_ep.warmup_frames);

        // BOCPD
        let bocpd = policy.to_bocpd_config();
        let expected_bocpd = BocpdConfig::default();
        assert_eq!(bocpd.mu_steady_ms, expected_bocpd.mu_steady_ms);
        assert_eq!(bocpd.mu_burst_ms, expected_bocpd.mu_burst_ms);
        assert_eq!(bocpd.hazard_lambda, expected_bocpd.hazard_lambda);
        assert_eq!(bocpd.max_run_length, expected_bocpd.max_run_length);

        // Throttle
        let throttle = policy.to_throttle_config();
        let expected_throttle = ThrottleConfig::default();
        assert_eq!(throttle.alpha, expected_throttle.alpha);
        assert_eq!(throttle.mu_0, expected_throttle.mu_0);
        assert_eq!(
            throttle.hard_deadline_ms,
            expected_throttle.hard_deadline_ms
        );

        // VOI
        let voi = policy.to_voi_config();
        let expected_voi = VoiConfig::default();
        assert_eq!(voi.alpha, expected_voi.alpha);
        assert_eq!(voi.sample_cost, expected_voi.sample_cost);
        assert_eq!(voi.max_interval_ms, expected_voi.max_interval_ms);
    }

    #[test]
    fn default_validates_clean() {
        let errors = PolicyConfig::default().validate();
        assert!(errors.is_empty(), "default should validate: {errors:?}");
    }

    #[test]
    fn validate_catches_bad_alpha() {
        let mut policy = PolicyConfig::default();
        policy.conformal.alpha = 0.0;
        let errors = policy.validate();
        assert!(errors.iter().any(|e| e.contains("conformal.alpha")));
    }

    #[test]
    fn validate_catches_invalid_cascade_levels() {
        let mut policy = PolicyConfig::default();
        policy.cascade.min_trigger_level = ftui_render::budget::DegradationLevel::SkipFrame;
        policy.cascade.max_degradation = ftui_render::budget::DegradationLevel::SimpleBorders;
        let errors = policy.validate();
        assert!(
            errors
                .iter()
                .any(|e| e.contains("cascade.min_trigger_level"))
        );
    }

    #[test]
    fn validate_catches_negative_pid() {
        let mut policy = PolicyConfig::default();
        policy.pid.kp = -1.0;
        let errors = policy.validate();
        assert!(errors.iter().any(|e| e.contains("pid.kp")));
    }

    #[test]
    fn validate_catches_zero_min_samples() {
        let mut policy = PolicyConfig::default();
        policy.conformal.min_samples = 0;
        let errors = policy.validate();
        assert!(errors.iter().any(|e| e.contains("min_samples")));
    }

    #[test]
    fn validate_catches_zero_ledger_capacity() {
        let mut policy = PolicyConfig::default();
        policy.evidence.ledger_capacity = 0;
        let errors = policy.validate();
        assert!(errors.iter().any(|e| e.contains("ledger_capacity")));
    }

    #[test]
    fn validate_catches_bad_eprocess_alpha() {
        let mut policy = PolicyConfig::default();
        policy.eprocess_budget.alpha = 1.5;
        let errors = policy.validate();
        assert!(errors.iter().any(|e| e.contains("eprocess_budget.alpha")));
    }

    #[test]
    fn validate_catches_bad_voi_cost() {
        let mut policy = PolicyConfig::default();
        policy.voi.sample_cost = -0.5;
        let errors = policy.validate();
        assert!(errors.iter().any(|e| e.contains("voi.sample_cost")));
    }

    #[test]
    fn validate_catches_bad_bocpd_hazard() {
        let mut policy = PolicyConfig::default();
        policy.bocpd.hazard_lambda = -1.0;
        let errors = policy.validate();
        assert!(errors.iter().any(|e| e.contains("bocpd.hazard_lambda")));
    }

    #[test]
    fn validate_catches_bad_throttle_alpha() {
        let mut policy = PolicyConfig::default();
        policy.eprocess_throttle.alpha = 0.0;
        let errors = policy.validate();
        assert!(errors.iter().any(|e| e.contains("eprocess_throttle.alpha")));
    }

    #[test]
    fn to_jsonl_produces_valid_json() {
        let jsonl = PolicyConfig::default().to_jsonl();
        assert!(jsonl.starts_with('{'));
        assert!(jsonl.ends_with('}'));
        assert!(jsonl.contains("policy-config-v1"));
    }

    #[test]
    fn evidence_sink_config_stdout_default() {
        let policy = PolicyConfig::default();
        let sink = policy.to_evidence_sink_config();
        assert!(!sink.enabled);
        assert!(sink.flush_on_write);
        assert!(matches!(sink.destination, EvidenceSinkDestination::Stdout));
    }

    #[test]
    fn evidence_sink_config_file_path() {
        let mut policy = PolicyConfig::default();
        policy.evidence.sink_file = Some("/tmp/evidence.jsonl".into());
        let sink = policy.to_evidence_sink_config();
        assert!(matches!(sink.destination, EvidenceSinkDestination::File(_)));
    }

    #[test]
    fn partial_override_preserves_defaults() {
        // Simulate what TOML partial loading does: only override a few fields
        let mut policy = PolicyConfig::default();
        policy.conformal.alpha = 0.01;
        policy.cascade.recovery_threshold = 20;

        // Everything else should still be default
        assert_eq!(policy.conformal.min_samples, 20);
        assert_eq!(policy.conformal.window_size, 256);
        assert_eq!(policy.pid.kp, 0.5);
        assert_eq!(policy.bocpd.hazard_lambda, 50.0);

        // Overrides should be preserved
        assert_eq!(policy.conformal.alpha, 0.01);
        assert_eq!(policy.cascade.recovery_threshold, 20);
    }

    #[test]
    fn multiple_validation_errors_collected() {
        let mut policy = PolicyConfig::default();
        policy.conformal.alpha = 0.0;
        policy.pid.kp = -1.0;
        policy.evidence.ledger_capacity = 0;
        let errors = policy.validate();
        assert!(
            errors.len() >= 3,
            "should catch multiple errors: {errors:?}"
        );
    }

    #[cfg(feature = "policy-config")]
    #[test]
    fn from_toml_str_rejects_invalid_loaded_values() {
        let err = PolicyConfig::from_toml_str(
            r#"
            [conformal]
            alpha = 0.0
            "#,
        )
        .expect_err("invalid TOML policy should fail validation");

        assert!(matches!(err, PolicyConfigError::Validation(_)));
    }

    #[cfg(feature = "policy-config")]
    #[test]
    fn from_json_str_rejects_invalid_loaded_values() {
        let err = PolicyConfig::from_json_str(r#"{"conformal":{"alpha":1.2}}"#)
            .expect_err("invalid JSON policy should fail validation");

        assert!(matches!(err, PolicyConfigError::Validation(_)));
    }

    #[test]
    fn validate_rejects_non_finite_values() {
        let mut policy = PolicyConfig::default();
        policy.conformal.q_default = f64::NAN;
        policy.frame_guard.fallback_budget_us = f64::INFINITY;
        policy.pid.kp = f64::NEG_INFINITY;
        policy.voi.sample_cost = f64::NEG_INFINITY;

        let errors = policy.validate();
        assert!(
            errors
                .iter()
                .any(|error| error.contains("conformal.q_default must be finite")),
            "missing q_default finite error: {errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("frame_guard.fallback_budget_us must be finite")),
            "missing frame_guard finite error: {errors:?}"
        );
        assert_eq!(
            errors
                .iter()
                .filter(|error| error.contains("pid.kp"))
                .count(),
            1,
            "pid.kp should emit a single finite-value error: {errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("voi.sample_cost must be finite")),
            "missing sample_cost finite error: {errors:?}"
        );
    }

    #[cfg(feature = "policy-config")]
    #[test]
    fn from_toml_str_rejects_non_finite_loaded_values() {
        let err = PolicyConfig::from_toml_str(
            r#"
            [conformal]
            alpha = nan
            q_default = inf

            [frame_guard]
            fallback_budget_us = inf
            "#,
        )
        .expect_err("non-finite TOML policy should fail validation");

        match err {
            PolicyConfigError::Validation(errors) => {
                assert!(
                    errors
                        .iter()
                        .any(|error| error.contains("conformal.alpha must be finite")),
                    "missing conformal.alpha finite error: {errors:?}"
                );
                assert!(
                    errors
                        .iter()
                        .any(|error| error.contains("conformal.q_default must be finite")),
                    "missing conformal.q_default finite error: {errors:?}"
                );
                assert!(
                    errors.iter().any(
                        |error| error.contains("frame_guard.fallback_budget_us must be finite")
                    ),
                    "missing frame_guard.fallback_budget_us finite error: {errors:?}"
                );
            }
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[cfg(feature = "policy-config")]
    #[test]
    fn from_cargo_toml_str_extracts_embedded_policy() {
        let policy = PolicyConfig::from_cargo_toml_str(
            r#"
            [package]
            name = "demo"
            version = "0.1.0"

            [package.metadata.ftui.conformal]
            alpha = 0.01

            [package.metadata.ftui.cascade]
            recovery_threshold = 24
            "#,
        )
        .expect("embedded Cargo metadata should load");

        assert!((policy.conformal.alpha - 0.01).abs() < f64::EPSILON);
        assert_eq!(policy.cascade.recovery_threshold, 24);
        assert_eq!(policy.pid.kp, PidPolicyConfig::default().kp);
    }

    #[cfg(feature = "policy-config")]
    #[test]
    fn from_cargo_toml_str_requires_ftui_metadata_section() {
        let err = PolicyConfig::from_cargo_toml_str(
            r#"
            [package]
            name = "demo"
            version = "0.1.0"
            "#,
        )
        .expect_err("missing metadata section should fail");

        assert!(matches!(
            err,
            PolicyConfigError::MissingMetadataSection("[package.metadata.ftui]")
        ));
    }

    #[cfg(feature = "policy-config")]
    #[test]
    fn discover_in_dir_prefers_standalone_toml() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join(STANDALONE_POLICY_TOML),
            r#"
            [conformal]
            alpha = 0.03
            "#,
        )
        .expect("write standalone policy");
        std::fs::write(
            dir.path().join(CARGO_MANIFEST_NAME),
            r#"
            [package]
            name = "demo"
            version = "0.1.0"

            [package.metadata.ftui.conformal]
            alpha = 0.01
            "#,
        )
        .expect("write Cargo manifest");

        let policy = PolicyConfig::discover_in_dir(dir.path()).expect("discover TOML policy");
        assert!((policy.conformal.alpha - 0.03).abs() < f64::EPSILON);
    }

    #[cfg(feature = "policy-config")]
    #[test]
    fn discover_in_dir_prefers_json_over_cargo_manifest() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join(STANDALONE_POLICY_JSON),
            r#"{"cascade":{"recovery_threshold":17}}"#,
        )
        .expect("write standalone json");
        std::fs::write(
            dir.path().join(CARGO_MANIFEST_NAME),
            r#"
            [package]
            name = "demo"
            version = "0.1.0"

            [package.metadata.ftui.cascade]
            recovery_threshold = 33
            "#,
        )
        .expect("write Cargo manifest");

        let policy = PolicyConfig::discover_in_dir(dir.path()).expect("discover JSON policy");
        assert_eq!(policy.cascade.recovery_threshold, 17);
    }

    #[cfg(feature = "policy-config")]
    #[test]
    fn discover_in_dir_falls_back_to_cargo_manifest() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join(CARGO_MANIFEST_NAME),
            r#"
            [package]
            name = "demo"
            version = "0.1.0"

            [package.metadata.ftui.cascade]
            recovery_threshold = 33
            "#,
        )
        .expect("write Cargo manifest");

        let policy =
            PolicyConfig::discover_in_dir(dir.path()).expect("discover Cargo metadata policy");
        assert_eq!(policy.cascade.recovery_threshold, 33);
    }
}
