//! Rough-path signatures for sequential trace feature extraction.
//!
//! Rough-path signatures are universal noncommutative features extracted from
//! sequential data (frame time series, event streams, syscall traces). They
//! capture the "shape" of a path regardless of parameterization, making them
//! ideal for anomaly detection and workload characterization.
//!
//! # Mathematical Model
//!
//! Given a d-dimensional path `X: [0,T] → ℝ^d`, the **signature** is the
//! collection of iterated integrals:
//!
//! ```text
//! S(X)^{i₁,...,iₖ} = ∫₀<t₁<...<tₖ<T dX^{i₁}_{t₁} ⊗ ... ⊗ dX^{iₖ}_{tₖ}
//! ```
//!
//! The **truncated signature** at depth N keeps only terms with k ≤ N.
//! For practical use, depth 3-4 captures sufficient structure.
//!
//! # Piecewise-Linear Discretization & Chen's Identity
//!
//! Given discrete observations $X = (x_0, x_1, \dots, x_n)$ with segment increments
//! $\Delta x_t = x_{t+1} - x_t$, each linear segment has exact signature given by the
//! truncated tensor exponential:
//!
//! ```text
//! S(Δx_t) = exp(Δx_t) = (1, Δx_t, Δx_t^{⊗2} / 2!, ..., Δx_t^{⊗K} / K!)
//! ```
//!
//! By **Chen's identity**, extending the path by segment $\Delta x_t$ right-multiplies
//! the running signature in the truncated tensor algebra:
//!
//! ```text
//! S(X_{[0,t+1]}) = S(X_{[0,t]}) ⊗ exp(Δx_t)
//! ```
//!
//! Component-wise at depth $k \in \{1, \dots, K\}$:
//!
//! ```text
//! S_new^{i₁,...,iₖ} = S_old^{i₁,...,iₖ}
//!                   + Σ_{m=1}^{k-1} S_old^{i₁,...,i_{k-m}} · (Δx^{i_{k-m+1}} ··· Δx^{iₖ}) / m!
//!                   + (Δx^{i₁} ··· Δx^{iₖ}) / k!
//! ```
//!
//! Including the diagonal $\Delta x^{\otimes m}/m!$ terms (rather than multiplying by
//! $1 + \Delta x_t$) makes the signature **invariant under monotone time
//! reparametrization and arbitrary segment subdivision** (e.g. splitting a segment
//! $\Delta x$ into $\alpha \Delta x$ and $(1-\alpha)\Delta x$ produces the identical
//! signature at every depth).
//!
//! ## Uniqueness vs. Truncation Scope
//!
//! - **Full infinite signature ($K = \infty$)**: characterizes a continuous bounded-variation
//!   path up to **tree-like equivalence** (Hambly & Lyons, *Ann. of Math.* 2010), i.e. paths
//!   that retrace a segment back and forth share the same signature.
//! - **Truncated signature ($K < \infty$)**: a finite-dimensional projection with
//!   $\sum_{k=1}^K d^k$ terms, updated in $O(K \cdot d^K)$ time per observation
//!   ($O(n \cdot K \cdot d^K)$ over $n$ steps). Distinct non-tree-equivalent paths can share
//!   the same depth-$K$ truncation.
//!
//! # Usage
//!
//! ```rust
//! use ftui_runtime::rough_path::SignatureExtractor;
//!
//! // 2D path: (frame_time_ms, alloc_count)
//! let mut ext = SignatureExtractor::new(2, 3); // dim=2, depth=3
//!
//! ext.observe(&[16.0, 1200.0]);
//! ext.observe(&[17.0, 1250.0]);
//! ext.observe(&[15.0, 1180.0]);
//! ext.observe(&[32.0, 1500.0]); // spike!
//!
//! let sig = ext.signature();
//! // sig contains depth-1, depth-2, and depth-3 terms
//! // Use for anomaly detection, distance computation, etc.
//! ```
//!
//! # Applications
//!
//! | Use case | Dimensions | What it detects |
//! |----------|-----------|-----------------|
//! | Frame timing | 1D (ms) | Total displacement (depth $k$ is $(\Delta X)^k/k!$; pair with time-augmentation `(t, ms)` for shape) |
//! | Render+alloc | 2D (ms, bytes) | Correlated regressions, Lévy area (lead-lag order) |
//! | Event stream | 3D (dt, type, payload) | Workload fingerprints |
//!
//! # Fallback
//!
//! When signatures are too expensive (very high-dimensional data), use
//! standard statistical features (mean, variance, skew, autocorrelation).

/// Configuration for signature extraction.
#[derive(Debug, Clone)]
pub struct SignatureConfig {
    /// Number of dimensions in the input path.
    pub dim: usize,
    /// Maximum depth of the truncated signature (typically 2-4).
    pub max_depth: usize,
}

/// Computes the number of signature components for given dim and depth.
///
/// Total terms = Σ_{k=1}^{depth} dim^k = dim * (dim^depth - 1) / (dim - 1)
/// For dim=1, it's just `depth`.
pub fn signature_size(dim: usize, max_depth: usize) -> usize {
    if dim == 0 || max_depth == 0 {
        return 0;
    }
    if dim == 1 {
        return max_depth;
    }
    let mut total: usize = 0;
    let mut power: usize = 1;
    for _ in 0..max_depth {
        power = power.saturating_mul(dim);
        total = total.saturating_add(power);
    }
    total
}

/// Incremental truncated signature extractor.
///
/// Maintains the running signature of a d-dimensional piecewise-linear path,
/// updated incrementally via Chen's identity $S \leftarrow S \otimes \exp(\Delta x)$.
#[derive(Debug, Clone)]
pub struct SignatureExtractor {
    dim: usize,
    max_depth: usize,
    /// Flattened signature terms, organized by depth.
    /// Depth k has dim^k terms, stored in row-major order of multi-indices.
    terms: Vec<f64>,
    /// Offsets into `terms` for each depth level.
    offsets: Vec<usize>,
    /// Number of observations seen.
    count: usize,
    /// Last observed point (for computing increments).
    last: Option<Vec<f64>>,
}

impl SignatureExtractor {
    /// Create a new extractor for `dim`-dimensional paths at truncation `max_depth`.
    pub fn new(dim: usize, max_depth: usize) -> Self {
        let total = signature_size(dim, max_depth);
        let mut offsets = Vec::with_capacity(max_depth + 1);
        offsets.push(0);
        let mut power = 1;
        for _ in 0..max_depth {
            power *= dim;
            offsets.push(offsets.last().unwrap() + power);
        }
        Self {
            dim,
            max_depth,
            terms: vec![0.0; total],
            offsets,
            count: 0,
            last: None,
        }
    }

    /// Number of dimensions.
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Maximum signature depth.
    pub fn max_depth(&self) -> usize {
        self.max_depth
    }

    /// Number of observations processed.
    pub fn count(&self) -> usize {
        self.count
    }

    /// Total number of signature terms.
    pub fn num_terms(&self) -> usize {
        self.terms.len()
    }

    /// Observe a new point on the path.
    ///
    /// `point` must have length `dim`. Panics if the length is wrong.
    /// The first observation establishes the baseline; increments start
    /// from the second observation onward.
    pub fn observe(&mut self, point: &[f64]) {
        assert_eq!(
            point.len(),
            self.dim,
            "point dimension {} != extractor dimension {}",
            point.len(),
            self.dim
        );

        self.count += 1;

        if let Some(ref last) = self.last {
            // Compute increment.
            let dx: Vec<f64> = point.iter().zip(last.iter()).map(|(a, b)| a - b).collect();
            // Update signature via Chen's identity: S <- S (x) exp(dx).
            self.extend_signature(&dx);
        }

        self.last = Some(point.to_vec());
    }

    /// Compute the truncated tensor exponential $\exp(\Delta x) = (\Delta x^{\otimes k} / k!)_{k=1}^K$
    /// for a single linear segment $\Delta x$.
    fn segment_exponential(&self, dx: &[f64]) -> Vec<f64> {
        let mut seg = vec![0.0; self.terms.len()];
        if self.dim == 0 || self.max_depth == 0 {
            return seg;
        }
        // Depth 1: E_1^i = dx^i / 1!
        seg[..self.dim].copy_from_slice(dx);

        // Depth m > 1: E_m^{..., i} = E_{m-1}^{...} * dx^i / m
        for m in 2..=self.max_depth {
            let prev_start = self.offsets[m - 2];
            let prev_end = self.offsets[m - 1];
            let curr_start = self.offsets[m - 1];
            let inv_m = 1.0 / (m as f64);

            for p_idx in 0..(prev_end - prev_start) {
                let parent_val = seg[prev_start + p_idx];
                let row_offset = curr_start + p_idx * self.dim;
                for (d_idx, &dx_val) in dx.iter().enumerate() {
                    seg[row_offset + d_idx] = parent_val * dx_val * inv_m;
                }
            }
        }
        seg
    }

    /// Right-multiply `self.terms` by another truncated tensor `rhs` using Chen's identity:
    /// $(A \otimes B)_k = A_k + B_k + \sum_{m=1}^{k-1} A_{k-m} \otimes B_m$.
    fn right_multiply_tensor(&mut self, rhs: &[f64]) {
        if self.dim == 0 || self.max_depth == 0 {
            return;
        }
        // Process target depth k from max_depth down to 1 so lower depths in
        // `self.terms` still hold their pre-multiplication values.
        for k in (1..=self.max_depth).rev() {
            let k_start = self.offsets[k - 1];
            let k_end = self.offsets[k];

            // Cross terms: sum_{m=1}^{k-1} A_{k-m} (x) B_m
            for m in 1..k {
                let p = k - m;
                let p_start = self.offsets[p - 1];
                let p_count = self.offsets[p] - p_start;
                let m_start = self.offsets[m - 1];
                let m_count = self.offsets[m] - m_start;

                for u in 0..p_count {
                    let a_val = self.terms[p_start + u];
                    if a_val == 0.0 {
                        continue;
                    }
                    let dst_base = k_start + u * m_count;
                    for v in 0..m_count {
                        self.terms[dst_base + v] += a_val * rhs[m_start + v];
                    }
                }
            }

            // Pure rhs term B_k
            for (dst, &b_val) in self.terms[k_start..k_end]
                .iter_mut()
                .zip(&rhs[k_start..k_end])
            {
                *dst += b_val;
            }
        }
    }

    /// Extend the running signature by one linear segment $\Delta x$ using Chen's identity:
    /// $S \leftarrow S \otimes \exp(\Delta x)$.
    fn extend_signature(&mut self, dx: &[f64]) {
        let seg = self.segment_exponential(dx);
        self.right_multiply_tensor(&seg);
    }

    /// Concatenate two truncated signatures via Chen's identity:
    /// $S(X * Y) = S(X) \otimes S(Y)$.
    ///
    /// Both extractors must have the same `dim` and `max_depth`.
    /// Note that concatenation operates purely on the two signatures; if `other`
    /// begins at the terminal point of `self`, the resulting signature equals the
    /// signature of the combined piecewise-linear path.
    pub fn concatenate(&self, other: &Self) -> Self {
        assert_eq!(self.dim, other.dim, "dimension mismatch in concatenate");
        assert_eq!(
            self.max_depth, other.max_depth,
            "max_depth mismatch in concatenate"
        );
        let mut combined = self.clone();
        combined.right_multiply_tensor(&other.terms);
        combined.count = self.count + other.count;
        if other.last.is_some() {
            combined.last = other.last.clone();
        }
        combined
    }

    /// Compute the signed Lévy area $A^{i,j} = \frac{1}{2}(S^{i,j} - S^{j,i})$
    /// between coordinates `i` and `j` (0-indexed).
    ///
    /// Returns `None` if `max_depth < 2` or if `i >= dim` or `j >= dim`.
    pub fn levy_area(&self, i: usize, j: usize) -> Option<f64> {
        if self.max_depth < 2 || i >= self.dim || j >= self.dim {
            return None;
        }
        let d2 = self.signature_at_depth(2)?;
        Some(0.5 * (d2[i * self.dim + j] - d2[j * self.dim + i]))
    }

    /// Get the current signature as a slice.
    pub fn signature(&self) -> &[f64] {
        &self.terms
    }

    /// Get signature terms at a specific depth (1-indexed).
    ///
    /// Returns `None` if depth is 0 or exceeds `max_depth`.
    pub fn signature_at_depth(&self, depth: usize) -> Option<&[f64]> {
        if depth == 0 || depth > self.max_depth {
            return None;
        }
        let start = self.offsets[depth - 1];
        let end = self.offsets[depth];
        Some(&self.terms[start..end])
    }

    /// L2 norm of the signature vector.
    pub fn norm(&self) -> f64 {
        self.terms.iter().map(|x| x * x).sum::<f64>().sqrt()
    }

    /// Reset the extractor to its initial state.
    pub fn reset(&mut self) {
        self.terms.fill(0.0);
        self.count = 0;
        self.last = None;
    }
}

/// Compute the L2 distance between two signature vectors.
///
/// The signatures must have the same length.
pub fn signature_distance(a: &[f64], b: &[f64]) -> f64 {
    assert_eq!(a.len(), b.len(), "signature length mismatch");
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| (x - y) * (x - y))
        .sum::<f64>()
        .sqrt()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn signature_size_basic() {
        assert_eq!(signature_size(0, 3), 0);
        assert_eq!(signature_size(3, 0), 0);
        assert_eq!(signature_size(1, 3), 3); // 1 + 1 + 1
        assert_eq!(signature_size(2, 1), 2); // 2
        assert_eq!(signature_size(2, 2), 6); // 2 + 4
        assert_eq!(signature_size(2, 3), 14); // 2 + 4 + 8
        assert_eq!(signature_size(3, 2), 12); // 3 + 9
    }

    #[test]
    fn extractor_initial_state() {
        let ext = SignatureExtractor::new(2, 3);
        assert_eq!(ext.dim(), 2);
        assert_eq!(ext.max_depth(), 3);
        assert_eq!(ext.count(), 0);
        assert_eq!(ext.num_terms(), 14); // 2 + 4 + 8
        assert!(ext.signature().iter().all(|&x| x == 0.0));
    }

    #[test]
    fn single_observation_no_signature() {
        let mut ext = SignatureExtractor::new(2, 2);
        ext.observe(&[1.0, 2.0]);
        // Only one point — no increment, signature stays zero.
        assert!(ext.signature().iter().all(|&x| x == 0.0));
        assert_eq!(ext.count(), 1);
    }

    #[test]
    fn depth1_is_total_increment() {
        let mut ext = SignatureExtractor::new(2, 2);
        ext.observe(&[0.0, 0.0]);
        ext.observe(&[1.0, 3.0]);
        ext.observe(&[4.0, 5.0]);

        // Depth 1: total increments = (1,3) + (3,2) = (4, 5)
        let d1 = ext.signature_at_depth(1).unwrap();
        assert!((d1[0] - 4.0).abs() < 1e-10);
        assert!((d1[1] - 5.0).abs() < 1e-10);
    }

    #[test]
    fn single_segment_closed_form_matches_tensor_exponential() {
        // For a single linear segment 0 -> a in R^d, S_k = a^{⊗k} / k!.
        let a = [2.0, -3.0];
        let mut ext = SignatureExtractor::new(2, 3);
        ext.observe(&[0.0, 0.0]);
        ext.observe(&a);

        let d1 = ext.signature_at_depth(1).unwrap();
        assert!((d1[0] - 2.0).abs() < 1e-12);
        assert!((d1[1] - (-3.0)).abs() < 1e-12);

        let d2 = ext.signature_at_depth(2).unwrap();
        assert!((d2[0] - 4.0 / 2.0).abs() < 1e-12); // a0^2 / 2!
        assert!((d2[1] - (-6.0) / 2.0).abs() < 1e-12); // a0*a1 / 2!
        assert!((d2[2] - (-6.0) / 2.0).abs() < 1e-12); // a1*a0 / 2!
        assert!((d2[3] - 9.0 / 2.0).abs() < 1e-12); // a1^2 / 2!
        assert!(ext.levy_area(0, 1).unwrap().abs() < 1e-12);

        let d3 = ext.signature_at_depth(3).unwrap();
        assert!((d3[0] - 8.0 / 6.0).abs() < 1e-12); // a0^3 / 3!
        assert!((d3[7] - (-27.0) / 6.0).abs() < 1e-12); // a1^3 / 3!
    }

    #[test]
    fn depth2_iterated_integrals() {
        let mut ext = SignatureExtractor::new(1, 3);
        // 1D path: 0 → 1 → 3 → 6 (total displacement ΔX = 6).
        // By Chen's identity with exp(Δx), any 1D path with total increment ΔX
        // has depth-k signature (ΔX)^k / k!, invariant to subdivision.
        ext.observe(&[0.0]);
        ext.observe(&[1.0]);
        ext.observe(&[3.0]);
        ext.observe(&[6.0]);

        let d1 = ext.signature_at_depth(1).unwrap();
        assert!((d1[0] - 6.0).abs() < 1e-10);

        // Depth 2: S^{1,1} = Σ_{k<l} Δx_k Δx_l + (1/2) Σ_k (Δx_k)^2
        // = (2 + 3 + 6) + 0.5 * (1 + 4 + 9) = 11 + 7 = 18 = 6^2 / 2!
        let d2 = ext.signature_at_depth(2).unwrap();
        assert!((d2[0] - 18.0).abs() < 1e-10);

        // Depth 3: S^{1,1,1} = 6^3 / 3! = 36
        let d3 = ext.signature_at_depth(3).unwrap();
        assert!((d3[0] - 36.0).abs() < 1e-10);
    }

    #[test]
    fn signature_depth_out_of_range() {
        let ext = SignatureExtractor::new(2, 3);
        assert!(ext.signature_at_depth(0).is_none());
        assert!(ext.signature_at_depth(4).is_none());
        assert!(ext.signature_at_depth(1).is_some());
        assert!(ext.signature_at_depth(3).is_some());
    }

    #[test]
    fn norm_computation() {
        let mut ext = SignatureExtractor::new(1, 1);
        ext.observe(&[0.0]);
        ext.observe(&[3.0]);
        // Depth 1: S^1 = 3
        assert!((ext.norm() - 3.0).abs() < 1e-10);
    }

    #[test]
    fn distance_computation() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![4.0, 6.0, 3.0];
        // distance = sqrt(9 + 16 + 0) = 5
        assert!((signature_distance(&a, &b) - 5.0).abs() < 1e-10);
    }

    #[test]
    fn reset() {
        let mut ext = SignatureExtractor::new(2, 2);
        ext.observe(&[0.0, 0.0]);
        ext.observe(&[1.0, 1.0]);
        assert!(ext.norm() > 0.0);
        ext.reset();
        assert_eq!(ext.count(), 0);
        assert!(ext.signature().iter().all(|&x| x == 0.0));
    }

    #[test]
    fn two_dim_depth3_l_shaped_path_and_levy_area() {
        let mut ext = SignatureExtractor::new(2, 3);
        ext.observe(&[0.0, 0.0]);
        ext.observe(&[1.0, 0.0]);
        ext.observe(&[1.0, 1.0]);

        // Increments: (1,0), then (0,1).
        // Depth 1: (1, 1)
        let d1 = ext.signature_at_depth(1).unwrap();
        assert!((d1[0] - 1.0).abs() < 1e-10);
        assert!((d1[1] - 1.0).abs() < 1e-10);

        // Depth 2 (2x2 = 4 terms):
        // S^{1,1} = 1^2 / 2! = 0.5
        // S^{1,2} = 1 * 1 = 1.0
        // S^{2,1} = 0.0
        // S^{2,2} = 1^2 / 2! = 0.5
        let d2 = ext.signature_at_depth(2).unwrap();
        assert!((d2[0] - 0.5).abs() < 1e-10); // S^{1,1}
        assert!((d2[1] - 1.0).abs() < 1e-10); // S^{1,2}
        assert!((d2[2] - 0.0).abs() < 1e-10); // S^{2,1}
        assert!((d2[3] - 0.5).abs() < 1e-10); // S^{2,2}

        // Signed Lévy area A^{0,1} = 0.5 * (S^{1,2} - S^{2,1}) = 0.5
        assert!((ext.levy_area(0, 1).unwrap() - 0.5).abs() < 1e-10);
        assert!((ext.levy_area(1, 0).unwrap() + 0.5).abs() < 1e-10);

        // Depth 3:
        // S^{1,1,1} = 1/6, S^{1,1,2} = 0.5, S^{1,2,2} = 0.5, S^{2,2,2} = 1/6
        let d3 = ext.signature_at_depth(3).unwrap();
        assert!((d3[0] - 1.0 / 6.0).abs() < 1e-10); // (0,0,0)
        assert!((d3[1] - 0.5).abs() < 1e-10); // (0,0,1)
        assert!((d3[2] - 0.0).abs() < 1e-10); // (0,1,0)
        assert!((d3[3] - 0.5).abs() < 1e-10); // (0,1,1)
        assert!((d3[4] - 0.0).abs() < 1e-10); // (1,0,0)
        assert!((d3[5] - 0.0).abs() < 1e-10); // (1,0,1)
        assert!((d3[6] - 0.0).abs() < 1e-10); // (1,1,0)
        assert!((d3[7] - 1.0 / 6.0).abs() < 1e-10); // (1,1,1)
    }

    #[test]
    fn chens_identity_concatenation_matches_single_pass() {
        let pts = [
            [0.0, 0.0],
            [1.5, -0.5],
            [2.0, 1.25],
            [-0.5, 2.0],
            [0.75, -1.0],
        ];

        let mut full = SignatureExtractor::new(2, 4);
        for p in &pts {
            full.observe(p);
        }

        // Split at pts[2]: first path pts[0..=2], second path pts[2..=4].
        let mut first = SignatureExtractor::new(2, 4);
        for p in &pts[..=2] {
            first.observe(p);
        }
        let mut second = SignatureExtractor::new(2, 4);
        for p in &pts[2..] {
            second.observe(p);
        }

        let joined = first.concatenate(&second);
        for (idx, (&a, &b)) in full.signature().iter().zip(joined.signature()).enumerate() {
            assert!(
                (a - b).abs() < 1e-10,
                "Chen concatenation mismatch at term {}: full={} joined={}",
                idx,
                a,
                b
            );
        }
    }

    #[test]
    fn tree_like_retracing_cancels_signature() {
        // Hambly-Lyons tree-like equivalence: going from P -> Q and back Q -> P
        // along the same linear segment produces the zero signature at all depths.
        let mut ext = SignatureExtractor::new(3, 4);
        ext.observe(&[1.0, -2.0, 0.5]);
        ext.observe(&[4.0, 2.5, -1.5]);
        ext.observe(&[1.0, -2.0, 0.5]);

        for (idx, &val) in ext.signature().iter().enumerate() {
            assert!(
                val.abs() < 1e-10,
                "Retraced segment should have zero signature at term {}, got {}",
                idx,
                val
            );
        }
    }

    #[test]
    fn translation_invariance() {
        // Signature should be the same regardless of starting point.
        let mut ext1 = SignatureExtractor::new(2, 2);
        ext1.observe(&[0.0, 0.0]);
        ext1.observe(&[1.0, 2.0]);
        ext1.observe(&[3.0, 1.0]);

        let mut ext2 = SignatureExtractor::new(2, 2);
        ext2.observe(&[100.0, 200.0]);
        ext2.observe(&[101.0, 202.0]);
        ext2.observe(&[103.0, 201.0]);

        let s1 = ext1.signature();
        let s2 = ext2.signature();
        for (a, b) in s1.iter().zip(s2.iter()) {
            assert!(
                (a - b).abs() < 1e-10,
                "Translation invariance violated: {} != {}",
                a,
                b
            );
        }
    }

    #[test]
    fn constant_path_zero_signature() {
        // If all points are the same, all increments are zero.
        let mut ext = SignatureExtractor::new(3, 3);
        for _ in 0..10 {
            ext.observe(&[5.0, 5.0, 5.0]);
        }
        assert!(ext.signature().iter().all(|&x| x == 0.0));
    }

    #[test]
    fn signature_distance_zero_for_same() {
        let a = vec![1.0, 2.0, 3.0];
        assert!((signature_distance(&a, &a)).abs() < 1e-15);
    }

    #[test]
    #[should_panic(expected = "point dimension")]
    fn wrong_dimension_panics() {
        let mut ext = SignatureExtractor::new(2, 2);
        ext.observe(&[1.0]); // wrong: expected 2
    }

    #[test]
    fn frame_timing_anomaly_detection() {
        // Time-augmented 2D path (t, frame_ms) captures transient spikes even when
        // the path returns to the baseline 16.0ms level.
        let mut normal = SignatureExtractor::new(2, 3);
        for (i, &t) in [16.0, 16.1, 15.9, 16.0, 16.2, 15.8, 16.0, 16.1]
            .iter()
            .enumerate()
        {
            normal.observe(&[i as f64, t]);
        }

        let mut anomalous = SignatureExtractor::new(2, 3);
        for (i, &t) in [16.0, 16.1, 15.9, 64.0, 16.0, 16.0, 16.0, 16.0]
            .iter()
            .enumerate()
        {
            anomalous.observe(&[i as f64, t]);
        }

        // The anomalous path should have a significantly different signature
        let dist = signature_distance(normal.signature(), anomalous.signature());
        assert!(
            dist > 1.0,
            "Anomalous path should be far from normal (dist={})",
            dist
        );
    }

    proptest! {
        #[test]
        fn proptest_reparametrization_invariance_under_subdivision(
            pts in proptest::collection::vec(
                proptest::array::uniform3(-10.0f64..10.0f64),
                2..=6
            ),
            splits in proptest::collection::vec(0.05f64..0.95f64, 1..=5),
        ) {
            let mut coarse = SignatureExtractor::new(3, 3);
            for p in &pts {
                coarse.observe(p);
            }

            // Subdivide each linear segment [pts[k], pts[k+1]] at intermediate fractions
            let mut refined = SignatureExtractor::new(3, 3);
            refined.observe(&pts[0]);
            for (seg_idx, window) in pts.windows(2).enumerate() {
                let a = window[0];
                let b = window[1];
                let frac = splits[seg_idx % splits.len()];
                let mid1 = [
                    a[0] + 0.5 * frac * (b[0] - a[0]),
                    a[1] + 0.5 * frac * (b[1] - a[1]),
                    a[2] + 0.5 * frac * (b[2] - a[2]),
                ];
                let mid2 = [
                    a[0] + frac * (b[0] - a[0]),
                    a[1] + frac * (b[1] - a[1]),
                    a[2] + frac * (b[2] - a[2]),
                ];
                refined.observe(&mid1);
                refined.observe(&mid2);
                refined.observe(&b);
            }

            for (idx, (&c, &r)) in coarse.signature().iter().zip(refined.signature()).enumerate() {
                prop_assert!(
                    (c - r).abs() < 1e-7 * (1.0 + c.abs()),
                    "Subdivision invariance failed at term {}: coarse={} refined={}",
                    idx,
                    c,
                    r
                );
            }
        }
    }
}
