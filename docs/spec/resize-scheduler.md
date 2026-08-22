# Resize Scheduler — Formal Model (Alien Artifact)

This spec defines the mathematically principled resize scheduler for
**Core Responsive Reflow** (`bd-1rz0.2`). It provides explicit priors,
loss tradeoffs, decision rules, and anytime-valid change detection for
resize storms.

---

## 1) Goals
- **Deterministic** scheduling decisions under fixed inputs.
- **Explainable** decision rule with explicit loss matrix.
- **Anytime-valid** regime detection (burst vs steady) with safe stopping.
- **Bounded worst-case** latency and render cost.

## 2) Non-Goals
- Final parameter tuning (handled by dedicated tuning beads).
- UI/visualization (handled by HUD/telemetry beads).
- Per-terminal heuristics that are not explicitly documented.

---

## 3) Model Overview

We model resize events as a non-stationary stream with regime shifts:
- **Regime S**: steady (single resize or slow sequence)
- **Regime B**: burst/storm (rapid resize events)

At each time step `t`, the scheduler observes a resize event (or lack of one)
with features `x_t` such as:
- `dt` = time since last resize event
- `dv` = terminal area delta (abs change in rows/cols)
- `v` = recent event rate (EMA over window)

We maintain a posterior belief over regimes:

```
P(R_t = B | x_1..x_t) proportional to P(R_t = B) * product_i P(x_i | B)
P(R_t = S | x_1..x_t) proportional to P(R_t = S) * product_i P(x_i | S)
```

### 3.1) Assumptions + Priors
- **Base rate**: burst regimes are rare. Default prior
  `P(R_0 = B) = p_burst = 0.1`, `P(R_0 = S) = 0.9`.
- **Regime stickiness**: once in a regime, it tends to persist for a
  short window. We model transitions with a simple 2-state HMM:

```
P(R_t = B | R_{t-1} = S) = h_SB
P(R_t = S | R_{t-1} = B) = h_BS
```

Recommended defaults:
- `h_SB = 0.08` (steady → burst)
- `h_BS = 0.20` (burst → steady)

These are tuning knobs; the spec requires they be explicit and logged
when changed.

### 3.2) Likelihood Model (Lightweight, Deterministic)
Use simple parametric models to keep runtime O(1) and deterministic:
- `dt` (time since last resize): Exponential, with
  `lambda_S < lambda_B` (bursts have smaller dt).
- `dv` (area delta): Laplace or log-normal, with
  heavier tails under burst mode.
- `v` (EMA rate): Gaussian around a regime mean.

The posterior update uses a likelihood ratio
`LR_t = P(x_t | B) / P(x_t | S)` and updates via Bayes.

---

## 4) Loss Matrix

We choose actions at each decision point:
- `render_now`: perform an immediate reflow/present
- `coalesce`: delay to coalesce more resize events
- `skip_frame`: drop a frame if in storm mode

Example loss matrix (tunable):

| Regime / Action | render_now | coalesce | skip_frame |
|----------------|------------|----------|------------|
| Steady (S)     | 0          | 15       | 30         |
| Burst (B)      | 10         | 1        | 1          |

Interpretation:
- In steady state, delaying or skipping is expensive (user sees lag).
- In bursts, coalescing and skipping are cheap and prevent overload.

---

## 5) Decision Rule (Bayes)

Given posterior `P(S)` and `P(B)` at time `t`, choose the action
with minimum expected loss:

```
E[L(action)] = L(action, S) * P(S) + L(action, B) * P(B)
choose action* = argmin_action E[L(action)]
```

### Safeguards
- Hard cap on maximum coalesce delay: `max_coalesce_ms`
- Hard cap on maximum skip rate: `max_skip_ratio`
- Always render if `elapsed_since_last_render > hard_deadline_ms`
- `forced` marks an SLA breach of WAITING work: an apply triggered by the
  hard deadline counts as forced only when a resize was already pending
  when the deadline elapsed. An isolated resize arriving after a quiet
  gap applies instantly with `forced = false`.

Recommended defaults (documented + logged):
- `max_coalesce_ms = 40`
- `hard_deadline_ms = 100`
- `max_skip_ratio = 0.5`
- `cooldown_frames = 3`

---

## 6) Change-Point Detection (BOCPD)

We use a **Bayesian Online Change-Point Detection** (BOCPD) component to
track regime transitions explicitly. The core latent variable is the
**run length** `r_t`: the number of steps since the last change point.

### Run-Length Recursion (Adams & MacKay)
Let `H` be the hazard function (probability of a change at any step).

```
P(r_t | x_1:t) ∝ Σ_{r_{t-1}} P(r_t | r_{t-1}) * P(x_t | r_{t-1}) * P(r_{t-1} | x_1:t-1)
```

Where
```
P(r_t = 0 | r_{t-1}) = H(r_{t-1})
P(r_t = r_{t-1} + 1 | r_{t-1}) = 1 - H(r_{t-1})
```

### Hazard Function
We use a constant hazard by default for determinism:
- `H(r) = h_bocpd`
- Recommended default: `h_bocpd = 0.08`

If we later want adaptive hazard (optional), it must be explicit and
logged.

### Likelihood Model for BOCPD
Use the same parametric form as in Section 3.2, but maintain **sufficient
stats per run length** (e.g., exponential rate for `dt`, Laplace scale
for `dv`). This keeps updates O(k) with a fixed truncation window.

### 6.1) Sufficient Stats (Deterministic, O(1) per run length)
We track minimal stats per run length to keep computation stable and cheap.
All updates are deterministic and bounded.

**`dt` (time between events)** — Exponential with Gamma prior.
- Prior: `lambda ~ Gamma(alpha0, beta0)`
- Stats per run length: `n_dt`, `sum_dt`
- MAP estimate: `lambda_hat = (alpha0 + n_dt - 1) / (beta0 + sum_dt)`
- Predictive likelihood (fast): `p(dt) = lambda_hat * exp(-lambda_hat * dt)`

**`dv` (area delta)** — Laplace with EMA scale.
- Track `mean_dv` via EMA and `mad_dv` = EMA of `|dv - mean_dv|`.
- Scale estimate: `b_hat = max(mad_dv, b_floor)`
- Likelihood: `p(dv) = (1 / (2 * b_hat)) * exp(-|dv - mean_dv| / b_hat)`

**`v` (event rate EMA)** — Gaussian with EMA mean/variance.
- Track `mu_v` and `var_v` via EMA (variance floor `var_floor`).
- Likelihood: `p(v) = Normal(mu_v, var_v)`

Defaults (documented + logged):
- `alpha0 = 2.0`, `beta0 = 0.5`
- `b_floor = 0.5`
- `var_floor = 1e-4`
- EMA decay `gamma = 0.1`

### 6.2) BOCPD Update (Log-Space, Truncated)
We maintain `log_runlen[r] = log P(r_t = r | x_1:t)` for `r in [0..R_max]`.

```
for r in 0..=R_max:
  log_pred[r] = log_likelihood(x_t | stats[r])

log_growth[r+1] = log_runlen[r] + log(1 - H(r)) + log_pred[r]
log_cp[0]       = logsumexp_r( log_runlen[r] + log(H(r)) + log_pred[r] )

log_runlen' = normalize( [log_cp[0], log_growth[1..]] )
```

Normalization is done via `logsumexp` to avoid underflow. We store only
`R_max + 1` entries to guarantee bounded cost.

### 6.3) Truncation + Pruning Strategy
To preserve determinism and O(R_max):
- Hard truncate at `R_max`.
- Optional top‑K pruning (if enabled) must be deterministic:
  - stable sort by `log_runlen` (tie‑break by smaller r)
  - renormalize after pruning

Recommended: disable top‑K and rely on `R_max = 200` unless profiling
shows need for pruning.

### 6.4) Change‑Point Signal
We compute:
- `p_change = P(r_t = 0 | x_1:t)`
- `conf_burst = P(B | x_1:t)`

Decision remains:
- If `conf_burst >= tau_burst` OR `p_change >= tau_change` ⇒ regime `B`
- Else ⇒ regime `S`

Recommended defaults:
- `tau_burst = 0.70`
- `tau_change = 0.55`

All thresholds must be logged in the evidence ledger.

### 6.5) BOCPD Evidence Ledger Fields
When BOCPD logging is enabled (`BocpdConfig::with_logging(true)`), each update
emits JSONL with:
- `schema_version`: `"bocpd-v1"`
- `event`: `"bocpd"`
- `p_burst`, `log_bf`, `obs_ms`, `regime`
- `ll_steady`, `ll_burst`
- `runlen_mean`, `runlen_var`, `runlen_mode`, `runlen_p95`, `runlen_tail`
- `delay_ms` (nullable), `forced_deadline` (nullable)
- `n_obs`

`delay_ms` / `forced_deadline` are populated when the coalescer supplies
decision context for the current update.

---

## 7) Control-Theoretic Frame Pacing (PID/PI)

We treat **inter-render interval** as a control variable. The scheduler
produces a **target interval** `T_target` (from regime + loss rule), and
the controller adjusts coalescing/skip decisions to track it smoothly.

### Control Signal
Let `T_actual` be the observed time between renders. Define:

```
error_t = T_target - T_actual
integral_t = clamp(integral_{t-1} + error_t, I_min, I_max)
derivative_t = (error_t - error_{t-1}) / dt
u_t = Kp * error_t + Ki * integral_t + Kd * derivative_t
```

Mapping:
- `u_t > 0` ⇒ increase coalesce delay (slow down)
- `u_t < 0` ⇒ decrease coalesce delay / render sooner

We default to **PI** (set `Kd = 0`) unless instability is observed.

### Stability + Anti-Windup
- Clamp `u_t` to `[-U_max, U_max]`
- Clamp integral term to `[I_min, I_max]`
- Freeze integral when `storm_mode = false`
- Reset integral on regime transitions

### Recommended Defaults
- `Kp = 0.6`, `Ki = 0.2`, `Kd = 0.0`
- `U_max = 30ms`, `I_min = -50ms`, `I_max = 50ms`

These are tuning knobs; adjustments must be logged.

### Metrics to Log (JSONL)
- `T_target`, `T_actual`, `error_t`
- `Kp`, `Ki`, `Kd`
- `integral_t`, `derivative_t`, `u_t`
- `coalesce_delay_ms`
- `overshoot_ms`, `settle_time_ms`

---

## 8) Anytime-Valid Detection (e-process)

We maintain an **e-value** to detect storm regimes with **optional stopping**
guarantees (i.e., we can stop or trigger at any time without inflating
false‑alarm risk).

```
Initialize e_0 = 1
For each event t:
  update e_t = e_{t-1} * f(x_t) / g(x_t)
```

Where `f` is the likelihood under storm, `g` under steady. Under the null
(steady regime), `E[e_t] <= 1` for all `t`. Thus, **anytime-valid** detection:

```
Trigger storm if e_t >= 1 / alpha
```

This yields a false‑alarm bound `<= alpha`, **regardless of stopping time**.

### 8.1) Log‑Space Update (Deterministic + Stable)
To avoid underflow/overflow, we compute in log‑space:

```
log_e_0 = 0
log_lr_t = log f(x_t) - log g(x_t)
log_e_t = clamp(log_e_{t-1} + log_lr_t, log_e_min, log_e_max)
e_t = exp(log_e_t)
```

Where `clamp` is deterministic and `log_e_max` prevents numeric blow‑ups.
We also **clip likelihoods** to floors:

```
f(x_t) = max(f(x_t), f_floor)
g(x_t) = max(g(x_t), g_floor)
```

This keeps `log_lr_t` bounded and deterministic across platforms.

### 8.2) Decision Rule + Hysteresis
We treat the e‑value as **sufficient evidence** for storm mode:

- Enter storm if `log_e_t >= log(1/alpha)`.
- Exit storm only after `log_e_t <= log(e_exit)` for `cooldown_frames`.

We **do not** reset `e_t` on exit. If we want decay, it must be explicit and
logged as `e_decay_lambda`.

### 8.3) Integration with BOCPD + Posterior
Storm mode can be triggered by any of:

```
conf_burst >= tau_burst  OR
p_change >= tau_change   OR
log_e_t >= log(1/alpha)
```

The e‑process provides **anytime validity**, while BOCPD/posterior provide
model‑based context. If they disagree, the scheduler logs the **evidence
ledger** fields below to justify the action.

### 8.4) Evidence Ledger Fields (Required)
Per decision, log:

- `log_e`, `e_value`
- `log_lr_t`, `f_floor`, `g_floor`
- `alpha`, `e_exit`, `log_e_max`, `log_e_min`
- `storm_e_triggered` (bool)

These fields make the decision rule explainable and auditable.

### Recommended Defaults
- `alpha = 0.05`
- `log_e_max = 20`, `log_e_min = -20`
- `f_floor = 1e-8`, `g_floor = 1e-8`
- `storm_mode` entered when `e_t >= 20` (i.e., `log_e >= log(20)`)
- `storm_mode` exited when `e_t <= 1` for `cooldown_frames`

### 8.5) Tests / Validation (Anytime‑Valid)
- **Optional stopping**: simulate variable‑length sequences under steady
  and verify empirical false‑alarm `<= alpha`.
- **Adversarial orderings**: permute steady sequences; decision must remain
  bounded and deterministic.
- **Monotonic evidence**: if `log_lr_t` is consistently positive, `log_e_t`
  should be non‑decreasing until it hits `log_e_max`.

### 8.6) Runtime Control-Policy Integration

The resize scheduler is one contributor to the broader runtime control plane; it
does not get to redefine service semantics on its own.

- Resize coalescing belongs to the `visible_coalescible` work class from the
  runtime control contract. It may degrade cadence, but it may not weaken
  terminal safety, accepted input ordering, shutdown visibility, or evidence
  continuity.
- In `healthy` mode, resize decisions may coalesce locally but must not claim
  degraded service. Repeated forced-deadline applies or persistent burst-mode
  residency are pressure signals that feed the runtime-mode classifier.
- In `stressed` mode, the scheduler may stay in burst mode longer, but
  latest-wins semantics and the hard deadline remain strict.
- In `degraded` mode, `skip_frame` is permitted only as an explicit,
  reason-coded work-disposition choice paired with the active runtime mode and
  degradation tier. It is never an invisible optimization.
- Any resize decision that forces an apply, drops a frame, or prolongs burst
  residency must emit enough evidence for the HUD and runtime-mode ledger to
  surface `pressure_class`, `coalescing_state`, `work_disposition`, and
  `reason_code`.

---

## 9) Pseudocode

```
state:
  last_render_ts
  last_event_ts
  posterior {P(S), P(B)}
  e_value
  storm_mode

on_resize_event(x_t):
  update_posterior(x_t)
  update_bocpd(x_t) -> p_change
  update_e_value(x_t)

  conf_burst = P(B)
  regime = if conf_burst >= tau_burst || p_change >= tau_change { B } else { S }

  if e_value >= 1/alpha:
    storm_mode = true

  if storm_mode and e_value <= 1:
    storm_mode = false after cooldown

  T_target = target_interval(regime)
  u_t = pid.update(T_target, T_actual)
  coalesce_delay = clamp(base_delay + u_t, 0, max_coalesce_ms)

  if now - last_render_ts > hard_deadline_ms:
    action = render_now
  else:
    action = argmin_expected_loss(P(S), P(B))

  if action == render_now:
    render()
  else if action == coalesce:
    schedule_coalesce_deadline(coalesce_delay)
  else if action == skip_frame:
    record_skip()
```

---

## 10) Invariants
- **Determinism**: given identical event stream and seed, decisions identical.
- **Bounded latency**: `hard_deadline_ms` guarantees max wait in steady state.
- **Bounded skip rate**: `skip_ratio <= max_skip_ratio`.
- **Explainability**: every decision logs its expected loss components.

---

## 11) Failure Modes + Evidence Ledger

### Failure Modes
- Posterior collapse (P(S) ~ 0 or P(B) ~ 0) due to bad priors.
- Storm misclassification leading to excessive skips.
- Coalesce starvation (no render for too long).

### Evidence Ledger (per decision)
When resize decision logging is enabled (`CoalescerConfig::with_logging(true)`),
the coalescer emits JSONL lines to the evidence sink:

**Event: `config` (once per run)**
- `steady_delay_ms`, `burst_delay_ms`, `hard_deadline_ms`
- `burst_enter_rate`, `burst_exit_rate`
- `cooldown_frames`, `rate_window_size`
- `logging_enabled`

**Event: `decision` (each decision)**
- `idx`, `elapsed_ms`, `dt_ms`, `event_rate`
- `regime` (`steady` | `burst`)
- `action` (`apply` | `apply_forced` | `apply_immediate` | `coalesce` | `skip_same_size`)
- `pending_w`, `pending_h`, `applied_w`, `applied_h`
- `time_since_render_ms`, `coalesce_ms`, `forced`

**Event: `decision_evidence` (optional)**
- `log_bayes_factor` (float or `"inf"`)
- `regime_contribution`, `timing_contribution`, `rate_contribution`
- `explanation`

If BOCPD is enabled, a `bocpd` event is also emitted per update
(`schema_version: "bocpd-v1"`; see §6.5).

---

## 12) Tests / Validation

### Property Tests
- Determinism under fixed seed
- Monotonicity: increasing `v` should not reduce `P(B)`
- Deadlines always enforce `render_now`

### Simulation Harness
- Synthetic streams: steady, burst, alternating, noise
- Assert skip ratio and max latency bounds
- BOCPD: inject change points and verify `p_change` spikes near the ground truth
- PID/PI: step change in `T_target`, verify bounded overshoot and settling time

### E2E Logs
JSONL fields include:
- `event` (`config` | `decision` | `decision_evidence` | `bocpd`)
- `idx`, `elapsed_ms`, `dt_ms`, `event_rate`
- `regime`, `action`, `forced`
- `time_since_render_ms`, `coalesce_ms`
- BOCPD fields from §6.5 when enabled

---

## 13) Integration Notes
- This model feeds bd-1rz0.2.1 (BOCPD), bd-1rz0.2.2 (PID pacing), and
  bd-1rz0.2.3 (anytime-valid decision rule).
- The implementation should live in `ftui-runtime` with minimal coupling.
- Use `RenderBudget` to enforce hard deadlines.

---

## 14) VOI Sampling Policy (bd-1rz0.28)

Resize telemetry can be expensive under storms. We therefore sample
latency measurements using a value-of-information (VOI) policy with
anytime-valid safety guarantees.

### Model + Priors
- Each resize measurement yields a violation indicator:
  `X_t = 1 ⇔ latency > target_latency_ms`.
- Prior: `p ~ Beta(α, β)` (default α=β=1).
- Posterior update: `α ← α + X_t`, `β ← β + (1−X_t)`.

### VOI (Expected Variance Reduction)
Let `Var[p]` be the Beta posterior variance. The expected variance after
one additional sample is:

```
E[Var | one sample] =
  p̂ · Var[Beta(α+1,β)] + (1−p̂) · Var[Beta(α,β+1)]
VOI = Var[p] − E[Var | one sample]
```

### E-Process Boundary Proximity
- Track wealth `W_t` using the same Bernoulli stream for anytime-valid control.
- Increase sampling when `log W_t` is near the threshold `log(1/α)`.

### Decision Rule (Explainable)
- Forced: if max interval exceeded ⇒ sample.
- Guard: if min interval not met ⇒ skip.
- Otherwise: sample iff `score ≥ cost`, where:

```
score = VOI × value_scale × (1 + boundary_weight × boundary_score)
boundary_score = 1 / (1 + |log W_t − log W*|)
```

Evidence ledger fields:
- `voi_gain`, `score`, `cost`, `log_bayes_factor`
- `posterior_mean`, `posterior_variance`
- `e_value`, `e_threshold`
- `events_since_sample`, `time_since_sample_ms`
- `reason_code` (`forced`, `min_interval`, `voi_ge_cost`, `voi_lt_cost`)

### Implementation
- `crates/ftui-runtime/src/voi_sampling.rs`
- Integrated into `ResizeSlaMonitor` via optional config.

### E2E Determinism
- Tests emit JSONL with `seed`, `decision`, and `checksum` fields.
- Deterministic mode uses `VOI_SEED`.
