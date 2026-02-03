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

### 6.5) BOCPD Evidence Ledger Fields
Add these fields to the per‑decision log:
- `r_max`, `p_change`, `top_runlen`, `runlen_entropy`
- `alpha0`, `beta0`, `lambda_hat`
- `mean_dv`, `mad_dv`, `b_hat`
- `mu_v`, `var_v`

### Regime Label + Confidence
We derive a regime label and confidence from the run-length posterior:
- **Change probability**: `p_change = P(r_t = 0 | x_1:t)`
- **Burst confidence**: `conf_burst = P(B | x_1:t)` (from model posterior)

Decision rule:
- If `conf_burst >= tau_burst` OR `p_change >= tau_change` → label `B`
- Else → label `S`

Recommended defaults:
- `tau_burst = 0.70`
- `tau_change = 0.55`

All thresholds must be logged in the evidence ledger.

### Truncation + Determinism
We cap run-length support to `R_max` to keep computation bounded.
Recommended: `R_max = 200` (≈ a few seconds at 60 Hz).

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

We maintain an e-value to detect storm regimes without invalidating
stopping rules:

```
Initialize e_0 = 1
For each event t:
  update e_t = e_{t-1} * f(x_t) / g(x_t)
```

Where `f` is the likelihood under storm, `g` under steady.
When `e_t >= 1/alpha`, we can safely declare storm mode (optional stopping
is valid).

### Recommended Defaults
- `alpha = 0.05`
- `storm_mode` entered when `e_t >= 20`
- `storm_mode` exited when `e_t <= 1` for `cooldown_frames`

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
Fields:
- `ts`, `dt`, `dv`, `v`
- `P(S)`, `P(B)`
- `p_change`, `conf_burst`
- `T_target`, `T_actual`, `u_t`
- `loss_render`, `loss_coalesce`, `loss_skip`
- `chosen_action`
- `e_value`, `storm_mode`
- `h_bocpd`, `R_max`, `tau_burst`, `tau_change`
- `deadline_ms`, `time_since_last_render`

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
- `event_idx`, `dt`, `dv`, `v`
- `P(S)`, `P(B)`
- `e_value`, `storm_mode`
- `action`, `latency_ms`, `skip_count`

---

## 13) Integration Notes
- This model feeds bd-1rz0.2.1 (BOCPD), bd-1rz0.2.2 (PID pacing), and
  bd-1rz0.2.3 (anytime-valid decision rule).
- The implementation should live in `ftui-runtime` with minimal coupling.
- Use `RenderBudget` to enforce hard deadlines.
