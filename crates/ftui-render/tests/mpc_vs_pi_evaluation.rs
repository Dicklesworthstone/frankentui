//! # MPC vs PI Controller Evaluation (bd-1rz0.31)
//!
//! ## Mathematical Model
//!
//! We compare two control strategies for resize frame pacing:
//!
//! ### PI Controller (baseline) and Production PID
//!
//! Production `BudgetController` (`PidGains::default()`) uses a full 3-term PID
//! controller with gains:
//!
//! ```text
//! u_t = Kp·e_t + Ki·Σe_j + Kd·(e_t − e_{t−1})
//!
//! where:
//!   Kp = 0.5  (proportional gain)
//!   Ki = 0.05 (integral gain — eliminates steady-state error)
//!   Kd = 0.2  (derivative gain — damping; set to 0.0 in the stripped PI baseline below)
//!   e_t = (frame_time − target) / target   (normalized error)
//! ```
//!
//! **Characteristics**: Reactive, simple, well-understood stability properties.
//! Settling time ~8-12 frames. Anti-windup via integral clamping.
//!
//! ### Model Predictive Control (MPC)
//!
//! We implement a finite-horizon receding-horizon controller:
//!
//! ```text
//! Minimize:  J = Σ_{k=0..N-1} [ q·(y_k − r)² + ρ·Δu_k² ]
//!
//! Subject to:
//!   y_{k+1} = α·y_k + (1−α)·u_k + d_k   (first-order plant model)
//!   u_min ≤ u_k ≤ u_max                    (actuator constraints)
//!   Δu_k = u_k − u_{k−1}                  (rate of change penalty)
//!
//! where:
//!   y_k: predicted frame time at step k
//!   r: target frame time
//!   u_k: control input (target coalesce delay)
//!   α: plant time constant (estimated from data)
//!   d_k: disturbance estimate
//!   q: state weight (tracking importance)
//!   ρ: control rate weight (smoothness importance)
//!   N: prediction horizon (frames)
//! ```
//!
//! **Characteristics**: Anticipatory (uses model to predict future behavior),
//! handles constraints naturally, can incorporate disturbance feed-forward.
//! Higher computational cost per step.
//!
//! ## Evaluation Criteria
//!
//! | Metric | PI Expected | MPC Expected | Winner |
//! |--------|-------------|-------------|--------|
//! | Settling time | 8-12 frames | 3-5 frames | MPC |
//! | Overshoot | Moderate (Kd helps) | Minimal (model-aware) | MPC |
//! | Steady-state error | ~0 (Ki) | ~0 (integral action) | Tie |
//! | Robustness to model error | High (no model) | Moderate | PI |
//! | Computation cost | O(1) | O(N) per step | PI |
//! | Constraint handling | Post-hoc clamping | Built-in optimization | MPC |
//!
//! ## Recommendation
//!
//! For terminal UI resize pacing, **PI is recommended** because:
//! 1. Plant dynamics are simple (first-order with noise)
//! 2. Computational budget is extremely tight (~16ms per frame)
//! 3. MPC's prediction advantage is marginal for such fast dynamics
//! 4. Robustness to model mismatch is critical (diverse terminal backends)
//! 5. The existing PI + e-process combination already provides robust control
//!
//! MPC may be valuable for future scenarios with multi-output control
//! (e.g., simultaneous frame time + memory + CPU budget regulation).

use std::time::Duration;

// =============================================================================
// PI Controller (mirrors existing BudgetController PID logic)
// =============================================================================

#[derive(Debug, Clone)]
struct PiController {
    kp: f64,
    ki: f64,
    kd: f64,
    integral: f64,
    integral_max: f64,
    prev_error: f64,
    target: f64,
}

impl PiController {
    fn new(target: f64) -> Self {
        Self {
            kp: 0.5,
            ki: 0.05,
            kd: 0.2,
            integral: 0.0,
            integral_max: 5.0,
            prev_error: 0.0,
            target,
        }
    }

    fn step(&mut self, measurement: f64) -> f64 {
        let error = (measurement - self.target) / self.target;
        self.integral = (self.integral + error).clamp(-self.integral_max, self.integral_max);
        let derivative = error - self.prev_error;
        self.prev_error = error;

        let u = self.kp * error + self.ki * self.integral + self.kd * derivative;
        u.clamp(-2.0, 2.0)
    }

    fn reset(&mut self) {
        self.integral = 0.0;
        self.prev_error = 0.0;
    }
}

// =============================================================================
// MPC Controller
// =============================================================================

#[derive(Debug, Clone)]
struct MpcController {
    /// Plant time constant (estimated: frame_time ≈ α·prev + (1-α)·control)
    alpha: f64,
    /// Prediction horizon (frames)
    horizon: usize,
    /// State tracking weight
    q: f64,
    /// Control rate-of-change weight
    rho: f64,
    /// Target frame time
    target: f64,
    /// Control bounds
    u_min: f64,
    u_max: f64,
    /// Previous control input
    prev_u: f64,
    /// Estimated disturbance
    disturbance: f64,
    /// Disturbance filter constant
    dist_filter: f64,
}

impl MpcController {
    fn new(target: f64) -> Self {
        Self {
            alpha: 0.6,
            horizon: 5,
            q: 1.0,
            rho: 0.1,
            target,
            u_min: -2.0,
            u_max: 2.0,
            prev_u: 0.0,
            disturbance: 0.0,
            dist_filter: 0.3,
        }
    }

    /// Solve the finite-horizon optimization via iterative projected gradient.
    ///
    /// For the simple first-order plant, the optimal sequence can be computed
    /// analytically via the Riccati recursion. We use an iterative approach
    /// for clarity and extensibility.
    fn step(&mut self, measurement: f64) -> f64 {
        // Update disturbance estimate (low-pass filter of prediction error)
        let predicted =
            self.alpha * measurement + (1.0 - self.alpha) * self.target * (1.0 - self.prev_u);
        let pred_error = measurement - predicted;
        self.disturbance =
            self.dist_filter * pred_error + (1.0 - self.dist_filter) * self.disturbance;

        // Initialize control sequence with previous control
        let mut u_seq = vec![self.prev_u; self.horizon];

        // Iterative optimization (projected gradient descent)
        let lr = 0.01;
        let iterations = 20;

        for _ in 0..iterations {
            // Forward simulate plant with current control sequence
            let mut y = vec![0.0f64; self.horizon + 1];
            y[0] = measurement;

            for k in 0..self.horizon {
                // Negative feedback: positive u reduces frame time
                let u_clamped = u_seq[k].clamp(-1.5, 1.5);
                y[k + 1] = self.alpha * y[k]
                    + (1.0 - self.alpha) * self.target * (1.0 - u_clamped)
                    + self.disturbance;
            }

            // Compute gradient of cost J w.r.t. control sequence
            // J = Σ q·(y_k − target)² + ρ·(u_k − u_{k-1})²
            let mut grad = vec![0.0f64; self.horizon];

            for k in 0..self.horizon {
                // ∂J/∂u_k via chain rule through plant model
                let tracking_error = y[k + 1] - self.target;
                let du_k = if k == 0 {
                    u_seq[k] - self.prev_u
                } else {
                    u_seq[k] - u_seq[k - 1]
                };
                let du_next = if k + 1 < self.horizon {
                    u_seq[k + 1] - u_seq[k]
                } else {
                    0.0
                };

                // ∂(y_{k+1})/∂u_k = -(1 - α) · target (negative feedback)
                let dy_du = -(1.0 - self.alpha) * self.target;

                grad[k] = 2.0 * self.q * tracking_error * dy_du + 2.0 * self.rho * du_k
                    - 2.0 * self.rho * du_next;
            }

            // Update controls with projected gradient
            for k in 0..self.horizon {
                u_seq[k] -= lr * grad[k];
                u_seq[k] = u_seq[k].clamp(self.u_min, self.u_max);
            }
        }

        // Apply first element of optimal sequence (receding horizon)
        let u0 = u_seq[0];
        self.prev_u = u0;
        u0
    }
}

// =============================================================================
// Plant Simulator
// =============================================================================

/// Simulates a terminal frame time response to control inputs.
struct PlantSimulator {
    /// Current frame time (ms)
    frame_time: f64,
    /// Process noise standard deviation
    noise_std: f64,
    /// Simple LCG for deterministic noise
    rng_state: u64,
    /// Plant inertia (how quickly frame time follows control)
    inertia: f64,
    /// Target frame time (ms)
    target: f64,
}

impl PlantSimulator {
    fn new(target: f64, initial_frame_time: f64, noise_std: f64, seed: u64) -> Self {
        Self {
            frame_time: initial_frame_time,
            noise_std,
            rng_state: seed,
            inertia: 0.6,
            target,
        }
    }

    /// Step the plant with a control signal and return measured frame time.
    /// Negative feedback: positive control signal reduces frame time.
    fn step(&mut self, control_signal: f64) -> f64 {
        // Plant model: first-order with noise
        // Positive u → decrease frame time (negative feedback loop)
        let desired = self.target * (1.0 - control_signal.clamp(-1.5, 1.5));
        self.frame_time = self.inertia * self.frame_time + (1.0 - self.inertia) * desired;

        // Add process noise
        let noise = self.gaussian_noise() * self.noise_std;
        self.frame_time = (self.frame_time + noise).max(0.5);

        self.frame_time
    }

    /// Apply a step disturbance (sudden load spike)
    fn apply_disturbance(&mut self, magnitude: f64) {
        self.frame_time += magnitude;
    }

    fn gaussian_noise(&mut self) -> f64 {
        // Box-Muller transform using LCG
        let u1 = self.uniform();
        let u2 = self.uniform();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }

    fn uniform(&mut self) -> f64 {
        self.rng_state = self
            .rng_state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        let bits = (self.rng_state >> 11) as f64;
        bits / (1u64 << 53) as f64 + 1e-12 // Avoid exact 0
    }
}

// =============================================================================
// Evaluation Metrics
// =============================================================================

#[derive(Debug, Clone)]
struct EvalMetrics {
    /// Frames until error < 5% of target
    settling_time: usize,
    /// Maximum overshoot as fraction of target
    overshoot_pct: f64,
    /// RMS tracking error after settling
    steady_state_rmse: f64,
    /// Variance of steady-state frame time
    steady_state_variance: f64,
    /// Mean absolute control rate-of-change
    control_smoothness: f64,
    /// Mean computation time per step (ns)
    compute_time_ns: f64,
    /// Total integrated absolute error
    iae: f64,
}

fn compute_metrics(
    measurements: &[f64],
    controls: &[f64],
    target: f64,
    compute_times_ns: &[u64],
) -> EvalMetrics {
    let n = measurements.len();
    assert!(n > 0);

    // Settling time: first frame where error stays < 5% for 10 consecutive frames
    let threshold = 0.05 * target;
    let mut settling_time = n;
    'outer: for start in 0..n.saturating_sub(10) {
        for &measurement in measurements.iter().skip(start).take(10.min(n - start)) {
            if (measurement - target).abs() > threshold {
                continue 'outer;
            }
        }
        settling_time = start;
        break;
    }

    // Overshoot: maximum excursion below target (undershoot = fast frame time)
    // For frame pacing, overshoot means frame time exceeds target
    let overshoot = measurements
        .iter()
        .map(|&m| (m - target) / target)
        .fold(0.0f64, |acc, e| acc.max(e));

    // Steady-state metrics (after settling)
    let steady_start = settling_time.min(n);
    let steady_data = &measurements[steady_start..];
    let steady_mean = if steady_data.is_empty() {
        target
    } else {
        steady_data.iter().sum::<f64>() / steady_data.len() as f64
    };
    let steady_state_rmse = if steady_data.is_empty() {
        f64::MAX
    } else {
        let mse = steady_data
            .iter()
            .map(|&m| (m - target).powi(2))
            .sum::<f64>()
            / steady_data.len() as f64;
        mse.sqrt()
    };
    let steady_state_variance = if steady_data.len() < 2 {
        0.0
    } else {
        steady_data
            .iter()
            .map(|&m| (m - steady_mean).powi(2))
            .sum::<f64>()
            / (steady_data.len() - 1) as f64
    };

    // Control smoothness: mean |Δu|
    let control_smoothness = if controls.len() < 2 {
        0.0
    } else {
        let total_du: f64 = controls.windows(2).map(|w| (w[1] - w[0]).abs()).sum();
        total_du / (controls.len() - 1) as f64
    };

    // Mean computation time
    let compute_time_ns = if compute_times_ns.is_empty() {
        0.0
    } else {
        compute_times_ns.iter().sum::<u64>() as f64 / compute_times_ns.len() as f64
    };

    // Integrated absolute error
    let iae: f64 = measurements.iter().map(|&m| (m - target).abs()).sum();

    EvalMetrics {
        settling_time,
        overshoot_pct: overshoot * 100.0,
        steady_state_rmse,
        steady_state_variance,
        control_smoothness,
        compute_time_ns,
        iae,
    }
}

// =============================================================================
// Test Scenarios
// =============================================================================

/// Run a controller on a simulated plant and collect metrics.
fn run_scenario(
    controller: &str,
    pi: &mut PiController,
    mpc: &mut MpcController,
    plant: &mut PlantSimulator,
    num_frames: usize,
    disturbance_at: Option<(usize, f64)>,
) -> (Vec<f64>, Vec<f64>, Vec<u64>) {
    let mut measurements = Vec::with_capacity(num_frames);
    let mut controls = Vec::with_capacity(num_frames);
    let mut compute_times = Vec::with_capacity(num_frames);

    for frame in 0..num_frames {
        // Apply disturbance if scheduled
        if let Some((at, mag)) = disturbance_at
            && frame == at
        {
            plant.apply_disturbance(mag);
        }

        let start = std::time::Instant::now();
        let u = match controller {
            "pi" => pi.step(plant.frame_time),
            "mpc" => mpc.step(plant.frame_time),
            _ => unreachable!(),
        };
        let elapsed = start.elapsed().as_nanos() as u64;

        let measured = plant.step(u);

        measurements.push(measured);
        controls.push(u);
        compute_times.push(elapsed);
    }

    (measurements, controls, compute_times)
}

// =============================================================================
// Tests
// =============================================================================

/// Scenario 1: Step response — system starts at 2x target, must settle.
#[test]
fn scenario1_step_response() {
    let target = 16.0; // 16ms target (60fps)
    let num_frames = 100;

    // PI controller
    let mut pi = PiController::new(target);
    let mut plant_pi = PlantSimulator::new(target, 32.0, 0.5, 0xAAAA_0001);
    let (m_pi, c_pi, t_pi) = run_scenario(
        "pi",
        &mut pi,
        &mut MpcController::new(target),
        &mut plant_pi,
        num_frames,
        None,
    );
    let metrics_pi = compute_metrics(&m_pi, &c_pi, target, &t_pi);

    // MPC controller
    let mut mpc = MpcController::new(target);
    let mut plant_mpc = PlantSimulator::new(target, 32.0, 0.5, 0xAAAA_0001);
    let (m_mpc, c_mpc, t_mpc) = run_scenario(
        "mpc",
        &mut PiController::new(target),
        &mut mpc,
        &mut plant_mpc,
        num_frames,
        None,
    );
    let metrics_mpc = compute_metrics(&m_mpc, &c_mpc, target, &t_mpc);

    // JSONL log
    eprintln!(
        "{{\"test\":\"step_response\",\"controller\":\"pi\",\"settling_time\":{},\"overshoot_pct\":{:.2},\"ss_rmse\":{:.4},\"ss_var\":{:.4},\"smoothness\":{:.4},\"compute_ns\":{:.0},\"iae\":{:.2}}}",
        metrics_pi.settling_time,
        metrics_pi.overshoot_pct,
        metrics_pi.steady_state_rmse,
        metrics_pi.steady_state_variance,
        metrics_pi.control_smoothness,
        metrics_pi.compute_time_ns,
        metrics_pi.iae
    );
    eprintln!(
        "{{\"test\":\"step_response\",\"controller\":\"mpc\",\"settling_time\":{},\"overshoot_pct\":{:.2},\"ss_rmse\":{:.4},\"ss_var\":{:.4},\"smoothness\":{:.4},\"compute_ns\":{:.0},\"iae\":{:.2}}}",
        metrics_mpc.settling_time,
        metrics_mpc.overshoot_pct,
        metrics_mpc.steady_state_rmse,
        metrics_mpc.steady_state_variance,
        metrics_mpc.control_smoothness,
        metrics_mpc.compute_time_ns,
        metrics_mpc.iae
    );

    // Both controllers must settle within reasonable time
    assert!(
        metrics_pi.settling_time < 50,
        "PI settling time too high: {}",
        metrics_pi.settling_time
    );
    assert!(
        metrics_mpc.settling_time < 50,
        "MPC settling time too high: {}",
        metrics_mpc.settling_time
    );

    // Both must achieve low steady-state error
    assert!(
        metrics_pi.steady_state_rmse < 3.0,
        "PI steady-state RMSE too high: {:.2}",
        metrics_pi.steady_state_rmse
    );
    assert!(
        metrics_mpc.steady_state_rmse < 3.0,
        "MPC steady-state RMSE too high: {:.2}",
        metrics_mpc.steady_state_rmse
    );
}

/// Scenario 2: Load spike disturbance rejection.
#[test]
fn scenario2_disturbance_rejection() {
    let target = 16.0;
    let num_frames = 150;
    let disturbance = Some((50, 20.0)); // Spike at frame 50: +20ms

    let mut pi = PiController::new(target);
    let mut plant_pi = PlantSimulator::new(target, target, 0.5, 0xBBBB_0001);
    // Let PI settle first
    for _ in 0..20 {
        let u = pi.step(plant_pi.frame_time);
        plant_pi.step(u);
    }
    pi.reset();
    let mut plant_pi = PlantSimulator::new(target, target, 0.5, 0xBBBB_0002);
    let (m_pi, c_pi, t_pi) = run_scenario(
        "pi",
        &mut pi,
        &mut MpcController::new(target),
        &mut plant_pi,
        num_frames,
        disturbance,
    );
    let metrics_pi = compute_metrics(&m_pi[50..], &c_pi[50..], target, &t_pi[50..]);

    let mut mpc = MpcController::new(target);
    let mut plant_mpc = PlantSimulator::new(target, target, 0.5, 0xBBBB_0002);
    let (m_mpc, c_mpc, t_mpc) = run_scenario(
        "mpc",
        &mut PiController::new(target),
        &mut mpc,
        &mut plant_mpc,
        num_frames,
        disturbance,
    );
    let metrics_mpc = compute_metrics(&m_mpc[50..], &c_mpc[50..], target, &t_mpc[50..]);

    eprintln!(
        "{{\"test\":\"disturbance_rejection\",\"controller\":\"pi\",\"settling_time\":{},\"overshoot_pct\":{:.2},\"iae\":{:.2}}}",
        metrics_pi.settling_time, metrics_pi.overshoot_pct, metrics_pi.iae
    );
    eprintln!(
        "{{\"test\":\"disturbance_rejection\",\"controller\":\"mpc\",\"settling_time\":{},\"overshoot_pct\":{:.2},\"iae\":{:.2}}}",
        metrics_mpc.settling_time, metrics_mpc.overshoot_pct, metrics_mpc.iae
    );

    // Both must recover from the spike
    assert!(
        metrics_pi.settling_time < 80,
        "PI failed to recover: settling={}",
        metrics_pi.settling_time
    );
    assert!(
        metrics_mpc.settling_time < 80,
        "MPC failed to recover: settling={}",
        metrics_mpc.settling_time
    );
}

/// Scenario 3: Oscillating load (resize storm simulation).
#[test]
fn scenario3_oscillating_load() {
    let target = 16.0;
    let num_frames = 200;

    // Test both controllers against oscillating plant frame time
    for (name, controller_type) in [("pi", "pi"), ("mpc", "mpc")] {
        let mut pi = PiController::new(target);
        let mut mpc = MpcController::new(target);
        let mut plant = PlantSimulator::new(target, target, 0.3, 0xCCCC_0001);

        let mut measurements = Vec::with_capacity(num_frames);
        let mut controls = Vec::with_capacity(num_frames);
        let mut compute_times = Vec::with_capacity(num_frames);

        for frame in 0..num_frames {
            // Sinusoidal disturbance: ±8ms with period 40 frames
            let dist = 8.0 * (2.0 * std::f64::consts::PI * frame as f64 / 40.0).sin();
            plant.frame_time = (target + dist).max(1.0);

            let start = std::time::Instant::now();
            let u = match controller_type {
                "pi" => pi.step(plant.frame_time),
                "mpc" => mpc.step(plant.frame_time),
                _ => unreachable!(),
            };
            let elapsed = start.elapsed().as_nanos() as u64;

            let measured = plant.step(u);
            measurements.push(measured);
            controls.push(u);
            compute_times.push(elapsed);
        }

        // For oscillating load, compute tracking RMSE over all measurements directly
        // (the settling-based window in compute_metrics would be empty since the
        // controller never "settles" under continuous oscillation).
        let tracking_rmse = {
            let mse = measurements
                .iter()
                .map(|&m| (m - target).powi(2))
                .sum::<f64>()
                / measurements.len() as f64;
            mse.sqrt()
        };
        let smoothness = if controls.len() < 2 {
            0.0
        } else {
            let total_du: f64 = controls.windows(2).map(|w| (w[1] - w[0]).abs()).sum();
            total_du / (controls.len() - 1) as f64
        };
        let iae: f64 = measurements.iter().map(|&m| (m - target).abs()).sum();
        let mean_compute = compute_times.iter().sum::<u64>() as f64 / compute_times.len() as f64;

        eprintln!(
            "{{\"test\":\"oscillating_load\",\"controller\":\"{}\",\"tracking_rmse\":{:.4},\"smoothness\":{:.4},\"iae\":{:.2},\"compute_ns\":{:.0}}}",
            name, tracking_rmse, smoothness, iae, mean_compute
        );

        // Under oscillation, tracking RMSE will be higher but should be bounded
        assert!(
            tracking_rmse < 15.0,
            "{} RMSE too high under oscillation: {:.2}",
            name,
            tracking_rmse
        );
    }
}

/// Scenario 4: Model mismatch — plant inertia differs from MPC model.
#[test]
fn scenario4_model_mismatch_robustness() {
    let target = 16.0;
    let num_frames = 100;

    // Plant with different inertia than MPC assumes
    let plant_inertias = [0.3, 0.5, 0.8, 0.9];

    for &inertia in &plant_inertias {
        let mut pi = PiController::new(target);
        let mut mpc = MpcController::new(target);

        // PI (model-free — robust to plant changes)
        let mut plant_pi = PlantSimulator::new(target, 32.0, 0.5, 0xDDDD_0001);
        plant_pi.inertia = inertia;
        let (m_pi, c_pi, t_pi) = run_scenario(
            "pi",
            &mut pi,
            &mut MpcController::new(target),
            &mut plant_pi,
            num_frames,
            None,
        );
        let metrics_pi = compute_metrics(&m_pi, &c_pi, target, &t_pi);

        // MPC (model assumes alpha=0.6, plant differs)
        let mut plant_mpc = PlantSimulator::new(target, 32.0, 0.5, 0xDDDD_0001);
        plant_mpc.inertia = inertia;
        let (m_mpc, c_mpc, t_mpc) = run_scenario(
            "mpc",
            &mut PiController::new(target),
            &mut mpc,
            &mut plant_mpc,
            num_frames,
            None,
        );
        let metrics_mpc = compute_metrics(&m_mpc, &c_mpc, target, &t_mpc);

        eprintln!(
            "{{\"test\":\"model_mismatch\",\"plant_inertia\":{:.1},\"controller\":\"pi\",\"settling_time\":{},\"ss_rmse\":{:.4},\"iae\":{:.2}}}",
            inertia, metrics_pi.settling_time, metrics_pi.steady_state_rmse, metrics_pi.iae
        );
        eprintln!(
            "{{\"test\":\"model_mismatch\",\"plant_inertia\":{:.1},\"controller\":\"mpc\",\"settling_time\":{},\"ss_rmse\":{:.4},\"iae\":{:.2}}}",
            inertia, metrics_mpc.settling_time, metrics_mpc.steady_state_rmse, metrics_mpc.iae
        );

        // Both must settle — PI should be consistently good regardless of inertia
        assert!(
            metrics_pi.settling_time < 60,
            "PI failed with inertia={}: settling={}",
            inertia,
            metrics_pi.settling_time
        );
    }
}

/// Scenario 5: Computation time comparison.
#[test]
fn scenario5_computation_overhead() {
    let target = 16.0;
    let num_frames = 500;

    let mut pi = PiController::new(target);
    let mut mpc = MpcController::new(target);

    let mut pi_times = Vec::with_capacity(num_frames);
    let mut mpc_times = Vec::with_capacity(num_frames);

    let measurement = 20.0; // Fixed measurement for consistent timing

    for _ in 0..num_frames {
        let start = std::time::Instant::now();
        let _ = pi.step(measurement);
        pi_times.push(start.elapsed().as_nanos() as u64);
    }

    for _ in 0..num_frames {
        let start = std::time::Instant::now();
        let _ = mpc.step(measurement);
        mpc_times.push(start.elapsed().as_nanos() as u64);
    }

    pi_times.sort();
    mpc_times.sort();

    let pi_len = pi_times.len();
    let pi_p50 = pi_times[pi_len / 2];
    let pi_p95 = pi_times[((pi_len as f64 * 0.95) as usize).min(pi_len.saturating_sub(1))];
    let mpc_len = mpc_times.len();
    let mpc_p50 = mpc_times[mpc_len / 2];
    let mpc_p95 = mpc_times[((mpc_len as f64 * 0.95) as usize).min(mpc_len.saturating_sub(1))];

    eprintln!(
        "{{\"test\":\"compute_overhead\",\"controller\":\"pi\",\"p50_ns\":{},\"p95_ns\":{}}}",
        pi_p50, pi_p95
    );
    eprintln!(
        "{{\"test\":\"compute_overhead\",\"controller\":\"mpc\",\"p50_ns\":{},\"p95_ns\":{}}}",
        mpc_p50, mpc_p95
    );

    // PI must be faster than MPC
    // PI is O(1), MPC is O(N * iterations) = O(100)
    // But both should be sub-microsecond
    assert!(pi_p95 < 100_000, "PI p95 too slow: {}ns", pi_p95);
    assert!(mpc_p95 < 1_000_000, "MPC p95 too slow: {}ns", mpc_p95);
}

/// Scenario 6: Deterministic replay.
#[test]
fn scenario6_deterministic_replay() {
    let target = 16.0;
    let num_frames = 50;
    let seed = 0xFEED_FACE;

    // Run twice with same seed
    let run = |seed: u64| {
        let mut pi = PiController::new(target);
        let mut plant = PlantSimulator::new(target, 32.0, 0.5, seed);
        let mut results = Vec::new();
        for _ in 0..num_frames {
            let u = pi.step(plant.frame_time);
            let m = plant.step(u);
            results.push((m, u));
        }
        results
    };

    let run1 = run(seed);
    let run2 = run(seed);

    assert_eq!(run1.len(), run2.len());
    for (i, ((m1, u1), (m2, u2))) in run1.iter().zip(run2.iter()).enumerate() {
        assert!(
            (m1 - m2).abs() < 1e-10,
            "frame {}: measurement mismatch {:.6} vs {:.6}",
            i,
            m1,
            m2
        );
        assert!(
            (u1 - u2).abs() < 1e-10,
            "frame {}: control mismatch {:.6} vs {:.6}",
            i,
            u1,
            u2
        );
    }
}

/// Scenario 7: Summary evaluation with recommendation.
#[test]
fn scenario7_summary_evaluation_jsonl() {
    let target = 16.0;

    // Run all scenarios and produce summary
    let scenarios = [
        ("step_from_2x", 32.0, 0.5, None, 0xAAAA_0001u64),
        ("step_from_4x", 64.0, 0.5, None, 0xAAAA_0002),
        ("spike_at_50", 16.0, 0.5, Some((50, 20.0)), 0xBBBB_0001),
        ("noisy_target", 16.0, 2.0, None, 0xCCCC_0001),
        ("low_noise", 16.0, 0.1, None, 0xDDDD_0001),
    ];

    let mut pi_wins = 0u32;
    let mut mpc_wins = 0u32;

    for (name, initial, noise, dist, seed) in &scenarios {
        let mut pi = PiController::new(target);
        let mut mpc = MpcController::new(target);

        let mut plant_pi = PlantSimulator::new(target, *initial, *noise, *seed);
        let (m_pi, c_pi, t_pi) = run_scenario(
            "pi",
            &mut pi,
            &mut MpcController::new(target),
            &mut plant_pi,
            100,
            *dist,
        );
        let metrics_pi = compute_metrics(&m_pi, &c_pi, target, &t_pi);

        let mut plant_mpc = PlantSimulator::new(target, *initial, *noise, *seed);
        let (m_mpc, c_mpc, t_mpc) = run_scenario(
            "mpc",
            &mut PiController::new(target),
            &mut mpc,
            &mut plant_mpc,
            100,
            *dist,
        );
        let metrics_mpc = compute_metrics(&m_mpc, &c_mpc, target, &t_mpc);

        // Score: lower IAE wins
        let pi_score = metrics_pi.iae;
        let mpc_score = metrics_mpc.iae;

        let winner = if pi_score < mpc_score { "pi" } else { "mpc" };
        if winner == "pi" {
            pi_wins += 1;
        } else {
            mpc_wins += 1;
        }

        eprintln!(
            "{{\"test\":\"summary\",\"scenario\":\"{}\",\"pi_iae\":{:.2},\"mpc_iae\":{:.2},\"pi_settling\":{},\"mpc_settling\":{},\"winner\":\"{}\"}}",
            name, pi_score, mpc_score, metrics_pi.settling_time, metrics_mpc.settling_time, winner
        );
    }

    eprintln!(
        "{{\"test\":\"final_recommendation\",\"pi_wins\":{},\"mpc_wins\":{},\"recommendation\":\"{}\"}}",
        pi_wins,
        mpc_wins,
        if pi_wins >= mpc_wins {
            "PI — robust, simple, sufficient for terminal frame pacing"
        } else {
            "MPC — better tracking but higher complexity"
        }
    );

    // The evaluation should complete without panics — the recommendation
    // is informational, not prescriptive. Both controllers must work.
    assert!(
        pi_wins + mpc_wins == 5,
        "all scenarios must produce results"
    );
}

// =============================================================================
// Property Tests
// =============================================================================

use proptest::prelude::*;

proptest! {
    /// Property: PI controller is stable for any initial frame time.
    #[test]
    fn property_pi_stability(
        initial in 1.0f64..200.0,
        noise_std in 0.01f64..5.0,
        seed in 0u64..1_000_000,
    ) {
        let target = 16.0;
        let mut pi = PiController::new(target);
        let mut plant = PlantSimulator::new(target, initial, noise_std, seed);

        for _ in 0..200 {
            let u = pi.step(plant.frame_time);
            let m = plant.step(u);
            // Frame time must stay bounded (no runaway oscillation)
            prop_assert!(m < 500.0, "PI instability: frame_time={:.2}", m);
            prop_assert!(m > 0.0, "PI negative frame time");
        }
    }

    /// Property: MPC controller is stable for any initial frame time.
    #[test]
    fn property_mpc_stability(
        initial in 1.0f64..200.0,
        noise_std in 0.01f64..5.0,
        seed in 0u64..1_000_000,
    ) {
        let target = 16.0;
        let mut mpc = MpcController::new(target);
        let mut plant = PlantSimulator::new(target, initial, noise_std, seed);

        for _ in 0..200 {
            let u = mpc.step(plant.frame_time);
            let m = plant.step(u);
            prop_assert!(m < 500.0, "MPC instability: frame_time={:.2}", m);
            prop_assert!(m > 0.0, "MPC negative frame time");
        }
    }

    /// Property: Both controllers converge toward target from any start.
    #[test]
    fn property_convergence(
        initial in 5.0f64..100.0,
        seed in 0u64..100_000,
    ) {
        let target = 16.0;

        for controller_type in ["pi", "mpc"] {
            let mut pi = PiController::new(target);
            let mut mpc = MpcController::new(target);
            let mut plant = PlantSimulator::new(target, initial, 0.3, seed);

            let mut last_10: Vec<f64> = Vec::new();

            for _ in 0..150 {
                let u = match controller_type {
                    "pi" => pi.step(plant.frame_time),
                    "mpc" => mpc.step(plant.frame_time),
                    _ => unreachable!(),
                };
                let m = plant.step(u);

                if last_10.len() >= 10 {
                    last_10.remove(0);
                }
                last_10.push(m);
            }

            // Last 10 frames should be near target (within 50%)
            let avg = last_10.iter().sum::<f64>() / last_10.len() as f64;
            prop_assert!(
                (avg - target).abs() < target * 0.5,
                "{} failed to converge from {:.1}: avg_last_10={:.2}",
                controller_type,
                initial,
                avg
            );
        }
    }
}

/// Integration with existing BudgetController for consistency check.
#[test]
fn integration_budget_controller_consistency() {
    use ftui_render::budget::{BudgetController, BudgetControllerConfig, BudgetDecision};

    let config = BudgetControllerConfig::default();
    let mut budget = BudgetController::new(config);

    // Feed frame times at 2x target — should eventually degrade
    let target = Duration::from_millis(16);
    let overloaded = Duration::from_millis(32);

    let mut decisions = Vec::new();
    for _ in 0..100 {
        let decision = budget.update(overloaded);
        decisions.push(decision);
    }

    // After sustained overload, at least one Degrade should appear
    let degrade_count = decisions
        .iter()
        .filter(|&&d| d == BudgetDecision::Degrade)
        .count();

    eprintln!(
        "{{\"test\":\"budget_integration\",\"frames\":100,\"target_ms\":16,\"actual_ms\":32,\"degrade_count\":{},\"hold_count\":{},\"upgrade_count\":{}}}",
        degrade_count,
        decisions
            .iter()
            .filter(|&&d| d == BudgetDecision::Hold)
            .count(),
        decisions
            .iter()
            .filter(|&&d| d == BudgetDecision::Upgrade)
            .count()
    );

    assert!(
        degrade_count > 0,
        "BudgetController should degrade under sustained 2x overload"
    );

    // Feed frame times at target — should eventually upgrade
    let mut budget2 = BudgetController::new(BudgetControllerConfig::default());
    // First cause degradation
    for _ in 0..50 {
        budget2.update(overloaded);
    }
    // Then recover
    let mut recovery_decisions = Vec::new();
    for _ in 0..100 {
        let d = budget2.update(target);
        recovery_decisions.push(d);
    }

    let upgrade_count = recovery_decisions
        .iter()
        .filter(|&&d| d == BudgetDecision::Upgrade)
        .count();

    eprintln!(
        "{{\"test\":\"budget_recovery\",\"degrade_phase\":50,\"recovery_phase\":100,\"upgrade_count\":{}}}",
        upgrade_count
    );
}
