//! Production render certificates (bd-6b9nr): explicit, named skip
//! decisions for the diff stage — never opaque heuristics.
//!
//! The runtime's `TerminalWriter` builds a [`RenderCertificateInputs`] from
//! facts it can prove locally each frame and asks
//! [`evaluate_render_certificate`] for a decision. The certificate maps to a
//! [`DiffSkipHint`] consumed by
//! [`BufferDiff::compute_certified_into`](crate::diff::BufferDiff::compute_certified_into):
//!
//! - `FullRequired` — no previous frame, viewport change, or a due
//!   full-redraw probe: a full-fidelity pass is mandatory;
//! - `SkipAll` — zero dirty rows: the frame provably introduces no cell
//!   changes relative to the tracked baseline, so the diff scan is skipped
//!   entirely;
//! - `NarrowToDirty` — the diff scan is narrowed to exactly the dirty rows
//!   (soundness inherited from the buffer's dirty-tracking invariant:
//!   dirty rows ⊇ changed rows, which in turn requires that the buffer's
//!   dirty state was cleared while its content was identical to the diff's
//!   `old` baseline — see `BufferDiff::compute_dirty`'s precondition; the
//!   production writer guarantees this by diffing consecutive frames of one
//!   buffer lineage).
//!
//! The decision tree is deliberately conservative and fail-open: any
//! condition that cannot be proven falls back to `FullRequired`. Every
//! certificate names its causes so evidence logs explain *why* work was
//! skipped or performed (the mirror of this model used by the offline
//! gauntlet lives in `ftui-harness::render_certificate`; this module is the
//! production-side evaluator, kept in `ftui-render` because the harness is
//! downstream of the runtime).

use crate::buffer::Buffer;
use crate::diff::DiffSkipHint;

/// Facts the writer can prove about the frame before diffing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderCertificateInputs {
    /// A previous frame exists to diff against.
    pub prev_available: bool,
    /// The viewport dimensions changed since the previous frame.
    pub dims_changed: bool,
    /// A periodic full-redraw probe is due.
    pub full_redraw_due: bool,
    /// Rows marked dirty by the buffer's row-level tracking invariant.
    pub dirty_row_count: usize,
    /// Independent dirty-row count derived from per-row dirty spans.
    pub span_dirty_rows: usize,
    /// Whether the buffer's dirty baseline and internal dirty bitmap/spans are valid for `prev`.
    pub baseline_valid: bool,
    /// Total rows in the frame.
    pub total_rows: u16,
}

impl RenderCertificateInputs {
    /// Build certificate inputs by cross-checking `current`'s dirty state against `prev`.
    #[must_use]
    pub fn from_buffers(prev: Option<&Buffer>, current: &Buffer, full_redraw_due: bool) -> Self {
        let prev_available = prev.is_some();
        let dims_changed =
            prev.is_some_and(|p| p.width() != current.width() || p.height() != current.height());
        let dirty_row_count = current.dirty_row_count();
        let span_dirty_rows = current.dirty_span_row_count();
        let baseline_valid = match prev {
            Some(p) if !dims_changed => current.is_dirty_witness_valid_for(p),
            _ => false,
        };
        Self {
            prev_available,
            dims_changed,
            full_redraw_due,
            dirty_row_count,
            span_dirty_rows,
            baseline_valid,
            total_rows: current.height(),
        }
    }
}

/// The certified level of work elimination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RenderCertificateLevel {
    /// Full-fidelity work is required; nothing may be skipped.
    FullRequired,
    /// The diff scan is skipped entirely (no dirty rows).
    SkipAll,
    /// The diff scan is narrowed to the dirty rows only.
    NarrowToDirty,
}

impl RenderCertificateLevel {
    /// Stable lowercase tag for evidence logs.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::FullRequired => "full-required",
            Self::SkipAll => "skip-all",
            Self::NarrowToDirty => "narrow-to-dirty",
        }
    }
}

/// An explicit, explainable skip decision for one frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderCertificate {
    /// The certified level.
    pub level: RenderCertificateLevel,
    /// Named causes behind the decision (never empty).
    pub causes: Vec<&'static str>,
    /// Rows the certificate narrows to (empty unless `NarrowToDirty`).
    pub dirty_rows: Vec<u16>,
    /// Whether the evaluator fell back to full work out of caution.
    pub fell_back: bool,
}

impl RenderCertificate {
    /// Translate the certificate into the diff-stage hint.
    #[must_use]
    pub fn to_hint(&self) -> DiffSkipHint {
        match self.level {
            RenderCertificateLevel::FullRequired => DiffSkipHint::FullDiff,
            RenderCertificateLevel::SkipAll => DiffSkipHint::SkipDiff,
            RenderCertificateLevel::NarrowToDirty => {
                DiffSkipHint::NarrowToRows(self.dirty_rows.clone())
            }
        }
    }

    /// Consume the certificate into the diff-stage hint without cloning `dirty_rows`.
    #[must_use]
    pub fn into_hint(self) -> DiffSkipHint {
        match self.level {
            RenderCertificateLevel::FullRequired => DiffSkipHint::FullDiff,
            RenderCertificateLevel::SkipAll => DiffSkipHint::SkipDiff,
            RenderCertificateLevel::NarrowToDirty => DiffSkipHint::NarrowToRows(self.dirty_rows),
        }
    }

    /// Compact JSON fragment for evidence lines (stable field order).
    #[must_use]
    pub fn to_evidence_json(&self) -> String {
        format!(
            r#"{{"level":"{}","causes":[{}],"narrowed_rows":{},"fell_back":{}}}"#,
            self.level.label(),
            self.causes
                .iter()
                .map(|c| format!("\"{c}\""))
                .collect::<Vec<_>>()
                .join(","),
            self.dirty_rows.len(),
            self.fell_back
        )
    }
}

/// Evaluate the conservative production decision tree.
///
/// `dirty_rows` must be the buffer's dirty row indices in strictly ascending order;
/// it is cross-checked against `inputs.dirty_row_count`, `inputs.span_dirty_rows`,
/// and `inputs.baseline_valid`. Any disagreement falls back to `FullRequired`.
#[must_use]
pub fn evaluate_render_certificate(
    inputs: &RenderCertificateInputs,
    dirty_rows: Vec<u16>,
) -> RenderCertificate {
    if !inputs.prev_available {
        return RenderCertificate {
            level: RenderCertificateLevel::FullRequired,
            causes: vec!["no-previous-frame"],
            dirty_rows: Vec::new(),
            fell_back: true,
        };
    }
    if inputs.dims_changed {
        return RenderCertificate {
            level: RenderCertificateLevel::FullRequired,
            causes: vec!["viewport-changed"],
            dirty_rows: Vec::new(),
            fell_back: true,
        };
    }
    if inputs.full_redraw_due {
        return RenderCertificate {
            level: RenderCertificateLevel::FullRequired,
            causes: vec!["full-redraw-probe-due"],
            dirty_rows: Vec::new(),
            fell_back: true,
        };
    }
    // Cross-check the row witness against independent span/bitmap/baseline
    // facts BEFORE certifying a skip or narrow scan.
    let rows_sorted_unique = dirty_rows.windows(2).all(|pair| pair[0] < pair[1]);
    let rows_in_bounds = dirty_rows.iter().all(|&row| row < inputs.total_rows);
    if !inputs.baseline_valid
        || inputs.span_dirty_rows != inputs.dirty_row_count
        || dirty_rows.len() != inputs.dirty_row_count
        || !rows_sorted_unique
        || !rows_in_bounds
    {
        return RenderCertificate {
            level: RenderCertificateLevel::FullRequired,
            causes: vec!["dirty-row-witness-inconsistent"],
            dirty_rows: Vec::new(),
            fell_back: true,
        };
    }
    if inputs.dirty_row_count == 0 {
        return RenderCertificate {
            level: RenderCertificateLevel::SkipAll,
            causes: vec!["zero-dirty-rows"],
            dirty_rows: Vec::new(),
            fell_back: false,
        };
    }
    RenderCertificate {
        level: RenderCertificateLevel::NarrowToDirty,
        causes: vec!["dirty-rows-witnessed"],
        dirty_rows,
        fell_back: false,
    }
}

/// Evaluate a render certificate directly for a `(prev, current)` buffer pair.
#[must_use]
pub fn evaluate_render_certificate_for_buffers(
    prev: Option<&Buffer>,
    current: &Buffer,
    full_redraw_due: bool,
) -> RenderCertificate {
    let Some(prev_buf) = prev else {
        return RenderCertificate {
            level: RenderCertificateLevel::FullRequired,
            causes: vec!["no-previous-frame"],
            dirty_rows: Vec::new(),
            fell_back: true,
        };
    };
    if prev_buf.width() != current.width() || prev_buf.height() != current.height() {
        return RenderCertificate {
            level: RenderCertificateLevel::FullRequired,
            causes: vec!["viewport-changed"],
            dirty_rows: Vec::new(),
            fell_back: true,
        };
    };
    if full_redraw_due {
        return RenderCertificate {
            level: RenderCertificateLevel::FullRequired,
            causes: vec!["full-redraw-probe-due"],
            dirty_rows: Vec::new(),
            fell_back: true,
        };
    }
    let (dirty_row_count, span_dirty_rows, baseline_valid, dirty_rows) =
        current.collect_dirty_witness_for(prev_buf);
    if !baseline_valid || span_dirty_rows != dirty_row_count {
        return RenderCertificate {
            level: RenderCertificateLevel::FullRequired,
            causes: vec!["dirty-row-witness-inconsistent"],
            dirty_rows: Vec::new(),
            fell_back: true,
        };
    }
    if dirty_row_count == 0 {
        return RenderCertificate {
            level: RenderCertificateLevel::SkipAll,
            causes: vec!["zero-dirty-rows"],
            dirty_rows: Vec::new(),
            fell_back: false,
        };
    }
    RenderCertificate {
        level: RenderCertificateLevel::NarrowToDirty,
        causes: vec!["dirty-rows-witnessed"],
        dirty_rows,
        fell_back: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Buffer;
    use crate::cell::Cell;
    use crate::diff::BufferDiff;

    fn inputs(dirty: usize, rows: u16) -> RenderCertificateInputs {
        RenderCertificateInputs {
            prev_available: true,
            dims_changed: false,
            full_redraw_due: false,
            dirty_row_count: dirty,
            span_dirty_rows: dirty,
            baseline_valid: true,
            total_rows: rows,
        }
    }

    #[test]
    fn unprovable_conditions_force_full_work() {
        let mut no_prev = inputs(3, 10);
        no_prev.prev_available = false;
        let cert = evaluate_render_certificate(&no_prev, vec![1, 2, 3]);
        assert_eq!(cert.level, RenderCertificateLevel::FullRequired);
        assert!(cert.fell_back);
        assert_eq!(cert.causes, vec!["no-previous-frame"]);

        let mut resized = inputs(3, 10);
        resized.dims_changed = true;
        let cert = evaluate_render_certificate(&resized, vec![1, 2, 3]);
        assert_eq!(cert.causes, vec!["viewport-changed"]);

        let mut probe = inputs(3, 10);
        probe.full_redraw_due = true;
        let cert = evaluate_render_certificate(&probe, vec![1, 2, 3]);
        assert_eq!(cert.causes, vec!["full-redraw-probe-due"]);
    }

    #[test]
    fn zero_dirty_rows_certifies_a_skip() {
        let cert = evaluate_render_certificate(&inputs(0, 10), Vec::new());
        assert_eq!(cert.level, RenderCertificateLevel::SkipAll);
        assert!(!cert.fell_back);
        assert!(matches!(cert.to_hint(), DiffSkipHint::SkipDiff));
    }

    #[test]
    fn dirty_rows_certify_a_narrow_scan() {
        let cert = evaluate_render_certificate(&inputs(2, 10), vec![3, 7]);
        assert_eq!(cert.level, RenderCertificateLevel::NarrowToDirty);
        match cert.to_hint() {
            DiffSkipHint::NarrowToRows(rows) => assert_eq!(rows, vec![3, 7]),
            other => panic!("expected narrow hint, got {other:?}"),
        }
    }

    #[test]
    fn inconsistent_witness_falls_back_to_full() {
        // Count mismatch.
        let cert = evaluate_render_certificate(&inputs(2, 10), vec![3]);
        assert_eq!(cert.level, RenderCertificateLevel::FullRequired);
        assert!(cert.fell_back);
        // Out-of-bounds row.
        let cert = evaluate_render_certificate(&inputs(1, 10), vec![10]);
        assert_eq!(cert.level, RenderCertificateLevel::FullRequired);
        assert_eq!(cert.causes, vec!["dirty-row-witness-inconsistent"]);
        // Unsorted / duplicate rows.
        let cert = evaluate_render_certificate(&inputs(2, 10), vec![5, 2]);
        assert_eq!(cert.level, RenderCertificateLevel::FullRequired);
        assert_eq!(cert.causes, vec!["dirty-row-witness-inconsistent"]);
        let cert = evaluate_render_certificate(&inputs(2, 10), vec![3, 3]);
        assert_eq!(cert.level, RenderCertificateLevel::FullRequired);
        // Span-vs-row count disagreement.
        let mut desync = inputs(2, 10);
        desync.span_dirty_rows = 1;
        let cert = evaluate_render_certificate(&desync, vec![3, 7]);
        assert_eq!(cert.level, RenderCertificateLevel::FullRequired);
        assert_eq!(cert.causes, vec!["dirty-row-witness-inconsistent"]);
        // Invalid baseline even when dirty_row_count == 0.
        let mut stale_zero = inputs(0, 10);
        stale_zero.baseline_valid = false;
        let cert = evaluate_render_certificate(&stale_zero, Vec::new());
        assert_eq!(cert.level, RenderCertificateLevel::FullRequired);
        assert!(cert.fell_back);
        assert_eq!(cert.causes, vec!["dirty-row-witness-inconsistent"]);
    }

    #[test]
    fn stale_dirty_bits_on_buffer_pair_fall_back_and_find_all_changes() {
        let mut old = Buffer::new(16, 6);
        old.set(2, 1, Cell::from_char('A'));
        old.clear_dirty();

        let mut new = old.clone();
        new.clear_dirty();
        // Mutate row 3, then wipe dirty state so row 3 is stale-clean!
        new.set(7, 3, Cell::from_char('X'));
        new.clear_dirty();
        // Mutate row 5 so only row 5 appears dirty in `new.dirty_row_indices()`.
        new.set(1, 5, Cell::from_char('Y'));

        let cert = evaluate_render_certificate_for_buffers(Some(&old), &new, false);
        assert_eq!(cert.level, RenderCertificateLevel::FullRequired);
        assert!(cert.fell_back);
        assert_eq!(cert.causes, vec!["dirty-row-witness-inconsistent"]);

        let mut certified = BufferDiff::new();
        certified.compute_certified_into(&old, &new, cert.to_hint());
        let mut full = BufferDiff::new();
        full.compute_into(&old, &new);
        assert_eq!(certified.changes(), full.changes());
        assert_eq!(certified.changes(), &[(7, 3), (1, 5)]);
    }

    #[test]
    fn evidence_json_is_stable_and_named() {
        let cert = evaluate_render_certificate(&inputs(2, 10), vec![3, 7]);
        let json = cert.to_evidence_json();
        assert_eq!(
            json,
            r#"{"level":"narrow-to-dirty","causes":["dirty-rows-witnessed"],"narrowed_rows":2,"fell_back":false}"#
        );
    }

    /// The certified path must produce exactly the change set of the
    /// uncertified dirty path across generated frames (behavior
    /// preservation, deterministic xorshift corpus).
    #[test]
    fn certified_changes_equal_dirty_changes_across_generated_frames() {
        let (w, h) = (24u16, 8u16);
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };

        for round in 0..50 {
            let old = Buffer::new(w, h);
            let mut new = Buffer::new(w, h);
            new.clear_dirty();

            let mutations = (next() % 20) as usize;
            for _ in 0..mutations {
                let x = (next() % u64::from(w)) as u16;
                let y = (next() % u64::from(h)) as u16;
                let ch = char::from(b'A' + (next() % 26) as u8);
                new.set(x, y, Cell::from_char(ch));
            }

            let cert = evaluate_render_certificate_for_buffers(Some(&old), &new, false);
            assert!(!cert.fell_back, "round {round}: unexpected fallback");

            let mut certified = BufferDiff::new();
            certified.compute_certified_into(&old, &new, cert.to_hint());
            let mut truth = BufferDiff::new();
            truth.compute_dirty_into(&old, &new);
            assert_eq!(
                certified.changes(),
                truth.changes(),
                "round {round}: certified path diverged ({} mutations)",
                mutations
            );
        }
    }
}
