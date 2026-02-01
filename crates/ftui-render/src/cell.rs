#![forbid(unsafe_code)]

//! Cell types and invariants.

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cell;

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
    pub const TRANSPARENT: Self = Self(0);
    pub const BLACK: Self = Self::rgb(0, 0, 0);
    pub const WHITE: Self = Self::rgb(255, 255, 255);

    #[inline]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self::rgba(r, g, b, 255)
    }

    #[inline]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self(((r as u32) << 24) | ((g as u32) << 16) | ((b as u32) << 8) | (a as u32))
    }

    #[inline]
    pub const fn r(self) -> u8 {
        (self.0 >> 24) as u8
    }

    #[inline]
    pub const fn g(self) -> u8 {
        (self.0 >> 16) as u8
    }

    #[inline]
    pub const fn b(self) -> u8 {
        (self.0 >> 8) as u8
    }

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
        const BOLD          = 0b0000_0001;
        const DIM           = 0b0000_0010;
        const ITALIC        = 0b0000_0100;
        const UNDERLINE     = 0b0000_1000;
        const BLINK         = 0b0001_0000;
        const REVERSE       = 0b0010_0000;
        const STRIKETHROUGH = 0b0100_0000;
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
    pub const NONE: Self = Self(0);

    pub const LINK_ID_NONE: u32 = 0;
    pub const LINK_ID_MAX: u32 = 0x00FF_FFFE;

    #[inline]
    pub fn new(flags: StyleFlags, link_id: u32) -> Self {
        debug_assert!(
            link_id <= Self::LINK_ID_MAX,
            "link_id overflow: {link_id} (max={})",
            Self::LINK_ID_MAX
        );
        Self(((flags.bits() as u32) << 24) | (link_id & 0x00FF_FFFF))
    }

    #[inline]
    pub fn flags(self) -> StyleFlags {
        StyleFlags::from_bits_truncate((self.0 >> 24) as u8)
    }

    #[inline]
    pub fn link_id(self) -> u32 {
        self.0 & 0x00FF_FFFF
    }

    #[inline]
    pub fn with_flags(self, flags: StyleFlags) -> Self {
        Self((self.0 & 0x00FF_FFFF) | ((flags.bits() as u32) << 24))
    }

    #[inline]
    pub fn with_link(self, link_id: u32) -> Self {
        debug_assert!(
            link_id <= Self::LINK_ID_MAX,
            "link_id overflow: {link_id} (max={})",
            Self::LINK_ID_MAX
        );
        Self((self.0 & 0xFF00_0000) | (link_id & 0x00FF_FFFF))
    }

    #[inline]
    pub fn has_flag(self, flag: StyleFlags) -> bool {
        self.flags().contains(flag)
    }
}

#[cfg(test)]
mod tests {
    use super::{CellAttrs, PackedRgba, StyleFlags};

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
}
