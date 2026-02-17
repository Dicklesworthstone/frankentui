//! Convert ftui-render Buffers and BufferDiffs into WebGPU CellPatches.
//!
//! This module bridges the ftui rendering pipeline to the FrankenTerm WebGPU
//! renderer. The key entry points are:
//!
//! - [`cell_from_render`]: convert a single ftui-render `Cell` to a GPU `CellData`.
//! - [`diff_to_patches`]: convert a `BufferDiff` into contiguous `CellPatch` spans.
//! - [`full_buffer_patch`]: produce a single patch covering the entire buffer.
//!
//! The output patches are ready for `WebGpuRenderer::apply_patches()`.

use crate::renderer::{CellData, CellPatch};
use frankenterm_core::{Cell as CoreCell, Color as CoreColor, Patch as CorePatch};
use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, CellAttrs, CellContent};
use ftui_render::diff::BufferDiff;

/// Convert a single ftui-render `Cell` to a GPU-ready `CellData`.
///
/// Color mapping: `PackedRgba(u32)` is passed through directly (same
/// encoding: R in high byte, A in low byte).
///
/// Glyph ID: for direct chars we pass the Unicode codepoint to the renderer,
/// which resolves atlas slots lazily.
///
/// Grapheme-cluster cells currently use a deterministic visible placeholder
/// because the web patch feed does not yet carry grapheme-pool payloads.
/// This is a graceful fallback until full cluster support lands.
///
/// Attributes: lower 8 bits store style flags; upper 24 bits store hyperlink ID.
const GRAPHEME_FALLBACK_CODEPOINT: u32 = '□' as u32;
const ATTR_STYLE_MASK: u32 = 0xFF;
const ATTR_LINK_ID_MAX: u32 = CellAttrs::LINK_ID_MAX;
const DEFAULT_FG_RGBA: u32 = 0xFFFF_FFFF;
const DEFAULT_BG_RGBA: u32 = 0x0000_00FF;
const ANSI16_RGBA: [u32; 16] = [
    0x0000_00FF,
    0xCD00_00FF,
    0x00CD_00FF,
    0xCDCD_00FF,
    0x0000_EEFF,
    0xCD00_CDFF,
    0x00CD_CDFF,
    0xE5E5_E5FF,
    0x7F7F_7FFF,
    0xFF00_00FF,
    0x00FF_00FF,
    0xFFFF_00FF,
    0x5C5C_FFFF,
    0xFF00_FFFF,
    0x00FF_FFFF,
    0xFFFF_FFFF,
];

#[must_use]
pub fn cell_from_render(cell: &Cell) -> CellData {
    let glyph_id = match cell.content {
        CellContent::EMPTY | CellContent::CONTINUATION => 0,
        other if other.is_grapheme() => GRAPHEME_FALLBACK_CODEPOINT,
        other => other.as_char().map_or(0, |c| c as u32),
    };

    CellData {
        bg_rgba: cell.bg.0,
        fg_rgba: cell.fg.0,
        glyph_id,
        attrs: pack_cell_attrs(cell),
    }
}

#[must_use]
fn pack_cell_attrs(cell: &Cell) -> u32 {
    let style_bits = u32::from(cell.attrs.flags().bits()) & ATTR_STYLE_MASK;
    let link_id = cell.attrs.link_id().min(ATTR_LINK_ID_MAX);
    style_bits | (link_id << 8)
}

/// Convert a single `frankenterm-core` cell to GPU-ready `CellData`.
///
/// This powers the VT byte-stream (`TerminalEngine`) feed path used by wasm.
#[must_use]
pub fn cell_from_core(cell: &CoreCell) -> CellData {
    let glyph_id = if cell.is_wide_continuation() || cell.content() == ' ' {
        0
    } else {
        u32::from(cell.content())
    };
    let style_bits = u32::from(cell.attrs.flags.bits()) & ATTR_STYLE_MASK;
    let link_id = u32::from(cell.hyperlink);

    CellData {
        bg_rgba: core_color_to_rgba(cell.attrs.bg, DEFAULT_BG_RGBA),
        fg_rgba: core_color_to_rgba(cell.attrs.fg, DEFAULT_FG_RGBA),
        glyph_id,
        attrs: style_bits | (link_id << 8),
    }
}

/// Convert a `frankenterm-core` patch into contiguous `CellPatch` runs.
///
/// Input updates are expected to be row-major ordered; this function coalesces
/// contiguous linear offsets to minimize patch upload calls.
#[must_use]
pub fn core_patch_to_patches(patch: &CorePatch) -> Vec<CellPatch> {
    if patch.updates.is_empty() || patch.cols == 0 || patch.rows == 0 {
        return Vec::new();
    }

    let cols = u32::from(patch.cols);
    let mut patches = Vec::with_capacity(patch.updates.len().div_ceil(8).max(1));
    let mut span_start = 0u32;
    let mut span_cells: Vec<CellData> = Vec::new();
    let mut prev_offset = 0u32;
    let mut has_span = false;

    for update in &patch.updates {
        if update.row >= patch.rows || update.col >= patch.cols {
            continue;
        }
        let offset = u32::from(update.row)
            .saturating_mul(cols)
            .saturating_add(u32::from(update.col));
        let cell = cell_from_core(&update.cell);

        if !has_span {
            span_start = offset;
            prev_offset = offset;
            has_span = true;
            span_cells.push(cell);
            continue;
        }

        if offset == prev_offset {
            if let Some(last) = span_cells.last_mut() {
                *last = cell;
            }
            continue;
        }

        if offset == prev_offset + 1 {
            span_cells.push(cell);
            prev_offset = offset;
            continue;
        }

        patches.push(CellPatch {
            offset: span_start,
            cells: std::mem::take(&mut span_cells),
        });
        span_start = offset;
        prev_offset = offset;
        span_cells.push(cell);
    }

    if !span_cells.is_empty() {
        patches.push(CellPatch {
            offset: span_start,
            cells: span_cells,
        });
    }

    patches
}

/// Convert a `BufferDiff` into contiguous `CellPatch` spans for GPU upload.
///
/// Adjacent dirty cells are coalesced into a single patch to minimize
/// `queue.write_buffer` calls. The output is sorted by linear offset.
///
/// Returns an empty vec if the diff is empty.
#[must_use]
pub fn diff_to_patches(buffer: &Buffer, diff: &BufferDiff) -> Vec<CellPatch> {
    if diff.is_empty() {
        return Vec::new();
    }

    // BufferDiff changes are produced by a row-major scan and are therefore
    // already sorted by (y, x) which is also linear-offset order.
    let width = buffer.width();
    let height = buffer.height();
    let cols = u32::from(width);

    // Heuristic: most sparse diffs produce one patch per ~8 dirty cells.
    let est_patches = diff.len().div_ceil(8).max(1);
    let mut patches = Vec::with_capacity(est_patches);
    let mut span_start: u32 = 0;
    let mut span_cells: Vec<CellData> = Vec::with_capacity(diff.len());
    let mut prev_offset: u32 = 0;
    let mut has_span = false;

    for &(x, y) in diff.changes() {
        // Safety: diffs can become stale across resize; fall back to a full patch.
        if x >= width || y >= height {
            return vec![full_buffer_patch(buffer)];
        }
        let offset = u32::from(y) * cols + u32::from(x);

        if !has_span {
            span_start = offset;
            prev_offset = offset;
            has_span = true;
            span_cells.push(cell_at_xy(buffer, x, y));
            continue;
        }

        if offset == prev_offset {
            // Defensive: ignore duplicates (shouldn't happen, but keep output stable).
            continue;
        }

        if offset == prev_offset + 1 {
            span_cells.push(cell_at_xy(buffer, x, y));
        } else {
            patches.push(CellPatch {
                offset: span_start,
                cells: std::mem::take(&mut span_cells),
            });
            span_start = offset;
            span_cells.push(cell_at_xy(buffer, x, y));
        }

        prev_offset = offset;
    }

    if !span_cells.is_empty() {
        patches.push(CellPatch {
            offset: span_start,
            cells: span_cells,
        });
    }

    patches
}

/// Produce a single patch covering the entire buffer.
///
/// Used for full repaints (first frame, after resize, etc.).
#[must_use]
pub fn full_buffer_patch(buffer: &Buffer) -> CellPatch {
    let cols = buffer.width();
    let rows = buffer.height();
    let total = cols as usize * rows as usize;

    let mut cells = Vec::with_capacity(total);
    for y in 0..rows {
        for x in 0..cols {
            // `x`,`y` are within bounds by construction (0..cols/rows).
            cells.push(cell_from_render(buffer.get_unchecked(x, y)));
        }
    }

    CellPatch { offset: 0, cells }
}

/// Aggregate upload stats for a batch of patches.
///
/// These values are useful for deterministic frame harness logging without
/// having to duplicate accounting logic at call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatchBatchStats {
    /// Number of logical dirty cells represented by the patch batch.
    pub dirty_cells: u32,
    /// Number of contiguous patch runs.
    pub patch_count: u32,
    /// Total bytes expected to be uploaded for the patch payload.
    pub bytes_uploaded: u64,
}

/// Compute aggregate stats for a patch batch.
#[must_use]
pub fn patch_batch_stats(patches: &[CellPatch]) -> PatchBatchStats {
    let dirty_cells_u64 = patches
        .iter()
        .map(|patch| patch.cells.len() as u64)
        .sum::<u64>();
    let patch_count = patches.len().min(u32::MAX as usize) as u32;
    let dirty_cells = dirty_cells_u64.min(u64::from(u32::MAX)) as u32;
    let bytes_uploaded = dirty_cells_u64.saturating_mul(std::mem::size_of::<CellData>() as u64);

    PatchBatchStats {
        dirty_cells,
        patch_count,
        bytes_uploaded,
    }
}

fn cell_at_xy(buffer: &Buffer, x: u16, y: u16) -> CellData {
    debug_assert!(x < buffer.width(), "diff x out of bounds");
    debug_assert!(y < buffer.height(), "diff y out of bounds");
    cell_from_render(buffer.get_unchecked(x, y))
}

fn core_color_to_rgba(color: CoreColor, default: u32) -> u32 {
    match color {
        CoreColor::Default => default,
        CoreColor::Named(idx) => ANSI16_RGBA[usize::from(idx.min(15))],
        CoreColor::Indexed(idx) => indexed_color_to_rgba(idx),
        CoreColor::Rgb(r, g, b) => pack_rgba(r, g, b),
    }
}

fn indexed_color_to_rgba(index: u8) -> u32 {
    if index < 16 {
        return ANSI16_RGBA[usize::from(index)];
    }

    if index < 232 {
        let base = index - 16;
        let r = base / 36;
        let g = (base % 36) / 6;
        let b = base % 6;
        const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
        return pack_rgba(
            LEVELS[usize::from(r)],
            LEVELS[usize::from(g)],
            LEVELS[usize::from(b)],
        );
    }

    // Grayscale ramp [232..255] => [8, 18, ..., 238]
    let gray = 8u8.saturating_add((index - 232).saturating_mul(10));
    pack_rgba(gray, gray, gray)
}

fn pack_rgba(r: u8, g: u8, b: u8) -> u32 {
    (u32::from(r) << 24) | (u32::from(g) << 16) | (u32::from(b) << 8) | 0xFF
}

#[cfg(test)]
mod tests {
    use super::*;
    use frankenterm_core::{
        Cell as CoreCell, Color as CoreColor, Patch as CorePatch, SgrAttrs as CoreSgrAttrs,
        SgrFlags as CoreSgrFlags, TerminalEngine,
    };
    use ftui_render::cell::{CellAttrs, GraphemeId, PackedRgba, StyleFlags};

    fn make_cell(ch: char, fg: u32, bg: u32, flags: StyleFlags) -> Cell {
        Cell {
            content: CellContent::from_char(ch),
            fg: PackedRgba(fg),
            bg: PackedRgba(bg),
            attrs: CellAttrs::new(flags, 0),
        }
    }

    fn unicode_fixture_row() -> [Cell; 8] {
        [
            Cell::from_char('A').with_attrs(CellAttrs::new(StyleFlags::BOLD, 0x12)),
            Cell::from_char('界'),
            Cell::from_char('\u{0301}'), // combining acute accent
            Cell::from_char('\u{FE0F}'), // VS16
            Cell::from_char('\u{200D}'), // ZWJ
            Cell::from_char('🧪'),
            Cell {
                content: CellContent::from_grapheme(GraphemeId::new(11, 2)),
                ..Cell::default()
            },
            Cell::CONTINUATION,
        ]
    }

    fn apply_cell_patches_to_shadow(cols: u16, rows: u16, patches: &[CellPatch]) -> Vec<CellData> {
        let total = usize::from(cols) * usize::from(rows);
        let mut shadow = vec![CellData::EMPTY; total];
        for patch in patches {
            let start = usize::try_from(patch.offset).unwrap_or(total).min(total);
            let count = patch.cells.len().min(total.saturating_sub(start));
            for (idx, cell) in patch.cells.iter().take(count).enumerate() {
                shadow[start + idx] = *cell;
            }
        }
        shadow
    }

    fn shadow_row_text(shadow: &[CellData], cols: u16, row: u16) -> String {
        let cols_usize = usize::from(cols);
        let start = usize::from(row) * cols_usize;
        let end = (start + cols_usize).min(shadow.len());
        shadow[start..end]
            .iter()
            .map(|cell| {
                if cell.glyph_id == 0 {
                    ' '
                } else {
                    char::from_u32(cell.glyph_id).unwrap_or('□')
                }
            })
            .collect()
    }

    #[test]
    fn cell_from_render_basic() {
        let cell = make_cell('A', 0xFFFFFFFF, 0x000000FF, StyleFlags::empty());
        let gpu = cell_from_render(&cell);
        assert_eq!(gpu.bg_rgba, 0x000000FF);
        assert_eq!(gpu.fg_rgba, 0xFFFFFFFF);
        assert_eq!(gpu.glyph_id, 'A' as u32);
        assert_eq!(gpu.attrs, 0);
    }

    #[test]
    fn cell_from_render_with_attrs() {
        let flags = StyleFlags::BOLD | StyleFlags::UNDERLINE;
        let cell = make_cell('X', 0xFF0000FF, 0x00FF00FF, flags);
        let gpu = cell_from_render(&cell);
        assert_eq!(gpu.attrs & ATTR_STYLE_MASK, flags.bits() as u32);
        assert_eq!(gpu.attrs >> 8, 0);
        assert_ne!(gpu.attrs, 0);
    }

    #[test]
    fn cell_from_render_packs_style_and_link_id() {
        let flags = StyleFlags::ITALIC | StyleFlags::UNDERLINE;
        let link_id = 0x000A_BCDE;
        let cell = Cell::from_char('L')
            .with_fg(PackedRgba::rgb(255, 255, 255))
            .with_bg(PackedRgba::rgb(0, 0, 0))
            .with_attrs(CellAttrs::new(flags, link_id));

        let gpu = cell_from_render(&cell);
        assert_eq!(gpu.attrs & ATTR_STYLE_MASK, flags.bits() as u32);
        assert_eq!(gpu.attrs >> 8, link_id);
    }

    #[test]
    fn cell_from_render_empty() {
        let cell = Cell::default();
        let gpu = cell_from_render(&cell);
        assert_eq!(gpu.glyph_id, 0);
    }

    #[test]
    fn cell_from_render_continuation() {
        let cell = Cell::CONTINUATION;
        let gpu = cell_from_render(&cell);
        assert_eq!(gpu.glyph_id, 0);
    }

    #[test]
    fn cell_from_render_wide_unicode_scalar_passthrough() {
        let ch = '界';
        let cell = make_cell(ch, 0xF0F0F0FF, 0x000000FF, StyleFlags::empty());
        let gpu = cell_from_render(&cell);
        assert_eq!(gpu.glyph_id, ch as u32);
    }

    #[test]
    fn cell_from_render_combining_scalar_passthrough() {
        let ch = '\u{0301}'; // combining acute accent
        let cell = make_cell(ch, 0xFFFFFFFF, 0x000000FF, StyleFlags::empty());
        let gpu = cell_from_render(&cell);
        assert_eq!(gpu.glyph_id, ch as u32);
    }

    #[test]
    fn cell_from_render_emoji_scalar_passthrough() {
        let ch = '🧪';
        let cell = make_cell(ch, 0xFFFFFFFF, 0x000000FF, StyleFlags::empty());
        let gpu = cell_from_render(&cell);
        assert_eq!(gpu.glyph_id, ch as u32);
    }

    #[test]
    fn cell_from_render_zwj_scalar_passthrough() {
        let ch = '\u{200D}'; // zero-width joiner
        let cell = make_cell(ch, 0xFFFFFFFF, 0x000000FF, StyleFlags::empty());
        let gpu = cell_from_render(&cell);
        assert_eq!(gpu.glyph_id, ch as u32);
    }

    #[test]
    fn cell_from_render_variation_selector_scalar_passthrough() {
        let ch = '\u{FE0F}'; // variation selector-16
        let cell = make_cell(ch, 0xFFFFFFFF, 0x000000FF, StyleFlags::empty());
        let gpu = cell_from_render(&cell);
        assert_eq!(gpu.glyph_id, ch as u32);
    }

    #[test]
    fn cell_from_render_grapheme_uses_placeholder() {
        let cell = Cell {
            content: CellContent::from_grapheme(GraphemeId::new(7, 2)),
            ..Cell::default()
        };
        let gpu = cell_from_render(&cell);
        assert_eq!(gpu.glyph_id, GRAPHEME_FALLBACK_CODEPOINT);
    }

    #[test]
    fn cell_from_render_unicode_fixture_has_expected_glyph_mapping() {
        let fixture = unicode_fixture_row();
        let glyphs: Vec<u32> = fixture
            .iter()
            .map(cell_from_render)
            .map(|c| c.glyph_id)
            .collect();
        assert_eq!(
            glyphs,
            vec![
                'A' as u32,
                '界' as u32,
                0x0301,
                0xFE0F,
                0x200D,
                '🧪' as u32,
                GRAPHEME_FALLBACK_CODEPOINT,
                0,
            ]
        );
        let first = cell_from_render(&fixture[0]);
        assert_eq!(
            first.attrs & ATTR_STYLE_MASK,
            StyleFlags::BOLD.bits() as u32
        );
        assert_eq!(first.attrs >> 8, 0x12);
    }

    #[test]
    fn diff_to_patches_unicode_fixture_stays_contiguous_and_ordered() {
        let old = Buffer::new(8, 1);
        let mut new = Buffer::new(8, 1);
        let fixture = unicode_fixture_row();
        for (x, cell) in fixture.iter().enumerate() {
            new.set_raw(x as u16, 0, *cell);
        }

        let diff = BufferDiff::compute(&old, &new);
        let patches = diff_to_patches(&new, &diff);
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].offset, 0);

        let glyphs: Vec<u32> = patches[0].cells.iter().map(|c| c.glyph_id).collect();
        assert_eq!(
            glyphs,
            vec![
                'A' as u32,
                '界' as u32,
                0x0301,
                0xFE0F,
                0x200D,
                '🧪' as u32,
                GRAPHEME_FALLBACK_CODEPOINT,
                0,
            ]
        );
    }

    #[test]
    fn full_buffer_patch_size() {
        let mut buf = Buffer::new(10, 5);
        buf.set_raw(0, 0, Cell::from_char('A'));

        let patch = full_buffer_patch(&buf);
        assert_eq!(patch.offset, 0);
        assert_eq!(patch.cells.len(), 50); // 10 * 5
        assert_eq!(patch.cells[0].glyph_id, 'A' as u32);
    }

    #[test]
    fn diff_to_patches_empty_diff() {
        let buf = Buffer::new(10, 5);
        let diff = BufferDiff::new();
        let patches = diff_to_patches(&buf, &diff);
        assert!(patches.is_empty());
    }

    #[test]
    fn diff_to_patches_single_change() {
        let old = Buffer::new(10, 5);
        let mut new = Buffer::new(10, 5);
        new.set_raw(3, 2, Cell::from_char('Z'));

        let diff = BufferDiff::compute(&old, &new);
        let patches = diff_to_patches(&new, &diff);

        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].offset, 2 * 10 + 3); // row 2, col 3
        assert_eq!(patches[0].cells.len(), 1);
        assert_eq!(patches[0].cells[0].glyph_id, 'Z' as u32);
    }

    #[test]
    fn diff_to_patches_coalesces_adjacent() {
        let old = Buffer::new(10, 5);
        let mut new = Buffer::new(10, 5);
        // Set three consecutive cells in row 0.
        new.set_raw(2, 0, Cell::from_char('A'));
        new.set_raw(3, 0, Cell::from_char('B'));
        new.set_raw(4, 0, Cell::from_char('C'));

        let diff = BufferDiff::compute(&old, &new);
        let patches = diff_to_patches(&new, &diff);

        // Should coalesce into one span.
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].offset, 2);
        assert_eq!(patches[0].cells.len(), 3);
        assert_eq!(patches[0].cells[0].glyph_id, 'A' as u32);
        assert_eq!(patches[0].cells[1].glyph_id, 'B' as u32);
        assert_eq!(patches[0].cells[2].glyph_id, 'C' as u32);
    }

    #[test]
    fn diff_to_patches_separate_spans() {
        let old = Buffer::new(10, 5);
        let mut new = Buffer::new(10, 5);
        // Two non-adjacent changes.
        new.set_raw(1, 0, Cell::from_char('X'));
        new.set_raw(8, 0, Cell::from_char('Y'));

        let diff = BufferDiff::compute(&old, &new);
        let patches = diff_to_patches(&new, &diff);

        // Should produce two separate patches.
        assert_eq!(patches.len(), 2);
        assert_eq!(patches[0].offset, 1);
        assert_eq!(patches[0].cells[0].glyph_id, 'X' as u32);
        assert_eq!(patches[1].offset, 8);
        assert_eq!(patches[1].cells[0].glyph_id, 'Y' as u32);
    }

    #[test]
    fn diff_to_patches_cross_row() {
        let old = Buffer::new(5, 3);
        let mut new = Buffer::new(5, 3);
        // Last cell of row 0 and first cell of row 1 (contiguous in linear layout).
        new.set_raw(4, 0, Cell::from_char('A'));
        new.set_raw(0, 1, Cell::from_char('B'));

        let diff = BufferDiff::compute(&old, &new);
        let patches = diff_to_patches(&new, &diff);

        // Offsets 4 and 5 are contiguous — should coalesce.
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].offset, 4);
        assert_eq!(patches[0].cells.len(), 2);
    }

    #[test]
    fn diff_to_patches_stale_diff_falls_back_to_full_patch() {
        let old = Buffer::new(4, 2);
        let mut next = Buffer::new(4, 2);
        next.set_raw(3, 1, Cell::from_char('X'));
        let stale_diff = BufferDiff::compute(&old, &next);

        let resized = Buffer::new(2, 1);
        let patches = diff_to_patches(&resized, &stale_diff);

        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].offset, 0);
        assert_eq!(patches[0].cells.len(), 2);
    }

    #[test]
    fn patch_batch_stats_empty() {
        let stats = patch_batch_stats(&[]);
        assert_eq!(
            stats,
            PatchBatchStats {
                dirty_cells: 0,
                patch_count: 0,
                bytes_uploaded: 0,
            }
        );
    }

    #[test]
    fn patch_batch_stats_matches_patch_payload() {
        let old = Buffer::new(10, 2);
        let mut new = Buffer::new(10, 2);
        new.set_raw(0, 0, Cell::from_char('A'));
        new.set_raw(1, 0, Cell::from_char('B'));
        new.set_raw(7, 1, Cell::from_char('Z'));

        let diff = BufferDiff::compute(&old, &new);
        let patches = diff_to_patches(&new, &diff);
        let stats = patch_batch_stats(&patches);

        assert_eq!(stats.patch_count, 2);
        assert_eq!(stats.dirty_cells, 3);
        assert_eq!(
            stats.bytes_uploaded,
            3 * std::mem::size_of::<CellData>() as u64
        );
    }

    #[test]
    fn cell_from_render_preserves_max_link_id() {
        let link_id = ATTR_LINK_ID_MAX;
        let flags = StyleFlags::UNDERLINE;
        let cell = Cell::from_char('L')
            .with_fg(PackedRgba::rgb(255, 255, 255))
            .with_bg(PackedRgba::rgb(0, 0, 0))
            .with_attrs(CellAttrs::new(flags, link_id));

        let gpu = cell_from_render(&cell);
        assert_eq!(gpu.attrs & ATTR_STYLE_MASK, flags.bits() as u32);
        assert_eq!(gpu.attrs >> 8, ATTR_LINK_ID_MAX);
    }

    // -----------------------------------------------------------------------
    // Unicode wasm patch/hash parity fixtures (bd-lff4p.2.18.5)
    //
    // These tests verify that the full patch pipeline (Cell → CellData →
    // patch → stats → frame hash) handles Unicode content correctly and
    // produces deterministic, web-path-parity results.
    // -----------------------------------------------------------------------

    use crate::frame_harness::{GeometrySnapshot, stable_frame_hash};

    /// Build a row of CJK characters (wide, 2-column each) with continuations.
    fn cjk_fixture_row() -> Vec<Cell> {
        // 中(U+4E2D) 文(U+6587) 字(U+5B57) — each takes 2 columns
        vec![
            Cell::from_char('中'),
            Cell::CONTINUATION,
            Cell::from_char('文'),
            Cell::CONTINUATION,
            Cell::from_char('字'),
            Cell::CONTINUATION,
        ]
    }

    /// Build a row of Korean Hangul characters.
    fn hangul_fixture_row() -> Vec<Cell> {
        // 한(U+D55C) 글(U+AE00) — each wide
        vec![
            Cell::from_char('한'),
            Cell::CONTINUATION,
            Cell::from_char('글'),
            Cell::CONTINUATION,
        ]
    }

    /// Build a row of Japanese Katakana (narrow, single-column).
    fn katakana_fixture_row() -> Vec<Cell> {
        // ア(U+30A2) イ(U+30A4) ウ(U+30A6) エ(U+30A8) オ(U+30AA)
        vec![
            Cell::from_char('ア'),
            Cell::from_char('イ'),
            Cell::from_char('ウ'),
            Cell::from_char('エ'),
            Cell::from_char('オ'),
        ]
    }

    /// Build a mixed row: ASCII + wide CJK + emoji + combining + continuation.
    fn mixed_unicode_fixture_row() -> Vec<Cell> {
        vec![
            Cell::from_char('H'),        // ASCII
            Cell::from_char('界'),       // CJK wide
            Cell::CONTINUATION,          // continuation for 界
            Cell::from_char('\u{0301}'), // combining acute
            Cell::from_char('🧪'),       // emoji
            Cell::CONTINUATION,          // continuation for 🧪
            Cell::from_char('\u{FE0F}'), // VS16
            Cell::from_char('Z'),        // ASCII
        ]
    }

    /// Build a row exercising Latin Extended characters (diacritics as scalars).
    fn latin_extended_fixture_row() -> Vec<Cell> {
        // é(U+00E9)  ñ(U+00F1)  ü(U+00FC)  ß(U+00DF)  ø(U+00F8)
        vec![
            Cell::from_char('é'),
            Cell::from_char('ñ'),
            Cell::from_char('ü'),
            Cell::from_char('ß'),
            Cell::from_char('ø'),
        ]
    }

    // --- cell_from_render Unicode parity tests ---

    #[test]
    fn cell_from_render_cjk_passthrough() {
        for &ch in &['中', '文', '字', '国', '人'] {
            let cell = make_cell(ch, 0xFFFFFFFF, 0x000000FF, StyleFlags::empty());
            let gpu = cell_from_render(&cell);
            assert_eq!(gpu.glyph_id, ch as u32, "CJK char {ch} should pass through");
        }
    }

    #[test]
    fn cell_from_render_hangul_passthrough() {
        for &ch in &['한', '글', '가', '나', '다'] {
            let cell = make_cell(ch, 0xFFFFFFFF, 0x000000FF, StyleFlags::empty());
            let gpu = cell_from_render(&cell);
            assert_eq!(
                gpu.glyph_id, ch as u32,
                "Hangul char {ch} should pass through"
            );
        }
    }

    #[test]
    fn cell_from_render_katakana_passthrough() {
        for &ch in &['ア', 'イ', 'ウ', 'エ', 'オ'] {
            let cell = make_cell(ch, 0xFFFFFFFF, 0x000000FF, StyleFlags::empty());
            let gpu = cell_from_render(&cell);
            assert_eq!(
                gpu.glyph_id, ch as u32,
                "Katakana char {ch} should pass through"
            );
        }
    }

    #[test]
    fn cell_from_render_latin_extended_passthrough() {
        for &ch in &['é', 'ñ', 'ü', 'ß', 'ø', 'à', 'ê', 'î'] {
            let cell = make_cell(ch, 0xFFFFFFFF, 0x000000FF, StyleFlags::empty());
            let gpu = cell_from_render(&cell);
            assert_eq!(
                gpu.glyph_id, ch as u32,
                "Latin extended char {ch} should pass through"
            );
        }
    }

    #[test]
    fn cell_from_render_emoji_codepoints_above_bmp() {
        // Emoji above BMP (U+1xxxx) — verify u32 encoding is correct
        for &ch in &['🎉', '🚀', '💡', '🔥', '🧪'] {
            let cell = make_cell(ch, 0xFFFFFFFF, 0x000000FF, StyleFlags::empty());
            let gpu = cell_from_render(&cell);
            assert_eq!(gpu.glyph_id, ch as u32, "Emoji {ch} should pass through");
            assert!(gpu.glyph_id > 0xFFFF, "Emoji should be above BMP");
        }
    }

    #[test]
    fn cell_from_render_cjk_with_styled_attrs_preserved() {
        let flags = StyleFlags::BOLD | StyleFlags::ITALIC;
        let link_id = 0x42u32;
        let cell = Cell::from_char('界')
            .with_fg(PackedRgba::rgb(255, 0, 0))
            .with_bg(PackedRgba::rgb(0, 0, 128))
            .with_attrs(CellAttrs::new(flags, link_id));
        let gpu = cell_from_render(&cell);

        assert_eq!(gpu.glyph_id, '界' as u32);
        assert_eq!(gpu.fg_rgba, PackedRgba::rgb(255, 0, 0).0);
        assert_eq!(gpu.bg_rgba, PackedRgba::rgb(0, 0, 128).0);
        assert_eq!(gpu.attrs & ATTR_STYLE_MASK, flags.bits() as u32);
        assert_eq!(gpu.attrs >> 8, link_id);
    }

    // --- full_buffer_patch Unicode parity tests ---

    #[test]
    fn full_buffer_patch_cjk_row_preserves_glyph_ids_and_continuations() {
        let fixture = cjk_fixture_row();
        let mut buf = Buffer::new(fixture.len() as u16, 1);
        for (x, cell) in fixture.iter().enumerate() {
            buf.set_raw(x as u16, 0, *cell);
        }
        let patch = full_buffer_patch(&buf);
        assert_eq!(patch.offset, 0);
        assert_eq!(patch.cells.len(), fixture.len());

        let glyphs: Vec<u32> = patch.cells.iter().map(|c| c.glyph_id).collect();
        assert_eq!(glyphs, vec!['中' as u32, 0, '文' as u32, 0, '字' as u32, 0]);
    }

    #[test]
    fn full_buffer_patch_hangul_row() {
        let fixture = hangul_fixture_row();
        let mut buf = Buffer::new(fixture.len() as u16, 1);
        for (x, cell) in fixture.iter().enumerate() {
            buf.set_raw(x as u16, 0, *cell);
        }
        let patch = full_buffer_patch(&buf);
        let glyphs: Vec<u32> = patch.cells.iter().map(|c| c.glyph_id).collect();
        assert_eq!(glyphs, vec!['한' as u32, 0, '글' as u32, 0]);
    }

    #[test]
    fn full_buffer_patch_katakana_row() {
        let fixture = katakana_fixture_row();
        let mut buf = Buffer::new(fixture.len() as u16, 1);
        for (x, cell) in fixture.iter().enumerate() {
            buf.set_raw(x as u16, 0, *cell);
        }
        let patch = full_buffer_patch(&buf);
        let glyphs: Vec<u32> = patch.cells.iter().map(|c| c.glyph_id).collect();
        assert_eq!(
            glyphs,
            vec![
                'ア' as u32,
                'イ' as u32,
                'ウ' as u32,
                'エ' as u32,
                'オ' as u32
            ]
        );
    }

    #[test]
    fn full_buffer_patch_mixed_unicode_row() {
        let fixture = mixed_unicode_fixture_row();
        let mut buf = Buffer::new(fixture.len() as u16, 1);
        for (x, cell) in fixture.iter().enumerate() {
            buf.set_raw(x as u16, 0, *cell);
        }
        let patch = full_buffer_patch(&buf);
        let glyphs: Vec<u32> = patch.cells.iter().map(|c| c.glyph_id).collect();
        assert_eq!(
            glyphs,
            vec![
                'H' as u32,
                '界' as u32,
                0, // continuation
                '\u{0301}' as u32,
                '🧪' as u32,
                0, // continuation
                '\u{FE0F}' as u32,
                'Z' as u32,
            ]
        );
    }

    #[test]
    fn full_buffer_patch_latin_extended_row() {
        let fixture = latin_extended_fixture_row();
        let mut buf = Buffer::new(fixture.len() as u16, 1);
        for (x, cell) in fixture.iter().enumerate() {
            buf.set_raw(x as u16, 0, *cell);
        }
        let patch = full_buffer_patch(&buf);
        let glyphs: Vec<u32> = patch.cells.iter().map(|c| c.glyph_id).collect();
        assert_eq!(
            glyphs,
            vec!['é' as u32, 'ñ' as u32, 'ü' as u32, 'ß' as u32, 'ø' as u32]
        );
    }

    // --- diff_to_patches Unicode parity tests ---

    #[test]
    fn diff_to_patches_cjk_insertion_coalesces() {
        let cols = 6u16;
        let old = Buffer::new(cols, 1);
        let mut new = Buffer::new(cols, 1);
        for (x, cell) in cjk_fixture_row().iter().enumerate() {
            new.set_raw(x as u16, 0, *cell);
        }

        let diff = BufferDiff::compute(&old, &new);
        let patches = diff_to_patches(&new, &diff);
        // All 6 cells should coalesce into a single patch.
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].offset, 0);
        assert_eq!(patches[0].cells.len(), 6);
    }

    #[test]
    fn diff_to_patches_sparse_unicode_produces_separate_spans() {
        let cols = 10u16;
        let old = Buffer::new(cols, 1);
        let mut new = Buffer::new(cols, 1);
        // Place CJK at col 0-1 and emoji at col 8-9 (gap in between).
        new.set_raw(0, 0, Cell::from_char('中'));
        new.set_raw(1, 0, Cell::CONTINUATION);
        new.set_raw(8, 0, Cell::from_char('🧪'));
        new.set_raw(9, 0, Cell::CONTINUATION);

        let diff = BufferDiff::compute(&old, &new);
        let patches = diff_to_patches(&new, &diff);
        assert_eq!(
            patches.len(),
            2,
            "non-adjacent Unicode should produce 2 spans"
        );
        assert_eq!(patches[0].offset, 0);
        assert_eq!(patches[0].cells.len(), 2);
        assert_eq!(patches[0].cells[0].glyph_id, '中' as u32);
        assert_eq!(patches[1].offset, 8);
        assert_eq!(patches[1].cells.len(), 2);
        assert_eq!(patches[1].cells[0].glyph_id, '🧪' as u32);
    }

    #[test]
    fn diff_to_patches_mixed_unicode_patch_stats_correct() {
        let fixture = mixed_unicode_fixture_row();
        let cols = fixture.len() as u16;
        let old = Buffer::new(cols, 1);
        let mut new = Buffer::new(cols, 1);
        for (x, cell) in fixture.iter().enumerate() {
            new.set_raw(x as u16, 0, *cell);
        }

        let diff = BufferDiff::compute(&old, &new);
        let patches = diff_to_patches(&new, &diff);
        let stats = patch_batch_stats(&patches);

        assert_eq!(stats.dirty_cells, cols as u32);
        assert_eq!(stats.patch_count, 1);
        assert_eq!(
            stats.bytes_uploaded,
            u64::from(cols) * std::mem::size_of::<CellData>() as u64
        );
    }

    // --- CellData::to_bytes round-trip for Unicode ---

    #[test]
    fn cell_data_to_bytes_roundtrip_unicode_codepoints() {
        // Verify that to_bytes correctly encodes glyph_id for non-ASCII codepoints
        let test_cases: &[(char, u32)] = &[
            ('界', 0x754C),
            ('🧪', 0x1F9EA),
            ('é', 0x00E9),
            ('한', 0xD55C),
            ('ア', 0x30A2),
            ('\u{0301}', 0x0301),
            ('\u{200D}', 0x200D),
            ('\u{FE0F}', 0xFE0F),
        ];
        for &(ch, expected_codepoint) in test_cases {
            let cell = make_cell(ch, 0xFFFFFFFF, 0x000000FF, StyleFlags::empty());
            let gpu = cell_from_render(&cell);
            assert_eq!(gpu.glyph_id, expected_codepoint);

            let bytes = gpu.to_bytes();
            // glyph_id is at bytes 8..12, little-endian
            let glyph_from_bytes = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
            assert_eq!(
                glyph_from_bytes, expected_codepoint,
                "to_bytes round-trip for {ch} (U+{expected_codepoint:04X})"
            );
        }
    }

    // --- Frame hash parity for Unicode content ---

    fn test_geometry(cols: u16, rows: u16) -> GeometrySnapshot {
        GeometrySnapshot {
            cols,
            rows,
            pixel_width: u32::from(cols) * 10,
            pixel_height: u32::from(rows) * 20,
            cell_width_px: 10.0,
            cell_height_px: 20.0,
            dpr: 1.0,
            zoom: 1.0,
        }
    }

    #[test]
    fn frame_hash_cjk_buffer_is_deterministic() {
        let fixture = cjk_fixture_row();
        let cols = fixture.len() as u16;
        let mut buf = Buffer::new(cols, 1);
        for (x, cell) in fixture.iter().enumerate() {
            buf.set_raw(x as u16, 0, *cell);
        }

        let patch = full_buffer_patch(&buf);
        let geometry = test_geometry(cols, 1);
        let hash_a = stable_frame_hash(&patch.cells, geometry);
        let hash_b = stable_frame_hash(&patch.cells, geometry);
        assert_eq!(hash_a, hash_b);
        assert!(hash_a.starts_with("fnv1a64:"));
    }

    #[test]
    fn frame_hash_differs_for_different_cjk_content() {
        let geometry = test_geometry(6, 1);

        // Row A: 中 文 字
        let mut buf_a = Buffer::new(6, 1);
        for (x, cell) in cjk_fixture_row().iter().enumerate() {
            buf_a.set_raw(x as u16, 0, *cell);
        }
        let patch_a = full_buffer_patch(&buf_a);

        // Row B: 国 人 大
        let mut buf_b = Buffer::new(6, 1);
        let row_b = [
            Cell::from_char('国'),
            Cell::CONTINUATION,
            Cell::from_char('人'),
            Cell::CONTINUATION,
            Cell::from_char('大'),
            Cell::CONTINUATION,
        ];
        for (x, cell) in row_b.iter().enumerate() {
            buf_b.set_raw(x as u16, 0, *cell);
        }
        let patch_b = full_buffer_patch(&buf_b);

        let hash_a = stable_frame_hash(&patch_a.cells, geometry);
        let hash_b = stable_frame_hash(&patch_b.cells, geometry);
        assert_ne!(
            hash_a, hash_b,
            "different CJK content should produce different hashes"
        );
    }

    #[test]
    fn frame_hash_mixed_unicode_row_is_deterministic() {
        let fixture = mixed_unicode_fixture_row();
        let cols = fixture.len() as u16;
        let geometry = test_geometry(cols, 1);

        let mut buf = Buffer::new(cols, 1);
        for (x, cell) in fixture.iter().enumerate() {
            buf.set_raw(x as u16, 0, *cell);
        }

        let patch = full_buffer_patch(&buf);
        let hash_a = stable_frame_hash(&patch.cells, geometry);
        let hash_b = stable_frame_hash(&patch.cells, geometry);
        assert_eq!(hash_a, hash_b);
    }

    #[test]
    fn frame_hash_hangul_vs_cjk_differ() {
        // Verify that hangul and CJK rows with same geometry produce different hashes
        let geometry = test_geometry(4, 1);

        let mut buf_hangul = Buffer::new(4, 1);
        for (x, cell) in hangul_fixture_row().iter().enumerate() {
            buf_hangul.set_raw(x as u16, 0, *cell);
        }

        let mut buf_cjk = Buffer::new(4, 1);
        let cjk_4 = [
            Cell::from_char('中'),
            Cell::CONTINUATION,
            Cell::from_char('文'),
            Cell::CONTINUATION,
        ];
        for (x, cell) in cjk_4.iter().enumerate() {
            buf_cjk.set_raw(x as u16, 0, *cell);
        }

        let hash_hangul = stable_frame_hash(&full_buffer_patch(&buf_hangul).cells, geometry);
        let hash_cjk = stable_frame_hash(&full_buffer_patch(&buf_cjk).cells, geometry);
        assert_ne!(hash_hangul, hash_cjk);
    }

    #[test]
    fn frame_hash_styled_emoji_differs_from_unstyled() {
        let geometry = test_geometry(2, 1);

        // Unstyled emoji
        let mut buf_plain = Buffer::new(2, 1);
        buf_plain.set_raw(0, 0, Cell::from_char('🧪'));
        buf_plain.set_raw(1, 0, Cell::CONTINUATION);

        // Bold + colored emoji
        let mut buf_styled = Buffer::new(2, 1);
        buf_styled.set_raw(
            0,
            0,
            Cell::from_char('🧪')
                .with_fg(PackedRgba::rgb(255, 0, 0))
                .with_attrs(CellAttrs::new(StyleFlags::BOLD, 0)),
        );
        buf_styled.set_raw(1, 0, Cell::CONTINUATION);

        let hash_plain = stable_frame_hash(&full_buffer_patch(&buf_plain).cells, geometry);
        let hash_styled = stable_frame_hash(&full_buffer_patch(&buf_styled).cells, geometry);
        assert_ne!(
            hash_plain, hash_styled,
            "styled emoji should produce a different hash"
        );
    }

    #[test]
    fn full_pipeline_unicode_multirow_grid() {
        // Build a 10x3 grid with Unicode content across multiple rows,
        // verify the complete pipeline: Buffer → patch → stats → hash.
        let mut buf = Buffer::new(10, 3);

        // Row 0: ASCII "Hello" + CJK 界
        for (i, ch) in "Hello".chars().enumerate() {
            buf.set_raw(i as u16, 0, Cell::from_char(ch));
        }
        buf.set_raw(5, 0, Cell::from_char('界'));
        buf.set_raw(6, 0, Cell::CONTINUATION);

        // Row 1: Korean 한글 + Latin ñé
        buf.set_raw(0, 1, Cell::from_char('한'));
        buf.set_raw(1, 1, Cell::CONTINUATION);
        buf.set_raw(2, 1, Cell::from_char('글'));
        buf.set_raw(3, 1, Cell::CONTINUATION);
        buf.set_raw(4, 1, Cell::from_char('ñ'));
        buf.set_raw(5, 1, Cell::from_char('é'));

        // Row 2: Emoji row
        buf.set_raw(0, 2, Cell::from_char('🎉'));
        buf.set_raw(1, 2, Cell::CONTINUATION);
        buf.set_raw(2, 2, Cell::from_char('🚀'));
        buf.set_raw(3, 2, Cell::CONTINUATION);
        buf.set_raw(4, 2, Cell::from_char('💡'));
        buf.set_raw(5, 2, Cell::CONTINUATION);

        let patch = full_buffer_patch(&buf);
        assert_eq!(patch.cells.len(), 30); // 10 * 3

        let stats = patch_batch_stats(std::slice::from_ref(&patch));
        assert_eq!(stats.dirty_cells, 30);
        assert_eq!(stats.bytes_uploaded, 30 * 16); // 30 cells * 16 bytes each

        let geometry = test_geometry(10, 3);
        let hash = stable_frame_hash(&patch.cells, geometry);
        assert!(hash.starts_with("fnv1a64:"));

        // Verify determinism
        let hash2 = stable_frame_hash(&patch.cells, geometry);
        assert_eq!(hash, hash2);

        // Spot-check specific cells
        assert_eq!(patch.cells[0].glyph_id, 'H' as u32); // row 0 col 0
        assert_eq!(patch.cells[5].glyph_id, '界' as u32); // row 0 col 5
        assert_eq!(patch.cells[6].glyph_id, 0); // continuation
        assert_eq!(patch.cells[10].glyph_id, '한' as u32); // row 1 col 0
        assert_eq!(patch.cells[14].glyph_id, 'ñ' as u32); // row 1 col 4
        assert_eq!(patch.cells[20].glyph_id, '🎉' as u32); // row 2 col 0
        assert_eq!(patch.cells[22].glyph_id, '🚀' as u32); // row 2 col 2
    }

    #[test]
    fn diff_to_patches_unicode_incremental_update() {
        // Start with ASCII buffer, then change some cells to Unicode.
        // Verify diff-based patches are correct.
        let cols = 8u16;
        let mut old = Buffer::new(cols, 1);
        for (i, ch) in "ABCDEFGH".chars().enumerate() {
            old.set_raw(i as u16, 0, Cell::from_char(ch));
        }

        let mut new = old.clone();
        // Replace cols 2-3 with CJK wide char
        new.set_raw(2, 0, Cell::from_char('中'));
        new.set_raw(3, 0, Cell::CONTINUATION);
        // Replace col 6 with Latin extended
        new.set_raw(6, 0, Cell::from_char('ñ'));

        let diff = BufferDiff::compute(&old, &new);
        let patches = diff_to_patches(&new, &diff);
        assert_eq!(patches.len(), 2, "should produce 2 disjoint patches");

        // First patch: cols 2-3 (CJK + continuation)
        assert_eq!(patches[0].offset, 2);
        assert_eq!(patches[0].cells.len(), 2);
        assert_eq!(patches[0].cells[0].glyph_id, '中' as u32);
        assert_eq!(patches[0].cells[1].glyph_id, 0); // continuation

        // Second patch: col 6 (Latin extended)
        assert_eq!(patches[1].offset, 6);
        assert_eq!(patches[1].cells.len(), 1);
        assert_eq!(patches[1].cells[0].glyph_id, 'ñ' as u32);
    }

    #[test]
    fn cell_from_core_packs_colors_style_and_link() {
        let mut cell = CoreCell::new('A');
        cell.attrs = CoreSgrAttrs {
            flags: CoreSgrFlags::BOLD | CoreSgrFlags::UNDERLINE,
            fg: CoreColor::Rgb(1, 2, 3),
            bg: CoreColor::Named(4),
            underline_color: None,
        };
        cell.hyperlink = 0x0034;

        let gpu = cell_from_core(&cell);
        assert_eq!(gpu.glyph_id, u32::from('A'));
        assert_eq!(gpu.fg_rgba, pack_rgba(1, 2, 3));
        assert_eq!(gpu.bg_rgba, ANSI16_RGBA[4]);
        assert_eq!(
            gpu.attrs & ATTR_STYLE_MASK,
            u32::from((CoreSgrFlags::BOLD | CoreSgrFlags::UNDERLINE).bits())
        );
        assert_eq!(gpu.attrs >> 8, 0x0034);
    }

    #[test]
    fn cell_from_core_wide_continuation_maps_to_empty_glyph() {
        let (_, cont) = CoreCell::wide('界', CoreSgrAttrs::default());
        let gpu = cell_from_core(&cont);
        assert_eq!(gpu.glyph_id, 0);
    }

    #[test]
    fn cell_from_core_256_indexed_colors_follow_xterm_mapping() {
        let mut cell = CoreCell::new('Z');
        cell.attrs.fg = CoreColor::Indexed(196); // bright red from 6x6x6 cube
        cell.attrs.bg = CoreColor::Indexed(243); // grayscale ramp
        let gpu = cell_from_core(&cell);
        assert_eq!(gpu.fg_rgba, pack_rgba(255, 0, 0));
        assert_eq!(gpu.bg_rgba, pack_rgba(118, 118, 118));
    }

    #[test]
    fn core_patch_to_patches_coalesces_adjacent_offsets() {
        let mut patch = CorePatch::new(6, 2);
        patch.push(0, 1, CoreCell::new('A'));
        patch.push(0, 2, CoreCell::new('B'));
        patch.push(1, 0, CoreCell::new('C'));

        let runs = core_patch_to_patches(&patch);
        assert_eq!(runs.len(), 2);

        assert_eq!(runs[0].offset, 1);
        assert_eq!(runs[0].cells.len(), 2);
        assert_eq!(runs[0].cells[0].glyph_id, u32::from('A'));
        assert_eq!(runs[0].cells[1].glyph_id, u32::from('B'));

        assert_eq!(runs[1].offset, 6);
        assert_eq!(runs[1].cells.len(), 1);
        assert_eq!(runs[1].cells[0].glyph_id, u32::from('C'));
    }

    #[test]
    fn core_patch_to_patches_last_duplicate_update_wins() {
        let mut patch = CorePatch::new(4, 1);
        patch.push(0, 0, CoreCell::new('X'));
        patch.push(0, 0, CoreCell::new('Y'));

        let runs = core_patch_to_patches(&patch);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].offset, 0);
        assert_eq!(runs[0].cells.len(), 1);
        assert_eq!(runs[0].cells[0].glyph_id, u32::from('Y'));
    }

    #[test]
    fn remote_feed_decawm_nowrap_overwrites_rightmost_column() {
        let mut engine = TerminalEngine::new(5, 3);
        engine.feed_bytes(b"\x1b[?7lABCDEFGH");

        let patch = engine.snapshot_patches();
        let patches = core_patch_to_patches(&patch);
        let shadow = apply_cell_patches_to_shadow(5, 3, &patches);

        assert_eq!(shadow_row_text(&shadow, 5, 0), "ABCDH");
        assert_eq!(shadow_row_text(&shadow, 5, 1), "     ");
        assert_eq!(shadow_row_text(&shadow, 5, 2), "     ");
    }

    #[test]
    fn remote_feed_origin_mode_home_targets_scroll_region_top() {
        let mut engine = TerminalEngine::new(10, 6);
        engine.feed_bytes(b"\x1b[3;5r\x1b[?6h\x1b[HXY");

        let patch = engine.snapshot_patches();
        let patches = core_patch_to_patches(&patch);
        let shadow = apply_cell_patches_to_shadow(10, 6, &patches);

        assert_eq!(shadow_row_text(&shadow, 10, 2), "XY        ");
        assert_eq!(shadow_row_text(&shadow, 10, 0), "          ");
        assert_eq!(shadow_row_text(&shadow, 10, 1), "          ");
    }

    #[test]
    fn remote_feed_soft_reset_restores_autowrap_before_followup_writes() {
        let mut engine = TerminalEngine::new(3, 2);
        engine.feed_bytes(b"\x1b[?7l\x1b[!pABCDEF");

        let patch = engine.snapshot_patches();
        let patches = core_patch_to_patches(&patch);
        let shadow = apply_cell_patches_to_shadow(3, 2, &patches);

        assert_eq!(shadow_row_text(&shadow, 3, 0), "ABC");
        assert_eq!(shadow_row_text(&shadow, 3, 1), "DEF");
    }

    #[test]
    fn remote_feed_resize_then_home_write_stays_within_new_geometry() {
        let mut engine = TerminalEngine::new(4, 2);
        engine.feed_bytes(b"ABCD");
        let _ = engine.snapshot_patches();

        engine.resize(2, 2);
        engine.feed_bytes(b"\x1b[HXY");
        let patch = engine.snapshot_patches();
        let patches = core_patch_to_patches(&patch);

        assert!(patches.iter().all(|run| {
            let start = usize::try_from(run.offset).unwrap_or(usize::MAX);
            start
                .checked_add(run.cells.len())
                .is_some_and(|end| end <= 4usize)
        }));

        let shadow = apply_cell_patches_to_shadow(2, 2, &patches);
        assert_eq!(shadow_row_text(&shadow, 2, 0), "XY");
    }
}
