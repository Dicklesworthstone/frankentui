#![forbid(unsafe_code)]

//! Cell types and invariants.
//!
//! The `Cell` is the fundamental unit of the terminal grid. Each cell occupies
//! exactly **16 bytes** to ensure optimal cache utilization (4 cells per 64-byte
//! cache line) and enable fast SIMD comparisons.
//!
//! # Layout (16 bytes, non-negotiable)
//!
//! ```text
//! Cell {
//!     content: CellContent,  // 4 bytes - char or GraphemeId
//!     fg: PackedRgba,        // 4 bytes - foreground color
//!     bg: PackedRgba,        // 4 bytes - background color
//!     attrs: CellAttrs,      // 4 bytes - style flags + link ID
//! }
//! ```
//!
//! # Why 16 Bytes?
//!
//! - 4 cells per 64-byte cache line (perfect fit)
//! - Single 128-bit SIMD comparison
//! - No heap allocation for 99% of cells
//! - 24 bytes wastes cache, 32 bytes doubles bandwidth

use crate::char_width;

/// Grapheme ID: reference to an interned string in [`GraphemePool`].
///
/// # Layout
///
/// ```text
/// [30-24: width (7 bits)][23-0: pool slot (24 bits)]
/// ```
///
/// # Capacity
///
/// - Pool slots: 16,777,216 (24 bits = 16M entries)
/// - Width range: 0-127 (7 bits, plenty for any display width)
///
/// # Design Rationale
///
/// - 24 bits for slot allows 16M unique graphemes (far exceeding practical usage)
/// - 7 bits for width allows display widths 0-127 (most graphemes are 1-2)
/// - Embedded width avoids pool lookup for width queries
/// - Total 31 bits leaves bit 31 for `CellContent` type discrimination
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(transparent)]
pub struct GraphemeId(u32);

impl GraphemeId {
    /// Maximum slot index (24 bits).
    pub const MAX_SLOT: u32 = 0x00FF_FFFF;

    /// Maximum width (7 bits).
    pub const MAX_WIDTH: u8 = 127;

    /// Create a new `GraphemeId` from slot index and display width.
    ///
    /// # Panics
    ///
    /// Panics in debug mode if `slot > MAX_SLOT` or `width > MAX_WIDTH`.
    #[inline]
    pub const fn new(slot: u32, width: u8) -> Self {
        debug_assert!(slot <= Self::MAX_SLOT, "slot overflow");
        debug_assert!(width <= Self::MAX_WIDTH, "width overflow");
        Self((slot & Self::MAX_SLOT) | ((width as u32) << 24))
    }

    /// Extract the pool slot index (0-16M).
    #[inline]
    pub const fn slot(self) -> usize {
        (self.0 & Self::MAX_SLOT) as usize
    }

    /// Extract the display width (0-127).
    #[inline]
    pub const fn width(self) -> usize {
        ((self.0 >> 24) & 0x7F) as usize
    }

    /// Raw u32 value for storage in `CellContent`.
    #[inline]
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// Reconstruct from a raw u32.
    #[inline]
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }
}

impl core::fmt::Debug for GraphemeId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("GraphemeId")
            .field("slot", &self.slot())
            .field("width", &self.width())
            .finish()
    }
}

/// Cell content: either a direct Unicode char or a reference to a grapheme cluster.
///
/// # Encoding Scheme (4 bytes)
///
/// ```text
/// Bit 31 (type discriminator):
///   0: Direct char (bits 0-20 contain Unicode scalar value, max U+10FFFF)
///   1: GraphemeId reference (bits 0-30 contain slot + width)
/// ```
///
/// This allows:
/// - 99% of cells (ASCII/BMP) to be stored without heap allocation
/// - Complex graphemes (emoji, ZWJ sequences) stored in pool
///
/// # Special Values
///
/// - `EMPTY` (0x0): Empty cell, width 0
/// - `CONTINUATION` (0x1): Placeholder for wide character continuation
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct CellContent(u32);

impl CellContent {
    /// Empty cell content (no character).
    pub const EMPTY: Self = Self(0);

    /// Continuation marker for wide characters.
    ///
    /// When a character has display width > 1, subsequent cells are filled
    /// with this marker to indicate they are part of the previous character.
    ///
    /// Value is `0x7FFF_FFFF` (max i32), which is outside valid Unicode scalar
    /// range (0..0x10FFFF) but fits in 31 bits (Direct Char mode).
    pub const CONTINUATION: Self = Self(0x7FFF_FFFF);

    /// Create content from a single Unicode character.
    ///
    /// For characters with display width > 1, subsequent cells should be
    /// filled with `CONTINUATION`.
    #[inline]
    pub const fn from_char(c: char) -> Self {
        Self(c as u32)
    }

    /// Create content from a grapheme ID (for multi-codepoint clusters).
    ///
    /// The grapheme ID references an entry in the `GraphemePool`.
    #[inline]
    pub const fn from_grapheme(id: GraphemeId) -> Self {
        Self(0x8000_0000 | id.raw())
    }

    /// Check if this content is a grapheme reference (vs direct char).
    #[inline]
    pub const fn is_grapheme(self) -> bool {
        self.0 & 0x8000_0000 != 0
    }

    /// Check if this is a continuation cell (part of a wide character).
    #[inline]
    pub const fn is_continuation(self) -> bool {
        self.0 == Self::CONTINUATION.0
    }

    /// Check if this cell is empty.
    #[inline]
    pub const fn is_empty(self) -> bool {
        self.0 == Self::EMPTY.0
    }

    /// Extract the character if this is a direct char (not a grapheme).
    ///
    /// Returns `None` if this is empty, continuation, or a grapheme reference.
    #[inline]
    pub fn as_char(self) -> Option<char> {
        if self.is_grapheme() || self.0 == Self::EMPTY.0 || self.0 == Self::CONTINUATION.0 {
            None
        } else {
            char::from_u32(self.0)
        }
    }

    /// Extract the grapheme ID if this is a grapheme reference.
    ///
    /// Returns `None` if this is a direct char.
    #[inline]
    pub const fn grapheme_id(self) -> Option<GraphemeId> {
        if self.is_grapheme() {
            Some(GraphemeId::from_raw(self.0 & !0x8000_0000))
        } else {
            None
        }
    }

    /// Get the display width of this content.
    ///
    /// - Empty: 0
    /// - Continuation: 0
    /// - Grapheme: width embedded in GraphemeId
    /// - Char: requires external width lookup (returns 1 as default for ASCII)
    ///
    /// Note: For accurate char width, use the unicode-display-width-based
    /// helpers in this crate. This method provides a fast path for known cases.
    #[inline]
    pub const fn width_hint(self) -> usize {
        if self.is_empty() || self.is_continuation() {
            0
        } else if self.is_grapheme() {
            ((self.0 >> 24) & 0x7F) as usize
        } else {
            // For direct chars, assume width 1 (fast path for ASCII)
            // Callers should use unicode-width for accurate measurement
            1
        }
    }

    /// Get the display width of this content with Unicode width semantics.
    ///
    /// This is the accurate (but slower) width computation for direct chars.
    #[inline]
    pub fn width(self) -> usize {
        if self.is_empty() || self.is_continuation() {
            0
        } else if self.is_grapheme() {
            ((self.0 >> 24) & 0x7F) as usize
        } else {
            let Some(c) = self.as_char() else {
                return 1;
            };
            char_width(c)
        }
    }

    /// Raw u32 value.
    #[inline]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

impl Default for CellContent {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl core::fmt::Debug for CellContent {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.is_empty() {
            write!(f, "CellContent::EMPTY")
        } else if self.is_continuation() {
            write!(f, "CellContent::CONTINUATION")
        } else if let Some(c) = self.as_char() {
            write!(f, "CellContent::Char({c:?})")
        } else if let Some(id) = self.grapheme_id() {
            write!(f, "CellContent::Grapheme({id:?})")
        } else {
            write!(f, "CellContent(0x{:08x})", self.0)
        }
    }
}

/// A single terminal cell (16 bytes).
///
/// # Layout
///
/// ```text
/// #[repr(C, align(16))]
/// Cell {
///     content: CellContent,  // 4 bytes
///     fg: PackedRgba,        // 4 bytes
///     bg: PackedRgba,        // 4 bytes
///     attrs: CellAttrs,      // 4 bytes
/// }
/// ```
///
/// # Invariants
///
/// - Size is exactly 16 bytes (verified by compile-time assert)
/// - All fields are valid (no uninitialized memory)
/// - Continuation cells should not have meaningful fg/bg (they inherit from parent)
///
/// # Default
///
/// The default cell is empty with transparent background, white foreground,
/// and no style attributes.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(C, align(16))]
pub struct Cell {
    /// Character or grapheme content.
    pub content: CellContent,
    /// Foreground color.
    pub fg: PackedRgba,
    /// Background color.
    pub bg: PackedRgba,
    /// Style flags and hyperlink ID.
    pub attrs: CellAttrs,
}

// Compile-time size check
const _: () = assert!(core::mem::size_of::<Cell>() == 16);

impl Cell {
    /// A continuation cell (placeholder for wide characters).
    ///
    /// When a character has display width > 1, subsequent cells are filled
    /// with this to indicate they are "owned" by the previous cell.
    pub const CONTINUATION: Self = Self {
        content: CellContent::CONTINUATION,
        fg: PackedRgba::TRANSPARENT,
        bg: PackedRgba::TRANSPARENT,
        attrs: CellAttrs::NONE,
    };

    /// Create a new cell with the given content and default colors.
    #[inline]
    pub const fn new(content: CellContent) -> Self {
        Self {
            content,
            fg: PackedRgba::WHITE,
            bg: PackedRgba::TRANSPARENT,
            attrs: CellAttrs::NONE,
        }
    }

    /// Create a cell from a single character.
    #[inline]
    pub const fn from_char(c: char) -> Self {
        Self::new(CellContent::from_char(c))
    }

    /// Check if this is a continuation cell.
    #[inline]
    pub const fn is_continuation(&self) -> bool {
        self.content.is_continuation()
    }

    /// Check if this cell is empty.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.content.is_empty()
    }

    /// Get the display width hint for this cell.
    ///
    /// See [`CellContent::width_hint`] for details.
    #[inline]
    pub const fn width_hint(&self) -> usize {
        self.content.width_hint()
    }

    /// Bitwise equality comparison (fast path for diffing).
    ///
    /// Uses bitwise AND (`&`) instead of short-circuit AND (`&&`) so all
    /// four u32 comparisons are always evaluated. This avoids branch
    /// mispredictions in tight loops and allows LLVM to lower the check
    /// to a single 128-bit SIMD compare on supported targets.
    #[inline]
    pub fn bits_eq(&self, other: &Self) -> bool {
        (self.content.raw() == other.content.raw())
            & (self.fg == other.fg)
            & (self.bg == other.bg)
            & (self.attrs == other.attrs)
    }

    /// Set the cell content to a character, preserving other fields.
    #[inline]
    pub const fn with_char(mut self, c: char) -> Self {
        self.content = CellContent::from_char(c);
        self
    }

    /// Set the foreground color.
    #[inline]
    pub const fn with_fg(mut self, fg: PackedRgba) -> Self {
        self.fg = fg;
        self
    }

    /// Set the background color.
    #[inline]
    pub const fn with_bg(mut self, bg: PackedRgba) -> Self {
        self.bg = bg;
        self
    }

    /// Set the style attributes.
    #[inline]
    pub const fn with_attrs(mut self, attrs: CellAttrs) -> Self {
        self.attrs = attrs;
        self
    }
}
impl Default for Cell {
    fn default() -> Self {
        Self {
            content: CellContent::EMPTY,
            fg: PackedRgba::WHITE,
            bg: PackedRgba::TRANSPARENT,
            attrs: CellAttrs::NONE,
        }
    }
}

impl core::fmt::Debug for Cell {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Cell")
            .field("content", &self.content)
            .field("fg", &self.fg)
            .field("bg", &self.bg)
            .field("attrs", &self.attrs)
            .finish()
    }
}

/// A compact RGBA color.
///
/// - **Size:** 4 bytes (fits within the `Cell` 16-byte budget).
/// - **Layout:** `0xRRGGBBAA` (R in bits 31..24, A in bits 7..0).
///
/// Notes
/// -----
/// This is **straight alpha** storage (RGB channels are not pre-multiplied).
/// Compositing uses Porter-Duff **SourceOver** (`src over dst`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[repr(transparent)]
pub struct PackedRgba(pub u32);

impl PackedRgba {
    /// Fully transparent (alpha = 0).
    pub const TRANSPARENT: Self = Self(0);
    /// Opaque black.
    pub const BLACK: Self = Self::rgb(0, 0, 0);
    /// Opaque white.
    pub const WHITE: Self = Self::rgb(255, 255, 255);
    /// Opaque red.
    pub const RED: Self = Self::rgb(255, 0, 0);
    /// Opaque green.
    pub const GREEN: Self = Self::rgb(0, 255, 0);
    /// Opaque blue.
    pub const BLUE: Self = Self::rgb(0, 0, 255);

    /// Create an opaque RGB color (alpha = 255).
    #[inline]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self::rgba(r, g, b, 255)
    }

    /// Create an RGBA color with explicit alpha.
    #[inline]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self(((r as u32) << 24) | ((g as u32) << 16) | ((b as u32) << 8) | (a as u32))
    }

    /// Red channel.
    #[inline]
    pub const fn r(self) -> u8 {
        (self.0 >> 24) as u8
    }

    /// Green channel.
    #[inline]
    pub const fn g(self) -> u8 {
        (self.0 >> 16) as u8
    }

    /// Blue channel.
    #[inline]
    pub const fn b(self) -> u8 {
        (self.0 >> 8) as u8
    }

    /// Alpha channel.
    #[inline]
    pub const fn a(self) -> u8 {
        self.0 as u8
    }

    #[inline]
    const fn div_round_u8(numer: u64, denom: u64) -> u8 {
        debug_assert!(denom != 0);
        let v = (numer + (denom / 2)) / denom;
        if v > 255 { 255 } else { v as u8 }
    }

    /// Porter-Duff SourceOver: `src over dst`.
    ///
    /// Stored as straight alpha, so we compute the exact rational form and round at the end
    /// (avoids accumulating rounding error across intermediate steps).
    #[inline]
    pub fn over(self, dst: Self) -> Self {
        let s_a = self.a() as u64;
        if s_a == 255 {
            return self;
        }
        if s_a == 0 {
            return dst;
        }

        let d_a = dst.a() as u64;
        let inv_s_a = 255 - s_a;

        // out_a = s_a + d_a*(1 - s_a)  (all in [0,1], scaled by 255)
        // We compute numer_a in the "255^2 domain" to keep channels exact:
        // numer_a = 255*s_a + d_a*(255 - s_a)
        // out_a_u8 = round(numer_a / 255)
        let numer_a = 255 * s_a + d_a * inv_s_a;
        if numer_a == 0 {
            return Self::TRANSPARENT;
        }

        let out_a = Self::div_round_u8(numer_a, 255);

        // For straight alpha, the exact rational (scaled to [0,255]) is:
        // out_c_u8 = round( (src_c*s_a*255 + dst_c*d_a*(255 - s_a)) / numer_a )
        let r = Self::div_round_u8(
            (self.r() as u64) * s_a * 255 + (dst.r() as u64) * d_a * inv_s_a,
            numer_a,
        );
        let g = Self::div_round_u8(
            (self.g() as u64) * s_a * 255 + (dst.g() as u64) * d_a * inv_s_a,
            numer_a,
        );
        let b = Self::div_round_u8(
            (self.b() as u64) * s_a * 255 + (dst.b() as u64) * d_a * inv_s_a,
            numer_a,
        );

        Self::rgba(r, g, b, out_a)
    }

    /// Apply uniform opacity in `[0.0, 1.0]` by scaling alpha.
    #[inline]
    pub fn with_opacity(self, opacity: f32) -> Self {
        let opacity = opacity.clamp(0.0, 1.0);
        let a = ((self.a() as f32) * opacity).round().clamp(0.0, 255.0) as u8;
        Self::rgba(self.r(), self.g(), self.b(), a)
    }
}

bitflags::bitflags! {
    /// 8-bit cell style flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct StyleFlags: u8 {
        /// Bold / increased intensity.
        const BOLD          = 0b0000_0001;
        /// Dim / decreased intensity.
        const DIM           = 0b0000_0010;
        /// Italic text.
        const ITALIC        = 0b0000_0100;
        /// Underlined text.
        const UNDERLINE     = 0b0000_1000;
        /// Blinking text.
        const BLINK         = 0b0001_0000;
        /// Reverse video (swap fg/bg).
        const REVERSE       = 0b0010_0000;
        /// Strikethrough text.
        const STRIKETHROUGH = 0b0100_0000;
        /// Hidden / invisible text.
        const HIDDEN        = 0b1000_0000;
    }
}

/// Packed cell attributes:
/// - bits 31..24: `StyleFlags` (8 bits)
/// - bits 23..0: `link_id` (24 bits)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[repr(transparent)]
pub struct CellAttrs(u32);

impl CellAttrs {
    /// No attributes or link.
    pub const NONE: Self = Self(0);

    /// Sentinel value for "no hyperlink".
    pub const LINK_ID_NONE: u32 = 0;
    /// Maximum link ID (24-bit range).
    pub const LINK_ID_MAX: u32 = 0x00FF_FFFE;

    /// Create attributes from flags and a hyperlink ID.
    #[inline]
    pub fn new(flags: StyleFlags, link_id: u32) -> Self {
        debug_assert!(
            link_id <= Self::LINK_ID_MAX,
            "link_id overflow: {link_id} (max={})",
            Self::LINK_ID_MAX
        );
        Self(((flags.bits() as u32) << 24) | (link_id & 0x00FF_FFFF))
    }

    /// Extract the style flags.
    #[inline]
    pub fn flags(self) -> StyleFlags {
        StyleFlags::from_bits_truncate((self.0 >> 24) as u8)
    }

    /// Extract the hyperlink ID.
    #[inline]
    pub fn link_id(self) -> u32 {
        self.0 & 0x00FF_FFFF
    }

    /// Return a copy with different style flags.
    #[inline]
    pub fn with_flags(self, flags: StyleFlags) -> Self {
        Self((self.0 & 0x00FF_FFFF) | ((flags.bits() as u32) << 24))
    }

    /// Return a copy with a different hyperlink ID.
    #[inline]
    pub fn with_link(self, link_id: u32) -> Self {
        debug_assert!(
            link_id <= Self::LINK_ID_MAX,
            "link_id overflow: {link_id} (max={})",
            Self::LINK_ID_MAX
        );
        Self((self.0 & 0xFF00_0000) | (link_id & 0x00FF_FFFF))
    }

    /// Check whether a specific flag is set.
    #[inline]
    pub fn has_flag(self, flag: StyleFlags) -> bool {
        self.flags().contains(flag)
    }
}

#[cfg(test)]
mod tests {
    use super::{Cell, CellAttrs, CellContent, GraphemeId, PackedRgba, StyleFlags};

    fn reference_over(src: PackedRgba, dst: PackedRgba) -> PackedRgba {
        let sr = src.r() as f64 / 255.0;
        let sg = src.g() as f64 / 255.0;
        let sb = src.b() as f64 / 255.0;
        let sa = src.a() as f64 / 255.0;

        let dr = dst.r() as f64 / 255.0;
        let dg = dst.g() as f64 / 255.0;
        let db = dst.b() as f64 / 255.0;
        let da = dst.a() as f64 / 255.0;

        let out_a = sa + da * (1.0 - sa);
        if out_a <= 0.0 {
            return PackedRgba::TRANSPARENT;
        }

        let out_r = (sr * sa + dr * da * (1.0 - sa)) / out_a;
        let out_g = (sg * sa + dg * da * (1.0 - sa)) / out_a;
        let out_b = (sb * sa + db * da * (1.0 - sa)) / out_a;

        let to_u8 = |x: f64| -> u8 { (x * 255.0).round().clamp(0.0, 255.0) as u8 };
        PackedRgba::rgba(to_u8(out_r), to_u8(out_g), to_u8(out_b), to_u8(out_a))
    }

    #[test]
    fn packed_rgba_is_4_bytes() {
        assert_eq!(core::mem::size_of::<PackedRgba>(), 4);
    }

    #[test]
    fn rgb_sets_alpha_to_255() {
        let c = PackedRgba::rgb(1, 2, 3);
        assert_eq!(c.r(), 1);
        assert_eq!(c.g(), 2);
        assert_eq!(c.b(), 3);
        assert_eq!(c.a(), 255);
    }

    #[test]
    fn rgba_round_trips_components() {
        let c = PackedRgba::rgba(10, 20, 30, 40);
        assert_eq!(c.r(), 10);
        assert_eq!(c.g(), 20);
        assert_eq!(c.b(), 30);
        assert_eq!(c.a(), 40);
    }

    #[test]
    fn over_with_opaque_src_returns_src() {
        let src = PackedRgba::rgba(1, 2, 3, 255);
        let dst = PackedRgba::rgba(9, 8, 7, 200);
        assert_eq!(src.over(dst), src);
    }

    #[test]
    fn over_with_transparent_src_returns_dst() {
        let src = PackedRgba::TRANSPARENT;
        let dst = PackedRgba::rgba(9, 8, 7, 200);
        assert_eq!(src.over(dst), dst);
    }

    #[test]
    fn over_blends_correctly_for_half_alpha_over_opaque() {
        // 50% red over opaque blue -> purple-ish, and resulting alpha stays opaque.
        let src = PackedRgba::rgba(255, 0, 0, 128);
        let dst = PackedRgba::rgba(0, 0, 255, 255);
        assert_eq!(src.over(dst), PackedRgba::rgba(128, 0, 127, 255));
    }

    #[test]
    fn over_matches_reference_for_partial_alpha_cases() {
        let cases = [
            (
                PackedRgba::rgba(200, 10, 10, 64),
                PackedRgba::rgba(10, 200, 10, 128),
            ),
            (
                PackedRgba::rgba(1, 2, 3, 1),
                PackedRgba::rgba(250, 251, 252, 254),
            ),
            (
                PackedRgba::rgba(100, 0, 200, 200),
                PackedRgba::rgba(0, 120, 30, 50),
            ),
        ];

        for (src, dst) in cases {
            assert_eq!(src.over(dst), reference_over(src, dst));
        }
    }

    #[test]
    fn with_opacity_scales_alpha() {
        let c = PackedRgba::rgba(10, 20, 30, 255);
        assert_eq!(c.with_opacity(0.5).a(), 128);
        assert_eq!(c.with_opacity(-1.0).a(), 0);
        assert_eq!(c.with_opacity(2.0).a(), 255);
    }

    #[test]
    fn cell_attrs_is_4_bytes() {
        assert_eq!(core::mem::size_of::<CellAttrs>(), 4);
    }

    #[test]
    fn cell_attrs_none_has_no_flags_and_no_link() {
        assert!(CellAttrs::NONE.flags().is_empty());
        assert_eq!(CellAttrs::NONE.link_id(), 0);
    }

    #[test]
    fn cell_attrs_new_stores_flags_and_link() {
        let flags = StyleFlags::BOLD | StyleFlags::ITALIC;
        let a = CellAttrs::new(flags, 42);
        assert_eq!(a.flags(), flags);
        assert_eq!(a.link_id(), 42);
    }

    #[test]
    fn cell_attrs_with_flags_preserves_link_id() {
        let a = CellAttrs::new(StyleFlags::BOLD, 123);
        let b = a.with_flags(StyleFlags::UNDERLINE);
        assert_eq!(b.flags(), StyleFlags::UNDERLINE);
        assert_eq!(b.link_id(), 123);
    }

    #[test]
    fn cell_attrs_with_link_preserves_flags() {
        let a = CellAttrs::new(StyleFlags::BOLD | StyleFlags::ITALIC, 1);
        let b = a.with_link(999);
        assert_eq!(b.flags(), StyleFlags::BOLD | StyleFlags::ITALIC);
        assert_eq!(b.link_id(), 999);
    }

    #[test]
    fn cell_attrs_flag_combinations_work() {
        let flags = StyleFlags::BOLD | StyleFlags::ITALIC;
        let a = CellAttrs::new(flags, 0);
        assert!(a.has_flag(StyleFlags::BOLD));
        assert!(a.has_flag(StyleFlags::ITALIC));
        assert!(!a.has_flag(StyleFlags::UNDERLINE));
    }

    #[test]
    fn cell_attrs_link_id_max_boundary() {
        let a = CellAttrs::new(StyleFlags::empty(), CellAttrs::LINK_ID_MAX);
        assert_eq!(a.link_id(), CellAttrs::LINK_ID_MAX);
    }

    // ====== GraphemeId tests ======

    #[test]
    fn grapheme_id_is_4_bytes() {
        assert_eq!(core::mem::size_of::<GraphemeId>(), 4);
    }

    #[test]
    fn grapheme_id_encoding_roundtrip() {
        let id = GraphemeId::new(12345, 2);
        assert_eq!(id.slot(), 12345);
        assert_eq!(id.width(), 2);
    }

    #[test]
    fn grapheme_id_max_values() {
        let id = GraphemeId::new(GraphemeId::MAX_SLOT, GraphemeId::MAX_WIDTH);
        assert_eq!(id.slot(), 0x00FF_FFFF);
        assert_eq!(id.width(), 127);
    }

    #[test]
    fn grapheme_id_zero_values() {
        let id = GraphemeId::new(0, 0);
        assert_eq!(id.slot(), 0);
        assert_eq!(id.width(), 0);
    }

    #[test]
    fn grapheme_id_raw_roundtrip() {
        let id = GraphemeId::new(999, 5);
        let raw = id.raw();
        let restored = GraphemeId::from_raw(raw);
        assert_eq!(restored.slot(), 999);
        assert_eq!(restored.width(), 5);
    }

    // ====== CellContent tests ======

    #[test]
    fn cell_content_is_4_bytes() {
        assert_eq!(core::mem::size_of::<CellContent>(), 4);
    }

    #[test]
    fn cell_content_empty_properties() {
        assert!(CellContent::EMPTY.is_empty());
        assert!(!CellContent::EMPTY.is_continuation());
        assert!(!CellContent::EMPTY.is_grapheme());
        assert_eq!(CellContent::EMPTY.width_hint(), 0);
    }

    #[test]
    fn cell_content_continuation_properties() {
        assert!(CellContent::CONTINUATION.is_continuation());
        assert!(!CellContent::CONTINUATION.is_empty());
        assert!(!CellContent::CONTINUATION.is_grapheme());
        assert_eq!(CellContent::CONTINUATION.width_hint(), 0);
    }

    #[test]
    fn cell_content_from_char_ascii() {
        let c = CellContent::from_char('A');
        assert!(!c.is_grapheme());
        assert!(!c.is_empty());
        assert!(!c.is_continuation());
        assert_eq!(c.as_char(), Some('A'));
        assert_eq!(c.width_hint(), 1);
    }

    #[test]
    fn cell_content_from_char_unicode() {
        // BMP character
        let c = CellContent::from_char('日');
        assert_eq!(c.as_char(), Some('日'));
        assert!(!c.is_grapheme());

        // Supplementary plane character (emoji)
        let c2 = CellContent::from_char('🎉');
        assert_eq!(c2.as_char(), Some('🎉'));
        assert!(!c2.is_grapheme());
    }

    #[test]
    fn cell_content_from_grapheme() {
        let id = GraphemeId::new(42, 2);
        let c = CellContent::from_grapheme(id);

        assert!(c.is_grapheme());
        assert!(!c.is_empty());
        assert!(!c.is_continuation());
        assert_eq!(c.grapheme_id(), Some(id));
        assert_eq!(c.as_char(), None);
        assert_eq!(c.width_hint(), 2);
    }

    #[test]
    fn cell_content_width_for_chars() {
        let ascii = CellContent::from_char('A');
        assert_eq!(ascii.width(), 1);

        let wide = CellContent::from_char('日');
        assert_eq!(wide.width(), 2);

        let emoji = CellContent::from_char('🎉');
        assert_eq!(emoji.width(), 2);

        // Unicode East Asian Width properties:
        // - '⚡' (U+26A1) is Wide → always width 2
        // - '⚙' (U+2699) is Neutral → 1 (non-CJK) or 2 (CJK)
        // - '❤' (U+2764) is Neutral → 1 (non-CJK) or 2 (CJK)
        let bolt = CellContent::from_char('⚡');
        assert_eq!(bolt.width(), 2, "bolt is Wide, always width 2");

        // Neutral-width characters: width depends on CJK mode
        let gear = CellContent::from_char('⚙');
        let heart = CellContent::from_char('❤');
        assert!(
            [1, 2].contains(&gear.width()),
            "gear should be 1 (non-CJK) or 2 (CJK), got {}",
            gear.width()
        );
        assert_eq!(
            gear.width(),
            heart.width(),
            "gear and heart should have same width (both Neutral)"
        );
    }

    #[test]
    fn cell_content_width_for_grapheme() {
        let id = GraphemeId::new(7, 3);
        let c = CellContent::from_grapheme(id);
        assert_eq!(c.width(), 3);
    }

    #[test]
    fn cell_content_width_empty_is_zero() {
        assert_eq!(CellContent::EMPTY.width(), 0);
        assert_eq!(CellContent::CONTINUATION.width(), 0);
    }

    #[test]
    fn cell_content_grapheme_discriminator_bit() {
        // Chars should have bit 31 = 0
        let char_content = CellContent::from_char('X');
        assert_eq!(char_content.raw() & 0x8000_0000, 0);

        // Graphemes should have bit 31 = 1
        let grapheme_content = CellContent::from_grapheme(GraphemeId::new(1, 1));
        assert_ne!(grapheme_content.raw() & 0x8000_0000, 0);
    }

    // ====== Cell tests ======

    #[test]
    fn cell_is_16_bytes() {
        assert_eq!(core::mem::size_of::<Cell>(), 16);
    }

    #[test]
    fn cell_alignment_is_16() {
        assert_eq!(core::mem::align_of::<Cell>(), 16);
    }

    #[test]
    fn cell_default_properties() {
        let cell = Cell::default();
        assert!(cell.is_empty());
        assert!(!cell.is_continuation());
        assert_eq!(cell.fg, PackedRgba::WHITE);
        assert_eq!(cell.bg, PackedRgba::TRANSPARENT);
        assert_eq!(cell.attrs, CellAttrs::NONE);
    }

    #[test]
    fn cell_continuation_constant() {
        assert!(Cell::CONTINUATION.is_continuation());
        assert!(!Cell::CONTINUATION.is_empty());
    }

    #[test]
    fn cell_from_char() {
        let cell = Cell::from_char('X');
        assert_eq!(cell.content.as_char(), Some('X'));
        assert_eq!(cell.fg, PackedRgba::WHITE);
        assert_eq!(cell.bg, PackedRgba::TRANSPARENT);
    }

    #[test]
    fn cell_builder_methods() {
        let cell = Cell::from_char('A')
            .with_fg(PackedRgba::rgb(255, 0, 0))
            .with_bg(PackedRgba::rgb(0, 0, 255))
            .with_attrs(CellAttrs::new(StyleFlags::BOLD, 0));

        assert_eq!(cell.content.as_char(), Some('A'));
        assert_eq!(cell.fg, PackedRgba::rgb(255, 0, 0));
        assert_eq!(cell.bg, PackedRgba::rgb(0, 0, 255));
        assert!(cell.attrs.has_flag(StyleFlags::BOLD));
    }

    #[test]
    fn cell_bits_eq_same_cells() {
        let cell1 = Cell::from_char('X').with_fg(PackedRgba::rgb(1, 2, 3));
        let cell2 = Cell::from_char('X').with_fg(PackedRgba::rgb(1, 2, 3));
        assert!(cell1.bits_eq(&cell2));
    }

    #[test]
    fn cell_bits_eq_different_cells() {
        let cell1 = Cell::from_char('X');
        let cell2 = Cell::from_char('Y');
        assert!(!cell1.bits_eq(&cell2));

        let cell3 = Cell::from_char('X').with_fg(PackedRgba::rgb(1, 2, 3));
        assert!(!cell1.bits_eq(&cell3));
    }

    #[test]
    fn cell_width_hint() {
        let empty = Cell::default();
        assert_eq!(empty.width_hint(), 0);

        let cont = Cell::CONTINUATION;
        assert_eq!(cont.width_hint(), 0);

        let ascii = Cell::from_char('A');
        assert_eq!(ascii.width_hint(), 1);
    }

    // Property tests moved to top-level `cell_proptests` module for edition 2024 compat.

    // ====== PackedRgba extended coverage ======

    #[test]
    fn packed_rgba_named_constants() {
        assert_eq!(PackedRgba::TRANSPARENT, PackedRgba(0));
        assert_eq!(PackedRgba::TRANSPARENT.a(), 0);

        assert_eq!(PackedRgba::BLACK.r(), 0);
        assert_eq!(PackedRgba::BLACK.g(), 0);
        assert_eq!(PackedRgba::BLACK.b(), 0);
        assert_eq!(PackedRgba::BLACK.a(), 255);

        assert_eq!(PackedRgba::WHITE.r(), 255);
        assert_eq!(PackedRgba::WHITE.g(), 255);
        assert_eq!(PackedRgba::WHITE.b(), 255);
        assert_eq!(PackedRgba::WHITE.a(), 255);

        assert_eq!(PackedRgba::RED, PackedRgba::rgb(255, 0, 0));
        assert_eq!(PackedRgba::GREEN, PackedRgba::rgb(0, 255, 0));
        assert_eq!(PackedRgba::BLUE, PackedRgba::rgb(0, 0, 255));
    }

    #[test]
    fn packed_rgba_default_is_transparent() {
        assert_eq!(PackedRgba::default(), PackedRgba::TRANSPARENT);
    }

    #[test]
    fn over_both_transparent_returns_transparent() {
        // Exercises numer_a == 0 branch (line 508)
        let result = PackedRgba::TRANSPARENT.over(PackedRgba::TRANSPARENT);
        assert_eq!(result, PackedRgba::TRANSPARENT);
    }

    #[test]
    fn over_partial_alpha_over_transparent_dst() {
        // d_a == 0 path: src partial alpha over fully transparent
        let src = PackedRgba::rgba(200, 100, 50, 128);
        let result = src.over(PackedRgba::TRANSPARENT);
        // Output alpha = src_alpha (dst contributes nothing)
        assert_eq!(result.a(), 128);
        // Colors should be src colors since dst has no contribution
        assert_eq!(result.r(), 200);
        assert_eq!(result.g(), 100);
        assert_eq!(result.b(), 50);
    }

    #[test]
    fn over_very_low_alpha() {
        // Near-transparent source (alpha=1) over opaque destination
        let src = PackedRgba::rgba(255, 0, 0, 1);
        let dst = PackedRgba::rgba(0, 0, 255, 255);
        let result = src.over(dst);
        // Result should be very close to dst
        assert_eq!(result.a(), 255);
        assert!(result.b() > 250, "b={} should be near 255", result.b());
        assert!(result.r() < 5, "r={} should be near 0", result.r());
    }

    #[test]
    fn with_opacity_exact_zero() {
        let c = PackedRgba::rgba(10, 20, 30, 200);
        let result = c.with_opacity(0.0);
        assert_eq!(result.a(), 0);
        assert_eq!(result.r(), 10); // RGB preserved
        assert_eq!(result.g(), 20);
        assert_eq!(result.b(), 30);
    }

    #[test]
    fn with_opacity_exact_one() {
        let c = PackedRgba::rgba(10, 20, 30, 200);
        let result = c.with_opacity(1.0);
        assert_eq!(result.a(), 200); // Alpha unchanged
        assert_eq!(result.r(), 10);
    }

    #[test]
    fn with_opacity_preserves_rgb() {
        let c = PackedRgba::rgba(42, 84, 168, 255);
        let result = c.with_opacity(0.25);
        assert_eq!(result.r(), 42);
        assert_eq!(result.g(), 84);
        assert_eq!(result.b(), 168);
        assert_eq!(result.a(), 64); // 255 * 0.25 = 63.75 → 64
    }

    // ====== CellContent extended coverage ======

    #[test]
    fn cell_content_as_char_none_for_empty() {
        assert_eq!(CellContent::EMPTY.as_char(), None);
    }

    #[test]
    fn cell_content_as_char_none_for_continuation() {
        assert_eq!(CellContent::CONTINUATION.as_char(), None);
    }

    #[test]
    fn cell_content_as_char_none_for_grapheme() {
        let id = GraphemeId::new(1, 2);
        let c = CellContent::from_grapheme(id);
        assert_eq!(c.as_char(), None);
    }

    #[test]
    fn cell_content_grapheme_id_none_for_char() {
        let c = CellContent::from_char('A');
        assert_eq!(c.grapheme_id(), None);
    }

    #[test]
    fn cell_content_grapheme_id_none_for_empty() {
        assert_eq!(CellContent::EMPTY.grapheme_id(), None);
    }

    #[test]
    fn cell_content_width_control_chars() {
        // Control characters have width 0, except tab/newline/CR which are 1 cell
        // Note: NUL (0x00) is CellContent::EMPTY, so test with other controls
        let tab = CellContent::from_char('\t');
        assert_eq!(tab.width(), 1);

        let bel = CellContent::from_char('\x07');
        assert_eq!(bel.width(), 0);
    }

    #[test]
    fn cell_content_width_hint_always_1_for_chars() {
        // width_hint is the fast path that always returns 1 for non-special chars
        let wide = CellContent::from_char('日');
        assert_eq!(wide.width_hint(), 1); // fast path says 1
        assert_eq!(wide.width(), 2); // accurate path says 2
    }

    #[test]
    fn cell_content_default_is_empty() {
        assert_eq!(CellContent::default(), CellContent::EMPTY);
    }

    #[test]
    fn cell_content_debug_empty() {
        let s = format!("{:?}", CellContent::EMPTY);
        assert_eq!(s, "CellContent::EMPTY");
    }

    #[test]
    fn cell_content_debug_continuation() {
        let s = format!("{:?}", CellContent::CONTINUATION);
        assert_eq!(s, "CellContent::CONTINUATION");
    }

    #[test]
    fn cell_content_debug_char() {
        let s = format!("{:?}", CellContent::from_char('X'));
        assert!(s.starts_with("CellContent::Char("), "got: {s}");
    }

    #[test]
    fn cell_content_debug_grapheme() {
        let id = GraphemeId::new(1, 2);
        let s = format!("{:?}", CellContent::from_grapheme(id));
        assert!(s.starts_with("CellContent::Grapheme("), "got: {s}");
    }

    #[test]
    fn cell_content_raw_value() {
        let c = CellContent::from_char('A');
        assert_eq!(c.raw(), 'A' as u32);

        let g = CellContent::from_grapheme(GraphemeId::new(5, 2));
        assert_ne!(g.raw() & 0x8000_0000, 0);
    }

    // ====== CellAttrs extended coverage ======

    #[test]
    fn cell_attrs_default_is_none() {
        assert_eq!(CellAttrs::default(), CellAttrs::NONE);
    }

    #[test]
    fn cell_attrs_each_flag_isolated() {
        let all_flags = [
            StyleFlags::BOLD,
            StyleFlags::DIM,
            StyleFlags::ITALIC,
            StyleFlags::UNDERLINE,
            StyleFlags::BLINK,
            StyleFlags::REVERSE,
            StyleFlags::STRIKETHROUGH,
            StyleFlags::HIDDEN,
        ];

        for &flag in &all_flags {
            let a = CellAttrs::new(flag, 0);
            assert!(a.has_flag(flag), "flag {:?} should be set", flag);

            // Verify no other flags are set
            for &other in &all_flags {
                if other != flag {
                    assert!(
                        !a.has_flag(other),
                        "flag {:?} should NOT be set when only {:?} is",
                        other,
                        flag
                    );
                }
            }
        }
    }

    #[test]
    fn cell_attrs_all_flags_combined() {
        let all = StyleFlags::BOLD
            | StyleFlags::DIM
            | StyleFlags::ITALIC
            | StyleFlags::UNDERLINE
            | StyleFlags::BLINK
            | StyleFlags::REVERSE
            | StyleFlags::STRIKETHROUGH
            | StyleFlags::HIDDEN;
        let a = CellAttrs::new(all, 42);
        assert_eq!(a.flags(), all);
        assert!(a.has_flag(StyleFlags::BOLD));
        assert!(a.has_flag(StyleFlags::HIDDEN));
        assert_eq!(a.link_id(), 42);
    }

    #[test]
    fn cell_attrs_link_id_zero() {
        let a = CellAttrs::new(StyleFlags::BOLD, CellAttrs::LINK_ID_NONE);
        assert_eq!(a.link_id(), 0);
        assert!(a.has_flag(StyleFlags::BOLD));
    }

    #[test]
    fn cell_attrs_with_link_to_none() {
        let a = CellAttrs::new(StyleFlags::ITALIC, 500);
        let b = a.with_link(CellAttrs::LINK_ID_NONE);
        assert_eq!(b.link_id(), 0);
        assert!(b.has_flag(StyleFlags::ITALIC));
    }

    #[test]
    fn cell_attrs_with_flags_to_empty() {
        let a = CellAttrs::new(StyleFlags::BOLD | StyleFlags::ITALIC, 123);
        let b = a.with_flags(StyleFlags::empty());
        assert!(b.flags().is_empty());
        assert_eq!(b.link_id(), 123);
    }

    // ====== Cell extended coverage ======

    #[test]
    fn cell_bits_eq_detects_bg_difference() {
        let cell1 = Cell::from_char('X');
        let cell2 = Cell::from_char('X').with_bg(PackedRgba::RED);
        assert!(!cell1.bits_eq(&cell2));
    }

    #[test]
    fn cell_bits_eq_detects_attrs_difference() {
        let cell1 = Cell::from_char('X');
        let cell2 = Cell::from_char('X').with_attrs(CellAttrs::new(StyleFlags::BOLD, 0));
        assert!(!cell1.bits_eq(&cell2));
    }

    #[test]
    fn cell_with_char_preserves_colors_and_attrs() {
        let cell = Cell::from_char('A')
            .with_fg(PackedRgba::RED)
            .with_bg(PackedRgba::BLUE)
            .with_attrs(CellAttrs::new(StyleFlags::BOLD, 42));

        let updated = cell.with_char('Z');
        assert_eq!(updated.content.as_char(), Some('Z'));
        assert_eq!(updated.fg, PackedRgba::RED);
        assert_eq!(updated.bg, PackedRgba::BLUE);
        assert!(updated.attrs.has_flag(StyleFlags::BOLD));
        assert_eq!(updated.attrs.link_id(), 42);
    }

    #[test]
    fn cell_new_vs_from_char() {
        let a = Cell::new(CellContent::from_char('A'));
        let b = Cell::from_char('A');
        assert!(a.bits_eq(&b));
    }

    #[test]
    fn cell_continuation_has_transparent_colors() {
        assert_eq!(Cell::CONTINUATION.fg, PackedRgba::TRANSPARENT);
        assert_eq!(Cell::CONTINUATION.bg, PackedRgba::TRANSPARENT);
        assert_eq!(Cell::CONTINUATION.attrs, CellAttrs::NONE);
    }

    #[test]
    fn cell_debug_format() {
        let cell = Cell::from_char('A');
        let s = format!("{:?}", cell);
        assert!(s.contains("Cell"), "got: {s}");
        assert!(s.contains("content"), "got: {s}");
        assert!(s.contains("fg"), "got: {s}");
        assert!(s.contains("bg"), "got: {s}");
        assert!(s.contains("attrs"), "got: {s}");
    }

    #[test]
    fn cell_is_empty_for_various() {
        assert!(Cell::default().is_empty());
        assert!(!Cell::from_char('A').is_empty());
        assert!(!Cell::CONTINUATION.is_empty());
    }

    #[test]
    fn cell_is_continuation_for_various() {
        assert!(!Cell::default().is_continuation());
        assert!(!Cell::from_char('A').is_continuation());
        assert!(Cell::CONTINUATION.is_continuation());
    }

    #[test]
    fn cell_width_hint_for_grapheme() {
        let id = GraphemeId::new(100, 3);
        let cell = Cell::new(CellContent::from_grapheme(id));
        assert_eq!(cell.width_hint(), 3);
    }

    // ====== GraphemeId extended coverage ======

    #[test]
    fn grapheme_id_default() {
        let id = GraphemeId::default();
        assert_eq!(id.slot(), 0);
        assert_eq!(id.width(), 0);
    }

    #[test]
    fn grapheme_id_debug_format() {
        let id = GraphemeId::new(42, 2);
        let s = format!("{:?}", id);
        assert!(s.contains("GraphemeId"), "got: {s}");
        assert!(s.contains("42"), "got: {s}");
        assert!(s.contains("2"), "got: {s}");
    }

    #[test]
    fn grapheme_id_width_isolated_from_slot() {
        // Verify slot bits don't leak into width field
        let id = GraphemeId::new(0x00FF_FFFF, 0);
        assert_eq!(id.width(), 0);
        assert_eq!(id.slot(), 0x00FF_FFFF);

        let id2 = GraphemeId::new(0, 127);
        assert_eq!(id2.slot(), 0);
        assert_eq!(id2.width(), 127);
    }

    // ====== StyleFlags coverage ======

    #[test]
    fn style_flags_empty_has_no_bits() {
        assert!(StyleFlags::empty().is_empty());
        assert_eq!(StyleFlags::empty().bits(), 0);
    }

    #[test]
    fn style_flags_all_has_all_bits() {
        let all = StyleFlags::all();
        assert!(all.contains(StyleFlags::BOLD));
        assert!(all.contains(StyleFlags::DIM));
        assert!(all.contains(StyleFlags::ITALIC));
        assert!(all.contains(StyleFlags::UNDERLINE));
        assert!(all.contains(StyleFlags::BLINK));
        assert!(all.contains(StyleFlags::REVERSE));
        assert!(all.contains(StyleFlags::STRIKETHROUGH));
        assert!(all.contains(StyleFlags::HIDDEN));
    }

    #[test]
    fn style_flags_union_and_intersection() {
        let a = StyleFlags::BOLD | StyleFlags::ITALIC;
        let b = StyleFlags::ITALIC | StyleFlags::UNDERLINE;
        assert_eq!(
            a | b,
            StyleFlags::BOLD | StyleFlags::ITALIC | StyleFlags::UNDERLINE
        );
        assert_eq!(a & b, StyleFlags::ITALIC);
    }

    #[test]
    fn style_flags_from_bits_truncate() {
        // 0xFF should give all flags
        let all = StyleFlags::from_bits_truncate(0xFF);
        assert_eq!(all, StyleFlags::all());

        // 0x00 should give empty
        let none = StyleFlags::from_bits_truncate(0x00);
        assert!(none.is_empty());
    }
}

/// Property tests for Cell types (bd-10i.13.2).
///
/// Top-level `#[cfg(test)]` scope: the `proptest!` macro has edition-2024
/// compatibility issues when nested inside another test module.
#[cfg(test)]
mod cell_proptests {
    use super::{Cell, CellAttrs, CellContent, GraphemeId, PackedRgba, StyleFlags};
    use proptest::prelude::*;

    fn arb_packed_rgba() -> impl Strategy<Value = PackedRgba> {
        (any::<u8>(), any::<u8>(), any::<u8>(), any::<u8>())
            .prop_map(|(r, g, b, a)| PackedRgba::rgba(r, g, b, a))
    }

    fn arb_grapheme_id() -> impl Strategy<Value = GraphemeId> {
        (0u32..=GraphemeId::MAX_SLOT, 0u8..=GraphemeId::MAX_WIDTH)
            .prop_map(|(slot, width)| GraphemeId::new(slot, width))
    }

    fn arb_style_flags() -> impl Strategy<Value = StyleFlags> {
        any::<u8>().prop_map(StyleFlags::from_bits_truncate)
    }

    proptest! {
        #[test]
        fn packed_rgba_roundtrips_all_components(tuple in (any::<u8>(), any::<u8>(), any::<u8>(), any::<u8>())) {
            let (r, g, b, a) = tuple;
            let c = PackedRgba::rgba(r, g, b, a);
            prop_assert_eq!(c.r(), r);
            prop_assert_eq!(c.g(), g);
            prop_assert_eq!(c.b(), b);
            prop_assert_eq!(c.a(), a);
        }

        #[test]
        fn packed_rgba_rgb_always_opaque(tuple in (any::<u8>(), any::<u8>(), any::<u8>())) {
            let (r, g, b) = tuple;
            let c = PackedRgba::rgb(r, g, b);
            prop_assert_eq!(c.a(), 255);
            prop_assert_eq!(c.r(), r);
            prop_assert_eq!(c.g(), g);
            prop_assert_eq!(c.b(), b);
        }

        #[test]
        fn packed_rgba_over_identity_transparent(dst in arb_packed_rgba()) {
            // Transparent source leaves destination unchanged
            let result = PackedRgba::TRANSPARENT.over(dst);
            prop_assert_eq!(result, dst);
        }

        #[test]
        fn packed_rgba_over_identity_opaque(tuple in (any::<u8>(), any::<u8>(), any::<u8>(), arb_packed_rgba())) {
            // Fully opaque source replaces destination
            let (r, g, b, dst) = tuple;
            let src = PackedRgba::rgba(r, g, b, 255);
            let result = src.over(dst);
            prop_assert_eq!(result, src);
        }

        #[test]
        fn grapheme_id_slot_width_roundtrip(tuple in (0u32..=GraphemeId::MAX_SLOT, 0u8..=GraphemeId::MAX_WIDTH)) {
            let (slot, width) = tuple;
            let id = GraphemeId::new(slot, width);
            prop_assert_eq!(id.slot(), slot as usize);
            prop_assert_eq!(id.width(), width as usize);
        }

        #[test]
        fn grapheme_id_raw_roundtrip(id in arb_grapheme_id()) {
            let raw = id.raw();
            let restored = GraphemeId::from_raw(raw);
            prop_assert_eq!(restored.slot(), id.slot());
            prop_assert_eq!(restored.width(), id.width());
        }

        #[test]
        fn cell_content_char_roundtrip(c in (0x20u32..0xD800u32).prop_union(0xE000u32..0x110000u32)) {
            if let Some(ch) = char::from_u32(c) {
                let content = CellContent::from_char(ch);
                prop_assert_eq!(content.as_char(), Some(ch));
                prop_assert!(!content.is_grapheme());
                prop_assert!(!content.is_empty());
                prop_assert!(!content.is_continuation());
            }
        }

        #[test]
        fn cell_content_grapheme_roundtrip(id in arb_grapheme_id()) {
            let content = CellContent::from_grapheme(id);
            prop_assert!(content.is_grapheme());
            prop_assert_eq!(content.grapheme_id(), Some(id));
            prop_assert_eq!(content.width_hint(), id.width());
        }

        #[test]
        fn cell_bits_eq_is_reflexive(
            tuple in (
                (0x20u32..0x80u32).prop_map(|c| char::from_u32(c).unwrap()),
                any::<u8>(), any::<u8>(), any::<u8>(),
                arb_style_flags(),
            ),
        ) {
            let (c, r, g, b, flags) = tuple;
            let cell = Cell::from_char(c)
                .with_fg(PackedRgba::rgb(r, g, b))
                .with_attrs(CellAttrs::new(flags, 0));
            prop_assert!(cell.bits_eq(&cell));
        }

        #[test]
        fn cell_bits_eq_detects_fg_difference(
            tuple in (
                (0x41u32..0x5Bu32).prop_map(|c| char::from_u32(c).unwrap()),
                any::<u8>(), any::<u8>(),
            ),
        ) {
            let (c, r1, r2) = tuple;
            prop_assume!(r1 != r2);
            let cell1 = Cell::from_char(c).with_fg(PackedRgba::rgb(r1, 0, 0));
            let cell2 = Cell::from_char(c).with_fg(PackedRgba::rgb(r2, 0, 0));
            prop_assert!(!cell1.bits_eq(&cell2));
        }

        #[test]
        fn cell_attrs_flags_roundtrip(tuple in (arb_style_flags(), 0u32..CellAttrs::LINK_ID_MAX)) {
            let (flags, link) = tuple;
            let attrs = CellAttrs::new(flags, link);
            prop_assert_eq!(attrs.flags(), flags);
            prop_assert_eq!(attrs.link_id(), link);
        }

        #[test]
        fn cell_attrs_with_flags_preserves_link(tuple in (arb_style_flags(), 0u32..CellAttrs::LINK_ID_MAX, arb_style_flags())) {
            let (flags, link, new_flags) = tuple;
            let attrs = CellAttrs::new(flags, link);
            let updated = attrs.with_flags(new_flags);
            prop_assert_eq!(updated.flags(), new_flags);
            prop_assert_eq!(updated.link_id(), link);
        }

        #[test]
        fn cell_attrs_with_link_preserves_flags(tuple in (arb_style_flags(), 0u32..CellAttrs::LINK_ID_MAX, 0u32..CellAttrs::LINK_ID_MAX)) {
            let (flags, link1, link2) = tuple;
            let attrs = CellAttrs::new(flags, link1);
            let updated = attrs.with_link(link2);
            prop_assert_eq!(updated.flags(), flags);
            prop_assert_eq!(updated.link_id(), link2);
        }

        // --- Executable Invariant Tests (bd-10i.13.2) ---

        #[test]
        fn cell_bits_eq_is_symmetric(
            tuple in (
                (0x41u32..0x5Bu32).prop_map(|c| char::from_u32(c).unwrap()),
                (0x41u32..0x5Bu32).prop_map(|c| char::from_u32(c).unwrap()),
                arb_packed_rgba(),
                arb_packed_rgba(),
            ),
        ) {
            let (c1, c2, fg1, fg2) = tuple;
            let cell_a = Cell::from_char(c1).with_fg(fg1);
            let cell_b = Cell::from_char(c2).with_fg(fg2);
            prop_assert_eq!(cell_a.bits_eq(&cell_b), cell_b.bits_eq(&cell_a),
                "bits_eq is not symmetric");
        }

        #[test]
        fn cell_content_bit31_discriminates(id in arb_grapheme_id()) {
            // Char content: bit 31 is 0
            let char_content = CellContent::from_char('A');
            prop_assert!(!char_content.is_grapheme());
            prop_assert!(char_content.as_char().is_some());
            prop_assert!(char_content.grapheme_id().is_none());

            // Grapheme content: bit 31 is 1
            let grapheme_content = CellContent::from_grapheme(id);
            prop_assert!(grapheme_content.is_grapheme());
            prop_assert!(grapheme_content.grapheme_id().is_some());
            prop_assert!(grapheme_content.as_char().is_none());
        }

        #[test]
        fn cell_from_char_width_matches_unicode(
            c in (0x20u32..0x7Fu32).prop_map(|c| char::from_u32(c).unwrap()),
        ) {
            let cell = Cell::from_char(c);
            prop_assert_eq!(cell.width_hint(), 1,
                "Cell width hint for '{}' should be 1 for ASCII", c);
        }
    }

    // Zero-parameter invariant tests (cannot be inside proptest! macro)

    #[test]
    fn cell_content_continuation_has_zero_width() {
        let cont = CellContent::CONTINUATION;
        assert_eq!(cont.width(), 0, "CONTINUATION cell should have width 0");
        assert!(cont.is_continuation());
        assert!(!cont.is_grapheme());
    }

    #[test]
    fn cell_content_empty_has_zero_width() {
        let empty = CellContent::EMPTY;
        assert_eq!(empty.width(), 0, "EMPTY cell should have width 0");
        assert!(empty.is_empty());
        assert!(!empty.is_grapheme());
        assert!(!empty.is_continuation());
    }

    #[test]
    fn cell_default_is_empty() {
        let cell = Cell::default();
        assert!(cell.is_empty());
        assert_eq!(cell.width_hint(), 0);
    }
}
