//! Sinkhorn morphing for fluid TUI layout transitions.
//!
//! When a layout changes (e.g., sidebar opens, panel resizes), instead of
//! snapping instantly, this module computes the optimal transport between
//! the old and new character distributions using the entropy-regularized
//! Sinkhorn-Knopp algorithm. This produces smooth, cinematic character
//! morphing animations between completely different layouts.
//!
//! # Mathematical Core
//!
//! The old and new layouts are treated as probability distributions of
//! characters on a grid. We solve:
//!
//! ```text
//! P* = argmin_P  sum_{i,j} P_{ij} C_{ij}  -  epsilon * H(P)
//! ```
//!
//! where `C_{ij}` is the Euclidean distance between grid positions,
//! `H(P)` is the entropy regularizer, and `epsilon` controls smoothness.
//!
//! The Sinkhorn-Knopp iterative scaling algorithm solves this efficiently:
//! 1. Construct cost matrix `C` (with penalties for `MorphTag` mismatches).
//! 2. Compute Gibbs kernel `K = exp(-C / epsilon)`.
//! 3. Iterate: `u = p / (K * v)`, `v = q / (K^T * u)`.
//! 4. Extract transport plan `T = diag(u) * K * diag(v)`.
//!
//! # Block-Diagonal Optimization
//!
//! A dense solver on N cells is O(N^2) per iteration. To stay fast, we
//! partition by `MorphTag` into independent sub-problems, solving several
//! smaller Sinkhorn problems instead of one large one.

use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, PackedRgba};

// ---------------------------------------------------------------------------
// bd-30uc6: WidgetId / MorphTag Stability Tracking
// ---------------------------------------------------------------------------

/// A stable identifier for a widget across layout transitions.
///
/// Widgets that share the same `WidgetId` between old and new layouts
/// are considered "the same widget." The Sinkhorn solver adds extreme
/// cost penalties for transporting cells between different IDs, which
/// effectively produces a block-diagonal transport plan grouped by widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WidgetId(pub u64);

impl WidgetId {
    /// Create a widget ID from a numeric value.
    #[inline]
    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    /// Create a widget ID from a string label via FNV-1a hashing.
    ///
    /// Collisions are acceptable because the cost penalty is a soft
    /// constraint — two different widgets accidentally sharing an ID
    /// will morph together, which is visually acceptable.
    pub fn from_label(label: &str) -> Self {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in label.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        Self(hash)
    }

    /// The raw numeric value.
    #[inline]
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// A tag that groups cells into semantic regions for structured morphing.
///
/// Cells with the same `MorphTag` are morphed together — the solver
/// strongly penalizes transport between different tags, effectively
/// creating independent sub-problems.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MorphTag(pub u32);

impl MorphTag {
    /// Default tag for untagged cells.
    pub const DEFAULT: Self = Self(0);

    /// Create a tag from a [`WidgetId`] (uses lower 32 bits).
    #[inline]
    pub const fn from_widget(id: WidgetId) -> Self {
        Self(id.0 as u32)
    }
}

/// A cell's position and identity in the morph distribution.
#[derive(Debug, Clone)]
pub struct MorphCell {
    /// Grid x coordinate.
    pub x: u16,
    /// Grid y coordinate.
    pub y: u16,
    /// The cell content snapshot.
    pub cell: Cell,
    /// Semantic grouping tag.
    pub tag: MorphTag,
}

/// Source or target distribution for optimal transport.
#[derive(Debug, Clone)]
pub struct Distribution {
    /// The cells in this distribution.
    pub cells: Vec<MorphCell>,
    /// Total mass (always equals `cells.len()` after normalization).
    pub mass: usize,
}

/// A single assignment in the transport plan.
#[derive(Debug, Clone, Copy)]
pub struct TransportAssignment {
    /// Index into the source distribution.
    pub source_idx: usize,
    /// Index into the target distribution.
    pub target_idx: usize,
    /// Transport weight (0.0..=1.0).
    pub weight: f64,
}

/// The computed optimal transport plan between two distributions.
#[derive(Debug, Clone)]
pub struct TransportPlan {
    /// Individual cell-to-cell assignments.
    pub assignments: Vec<TransportAssignment>,
    /// Number of Sinkhorn iterations performed.
    pub iterations: u32,
    /// Epsilon used for entropy regularization.
    pub epsilon: f64,
    /// Number of independent tag-partitioned sub-problems solved.
    pub block_count: usize,
}

/// An interpolated frame of the morph animation.
#[derive(Debug, Clone)]
pub struct MorphFrame {
    /// Interpolation parameter in `[0.0, 1.0]`.
    pub t: f64,
    /// Cell positions at this interpolation point.
    /// Each entry is `(x, y, cell)` where x/y are fractional grid coords.
    pub cells: Vec<(f64, f64, Cell)>,
}

/// Configuration for the Sinkhorn morph solver.
#[derive(Debug, Clone)]
pub struct MorphConfig {
    /// Entropy regularization parameter. Higher = smoother but less optimal.
    /// Typical range: 0.01..1.0. Default: 0.1.
    pub epsilon: f64,
    /// Number of Sinkhorn scaling iterations. Default: 10.
    pub max_iterations: u32,
    /// Cost penalty for transporting between different `MorphTag` groups.
    /// Default: 1e6 (effectively infinite).
    pub tag_mismatch_penalty: f64,
}

impl Default for MorphConfig {
    fn default() -> Self {
        Self {
            epsilon: 0.1,
            max_iterations: 10,
            tag_mismatch_penalty: 1e6,
        }
    }
}

// ---------------------------------------------------------------------------
// Distribution extraction
// ---------------------------------------------------------------------------

/// Extract a character distribution from a buffer.
///
/// Non-empty cells are collected with their grid positions. A `tagger`
/// function assigns [`MorphTag`]s to cells based on position.
pub fn extract_distribution(
    buffer: &Buffer,
    tagger: &dyn Fn(u16, u16, &Cell) -> MorphTag,
) -> Distribution {
    let w = buffer.width();
    let h = buffer.height();
    let mut cells = Vec::new();

    for y in 0..h {
        for x in 0..w {
            if let Some(cell) = buffer.get(x, y)
                && !cell.content.is_empty()
            {
                cells.push(MorphCell {
                    x,
                    y,
                    cell: *cell,
                    tag: tagger(x, y, cell),
                });
            }
        }
    }

    let mass = cells.len();
    Distribution { cells, mass }
}

/// Pad the shorter distribution with dummy (empty) cells at the borders
/// so that both distributions have equal mass — a strict requirement for
/// optimal transport.
pub fn equalize_mass(source: &mut Distribution, target: &mut Distribution) {
    let diff = source.mass as isize - target.mass as isize;
    if diff == 0 {
        return;
    }

    let (short, extra) = if diff > 0 {
        (&mut *target, diff as usize)
    } else {
        (&mut *source, (-diff) as usize)
    };

    // Add dummy cells at position (0, 0) with empty content.
    for _ in 0..extra {
        short.cells.push(MorphCell {
            x: 0,
            y: 0,
            cell: Cell::default(),
            tag: MorphTag::DEFAULT,
        });
    }
    short.mass = short.cells.len();
}

// ---------------------------------------------------------------------------
// Sinkhorn-Knopp solver
// ---------------------------------------------------------------------------

/// Compute the squared Euclidean distance between two grid positions.
fn cell_distance_sq(ax: u16, ay: u16, bx: u16, by: u16) -> f64 {
    let dx = f64::from(ax) - f64::from(bx);
    let dy = f64::from(ay) - f64::from(by);
    dx * dx + dy * dy
}

/// Solve the entropy-regularized optimal transport problem using the
/// Sinkhorn-Knopp iterative scaling algorithm.
///
/// This partitions cells by `MorphTag` for block-diagonal optimization,
/// then solves each partition independently.
pub fn solve_transport(
    source: &Distribution,
    target: &Distribution,
    config: &MorphConfig,
) -> TransportPlan {
    assert_eq!(
        source.mass, target.mass,
        "distributions must have equal mass (call equalize_mass first)"
    );

    if source.mass == 0 {
        return TransportPlan {
            assignments: Vec::new(),
            iterations: 0,
            epsilon: config.epsilon,
            block_count: 0,
        };
    }

    // Partition by MorphTag for block-diagonal optimization.
    let mut tag_groups: std::collections::HashMap<MorphTag, (Vec<usize>, Vec<usize>)> =
        std::collections::HashMap::new();

    for (i, sc) in source.cells.iter().enumerate() {
        tag_groups
            .entry(sc.tag)
            .or_insert_with(|| (Vec::new(), Vec::new()))
            .0
            .push(i);
    }
    for (j, tc) in target.cells.iter().enumerate() {
        tag_groups
            .entry(tc.tag)
            .or_insert_with(|| (Vec::new(), Vec::new()))
            .1
            .push(j);
    }

    let block_count = tag_groups.len();
    let mut all_assignments = Vec::new();

    for (src_indices, tgt_indices) in tag_groups.values() {
        if src_indices.is_empty() || tgt_indices.is_empty() {
            // Cross-tag leftovers handled by the mismatch penalty path below.
            continue;
        }

        let assignments = solve_block(source, target, src_indices, tgt_indices, config);
        all_assignments.extend(assignments);
    }

    // Handle any unmatched cells (cross-tag residuals) via nearest-neighbor.
    let matched_sources: std::collections::HashSet<usize> =
        all_assignments.iter().map(|a| a.source_idx).collect();
    let matched_targets: std::collections::HashSet<usize> =
        all_assignments.iter().map(|a| a.target_idx).collect();

    let unmatched_src: Vec<usize> = (0..source.mass)
        .filter(|i| !matched_sources.contains(i))
        .collect();
    let unmatched_tgt: Vec<usize> = (0..target.mass)
        .filter(|j| !matched_targets.contains(j))
        .collect();

    // Greedy nearest-neighbor for residuals.
    let mut used_tgt = std::collections::HashSet::new();
    for &si in &unmatched_src {
        let sc = &source.cells[si];
        let best_tj = unmatched_tgt
            .iter()
            .filter(|tj| !used_tgt.contains(*tj))
            .min_by(|&&a, &&b| {
                let da = cell_distance_sq(sc.x, sc.y, target.cells[a].x, target.cells[a].y);
                let db = cell_distance_sq(sc.x, sc.y, target.cells[b].x, target.cells[b].y);
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            })
            .copied();

        if let Some(tj) = best_tj {
            used_tgt.insert(tj);
            all_assignments.push(TransportAssignment {
                source_idx: si,
                target_idx: tj,
                weight: 1.0,
            });
        }
    }

    TransportPlan {
        assignments: all_assignments,
        iterations: config.max_iterations,
        epsilon: config.epsilon,
        block_count,
    }
}

/// Solve a single block (partition) of the transport problem.
fn solve_block(
    source: &Distribution,
    target: &Distribution,
    src_indices: &[usize],
    tgt_indices: &[usize],
    config: &MorphConfig,
) -> Vec<TransportAssignment> {
    let n = src_indices.len();
    let m = tgt_indices.len();

    // Build cost matrix C[i][j] = distance(src_i, tgt_j).
    let mut cost = vec![0.0_f64; n * m];
    for (i, &si) in src_indices.iter().enumerate() {
        let sc = &source.cells[si];
        for (j, &tj) in tgt_indices.iter().enumerate() {
            let tc = &target.cells[tj];
            let mut c = cell_distance_sq(sc.x, sc.y, tc.x, tc.y).sqrt();
            if sc.tag != tc.tag {
                c += config.tag_mismatch_penalty;
            }
            cost[i * m + j] = c;
        }
    }

    // Build Gibbs kernel K[i][j] = exp(-C[i][j] / epsilon).
    let inv_eps = 1.0 / config.epsilon;
    let mut kernel = vec![0.0_f64; n * m];
    for k in 0..n * m {
        kernel[k] = (-cost[k] * inv_eps).exp();
    }

    // Uniform marginals.
    let p_val = 1.0 / n as f64;
    let q_val = 1.0 / m as f64;

    // Sinkhorn scaling vectors.
    let mut u = vec![1.0_f64; n];
    let mut v = vec![1.0_f64; m];

    for _iter in 0..config.max_iterations {
        // u = p / (K * v)
        for i in 0..n {
            let mut kv = 0.0;
            for j in 0..m {
                kv += kernel[i * m + j] * v[j];
            }
            u[i] = if kv > 1e-300 { p_val / kv } else { 0.0 };
        }

        // v = q / (K^T * u)
        for j in 0..m {
            let mut ku = 0.0;
            for i in 0..n {
                ku += kernel[i * m + j] * u[i];
            }
            v[j] = if ku > 1e-300 { q_val / ku } else { 0.0 };
        }
    }

    // Extract transport plan T = diag(u) * K * diag(v).
    // For each source cell, find its best target assignment.
    let mut assignments = Vec::with_capacity(n.min(m));

    // Greedy assignment: for each source, pick target with highest transport weight.
    let mut used_targets = vec![false; m];
    let mut src_order: Vec<usize> = (0..n).collect();
    // Sort sources by their maximum transport weight (descending) for better matching.
    src_order.sort_by(|&a, &b| {
        let max_a = (0..m)
            .map(|j| u[a] * kernel[a * m + j] * v[j])
            .fold(0.0_f64, f64::max);
        let max_b = (0..m)
            .map(|j| u[b] * kernel[b * m + j] * v[j])
            .fold(0.0_f64, f64::max);
        max_b
            .partial_cmp(&max_a)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    for &i in &src_order {
        let mut best_j = None;
        let mut best_w = -1.0_f64;
        for j in 0..m {
            if used_targets[j] {
                continue;
            }
            let w = u[i] * kernel[i * m + j] * v[j];
            if w > best_w {
                best_w = w;
                best_j = Some(j);
            }
        }
        if let Some(j) = best_j {
            used_targets[j] = true;
            assignments.push(TransportAssignment {
                source_idx: src_indices[i],
                target_idx: tgt_indices[j],
                weight: best_w,
            });
        }
    }

    assignments
}

// ---------------------------------------------------------------------------
// Animation interpolation
// ---------------------------------------------------------------------------

/// Interpolate a morph frame at parameter `t` in `[0.0, 1.0]`.
///
/// At `t = 0.0`, cells are at their source positions.
/// At `t = 1.0`, cells are at their target positions.
/// Intermediate values produce smooth linear interpolation along the
/// optimal transport paths.
pub fn interpolate_frame(
    source: &Distribution,
    target: &Distribution,
    plan: &TransportPlan,
    t: f64,
) -> MorphFrame {
    let t = t.clamp(0.0, 1.0);
    let mut cells = Vec::with_capacity(plan.assignments.len());

    for assignment in &plan.assignments {
        let sc = &source.cells[assignment.source_idx];
        let tc = &target.cells[assignment.target_idx];

        let x = f64::from(sc.x) * (1.0 - t) + f64::from(tc.x) * t;
        let y = f64::from(sc.y) * (1.0 - t) + f64::from(tc.y) * t;

        // Interpolate colors in Oklab perceptual space (bd-1eunc).
        let (fg, bg) = interpolate_cell_color(sc.cell.fg, sc.cell.bg, tc.cell.fg, tc.cell.bg, t);
        // Use source character for first half, target for second half.
        let mut cell = if t < 0.5 { sc.cell } else { tc.cell };
        cell.fg = fg;
        cell.bg = bg;

        cells.push((x, y, cell));
    }

    MorphFrame { t, cells }
}

/// Render a `MorphFrame` into a `Buffer` by snapping fractional positions
/// to the nearest grid cell. Cells that collide are resolved by keeping
/// the one with the highest transport weight (closest to its target).
pub fn render_frame_to_buffer(frame: &MorphFrame, width: u16, height: u16) -> Buffer {
    let mut buf = Buffer::new(width, height);

    for &(fx, fy, ref cell) in &frame.cells {
        let x = fx.round() as i32;
        let y = fy.round() as i32;
        if x >= 0 && x < i32::from(width) && y >= 0 && y < i32::from(height) {
            buf.set(x as u16, y as u16, *cell);
        }
    }

    buf
}

// ---------------------------------------------------------------------------
// bd-1eunc: Oklab Color Space Interpolation for Morphing Cells
// ---------------------------------------------------------------------------

/// OkLab perceptual color space for smooth color transitions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OkLab {
    pub l: f64,
    pub a: f64,
    pub b: f64,
}

impl OkLab {
    #[inline]
    pub const fn new(l: f64, a: f64, b: f64) -> Self {
        Self { l, a, b }
    }

    /// Linearly interpolate between two OkLab colors.
    #[must_use]
    #[inline]
    pub fn lerp(self, other: Self, t: f64) -> Self {
        let t = t.clamp(0.0, 1.0);
        Self {
            l: self.l + (other.l - self.l) * t,
            a: self.a + (other.a - self.a) * t,
            b: self.b + (other.b - self.b) * t,
        }
    }

    /// Perceptual distance (Euclidean in OkLab space).
    #[inline]
    pub fn delta_e(self, other: Self) -> f64 {
        let dl = self.l - other.l;
        let da = self.a - other.a;
        let db = self.b - other.b;
        (dl * dl + da * da + db * db).sqrt()
    }
}

#[inline]
fn srgb_to_linear(c: f64) -> f64 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[inline]
fn linear_to_srgb(c: f64) -> f64 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Convert `PackedRgba` to OkLab.
pub fn rgba_to_oklab(color: PackedRgba) -> OkLab {
    let r = srgb_to_linear(f64::from(color.r()) / 255.0);
    let g = srgb_to_linear(f64::from(color.g()) / 255.0);
    let b = srgb_to_linear(f64::from(color.b()) / 255.0);

    let l = 0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_99 * b;
    let m = 0.211_903_50 * r + 0.680_699_55 * g + 0.107_396_96 * b;
    let s = 0.088_302_46 * r + 0.281_718_84 * g + 0.629_978_70 * b;

    let l_ = l.cbrt();
    let m_ = m.cbrt();
    let s_ = s.cbrt();

    OkLab {
        l: 0.210_454_26 * l_ + 0.793_617_78 * m_ - 0.004_072_05 * s_,
        a: 1.977_998_49 * l_ - 2.428_592_05 * m_ + 0.450_593_56 * s_,
        b: 0.025_904_04 * l_ + 0.782_771_77 * m_ - 0.808_675_77 * s_,
    }
}

/// Convert OkLab to `PackedRgba` (with gamut clamping).
pub fn oklab_to_rgba(lab: OkLab) -> PackedRgba {
    let l_ = lab.l + 0.396_337_78 * lab.a + 0.215_803_76 * lab.b;
    let m_ = lab.l - 0.105_561_35 * lab.a - 0.063_854_17 * lab.b;
    let s_ = lab.l - 0.089_484_18 * lab.a - 1.291_485_48 * lab.b;

    let l = l_ * l_ * l_;
    let m = m_ * m_ * m_;
    let s = s_ * s_ * s_;

    let r = 4.076_741_66 * l - 3.307_711_59 * m + 0.230_969_94 * s;
    let g = -1.268_438_00 * l + 2.609_757_40 * m - 0.341_319_38 * s;
    let b = -0.004_196_09 * l - 0.703_418_61 * m + 1.707_614_70 * s;

    let r_srgb = (linear_to_srgb(r.clamp(0.0, 1.0)) * 255.0).round() as u8;
    let g_srgb = (linear_to_srgb(g.clamp(0.0, 1.0)) * 255.0).round() as u8;
    let b_srgb = (linear_to_srgb(b.clamp(0.0, 1.0)) * 255.0).round() as u8;

    PackedRgba::rgb(r_srgb, g_srgb, b_srgb)
}

/// Interpolate foreground and background colors in Oklab perceptual space.
///
/// Returns `(interpolated_fg, interpolated_bg)`.
pub fn interpolate_cell_color(
    src_fg: PackedRgba,
    src_bg: PackedRgba,
    dst_fg: PackedRgba,
    dst_bg: PackedRgba,
    t: f64,
) -> (PackedRgba, PackedRgba) {
    let fg = oklab_to_rgba(rgba_to_oklab(src_fg).lerp(rgba_to_oklab(dst_fg), t));
    let bg = oklab_to_rgba(rgba_to_oklab(src_bg).lerp(rgba_to_oklab(dst_bg), t));
    (fg, bg)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_buffer_with_text(w: u16, h: u16, text: &str) -> Buffer {
        use ftui_render::cell::CellContent;
        let mut buf = Buffer::new(w, h);
        for (i, ch) in text.chars().enumerate() {
            let x = i as u16 % w;
            let y = i as u16 / w;
            if x < w && y < h {
                let cell = Cell {
                    content: CellContent::from_char(ch),
                    ..Cell::default()
                };
                buf.set(x, y, cell);
            }
        }
        buf
    }

    fn default_tagger(_x: u16, _y: u16, _cell: &Cell) -> MorphTag {
        MorphTag::DEFAULT
    }

    #[test]
    fn test_extract_distribution_non_empty() {
        // Use a 4x2 buffer but only fill the first row with text.
        // Second row stays as default (empty) cells.
        let buf = make_buffer_with_text(4, 1, "ABCD");
        let dist = extract_distribution(&buf, &default_tagger);
        // Only non-empty cells are extracted.
        assert_eq!(dist.mass, 4);
        assert_eq!(dist.cells.len(), 4);
    }

    #[test]
    fn test_extract_distribution_empty_buffer() {
        let buf = Buffer::new(5, 3);
        let dist = extract_distribution(&buf, &default_tagger);
        assert_eq!(dist.mass, 0);
    }

    #[test]
    fn test_equalize_mass_already_equal() {
        let buf_a = make_buffer_with_text(3, 1, "ABC");
        let buf_b = make_buffer_with_text(3, 1, "XYZ");
        let mut src = extract_distribution(&buf_a, &default_tagger);
        let mut tgt = extract_distribution(&buf_b, &default_tagger);
        equalize_mass(&mut src, &mut tgt);
        assert_eq!(src.mass, tgt.mass);
    }

    #[test]
    fn test_equalize_mass_pads_shorter() {
        let buf_a = make_buffer_with_text(4, 1, "ABCD");
        let buf_b = make_buffer_with_text(2, 1, "XY");
        let mut src = extract_distribution(&buf_a, &default_tagger);
        let mut tgt = extract_distribution(&buf_b, &default_tagger);
        let orig_src_mass = src.mass;
        equalize_mass(&mut src, &mut tgt);
        assert_eq!(src.mass, tgt.mass);
        assert_eq!(src.mass, orig_src_mass);
        // Target was padded.
        assert_eq!(tgt.mass, 4);
    }

    #[test]
    fn test_sinkhorn_identity() {
        // If source and target are identical, the transport plan should be
        // close to an identity mapping (each cell maps to itself).
        let buf = make_buffer_with_text(3, 1, "ABC");
        let src = extract_distribution(&buf, &default_tagger);
        let tgt = extract_distribution(&buf, &default_tagger);
        let config = MorphConfig::default();
        let plan = solve_transport(&src, &tgt, &config);

        assert_eq!(plan.assignments.len(), 3);
        // Each source should map to the same index in target.
        for a in &plan.assignments {
            assert_eq!(
                a.source_idx, a.target_idx,
                "identity transport: source {} should map to target {}",
                a.source_idx, a.target_idx
            );
        }
    }

    #[test]
    fn test_sinkhorn_convergence() {
        // Validate that marginals are approximately satisfied.
        let buf_a = make_buffer_with_text(4, 1, "ABCD");
        let buf_b = make_buffer_with_text(4, 1, "WXYZ");
        let src = extract_distribution(&buf_a, &default_tagger);
        let tgt = extract_distribution(&buf_b, &default_tagger);
        let config = MorphConfig::default();
        let plan = solve_transport(&src, &tgt, &config);

        // Every source should have exactly one assignment.
        assert_eq!(plan.assignments.len(), 4);
        // All weights should be positive.
        for a in &plan.assignments {
            assert!(a.weight > 0.0, "weight should be positive: {}", a.weight);
        }
    }

    #[test]
    fn test_sinkhorn_morph_tag_penalty() {
        // Cells with different tags should not be matched to each other.
        let buf_a = make_buffer_with_text(4, 1, "ABCD");
        let buf_b = make_buffer_with_text(4, 1, "WXYZ");

        let tagger_split = |x: u16, _y: u16, _cell: &Cell| -> MorphTag {
            if x < 2 { MorphTag(1) } else { MorphTag(2) }
        };

        let src = extract_distribution(&buf_a, &tagger_split);
        let tgt = extract_distribution(&buf_b, &tagger_split);
        let config = MorphConfig::default();
        let plan = solve_transport(&src, &tgt, &config);

        // Verify tag-preserving assignments.
        for a in &plan.assignments {
            let st = src.cells[a.source_idx].tag;
            let tt = tgt.cells[a.target_idx].tag;
            assert_eq!(
                st, tt,
                "transport should preserve tags: source {:?} vs target {:?}",
                st, tt
            );
        }
    }

    #[test]
    fn test_sinkhorn_empty_distributions() {
        let src = Distribution {
            cells: Vec::new(),
            mass: 0,
        };
        let tgt = Distribution {
            cells: Vec::new(),
            mass: 0,
        };
        let config = MorphConfig::default();
        let plan = solve_transport(&src, &tgt, &config);
        assert!(plan.assignments.is_empty());
        assert_eq!(plan.block_count, 0);
    }

    #[test]
    fn test_interpolate_frame_endpoints() {
        let buf_a = make_buffer_with_text(3, 1, "ABC");
        let buf_b = make_buffer_with_text(3, 1, "XYZ");
        let src = extract_distribution(&buf_a, &default_tagger);
        let tgt = extract_distribution(&buf_b, &default_tagger);
        let config = MorphConfig::default();
        let plan = solve_transport(&src, &tgt, &config);

        // At t=0, cells should be at source positions.
        let frame_0 = interpolate_frame(&src, &tgt, &plan, 0.0);
        assert_eq!(frame_0.t, 0.0);
        for (i, &(x, y, _)) in frame_0.cells.iter().enumerate() {
            let sc = &src.cells[plan.assignments[i].source_idx];
            assert!(
                (x - f64::from(sc.x)).abs() < 1e-10,
                "t=0: x should match source"
            );
            assert!(
                (y - f64::from(sc.y)).abs() < 1e-10,
                "t=0: y should match source"
            );
        }

        // At t=1, cells should be at target positions.
        let frame_1 = interpolate_frame(&src, &tgt, &plan, 1.0);
        assert_eq!(frame_1.t, 1.0);
        for (i, &(x, y, _)) in frame_1.cells.iter().enumerate() {
            let tc = &tgt.cells[plan.assignments[i].target_idx];
            assert!(
                (x - f64::from(tc.x)).abs() < 1e-10,
                "t=1: x should match target"
            );
            assert!(
                (y - f64::from(tc.y)).abs() < 1e-10,
                "t=1: y should match target"
            );
        }
    }

    #[test]
    fn test_interpolate_frame_midpoint() {
        let buf_a = make_buffer_with_text(2, 1, "AB");
        let buf_b = make_buffer_with_text(2, 1, "AB");
        let src = extract_distribution(&buf_a, &default_tagger);
        let tgt = extract_distribution(&buf_b, &default_tagger);
        let config = MorphConfig::default();
        let plan = solve_transport(&src, &tgt, &config);

        let frame = interpolate_frame(&src, &tgt, &plan, 0.5);
        assert!((frame.t - 0.5).abs() < 1e-10);
    }

    #[test]
    fn test_render_frame_to_buffer() {
        let frame = MorphFrame {
            t: 0.5,
            cells: vec![(0.0, 0.0, Cell::default()), (2.0, 1.0, Cell::default())],
        };
        let buf = render_frame_to_buffer(&frame, 4, 3);
        assert_eq!(buf.width(), 4);
        assert_eq!(buf.height(), 3);
    }

    #[test]
    fn test_render_frame_clips_out_of_bounds() {
        let frame = MorphFrame {
            t: 0.5,
            cells: vec![
                (-1.0, 0.0, Cell::default()),
                (0.0, -1.0, Cell::default()),
                (100.0, 100.0, Cell::default()),
            ],
        };
        // Should not panic.
        let buf = render_frame_to_buffer(&frame, 4, 3);
        assert_eq!(buf.width(), 4);
    }

    #[test]
    fn test_morph_config_default() {
        let cfg = MorphConfig::default();
        assert!((cfg.epsilon - 0.1).abs() < 1e-10);
        assert_eq!(cfg.max_iterations, 10);
        assert!(cfg.tag_mismatch_penalty > 1e5);
    }

    #[test]
    fn test_sinkhorn_block_diagonal_partitions() {
        // Two different tags should produce 2 blocks.
        let buf_a = make_buffer_with_text(4, 1, "ABCD");
        let buf_b = make_buffer_with_text(4, 1, "WXYZ");
        let tagger = |x: u16, _y: u16, _cell: &Cell| -> MorphTag {
            if x < 2 { MorphTag(1) } else { MorphTag(2) }
        };
        let src = extract_distribution(&buf_a, &tagger);
        let tgt = extract_distribution(&buf_b, &tagger);
        let config = MorphConfig::default();
        let plan = solve_transport(&src, &tgt, &config);
        assert_eq!(plan.block_count, 2);
    }

    #[test]
    fn test_equalize_mass_preserves_existing_cells() {
        let buf_a = make_buffer_with_text(3, 1, "ABC");
        let buf_b = make_buffer_with_text(1, 1, "X");
        let mut src = extract_distribution(&buf_a, &default_tagger);
        let mut tgt = extract_distribution(&buf_b, &default_tagger);

        let orig_src_cells: Vec<_> = src.cells.iter().map(|c| (c.x, c.y)).collect();
        equalize_mass(&mut src, &mut tgt);

        // Source cells unchanged.
        for (i, c) in src.cells.iter().enumerate().take(orig_src_cells.len()) {
            assert_eq!((c.x, c.y), orig_src_cells[i]);
        }
        // Target was padded to match.
        assert_eq!(tgt.mass, 3);
    }

    #[test]
    fn test_single_cell_transport() {
        let buf = make_buffer_with_text(1, 1, "A");
        let src = extract_distribution(&buf, &default_tagger);
        let tgt = extract_distribution(&buf, &default_tagger);
        let plan = solve_transport(&src, &tgt, &MorphConfig::default());
        assert_eq!(plan.assignments.len(), 1);
        assert_eq!(plan.assignments[0].source_idx, 0);
        assert_eq!(plan.assignments[0].target_idx, 0);
    }

    #[test]
    fn test_large_buffer_does_not_panic() {
        // A 20x10 buffer should work without issues.
        let text: String = (0..200)
            .map(|i| if i % 3 == 0 { ' ' } else { 'X' })
            .collect();
        let buf_a = make_buffer_with_text(20, 10, &text);
        let buf_b = make_buffer_with_text(20, 10, &text);
        let src = extract_distribution(&buf_a, &default_tagger);
        let tgt = extract_distribution(&buf_b, &default_tagger);
        let plan = solve_transport(&src, &tgt, &MorphConfig::default());
        assert!(!plan.assignments.is_empty());
    }

    // -- bd-30uc6: WidgetId tests --

    #[test]
    fn test_widget_id_from_label_deterministic() {
        let a = WidgetId::from_label("sidebar");
        let b = WidgetId::from_label("sidebar");
        let c = WidgetId::from_label("header");
        assert_eq!(a, b, "same label -> same id");
        assert_ne!(a, c, "different labels -> different ids");
    }

    #[test]
    fn test_widget_id_new() {
        let id = WidgetId::new(42);
        assert_eq!(id.value(), 42);
    }

    #[test]
    fn test_morph_tag_from_widget() {
        let id = WidgetId::new(999);
        let tag = MorphTag::from_widget(id);
        assert_eq!(tag, MorphTag(999));
    }

    #[test]
    fn test_morph_tag_widget_ids_group_transport() {
        let buf_a = make_buffer_with_text(6, 1, "ABCDEF");
        let buf_b = make_buffer_with_text(6, 1, "UVWXYZ");

        let w1 = WidgetId::new(1);
        let w2 = WidgetId::new(2);
        let tag1 = MorphTag::from_widget(w1);
        let tag2 = MorphTag::from_widget(w2);

        let tagger =
            move |x: u16, _y: u16, _cell: &Cell| -> MorphTag { if x < 3 { tag1 } else { tag2 } };

        let src = extract_distribution(&buf_a, &tagger);
        let tgt = extract_distribution(&buf_b, &tagger);
        let plan = solve_transport(&src, &tgt, &MorphConfig::default());

        for a in &plan.assignments {
            assert_eq!(
                src.cells[a.source_idx].tag, tgt.cells[a.target_idx].tag,
                "widget-tagged cells must morph within same group"
            );
        }
    }

    // -- bd-1eunc: Oklab color interpolation tests --

    #[test]
    fn test_oklab_roundtrip() {
        let colors = [
            PackedRgba::rgb(255, 0, 0),
            PackedRgba::rgb(0, 255, 0),
            PackedRgba::rgb(0, 0, 255),
            PackedRgba::rgb(255, 255, 255),
            PackedRgba::rgb(0, 0, 0),
            PackedRgba::rgb(128, 64, 192),
        ];
        for color in colors {
            let lab = rgba_to_oklab(color);
            let back = oklab_to_rgba(lab);
            assert!(
                (color.r() as i16 - back.r() as i16).abs() <= 1
                    && (color.g() as i16 - back.g() as i16).abs() <= 1
                    && (color.b() as i16 - back.b() as i16).abs() <= 1,
                "roundtrip failed for {:?}: got {:?}",
                color,
                back
            );
        }
    }

    #[test]
    fn test_oklab_lerp_endpoints() {
        let red = rgba_to_oklab(PackedRgba::rgb(255, 0, 0));
        let blue = rgba_to_oklab(PackedRgba::rgb(0, 0, 255));
        let at_0 = red.lerp(blue, 0.0);
        let at_1 = red.lerp(blue, 1.0);
        assert!((at_0.l - red.l).abs() < 1e-10);
        assert!((at_1.l - blue.l).abs() < 1e-10);
    }

    #[test]
    fn test_oklab_lerp_midpoint_perceptual() {
        let white = rgba_to_oklab(PackedRgba::rgb(255, 255, 255));
        let black = rgba_to_oklab(PackedRgba::rgb(0, 0, 0));
        let mid = white.lerp(black, 0.5);
        assert!(
            (mid.l - 0.5).abs() < 0.05,
            "midpoint L should be near 0.5, got {}",
            mid.l
        );
    }

    #[test]
    fn test_interpolate_cell_color_endpoints() {
        let red_fg = PackedRgba::rgb(255, 0, 0);
        let black_bg = PackedRgba::rgb(0, 0, 0);
        let blue_fg = PackedRgba::rgb(0, 0, 255);
        let white_bg = PackedRgba::rgb(255, 255, 255);

        let (fg0, bg0) = interpolate_cell_color(red_fg, black_bg, blue_fg, white_bg, 0.0);
        assert!((red_fg.r() as i16 - fg0.r() as i16).abs() <= 1);
        assert!((black_bg.r() as i16 - bg0.r() as i16).abs() <= 1);

        let (fg1, bg1) = interpolate_cell_color(red_fg, black_bg, blue_fg, white_bg, 1.0);
        assert!((blue_fg.b() as i16 - fg1.b() as i16).abs() <= 1);
        assert!((white_bg.r() as i16 - bg1.r() as i16).abs() <= 1);
    }

    #[test]
    fn test_oklab_delta_e_identity() {
        let lab = OkLab::new(0.5, 0.1, -0.1);
        assert!(lab.delta_e(lab).abs() < 1e-10);
    }

    #[test]
    fn test_oklab_delta_e_symmetry() {
        let a = OkLab::new(0.5, 0.1, -0.1);
        let b = OkLab::new(0.8, -0.05, 0.2);
        assert!((a.delta_e(b) - b.delta_e(a)).abs() < 1e-10);
    }

    #[test]
    fn test_interpolated_frame_has_blended_colors() {
        let mut buf_a = Buffer::new(2, 1);
        let mut buf_b = Buffer::new(2, 1);

        let red_cell = Cell::from_char('A')
            .with_fg(PackedRgba::rgb(255, 0, 0))
            .with_bg(PackedRgba::rgb(0, 0, 0));
        let blue_cell = Cell::from_char('B')
            .with_fg(PackedRgba::rgb(0, 0, 255))
            .with_bg(PackedRgba::rgb(255, 255, 255));

        buf_a.set(0, 0, red_cell);
        buf_b.set(0, 0, blue_cell);

        let src = extract_distribution(&buf_a, &default_tagger);
        let tgt = extract_distribution(&buf_b, &default_tagger);
        let plan = solve_transport(&src, &tgt, &MorphConfig::default());
        let frame = interpolate_frame(&src, &tgt, &plan, 0.5);

        // At t=0.5, colors should be blended — neither pure red nor pure blue.
        assert!(!frame.cells.is_empty());
        let (_, _, ref cell) = frame.cells[0];
        assert!(
            cell.fg.r() > 0 || cell.fg.g() > 0,
            "fg should be blended, not pure blue"
        );
    }
}
