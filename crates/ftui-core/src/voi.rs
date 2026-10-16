#![forbid(unsafe_code)]

//! Shared Value-of-Information (VOI) primitives for Beta-Bernoulli posteriors.
//!
//! Provides the expected variance reduction formula used across both runtime
//! inline auto-height sampling (`ftui-runtime::voi_sampling`) and widget-level
//! remeasure policies (`ftui-widgets::virtualized::RemeasurePolicy`).

/// Upper bound on Beta distribution variance (achieved at Beta(0+, 0+) -> 0.25).
pub const VAR_MAX: f64 = 0.25;

/// Posterior mean of `Beta(alpha, beta)`: $\frac{\alpha}{\alpha + \beta}$.
#[inline]
#[must_use]
pub fn beta_mean(alpha: f64, beta: f64) -> f64 {
    let sum = alpha + beta;
    if sum <= 0.0 {
        return 0.5;
    }
    alpha / sum
}

/// Posterior variance of `Beta(alpha, beta)`:
/// $\frac{\alpha \beta}{(\alpha + \beta)^2 (\alpha + \beta + 1)}$.
#[inline]
#[must_use]
pub fn beta_variance(alpha: f64, beta: f64) -> f64 {
    let sum = alpha + beta;
    if sum <= 0.0 {
        return 0.0;
    }
    let var = (alpha * beta) / (sum * sum * (sum + 1.0));
    var.min(VAR_MAX)
}

/// Expected posterior variance after observing one additional Bernoulli trial
/// under the current `Beta(alpha, beta)` belief:
///
/// $$\mathbb{E}[\mathrm{Var}(\theta \mid X)] = p\,\mathrm{Var}(\alpha+1,\beta) + (1-p)\,\mathrm{Var}(\alpha,\beta+1)$$
#[inline]
#[must_use]
pub fn expected_variance_after(alpha: f64, beta: f64) -> f64 {
    let p = beta_mean(alpha, beta);
    let var_success = beta_variance(alpha + 1.0, beta);
    let var_failure = beta_variance(alpha, beta + 1.0);
    p * var_success + (1.0 - p) * var_failure
}

/// Value of Information (expected variance reduction) from one additional
/// Bernoulli observation under a `Beta(alpha, beta)` posterior:
///
/// $$\mathrm{VOI}(\alpha, \beta) = \mathrm{Var}(\alpha, \beta) - \mathbb{E}[\mathrm{Var}(\theta \mid X)] = \frac{\alpha\beta}{(\alpha+\beta)^2(\alpha+\beta+1)^2}$$
#[inline]
#[must_use]
pub fn beta_voi(alpha: f64, beta: f64) -> f64 {
    let alpha = alpha.max(1e-6);
    let beta = beta.max(1e-6);
    (beta_variance(alpha, beta) - expected_variance_after(alpha, beta)).max(0.0)
}

/// Conjugate Beta posterior over a Bernoulli drift/violation rate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BetaPosterior {
    /// Pseudo-count of observed height changes / violations ($\alpha > 0$).
    pub alpha: f64,
    /// Pseudo-count of observed stable measurements ($\beta > 0$).
    pub beta: f64,
}

impl Default for BetaPosterior {
    fn default() -> Self {
        Self {
            alpha: 1.0,
            beta: 9.0,
        }
    }
}

impl BetaPosterior {
    /// Construct a Beta posterior with given parameters (clamped to $> 0$ on use).
    #[must_use]
    pub const fn new(alpha: f64, beta: f64) -> Self {
        Self { alpha, beta }
    }

    /// Posterior mean $\mathbb{E}[\theta] = \frac{\alpha}{\alpha + \beta}$.
    #[inline]
    #[must_use]
    pub fn mean(self) -> f64 {
        beta_mean(self.alpha.max(1e-6), self.beta.max(1e-6))
    }

    /// Posterior variance $\mathrm{Var}(\theta)$.
    #[inline]
    #[must_use]
    pub fn variance(self) -> f64 {
        beta_variance(self.alpha.max(1e-6), self.beta.max(1e-6))
    }

    /// Expected posterior variance after observing one additional Bernoulli sample.
    #[inline]
    #[must_use]
    pub fn expected_variance_after_sample(self) -> f64 {
        expected_variance_after(self.alpha.max(1e-6), self.beta.max(1e-6))
    }

    /// Expected variance reduction from taking one sample now.
    #[inline]
    #[must_use]
    pub fn voi(self) -> f64 {
        beta_voi(self.alpha, self.beta)
    }

    /// Update the conjugate posterior with a Bernoulli observation (`changed = true`
    /// increments `alpha`, `changed = false` increments `beta`).
    #[inline]
    pub fn observe(&mut self, changed: bool) {
        if changed {
            self.alpha += 1.0;
        } else {
            self.beta += 1.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn beta_voi_decreases_as_evidence_accumulates() {
        let mut p = BetaPosterior::new(1.0, 1.0);
        let initial = p.voi();
        for _ in 0..20 {
            p.observe(false);
        }
        assert!(
            p.voi() < initial * 0.1,
            "expected VOI after 20 stable observations ({}) to be < 10% of initial ({initial})",
            p.voi()
        );
    }

    #[test]
    fn beta_voi_is_nonnegative() {
        for a in [0.1, 1.0, 5.0, 50.0, 500.0] {
            for b in [0.1, 1.0, 5.0, 50.0, 500.0] {
                let v = beta_voi(a, b);
                assert!(v >= 0.0 && v.is_finite(), "invalid VOI {v} at ({a}, {b})");
            }
        }
    }

    proptest! {
        #[test]
        fn proptest_voi_monotone_in_beta(
            alpha in 1u32..50,
            beta_offset1 in 0u32..500,
            beta_step in 1u32..500,
        ) {
            // For Beta(alpha, beta), VOI(alpha, beta) = alpha * beta / ((a+b)^2 * (a+b+1)^2)
            // is strictly decreasing in beta once beta >= ceil(alpha / 3).
            let min_beta = alpha.div_ceil(3).max(1);
            let beta1 = min_beta + beta_offset1;
            let beta2 = beta1 + beta_step;
            let v1 = beta_voi(f64::from(alpha), f64::from(beta1));
            let v2 = beta_voi(f64::from(alpha), f64::from(beta2));
            prop_assert!(
                v2 <= v1 + 1e-15,
                "VOI not monotone decreasing for alpha={alpha}, beta1={beta1} ({v1}) -> beta2={beta2} ({v2})"
            );
        }
    }
}
