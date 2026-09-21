#![forbid(unsafe_code)]
#![feature(portable_simd)]

//! Portable-SIMD kernels for FrankenTUI's two lane-parallel hot loops.
//!
//! # Role in FrankenTUI
//! Two places in the render path do the same narrow thing over a long run of
//! bytes: the diff compares one row of 16-byte cells against the previous
//! frame's row, and the width fast path asks whether a string is entirely
//! ASCII. Both are a wide equality test followed by "where was the first
//! difference", which is what SIMD lanes are for.
//!
//! # How it fits in the system
//! Every kernel here is safe `std::simd` and carries a `_scalar` twin with
//! identical semantics. The twins are not dead weight: they are what the
//! parity tests compare against, what the benches measure against, and what
//! callers use when the `simd` feature is off. Nothing in the workspace
//! depends on this crate unless that feature is enabled, so the scalar paths
//! stay the default until benches justify otherwise.
//!
//! # Why `u128` and not `Cell`
//! `ftui_render::Cell` is `#[repr(C, align(16))]` over four `u32` fields, so a
//! cell is bit-for-bit a `u128`. Keeping the kernels on `u128` leaves this
//! crate free of a dependency on the render crate, and leaves the conversion
//! (which must stay safe, so it composes the fields rather than transmuting)
//! on the caller's side.
//!
//! # Lane widths
//! The kernels use 512-bit vectors (`u64x8`, `u8x64`). That is wider than most
//! targets execute natively; `std::simd` splits them into whatever the target
//! has, which keeps one code path across x86-64 and aarch64 and gives the
//! compiler a full unrolled chunk to work with.
//!
//! # Which of these are worth calling
//! Measured 2026-09-18, full numbers in
//! `docs/perf/simd_kernels_2026-09-18.md`:
//!
//! - [`all_ascii`] and [`ascii_width`] beat their twins by 5x at 64 bytes and
//!   by 20-44x from a kilobyte up. Call these.
//! - [`first_mismatch_u128`] and [`rows_equal_u128`] are **3-4x slower** than
//!   their twins and are deliberately not wired into the diff. `#![forbid(unsafe_code)]`
//!   leaves no way to view `&[u128]` as lanes, so the kernel must build each
//!   vector with shifts and masks, while the scalar `a[i] != b[i]` over `u128`
//!   is already one 128-bit compare after LLVM is done with it. They are kept
//!   because the measurement is worth keeping, and because the parity tests
//!   over them are what prove the lane indexing is right.
//!
//! Prefer the `_scalar` twin for cell comparison. That is not a placeholder.

use std::simd::cmp::{SimdPartialEq, SimdPartialOrd};
use std::simd::{Mask, Simd, u8x64, u64x8};

/// Cells compared per vector chunk: eight `u64` lanes is four `u128` cells.
const CELLS_PER_CHUNK: usize = 4;

/// Bytes compared per vector chunk.
const BYTES_PER_CHUNK: usize = 64;

/// Index of the first element where `a` and `b` differ, or `None` when the
/// common prefix runs to the end of the shorter slice.
///
/// Only the first `a.len().min(b.len())` elements are examined; a length
/// difference past that point is not a mismatch this reports, because the
/// diff's callers size both rows to the same width and a shorter slice means
/// a clipped row rather than a changed cell.
///
/// # Examples
/// ```
/// # use ftui_simd::first_mismatch_u128;
/// assert_eq!(first_mismatch_u128(&[1, 2, 3], &[1, 9, 3]), Some(1));
/// assert_eq!(first_mismatch_u128(&[1, 2, 3], &[1, 2, 3]), None);
/// ```
#[must_use]
pub fn first_mismatch_u128(a: &[u128], b: &[u128]) -> Option<usize> {
    let len = a.len().min(b.len());
    let mut offset = 0;

    while offset + CELLS_PER_CHUNK <= len {
        let lhs = load_cells(&a[offset..offset + CELLS_PER_CHUNK]);
        let rhs = load_cells(&b[offset..offset + CELLS_PER_CHUNK]);
        let differing: Mask<i64, 8> = lhs.simd_ne(rhs);
        if differing.any() {
            // Each cell is two consecutive u64 lanes, so the first differing
            // lane identifies the cell by halving its index. Both halves of a
            // cell can differ; the earlier lane is the one `first_set` gives.
            let lane = differing.first_set().unwrap_or(0);
            return Some(offset + lane / 2);
        }
        offset += CELLS_PER_CHUNK;
    }

    // Tail shorter than one chunk.
    (offset..len).find(|&i| a[i] != b[i])
}

/// Scalar twin of [`first_mismatch_u128`], and the reference its parity tests
/// compare against.
#[must_use]
pub fn first_mismatch_u128_scalar(a: &[u128], b: &[u128]) -> Option<usize> {
    let len = a.len().min(b.len());
    (0..len).find(|&i| a[i] != b[i])
}

/// Whether two equal-length runs of cells are bitwise identical.
///
/// Returns `false` for slices of differing length: callers use this to decide
/// whether a row can be skipped entirely, and a row whose width changed cannot.
///
/// # Examples
/// ```
/// # use ftui_simd::rows_equal_u128;
/// assert!(rows_equal_u128(&[7, 7], &[7, 7]));
/// assert!(!rows_equal_u128(&[7, 7], &[7, 7, 7]));
/// ```
#[must_use]
pub fn rows_equal_u128(a: &[u128], b: &[u128]) -> bool {
    a.len() == b.len() && first_mismatch_u128(a, b).is_none()
}

/// Scalar twin of [`rows_equal_u128`].
#[must_use]
pub fn rows_equal_u128_scalar(a: &[u128], b: &[u128]) -> bool {
    a.len() == b.len() && first_mismatch_u128_scalar(a, b).is_none()
}

/// Whether every byte is ASCII, i.e. has its high bit clear.
///
/// This is the question the width fast path actually asks: if no byte is
/// continuation or lead, the run needs no Unicode width lookup and its display
/// width is its length.
///
/// # Examples
/// ```
/// # use ftui_simd::all_ascii;
/// assert!(all_ascii(b"plain text"));
/// assert!(!all_ascii("caf\u{e9}".as_bytes()));
/// assert!(all_ascii(b""));
/// ```
#[must_use]
pub fn all_ascii(bytes: &[u8]) -> bool {
    let mut offset = 0;

    while offset + BYTES_PER_CHUNK <= bytes.len() {
        let chunk = u8x64::from_slice(&bytes[offset..offset + BYTES_PER_CHUNK]);
        if (chunk & u8x64::splat(0x80)).simd_ne(u8x64::splat(0)).any() {
            return false;
        }
        offset += BYTES_PER_CHUNK;
    }

    bytes[offset..].iter().all(u8::is_ascii)
}

/// Scalar twin of [`all_ascii`].
#[must_use]
pub fn all_ascii_scalar(bytes: &[u8]) -> bool {
    bytes.iter().all(u8::is_ascii)
}

/// Display width of a run that is entirely printable ASCII, or `None`.
///
/// `None` means "this run needs the real width tables", and covers both
/// non-ASCII bytes and ASCII control characters: C0 (`0x00..=0x1f`) and DEL
/// (`0x7f`) have no single agreed display width, so they are handed back to
/// the caller rather than counted as one column each. Space through `~` are
/// one column apiece, so the width of an accepted run is its length.
///
/// # Examples
/// ```
/// # use ftui_simd::ascii_width;
/// assert_eq!(ascii_width(b"hello"), Some(5));
/// assert_eq!(ascii_width(b"tab\there"), None);
/// assert_eq!(ascii_width("\u{e9}".as_bytes()), None);
/// ```
#[must_use]
pub fn ascii_width(bytes: &[u8]) -> Option<usize> {
    let mut offset = 0;

    while offset + BYTES_PER_CHUNK <= bytes.len() {
        let chunk = u8x64::from_slice(&bytes[offset..offset + BYTES_PER_CHUNK]);
        // Printable ASCII is 0x20..=0x7e, so one unsigned-wrapping subtract
        // puts every acceptable byte in 0..=0x5e and everything else above it:
        // 0x7f wraps to 0x5f, and any high-bit byte lands at 0x60 or more.
        let shifted = chunk - u8x64::splat(0x20);
        if shifted.simd_gt(u8x64::splat(0x5e)).any() {
            return None;
        }
        offset += BYTES_PER_CHUNK;
    }

    if bytes[offset..].iter().all(|b| (0x20..=0x7e).contains(b)) {
        Some(bytes.len())
    } else {
        None
    }
}

/// Scalar twin of [`ascii_width`].
#[must_use]
pub fn ascii_width_scalar(bytes: &[u8]) -> Option<usize> {
    if bytes.iter().all(|b| (0x20..=0x7e).contains(b)) {
        Some(bytes.len())
    } else {
        None
    }
}

/// Reinterpret four `u128` cells as the eight `u64` lanes of one vector.
///
/// Splitting each cell into two `u64` halves keeps the whole thing in safe
/// code: there is no 128-bit lane type to compare with, and `u64` is the
/// widest lane every supported target handles well.
#[inline]
fn load_cells(cells: &[u128]) -> u64x8 {
    debug_assert_eq!(cells.len(), CELLS_PER_CHUNK);
    let mut lanes = [0_u64; 8];
    for (i, cell) in cells.iter().enumerate() {
        lanes[i * 2] = (*cell & u128::from(u64::MAX)) as u64;
        lanes[i * 2 + 1] = (*cell >> 64) as u64;
    }
    Simd::from_array(lanes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_mismatch_reports_none_for_identical_rows() {
        let row: Vec<u128> = (0..200).collect();
        assert_eq!(first_mismatch_u128(&row, &row), None);
        assert_eq!(first_mismatch_u128_scalar(&row, &row), None);
    }

    #[test]
    fn first_mismatch_finds_the_earliest_difference_at_every_position() {
        // Every index matters: inside the first chunk, straddling a chunk
        // boundary, and in the scalar tail.
        for len in [1_usize, 3, 4, 5, 8, 63, 64, 65, 200] {
            let base: Vec<u128> = (0..len as u128).collect();
            for idx in 0..len {
                let mut changed = base.clone();
                changed[idx] = u128::MAX;
                assert_eq!(
                    first_mismatch_u128(&base, &changed),
                    Some(idx),
                    "len {len}, idx {idx}"
                );
                assert_eq!(
                    first_mismatch_u128_scalar(&base, &changed),
                    Some(idx),
                    "scalar len {len}, idx {idx}"
                );
            }
        }
    }

    #[test]
    fn first_mismatch_detects_a_difference_in_either_half_of_a_cell() {
        // The low half alone, the high half alone, and both: all three must
        // report the same cell, since a cell is two lanes.
        for delta in [1_u128, 1_u128 << 64, (1_u128 << 64) | 1] {
            let base = vec![0_u128; 8];
            let mut changed = base.clone();
            changed[5] = delta;
            assert_eq!(
                first_mismatch_u128(&base, &changed),
                Some(5),
                "delta {delta}"
            );
        }
    }

    #[test]
    fn first_mismatch_stops_at_the_shorter_slice() {
        let long: Vec<u128> = (0..16).collect();
        let short = &long[..6];
        assert_eq!(first_mismatch_u128(&long, short), None);
        assert_eq!(first_mismatch_u128(short, &long), None);
    }

    #[test]
    fn first_mismatch_handles_empty_input() {
        assert_eq!(first_mismatch_u128(&[], &[]), None);
        assert_eq!(first_mismatch_u128(&[], &[1, 2]), None);
    }

    #[test]
    fn rows_equal_requires_matching_length() {
        assert!(rows_equal_u128(&[1, 2, 3], &[1, 2, 3]));
        assert!(!rows_equal_u128(&[1, 2, 3], &[1, 2]));
        assert!(!rows_equal_u128(&[1, 2, 3], &[1, 2, 4]));
        assert!(rows_equal_u128(&[], &[]));
    }

    #[test]
    fn all_ascii_agrees_with_the_scalar_twin_across_lengths() {
        for len in [0_usize, 1, 63, 64, 65, 127, 128, 1000] {
            let ascii = vec![b'a'; len];
            assert!(all_ascii(&ascii), "len {len}");
            assert_eq!(all_ascii(&ascii), all_ascii_scalar(&ascii));

            if len > 0 {
                // A single non-ASCII byte must be found wherever it sits.
                for idx in [0, len / 2, len - 1] {
                    let mut probe = ascii.clone();
                    probe[idx] = 0xC3;
                    assert!(!all_ascii(&probe), "len {len}, idx {idx}");
                    assert_eq!(all_ascii(&probe), all_ascii_scalar(&probe));
                }
            }
        }
    }

    #[test]
    fn all_ascii_accepts_control_bytes() {
        // Control characters are ASCII; only ascii_width is fussy about them.
        assert!(all_ascii(b"\x00\x09\x1b\x7f"));
    }

    #[test]
    fn ascii_width_counts_printable_runs_and_rejects_the_rest() {
        assert_eq!(ascii_width(b""), Some(0));
        assert_eq!(ascii_width(b" "), Some(1));
        assert_eq!(ascii_width(b"~"), Some(1));
        assert_eq!(ascii_width(b"hello world"), Some(11));

        let long = vec![b'x'; 300];
        assert_eq!(ascii_width(&long), Some(300));

        // The boundaries either side of printable ASCII, and a high byte.
        assert_eq!(ascii_width(b"\x1f"), None);
        assert_eq!(ascii_width(b"\x7f"), None);
        assert_eq!(ascii_width(b"\xc3\xa9"), None);
    }

    #[test]
    fn ascii_width_rejects_a_bad_byte_anywhere_including_past_a_full_chunk() {
        for len in [64_usize, 65, 129, 300] {
            for idx in [0, len / 2, len - 1] {
                for bad in [0x00_u8, 0x1f, 0x7f, 0x80, 0xff] {
                    let mut probe = vec![b'a'; len];
                    probe[idx] = bad;
                    assert_eq!(ascii_width(&probe), None, "len {len}, idx {idx}, bad {bad}");
                    assert_eq!(ascii_width(&probe), ascii_width_scalar(&probe));
                }
            }
        }
    }

    #[test]
    fn kernels_agree_with_their_twins_over_a_deterministic_sweep() {
        // A cheap xorshift keeps this reproducible without a dev-dependency.
        let mut state = 0x2545_F491_4F6C_DD1D_u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };

        for len in 0..200_usize {
            // Fill both halves of each cell. Widening a single u64 left every
            // base value with a zero high half, so a kernel that only ever
            // compared the low 64 bits of equal cells would have passed —
            // the perturbation below can flip a high bit, but two *equal*
            // cells were never wide.
            let a: Vec<u128> = (0..len)
                .map(|_| (u128::from(next()) << 64) | u128::from(next()))
                .collect();
            let mut b = a.clone();
            if len > 0 {
                let idx = (next() as usize) % len;
                // Half the time perturb a cell, half the time leave them equal.
                if next() % 2 == 0 {
                    b[idx] ^= 1 << (next() % 128);
                }
            }
            assert_eq!(
                first_mismatch_u128(&a, &b),
                first_mismatch_u128_scalar(&a, &b),
                "len {len}"
            );
            assert_eq!(rows_equal_u128(&a, &b), rows_equal_u128_scalar(&a, &b));

            let bytes: Vec<u8> = (0..len).map(|_| (next() % 256) as u8).collect();
            assert_eq!(all_ascii(&bytes), all_ascii_scalar(&bytes), "len {len}");
            assert_eq!(ascii_width(&bytes), ascii_width_scalar(&bytes), "len {len}");
        }
    }

    /// The sweep above draws bytes uniformly from `0..=255`, so a run of
    /// `BYTES_PER_CHUNK` bytes is entirely ASCII with probability 2^-64. Every
    /// chunk it ever evaluates therefore takes the rejecting branch on its
    /// first iteration: the accepting path through the chunk loop - advancing
    /// `offset`, and the tail that is measured from wherever it stopped - is
    /// never reached for any input long enough to have a chunk at all.
    ///
    /// These runs are printable ASCII by construction, then poisoned one byte
    /// at a time so the rejecting branch is exercised at every position rather
    /// than only near the front.
    #[test]
    fn ascii_kernels_agree_with_their_twins_on_runs_that_reach_the_chunk_loop() {
        // Past two full chunks, so a bad byte can land in the first chunk, a
        // later chunk, or the tail.
        for len in 0..=(2 * BYTES_PER_CHUNK + 5) {
            // 0x20..=0x7e, cycled: printable, and not a single repeated byte.
            let mut run: Vec<u8> = (0..len).map(|i| 0x20 + (i % 0x5f) as u8).collect();

            assert!(all_ascii(&run), "len {len}");
            assert_eq!(all_ascii(&run), all_ascii_scalar(&run), "len {len}");
            assert_eq!(ascii_width(&run), Some(len), "len {len}");
            assert_eq!(ascii_width(&run), ascii_width_scalar(&run), "len {len}");

            for pos in 0..len {
                // NUL and 0x1f are ASCII but not printable, so the two kernels
                // disagree with each other by design - each is compared only
                // against its own twin. 0x7f is the byte the wrapping subtract
                // in `ascii_width` folds to the top of the accepted range.
                for bad in [0x00_u8, 0x1f, 0x7f, 0x80, 0xff] {
                    let saved = run[pos];
                    run[pos] = bad;
                    assert_eq!(
                        all_ascii(&run),
                        all_ascii_scalar(&run),
                        "all_ascii len {len} pos {pos} byte {bad:#04x}"
                    );
                    assert_eq!(
                        ascii_width(&run),
                        ascii_width_scalar(&run),
                        "ascii_width len {len} pos {pos} byte {bad:#04x}"
                    );
                    run[pos] = saved;
                }
            }
        }
    }
}
