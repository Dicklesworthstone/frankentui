//! Terminal cell: the fundamental unit of the grid.
//!
//! Each cell stores a base character plus up to [`MAX_COMBINING`] inline
//! combining marks, forming a grapheme cluster, along with SGR attributes.
//! This is intentionally simpler than `ftui-render::Cell` — it models the
//! terminal's internal state rather than the rendering pipeline.

use bitflags::bitflags;
use std::collections::HashMap;
use unicode_width::UnicodeWidthChar;

bitflags! {
    /// SGR text attribute flags.
    ///
    /// Maps directly to the ECMA-48 / VT100 SGR parameter values.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct SgrFlags: u16 {
        const BOLD          = 1 << 0;
        const DIM           = 1 << 1;
        const ITALIC        = 1 << 2;
        const UNDERLINE     = 1 << 3;
        const BLINK         = 1 << 4;
        const INVERSE       = 1 << 5;
        const HIDDEN        = 1 << 6;
        const STRIKETHROUGH = 1 << 7;
        const DOUBLE_UNDERLINE = 1 << 8;
        const CURLY_UNDERLINE  = 1 << 9;
        const OVERLINE      = 1 << 10;
    }
}

bitflags! {
    /// Cell-level flags that are orthogonal to SGR attributes.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct CellFlags: u8 {
        /// This cell is the leading (left) cell of a wide (2-column) character.
        const WIDE_CHAR = 1 << 0;
        /// This cell is the trailing (right) continuation of a wide character.
        /// Its content is meaningless; rendering uses the leading cell.
        const WIDE_CONTINUATION = 1 << 1;
        /// This cell has one or more combining marks attached to its base character.
        const HAS_COMBINING = 1 << 2;
    }
}

/// Maximum number of combining marks stored inline per cell.
///
/// Two marks cover the vast majority of real-world grapheme clusters
/// (e.g. base + accent + second diacritic). Excess marks are silently dropped.
pub const MAX_COMBINING: usize = 2;

/// Color representation for terminal cells.
///
/// Supports the standard terminal color model hierarchy:
/// default → 16 named → 256 indexed → 24-bit RGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Color {
    /// Terminal default (SGR 39 / SGR 49).
    #[default]
    Default,
    /// Named color index (0-15): standard 8 + bright 8.
    Named(u8),
    /// 256-color palette index (0-255).
    Indexed(u8),
    /// 24-bit true color.
    Rgb(u8, u8, u8),
}

/// SGR attributes for a cell: flags + foreground/background colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SgrAttrs {
    pub flags: SgrFlags,
    pub fg: Color,
    pub bg: Color,
    /// Underline color (SGR 58). `None` means use foreground.
    pub underline_color: Option<Color>,
}

impl SgrAttrs {
    /// Reset all attributes to default (SGR 0).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Apply an SGR parameter list (`CSI ... m`) to this attribute state.
    ///
    /// Notes
    /// - SGR is stateful and delta-based: parameters mutate the current state.
    /// - An empty parameter list is treated as `[0]` (full reset).
    pub fn apply_sgr_params(&mut self, params: &[u16]) {
        let mut i = 0usize;
        let params = if params.is_empty() {
            &[0u16][..]
        } else {
            params
        };

        while i < params.len() {
            let p = params[i];
            match p {
                0 => self.reset(),

                1 => self.flags.insert(SgrFlags::BOLD),
                2 => self.flags.insert(SgrFlags::DIM),
                3 => self.flags.insert(SgrFlags::ITALIC),
                4 => self.flags.insert(SgrFlags::UNDERLINE),
                5 => self.flags.insert(SgrFlags::BLINK),
                7 => self.flags.insert(SgrFlags::INVERSE),
                8 => self.flags.insert(SgrFlags::HIDDEN),
                9 => self.flags.insert(SgrFlags::STRIKETHROUGH),
                21 => self.flags.insert(SgrFlags::DOUBLE_UNDERLINE),
                53 => self.flags.insert(SgrFlags::OVERLINE),

                22 => self.flags.remove(SgrFlags::BOLD | SgrFlags::DIM),
                23 => self.flags.remove(SgrFlags::ITALIC),
                24 => self.flags.remove(
                    SgrFlags::UNDERLINE | SgrFlags::DOUBLE_UNDERLINE | SgrFlags::CURLY_UNDERLINE,
                ),
                25 => self.flags.remove(SgrFlags::BLINK),
                27 => self.flags.remove(SgrFlags::INVERSE),
                28 => self.flags.remove(SgrFlags::HIDDEN),
                29 => self.flags.remove(SgrFlags::STRIKETHROUGH),
                55 => self.flags.remove(SgrFlags::OVERLINE),

                30..=37 => self.fg = Color::Named((p - 30) as u8),
                39 => self.fg = Color::Default,
                40..=47 => self.bg = Color::Named((p - 40) as u8),
                49 => self.bg = Color::Default,
                90..=97 => self.fg = Color::Named(((p - 90) as u8).saturating_add(8)),
                100..=107 => self.bg = Color::Named(((p - 100) as u8).saturating_add(8)),

                // Extended colors.
                38 => {
                    if let Some((c, consumed)) = parse_extended_color(params, i) {
                        self.fg = c;
                        i = i.saturating_add(consumed);
                        continue;
                    }
                }
                48 => {
                    if let Some((c, consumed)) = parse_extended_color(params, i) {
                        self.bg = c;
                        i = i.saturating_add(consumed);
                        continue;
                    }
                }
                58 => {
                    if let Some((c, consumed)) = parse_extended_color(params, i) {
                        self.underline_color = Some(c);
                        i = i.saturating_add(consumed);
                        continue;
                    }
                }
                59 => self.underline_color = None,

                _ => {}
            }
            i = i.saturating_add(1);
        }

        fn parse_extended_color(params: &[u16], start: usize) -> Option<(Color, usize)> {
            // Formats:
            // - 38;5;n     (indexed fg)
            // - 38;2;r;g;b (truecolor fg)
            // ... same for 48 (bg) and 58 (underline color).
            let mode = *params.get(start + 1)?;
            match mode {
                5 => {
                    let idx = *params.get(start + 2)?;
                    Some((Color::Indexed(idx.min(255) as u8), 3))
                }
                2 => {
                    let r = *params.get(start + 2)?;
                    let g = *params.get(start + 3)?;
                    let b = *params.get(start + 4)?;
                    Some((
                        Color::Rgb(r.min(255) as u8, g.min(255) as u8, b.min(255) as u8),
                        5,
                    ))
                }
                _ => None,
            }
        }
    }
}

/// Hyperlink identifier for OSC 8 links.
///
/// Zero means "no link". Non-zero values index into an external link registry
/// that maps IDs to URIs.
pub type HyperlinkId = u16;

/// Registry for OSC 8 hyperlink URIs.
///
/// Cells store compact `HyperlinkId`s instead of full URI strings. This
/// registry provides ID allocation, deduplication, and reference-counted
/// release so hosts can clear unused hyperlinks when content is dropped
/// (e.g., scrollback eviction).
#[derive(Debug, Clone)]
pub struct HyperlinkRegistry {
    /// Slots indexed by ID (0 reserved for "no link").
    slots: Vec<Option<HyperlinkSlot>>,
    /// URI -> ID lookup for deduplication.
    lookup: HashMap<String, HyperlinkId>,
    /// Reusable IDs from released hyperlinks.
    free_list: Vec<HyperlinkId>,
}

#[derive(Debug, Clone)]
struct HyperlinkSlot {
    uri: String,
    ref_count: u32,
}

impl HyperlinkRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            slots: vec![None],
            lookup: HashMap::new(),
            free_list: Vec::new(),
        }
    }

    /// Intern a URI and return its hyperlink ID without changing refcounts.
    ///
    /// Empty URIs return 0 (interpreted as "no link").
    pub fn intern(&mut self, uri: &str) -> HyperlinkId {
        if uri.is_empty() {
            return 0;
        }
        if let Some(&id) = self.lookup.get(uri) {
            return id;
        }

        let id = if let Some(id) = self.free_list.pop() {
            id
        } else {
            let next = self.slots.len();
            if next > HyperlinkId::MAX as usize {
                return 0;
            }
            let id = next as HyperlinkId;
            self.slots.push(None);
            id
        };

        if id == 0 {
            return 0;
        }
        let idx = id as usize;
        if idx >= self.slots.len() {
            return 0;
        }

        self.slots[idx] = Some(HyperlinkSlot {
            uri: uri.to_string(),
            ref_count: 0,
        });
        self.lookup.insert(uri.to_string(), id);
        id
    }

    /// Convenience: intern a URI and increment its refcount once.
    pub fn acquire(&mut self, uri: &str) -> HyperlinkId {
        let id = self.intern(uri);
        self.acquire_id(id);
        id
    }

    /// Increment the refcount for an existing hyperlink ID.
    ///
    /// Invalid IDs and 0 are ignored.
    pub fn acquire_id(&mut self, id: HyperlinkId) {
        if id == 0 {
            return;
        }
        let Some(slot) = self.slots.get_mut(id as usize) else {
            return;
        };
        let Some(slot) = slot.as_mut() else {
            return;
        };
        slot.ref_count = slot.ref_count.saturating_add(1);
    }

    /// Decrement the refcount for an ID and release it when it reaches zero.
    ///
    /// Invalid IDs and 0 are ignored. Releasing an ID with refcount 0 is a no-op.
    pub fn release_id(&mut self, id: HyperlinkId) {
        if id == 0 {
            return;
        }
        let Some(entry) = self.slots.get_mut(id as usize) else {
            return;
        };

        let should_remove = match entry.as_mut() {
            Some(slot) if slot.ref_count > 0 => {
                slot.ref_count -= 1;
                slot.ref_count == 0
            }
            _ => false,
        };

        if should_remove && let Some(removed) = entry.take() {
            self.lookup.remove(&removed.uri);
            self.free_list.push(id);
        }
    }

    /// Release hyperlink references for all cells in the slice.
    ///
    /// Intended for use when dropping content (e.g., evicted scrollback lines).
    pub fn release_cells(&mut self, cells: &[Cell]) {
        for cell in cells {
            self.release_id(cell.hyperlink);
        }
    }

    /// Look up the URI for a hyperlink ID.
    pub fn get(&self, id: HyperlinkId) -> Option<&str> {
        self.slots
            .get(id as usize)
            .and_then(|slot| slot.as_ref())
            .map(|slot| slot.uri.as_str())
    }

    /// Clear all hyperlinks, resetting the registry to empty.
    pub fn clear(&mut self) {
        self.slots.clear();
        self.slots.push(None);
        self.lookup.clear();
        self.free_list.clear();
    }

    /// Number of currently registered hyperlinks.
    pub fn len(&self) -> usize {
        self.slots.iter().filter(|slot| slot.is_some()).count()
    }

    /// Whether the registry has no hyperlinks.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether the registry contains the given ID.
    pub fn contains(&self, id: HyperlinkId) -> bool {
        self.get(id).is_some()
    }
}

impl Default for HyperlinkRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// A single cell in the terminal grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    /// The character content. A space for empty/erased cells.
    content: char,
    /// Display width of the content in terminal columns (1 or 2 for wide chars).
    width: u8,
    /// Cell-level flags (wide char, continuation, etc.).
    pub flags: CellFlags,
    /// SGR text attributes.
    pub attrs: SgrAttrs,
    /// Hyperlink ID (0 = no link).
    pub hyperlink: HyperlinkId,
    /// Inline storage for combining marks (accents, ZWJ, etc.).
    combining: [char; MAX_COMBINING],
    /// Number of combining marks actually stored (0..=MAX_COMBINING).
    combining_len: u8,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            content: ' ',
            width: 1,
            flags: CellFlags::empty(),
            attrs: SgrAttrs::default(),
            hyperlink: 0,
            combining: ['\0'; MAX_COMBINING],
            combining_len: 0,
        }
    }
}

impl Cell {
    /// Create a new cell with the given character and default attributes.
    pub fn new(ch: char) -> Self {
        Self {
            content: ch,
            width: 1,
            flags: CellFlags::empty(),
            attrs: SgrAttrs::default(),
            hyperlink: 0,
            combining: ['\0'; MAX_COMBINING],
            combining_len: 0,
        }
    }

    /// Create a new cell with the given character, width, and attributes.
    pub fn with_attrs(ch: char, width: u8, attrs: SgrAttrs) -> Self {
        Self {
            content: ch,
            width,
            flags: CellFlags::empty(),
            attrs,
            hyperlink: 0,
            combining: ['\0'; MAX_COMBINING],
            combining_len: 0,
        }
    }

    /// Create a wide (2-column) character cell.
    ///
    /// Returns `(leading, continuation)` pair. The leading cell holds the
    /// character; the continuation cell is a placeholder.
    pub fn wide(ch: char, attrs: SgrAttrs) -> (Self, Self) {
        let leading = Self {
            content: ch,
            width: 2,
            flags: CellFlags::WIDE_CHAR,
            attrs,
            hyperlink: 0,
            combining: ['\0'; MAX_COMBINING],
            combining_len: 0,
        };
        let continuation = Self {
            content: ' ',
            width: 0,
            flags: CellFlags::WIDE_CONTINUATION,
            attrs,
            hyperlink: 0,
            combining: ['\0'; MAX_COMBINING],
            combining_len: 0,
        };
        (leading, continuation)
    }

    /// The character content of this cell.
    #[inline]
    pub fn content(&self) -> char {
        self.content
    }

    /// The display width in terminal columns.
    #[inline]
    pub fn width(&self) -> u8 {
        self.width
    }

    /// Whether this cell is the leading half of a wide character.
    #[inline]
    pub fn is_wide(&self) -> bool {
        self.flags.contains(CellFlags::WIDE_CHAR)
    }

    /// Whether this cell is a continuation (trailing half) of a wide character.
    #[inline]
    pub fn is_wide_continuation(&self) -> bool {
        self.flags.contains(CellFlags::WIDE_CONTINUATION)
    }

    /// Set the character content and display width.
    pub fn set_content(&mut self, ch: char, width: u8) {
        self.content = ch;
        self.width = width;
        // Clear wide and combining flags when replacing content.
        self.flags
            .remove(CellFlags::WIDE_CHAR | CellFlags::WIDE_CONTINUATION | CellFlags::HAS_COMBINING);
        self.combining_len = 0;
    }

    /// Reset this cell to a blank space with the given background attributes.
    ///
    /// Used by erase operations (ED, EL, ECH) which fill with the current
    /// background color but reset all other attributes.
    pub fn erase(&mut self, bg: Color) {
        self.content = ' ';
        self.width = 1;
        self.flags = CellFlags::empty();
        self.attrs = SgrAttrs {
            bg,
            ..SgrAttrs::default()
        };
        self.hyperlink = 0;
        self.combining_len = 0;
    }

    /// Reset this cell to a blank space with default attributes.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    // ── Combining mark support ────────────────────────────────────────

    /// Append a combining mark to this cell's grapheme cluster.
    ///
    /// Returns `true` if the mark was stored, `false` if the inline buffer
    /// is full (excess marks are silently dropped).
    pub fn push_combining(&mut self, mark: char) -> bool {
        let len = self.combining_len as usize;
        if len >= MAX_COMBINING {
            return false;
        }
        self.combining[len] = mark;
        self.combining_len += 1;
        self.flags.insert(CellFlags::HAS_COMBINING);
        true
    }

    /// The combining marks attached to this cell's base character.
    #[inline]
    pub fn combining_marks(&self) -> &[char] {
        // SAFETY: combining_len is always <= MAX_COMBINING and indices are valid
        &self.combining[..self.combining_len as usize]
    }

    /// Whether this cell has any combining marks.
    #[inline]
    pub fn has_combining(&self) -> bool {
        self.combining_len > 0
    }

    /// Compute terminal display width for a single Unicode scalar.
    ///
    /// Returns:
    /// - `0` for non-spacing marks/format controls (combining marks, ZWJ, VS16, etc.)
    /// - `1` for narrow characters
    /// - `2` for wide characters (CJK, emoji presentation)
    ///
    /// Widths above 2 are clamped to 2 for terminal cell semantics.
    pub fn display_width(ch: char) -> u8 {
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        width.min(2) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::grid::Grid;
    use crate::scrollback::Scrollback;

    #[test]
    fn default_cell_is_space() {
        let cell = Cell::default();
        assert_eq!(cell.content(), ' ');
        assert_eq!(cell.width(), 1);
        assert_eq!(cell.attrs, SgrAttrs::default());
        assert!(!cell.is_wide());
        assert!(!cell.is_wide_continuation());
        assert_eq!(cell.hyperlink, 0);
    }

    #[test]
    fn cell_new_has_default_attrs() {
        let cell = Cell::new('A');
        assert_eq!(cell.content(), 'A');
        assert_eq!(cell.attrs.flags, SgrFlags::empty());
        assert_eq!(cell.attrs.fg, Color::Default);
        assert_eq!(cell.attrs.bg, Color::Default);
    }

    #[test]
    fn cell_erase_clears_content_and_attrs() {
        let mut cell = Cell::with_attrs(
            'X',
            1,
            SgrAttrs {
                flags: SgrFlags::BOLD | SgrFlags::ITALIC,
                fg: Color::Named(1),
                bg: Color::Named(4),
                underline_color: None,
            },
        );
        cell.hyperlink = 42;
        cell.erase(Color::Named(2));
        assert_eq!(cell.content(), ' ');
        assert_eq!(cell.attrs.flags, SgrFlags::empty());
        assert_eq!(cell.attrs.fg, Color::Default);
        assert_eq!(cell.attrs.bg, Color::Named(2));
        assert_eq!(cell.hyperlink, 0);
    }

    #[test]
    fn wide_char_pair() {
        let attrs = SgrAttrs {
            flags: SgrFlags::BOLD,
            ..SgrAttrs::default()
        };
        let (lead, cont) = Cell::wide('\u{4E2D}', attrs); // '中'
        assert!(lead.is_wide());
        assert!(!lead.is_wide_continuation());
        assert_eq!(lead.width(), 2);
        assert_eq!(lead.content(), '中');

        assert!(!cont.is_wide());
        assert!(cont.is_wide_continuation());
        assert_eq!(cont.width(), 0);
    }

    #[test]
    fn set_content_clears_wide_flags() {
        let (mut lead, _) = Cell::wide('中', SgrAttrs::default());
        assert!(lead.is_wide());
        lead.set_content('A', 1);
        assert!(!lead.is_wide());
        assert!(!lead.is_wide_continuation());
    }

    #[test]
    fn erase_clears_wide_flags() {
        let (mut lead, _) = Cell::wide('中', SgrAttrs::default());
        lead.erase(Color::Default);
        assert!(!lead.is_wide());
    }

    #[test]
    fn display_width_unicode_cases() {
        assert_eq!(Cell::display_width('a'), 1);
        assert_eq!(Cell::display_width('中'), 2);
        assert_eq!(Cell::display_width('\u{1F680}'), 2); // 🚀
        assert_eq!(Cell::display_width('\u{0301}'), 0); // combining acute accent
        assert_eq!(Cell::display_width('\u{200D}'), 0); // ZWJ
        assert_eq!(Cell::display_width('\u{FE0F}'), 0); // VS16
    }

    #[test]
    fn sgr_attrs_reset() {
        let mut attrs = SgrAttrs {
            flags: SgrFlags::BOLD,
            fg: Color::Rgb(255, 0, 0),
            bg: Color::Indexed(42),
            underline_color: Some(Color::Named(3)),
        };
        attrs.reset();
        assert_eq!(attrs, SgrAttrs::default());
    }

    #[test]
    fn sgr_attrs_apply_params_basic_colors_and_reset() {
        let mut attrs = SgrAttrs::default();
        attrs.apply_sgr_params(&[31]);
        assert_eq!(attrs.fg, Color::Named(1));
        attrs.apply_sgr_params(&[44]);
        assert_eq!(attrs.bg, Color::Named(4));
        attrs.apply_sgr_params(&[0]);
        assert_eq!(attrs, SgrAttrs::default());
    }

    #[test]
    fn sgr_attrs_apply_params_extended_colors() {
        let mut attrs = SgrAttrs::default();
        attrs.apply_sgr_params(&[38, 5, 200]);
        assert_eq!(attrs.fg, Color::Indexed(200));
        attrs.apply_sgr_params(&[48, 2, 1, 2, 3]);
        assert_eq!(attrs.bg, Color::Rgb(1, 2, 3));
        attrs.apply_sgr_params(&[39, 49]);
        assert_eq!(attrs.fg, Color::Default);
        assert_eq!(attrs.bg, Color::Default);
    }

    #[test]
    fn sgr_attrs_apply_params_empty_means_reset() {
        let mut attrs = SgrAttrs {
            flags: SgrFlags::BOLD,
            fg: Color::Named(2),
            bg: Color::Named(3),
            underline_color: Some(Color::Indexed(7)),
        };
        attrs.apply_sgr_params(&[]);
        assert_eq!(attrs, SgrAttrs::default());
    }

    #[test]
    fn color_default() {
        assert_eq!(Color::default(), Color::Default);
    }

    #[test]
    fn cell_clear_resets_everything() {
        let mut cell = Cell::with_attrs(
            'Z',
            2,
            SgrAttrs {
                flags: SgrFlags::BOLD | SgrFlags::UNDERLINE,
                fg: Color::Rgb(1, 2, 3),
                bg: Color::Named(5),
                underline_color: Some(Color::Indexed(100)),
            },
        );
        cell.hyperlink = 99;
        cell.flags = CellFlags::WIDE_CHAR;
        cell.clear();
        assert_eq!(cell, Cell::default());
    }

    // --- Hyperlink registry fixtures (bd-lff4p.1.7) ---

    #[test]
    fn hyperlink_registry_intern_and_get() {
        let mut reg = HyperlinkRegistry::new();
        let id = reg.intern("https://example.com");
        assert_ne!(id, 0);
        assert_eq!(reg.get(id), Some("https://example.com"));
    }

    #[test]
    fn hyperlink_registry_dedup_and_id_reuse_on_release() {
        let mut reg = HyperlinkRegistry::new();
        let id1 = reg.intern("https://one.test");
        let id2 = reg.intern("https://one.test");
        assert_eq!(id1, id2);

        // Acquire twice (two cells) then release twice -> should free the slot.
        reg.acquire_id(id1);
        reg.acquire_id(id1);
        reg.release_id(id1);
        reg.release_id(id1);
        assert_eq!(reg.get(id1), None);

        // Next distinct URI should reuse the freed ID.
        let reused = reg.intern("https://two.test");
        assert_eq!(reused, id1);
        assert_eq!(reg.get(reused), Some("https://two.test"));
    }

    #[test]
    fn hyperlink_registry_overlap_and_reset() {
        let mut reg = HyperlinkRegistry::new();
        let id_a = reg.acquire("https://a.test");
        let id_b = reg.acquire("https://b.test");

        // Simulate two adjacent cells with different links (overlap boundary).
        let mut c0 = Cell::new('x');
        c0.hyperlink = id_a;
        let mut c1 = Cell::new('y');
        c1.hyperlink = id_b;

        assert_eq!(reg.get(c0.hyperlink), Some("https://a.test"));
        assert_eq!(reg.get(c1.hyperlink), Some("https://b.test"));

        // Reset: clear a cell's hyperlink and release the old reference.
        reg.release_id(c0.hyperlink);
        c0.hyperlink = 0;
        assert_eq!(reg.get(c0.hyperlink), None);
    }

    #[test]
    fn click_mapping_via_grid_helper() {
        let mut reg = HyperlinkRegistry::new();
        let id = reg.acquire("https://click.test");
        let mut grid = Grid::new(3, 1);
        let cell = grid.cell_mut(0, 1).unwrap();
        *cell = Cell::new('C');
        cell.hyperlink = id;

        assert_eq!(
            grid.hyperlink_uri_at(0, 1, &reg),
            Some("https://click.test")
        );
        assert_eq!(grid.hyperlink_uri_at(0, 0, &reg), None);
        assert_eq!(grid.hyperlink_uri_at(9, 9, &reg), None);
    }

    // ── SGR flag set/remove cycles ────────────────────────────────────

    #[test]
    fn sgr_dim_flag() {
        let mut a = SgrAttrs::default();
        a.apply_sgr_params(&[2]);
        assert!(a.flags.contains(SgrFlags::DIM));
        a.apply_sgr_params(&[22]); // removes BOLD | DIM
        assert!(!a.flags.contains(SgrFlags::DIM));
    }

    #[test]
    fn sgr_underline_and_variants() {
        let mut a = SgrAttrs::default();
        a.apply_sgr_params(&[4]);
        assert!(a.flags.contains(SgrFlags::UNDERLINE));
        a.apply_sgr_params(&[24]); // removes underline + double + curly
        assert!(!a.flags.contains(SgrFlags::UNDERLINE));

        a.apply_sgr_params(&[21]);
        assert!(a.flags.contains(SgrFlags::DOUBLE_UNDERLINE));
        a.apply_sgr_params(&[24]);
        assert!(!a.flags.contains(SgrFlags::DOUBLE_UNDERLINE));
    }

    #[test]
    fn sgr_blink_inverse_hidden_strikethrough() {
        let mut a = SgrAttrs::default();

        a.apply_sgr_params(&[5]);
        assert!(a.flags.contains(SgrFlags::BLINK));
        a.apply_sgr_params(&[25]);
        assert!(!a.flags.contains(SgrFlags::BLINK));

        a.apply_sgr_params(&[7]);
        assert!(a.flags.contains(SgrFlags::INVERSE));
        a.apply_sgr_params(&[27]);
        assert!(!a.flags.contains(SgrFlags::INVERSE));

        a.apply_sgr_params(&[8]);
        assert!(a.flags.contains(SgrFlags::HIDDEN));
        a.apply_sgr_params(&[28]);
        assert!(!a.flags.contains(SgrFlags::HIDDEN));

        a.apply_sgr_params(&[9]);
        assert!(a.flags.contains(SgrFlags::STRIKETHROUGH));
        a.apply_sgr_params(&[29]);
        assert!(!a.flags.contains(SgrFlags::STRIKETHROUGH));
    }

    #[test]
    fn sgr_overline() {
        let mut a = SgrAttrs::default();
        a.apply_sgr_params(&[53]);
        assert!(a.flags.contains(SgrFlags::OVERLINE));
        a.apply_sgr_params(&[55]);
        assert!(!a.flags.contains(SgrFlags::OVERLINE));
    }

    #[test]
    fn sgr_bold_dim_remove_clears_both() {
        let mut a = SgrAttrs::default();
        a.apply_sgr_params(&[1, 2]); // set bold + dim
        assert!(a.flags.contains(SgrFlags::BOLD));
        assert!(a.flags.contains(SgrFlags::DIM));
        a.apply_sgr_params(&[22]); // removes both
        assert!(!a.flags.contains(SgrFlags::BOLD));
        assert!(!a.flags.contains(SgrFlags::DIM));
    }

    #[test]
    fn sgr_bright_fg_colors() {
        let mut a = SgrAttrs::default();
        a.apply_sgr_params(&[90]);
        assert_eq!(a.fg, Color::Named(8)); // bright black
        a.apply_sgr_params(&[97]);
        assert_eq!(a.fg, Color::Named(15)); // bright white
    }

    #[test]
    fn sgr_bright_bg_colors() {
        let mut a = SgrAttrs::default();
        a.apply_sgr_params(&[100]);
        assert_eq!(a.bg, Color::Named(8));
        a.apply_sgr_params(&[107]);
        assert_eq!(a.bg, Color::Named(15));
    }

    #[test]
    fn sgr_underline_color_set_and_reset() {
        let mut a = SgrAttrs::default();
        a.apply_sgr_params(&[58, 5, 42]);
        assert_eq!(a.underline_color, Some(Color::Indexed(42)));
        a.apply_sgr_params(&[59]);
        assert_eq!(a.underline_color, None);

        a.apply_sgr_params(&[58, 2, 10, 20, 30]);
        assert_eq!(a.underline_color, Some(Color::Rgb(10, 20, 30)));
    }

    #[test]
    fn sgr_multiple_params_in_one_call() {
        let mut a = SgrAttrs::default();
        a.apply_sgr_params(&[1, 3, 31, 42]);
        assert!(a.flags.contains(SgrFlags::BOLD));
        assert!(a.flags.contains(SgrFlags::ITALIC));
        assert_eq!(a.fg, Color::Named(1)); // red
        assert_eq!(a.bg, Color::Named(2)); // green
    }

    #[test]
    fn sgr_unknown_param_ignored() {
        let mut a = SgrAttrs::default();
        a.apply_sgr_params(&[999]);
        assert_eq!(a, SgrAttrs::default());
    }

    #[test]
    fn sgr_indexed_fg_clamps_to_255() {
        let mut a = SgrAttrs::default();
        a.apply_sgr_params(&[38, 5, 300]);
        assert_eq!(a.fg, Color::Indexed(255));
    }

    #[test]
    fn sgr_rgb_clamps_to_255() {
        let mut a = SgrAttrs::default();
        a.apply_sgr_params(&[38, 2, 999, 500, 300]);
        assert_eq!(a.fg, Color::Rgb(255, 255, 255));
    }

    #[test]
    fn sgr_extended_color_with_bad_mode_ignored() {
        let mut a = SgrAttrs::default();
        // mode=99 is neither 5 (indexed) nor 2 (rgb) → ignored
        a.apply_sgr_params(&[38, 99, 100]);
        assert_eq!(a.fg, Color::Default);
    }

    #[test]
    fn sgr_truncated_extended_color_ignored() {
        let mut a = SgrAttrs::default();
        // 38 alone, no mode byte
        a.apply_sgr_params(&[38]);
        assert_eq!(a.fg, Color::Default);
        // 38;5 without index
        a.apply_sgr_params(&[38, 5]);
        assert_eq!(a.fg, Color::Default);
        // 38;2 without enough rgb values
        a.apply_sgr_params(&[38, 2, 10]);
        assert_eq!(a.fg, Color::Default);
    }

    // ── HyperlinkRegistry utility methods ───────────────────────────────

    #[test]
    fn hyperlink_intern_empty_returns_zero() {
        let mut reg = HyperlinkRegistry::new();
        assert_eq!(reg.intern(""), 0);
    }

    #[test]
    fn hyperlink_get_zero_returns_none() {
        let reg = HyperlinkRegistry::new();
        assert_eq!(reg.get(0), None);
    }

    #[test]
    fn hyperlink_get_invalid_id_returns_none() {
        let reg = HyperlinkRegistry::new();
        assert_eq!(reg.get(999), None);
    }

    #[test]
    fn hyperlink_contains() {
        let mut reg = HyperlinkRegistry::new();
        let id = reg.intern("https://test.com");
        assert!(reg.contains(id));
        assert!(!reg.contains(0));
        assert!(!reg.contains(999));
    }

    #[test]
    fn hyperlink_len_and_is_empty() {
        let mut reg = HyperlinkRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);

        let id = reg.intern("https://a.test");
        assert!(!reg.is_empty());
        assert_eq!(reg.len(), 1);

        reg.intern("https://b.test");
        assert_eq!(reg.len(), 2);

        // Same URI doesn't increase count
        reg.intern("https://a.test");
        assert_eq!(reg.len(), 2);

        // Release frees slot
        reg.acquire_id(id);
        reg.release_id(id);
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn hyperlink_clear_resets() {
        let mut reg = HyperlinkRegistry::new();
        let id = reg.intern("https://x.test");
        reg.clear();
        assert!(reg.is_empty());
        assert_eq!(reg.get(id), None);
    }

    #[test]
    fn hyperlink_release_id_zero_is_noop() {
        let mut reg = HyperlinkRegistry::new();
        reg.release_id(0); // should not panic
        assert!(reg.is_empty());
    }

    #[test]
    fn hyperlink_release_invalid_id_is_noop() {
        let mut reg = HyperlinkRegistry::new();
        reg.release_id(500); // should not panic
    }

    #[test]
    fn hyperlink_acquire_id_zero_is_noop() {
        let mut reg = HyperlinkRegistry::new();
        reg.acquire_id(0); // should not panic
        assert!(reg.is_empty());
    }

    // ── Cell with_attrs ────────────────────────────────────────────────

    #[test]
    fn cell_with_attrs_preserves_values() {
        let attrs = SgrAttrs {
            flags: SgrFlags::ITALIC | SgrFlags::UNDERLINE,
            fg: Color::Rgb(10, 20, 30),
            bg: Color::Indexed(42),
            underline_color: Some(Color::Named(3)),
        };
        let cell = Cell::with_attrs('Q', 2, attrs);
        assert_eq!(cell.content(), 'Q');
        assert_eq!(cell.width(), 2);
        assert_eq!(cell.attrs, attrs);
        assert_eq!(cell.hyperlink, 0);
        assert!(!cell.is_wide());
    }

    #[test]
    fn clear_on_scrollback_eviction() {
        let mut reg = HyperlinkRegistry::new();
        let mut sb = Scrollback::new(1);

        // First line uses link A in 3 cells.
        let mut row_a = vec![Cell::new('a'), Cell::new('a'), Cell::new('a')];
        let id_a = reg.intern("https://a.test");
        for cell in &mut row_a {
            reg.acquire_id(id_a);
            cell.hyperlink = id_a;
        }
        assert_eq!(reg.get(id_a), Some("https://a.test"));

        // Push A then push B, evicting A. Release references from the evicted line.
        let _ = sb.push_row(&row_a, false);
        let row_b = vec![Cell::new('b')];
        let evicted = sb.push_row(&row_b, false).expect("capacity=1 must evict");
        reg.release_cells(&evicted.cells);

        // A should be gone after all references were released.
        assert_eq!(reg.get(id_a), None);
    }

    // ── Combining mark support ────────────────────────────────────────

    #[test]
    fn push_combining_stores_mark() {
        let mut cell = Cell::new('e');
        assert!(!cell.has_combining());
        assert!(cell.combining_marks().is_empty());

        assert!(cell.push_combining('\u{0301}')); // combining acute accent
        assert!(cell.has_combining());
        assert_eq!(cell.combining_marks(), &['\u{0301}']);
        assert!(cell.flags.contains(CellFlags::HAS_COMBINING));
    }

    #[test]
    fn push_combining_multiple_marks() {
        let mut cell = Cell::new('a');
        assert!(cell.push_combining('\u{0300}')); // combining grave
        assert!(cell.push_combining('\u{0301}')); // combining acute
        assert_eq!(cell.combining_marks(), &['\u{0300}', '\u{0301}']);
    }

    #[test]
    fn push_combining_overflow_returns_false() {
        let mut cell = Cell::new('a');
        for i in 0..MAX_COMBINING {
            assert!(cell.push_combining(char::from(0x0300u16 + i as u16)));
        }
        // Buffer full — next push returns false.
        assert!(!cell.push_combining('\u{0302}'));
        assert_eq!(cell.combining_marks().len(), MAX_COMBINING);
    }

    #[test]
    fn set_content_clears_combining() {
        let mut cell = Cell::new('e');
        cell.push_combining('\u{0301}');
        assert!(cell.has_combining());

        cell.set_content('x', 1);
        assert!(!cell.has_combining());
        assert!(cell.combining_marks().is_empty());
        assert!(!cell.flags.contains(CellFlags::HAS_COMBINING));
    }

    #[test]
    fn erase_clears_combining() {
        let mut cell = Cell::new('e');
        cell.push_combining('\u{0301}');
        cell.erase(Color::Default);
        assert!(!cell.has_combining());
        assert!(cell.combining_marks().is_empty());
    }

    #[test]
    fn clear_clears_combining() {
        let mut cell = Cell::new('e');
        cell.push_combining('\u{0301}');
        cell.clear();
        assert!(!cell.has_combining());
        assert_eq!(cell, Cell::default());
    }

    #[test]
    fn combining_on_wide_char() {
        let (mut lead, _) = Cell::wide('中', SgrAttrs::default());
        assert!(lead.push_combining('\u{0300}'));
        assert_eq!(lead.combining_marks(), &['\u{0300}']);
        assert_eq!(lead.content(), '中');
        assert!(lead.is_wide());
    }

    #[test]
    fn default_cell_has_no_combining() {
        let cell = Cell::default();
        assert!(!cell.has_combining());
        assert!(cell.combining_marks().is_empty());
    }
}
