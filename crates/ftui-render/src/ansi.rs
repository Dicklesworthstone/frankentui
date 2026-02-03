#![forbid(unsafe_code)]

//! ANSI escape sequence generation helpers.
//!
//! This module provides pure byte-generation functions for ANSI/VT control sequences.
//! It handles the encoding details so the Presenter can focus on state tracking and diffing.
//!
//! # Design Principles
//!
//! - **Pure functions**: No state tracking, just byte generation
//! - **Zero allocation**: Use stack buffers for common sequences
//! - **Explicit**: Readable helpers over clever formatting
//!
//! # Sequence Reference
//!
//! | Category | Sequence | Description |
//! |----------|----------|-------------|
//! | CSI | `ESC [ n m` | SGR (Select Graphic Rendition) |
//! | CSI | `ESC [ row ; col H` | CUP (Cursor Position, 1-indexed) |
//! | CSI | `ESC [ n K` | EL (Erase Line) |
//! | CSI | `ESC [ n J` | ED (Erase Display) |
//! | CSI | `ESC [ top ; bottom r` | DECSTBM (Set Scroll Region) |
//! | CSI | `ESC [ ? 2026 h/l` | Synchronized Output (DEC) |
//! | OSC | `ESC ] 8 ; ; url ST` | Hyperlink (OSC 8) |
//! | DEC | `ESC 7` / `ESC 8` | Cursor save/restore (DECSC/DECRC) |

use std::io::{self, Write};
use std::sync::OnceLock;

use crate::cell::{PackedRgba, StyleFlags};

// =============================================================================
// SGR (Select Graphic Rendition)
// =============================================================================

/// SGR reset: `CSI 0 m`
pub const SGR_RESET: &[u8] = b"\x1b[0m";

/// Write SGR reset sequence.
#[inline]
pub fn sgr_reset<W: Write>(w: &mut W) -> io::Result<()> {
    w.write_all(SGR_RESET)
}

/// SGR attribute codes for style flags.
#[derive(Debug, Clone, Copy)]
pub struct SgrCodes {
    /// Enable code
    pub on: u8,
    /// Disable code
    pub off: u8,
}

/// SGR codes for bold (on=1, off=22).
pub const SGR_BOLD: SgrCodes = SgrCodes { on: 1, off: 22 };
/// SGR codes for dim (on=2, off=22).
pub const SGR_DIM: SgrCodes = SgrCodes { on: 2, off: 22 };
/// SGR codes for italic (on=3, off=23).
pub const SGR_ITALIC: SgrCodes = SgrCodes { on: 3, off: 23 };
/// SGR codes for underline (on=4, off=24).
pub const SGR_UNDERLINE: SgrCodes = SgrCodes { on: 4, off: 24 };
/// SGR codes for blink (on=5, off=25).
pub const SGR_BLINK: SgrCodes = SgrCodes { on: 5, off: 25 };
/// SGR codes for reverse video (on=7, off=27).
pub const SGR_REVERSE: SgrCodes = SgrCodes { on: 7, off: 27 };
/// SGR codes for hidden text (on=8, off=28).
pub const SGR_HIDDEN: SgrCodes = SgrCodes { on: 8, off: 28 };
/// SGR codes for strikethrough (on=9, off=29).
pub const SGR_STRIKETHROUGH: SgrCodes = SgrCodes { on: 9, off: 29 };

/// Get SGR codes for a style flag.
#[must_use]
pub const fn sgr_codes_for_flag(flag: StyleFlags) -> Option<SgrCodes> {
    match flag.bits() {
        0b0000_0001 => Some(SGR_BOLD),
        0b0000_0010 => Some(SGR_DIM),
        0b0000_0100 => Some(SGR_ITALIC),
        0b0000_1000 => Some(SGR_UNDERLINE),
        0b0001_0000 => Some(SGR_BLINK),
        0b0010_0000 => Some(SGR_REVERSE),
        0b1000_0000 => Some(SGR_HIDDEN),
        0b0100_0000 => Some(SGR_STRIKETHROUGH),
        _ => None,
    }
}

/// Write SGR sequence for style flags (all set flags).
///
/// Emits `CSI n ; n ; ... m` for each enabled flag.
/// Does not emit reset first - caller is responsible for state management.
pub fn sgr_flags<W: Write>(w: &mut W, flags: StyleFlags) -> io::Result<()> {
    if flags.is_empty() {
        return Ok(());
    }

    w.write_all(sgr_flags_cached(flags))
}

/// Ordered table of (flag, on/off codes) for iteration.
pub const FLAG_TABLE: [(StyleFlags, SgrCodes); 8] = [
    (StyleFlags::BOLD, SGR_BOLD),
    (StyleFlags::DIM, SGR_DIM),
    (StyleFlags::ITALIC, SGR_ITALIC),
    (StyleFlags::UNDERLINE, SGR_UNDERLINE),
    (StyleFlags::BLINK, SGR_BLINK),
    (StyleFlags::REVERSE, SGR_REVERSE),
    (StyleFlags::HIDDEN, SGR_HIDDEN),
    (StyleFlags::STRIKETHROUGH, SGR_STRIKETHROUGH),
];

static SGR_FLAGS_CACHE: OnceLock<Vec<Vec<u8>>> = OnceLock::new();

#[inline]
fn sgr_flags_cached(flags: StyleFlags) -> &'static [u8] {
    let cache = SGR_FLAGS_CACHE.get_or_init(build_sgr_flags_cache);
    &cache[flags.bits() as usize]
}

fn build_sgr_flags_cache() -> Vec<Vec<u8>> {
    let mut cache = Vec::with_capacity(256);
    for mask in 0u16..=0xFF {
        let flags = StyleFlags::from_bits_truncate(mask as u8);
        if flags.is_empty() {
            cache.push(Vec::new());
            continue;
        }

        let mut buf = Vec::with_capacity(16);
        buf.extend_from_slice(b"\x1b[");
        let mut first = true;

        for (flag, codes) in FLAG_TABLE {
            if flags.contains(flag) {
                if !first {
                    buf.push(b';');
                }
                write_u8_dec(&mut buf, codes.on);
                first = false;
            }
        }

        buf.push(b'm');
        cache.push(buf);
    }
    cache
}

#[inline]
fn write_u8_dec(buf: &mut Vec<u8>, value: u8) {
    if value >= 100 {
        buf.push(b'0' + (value / 100));
        buf.push(b'0' + ((value / 10) % 10));
        buf.push(b'0' + (value % 10));
    } else if value >= 10 {
        buf.push(b'0' + (value / 10));
        buf.push(b'0' + (value % 10));
    } else {
        buf.push(b'0' + value);
    }
}

/// Write SGR sequence to turn off specific style flags.
///
/// Emits the individual "off" codes for each flag in `flags_to_disable`.
/// Handles the Bold/Dim shared off code (22): if only one of Bold/Dim needs
/// to be disabled while the other must stay on, the caller must re-enable
/// the survivor separately. This function returns the set of flags that were
/// collaterally disabled (i.e., flags that share an off code with a disabled flag
/// but should remain enabled according to `flags_to_keep`).
///
/// Returns the set of flags that need to be re-enabled due to shared off codes.
pub fn sgr_flags_off<W: Write>(
    w: &mut W,
    flags_to_disable: StyleFlags,
    flags_to_keep: StyleFlags,
) -> io::Result<StyleFlags> {
    if flags_to_disable.is_empty() {
        return Ok(StyleFlags::empty());
    }

    let mut collateral = StyleFlags::empty();

    for (flag, codes) in FLAG_TABLE {
        if !flags_to_disable.contains(flag) {
            continue;
        }
        // Emit the off code
        write!(w, "\x1b[{}m", codes.off)?;
        // Check for collateral damage: Bold (off=22) and Dim (off=22) share the same off code
        if codes.off == 22 {
            // Off code 22 disables both Bold and Dim
            let other = if flag == StyleFlags::BOLD {
                StyleFlags::DIM
            } else {
                StyleFlags::BOLD
            };
            if flags_to_keep.contains(other) {
                collateral |= other;
            }
        }
    }

    Ok(collateral)
}

/// Write SGR sequence for true color foreground: `CSI 38;2;r;g;b m`
pub fn sgr_fg_rgb<W: Write>(w: &mut W, r: u8, g: u8, b: u8) -> io::Result<()> {
    write!(w, "\x1b[38;2;{r};{g};{b}m")
}

/// Write SGR sequence for true color background: `CSI 48;2;r;g;b m`
pub fn sgr_bg_rgb<W: Write>(w: &mut W, r: u8, g: u8, b: u8) -> io::Result<()> {
    write!(w, "\x1b[48;2;{r};{g};{b}m")
}

/// Write SGR sequence for 256-color foreground: `CSI 38;5;n m`
pub fn sgr_fg_256<W: Write>(w: &mut W, index: u8) -> io::Result<()> {
    write!(w, "\x1b[38;5;{index}m")
}

/// Write SGR sequence for 256-color background: `CSI 48;5;n m`
pub fn sgr_bg_256<W: Write>(w: &mut W, index: u8) -> io::Result<()> {
    write!(w, "\x1b[48;5;{index}m")
}

/// Write SGR sequence for 16-color foreground.
///
/// Uses codes 30-37 for normal colors, 90-97 for bright colors.
pub fn sgr_fg_16<W: Write>(w: &mut W, index: u8) -> io::Result<()> {
    let code = if index < 8 {
        30 + index
    } else {
        90 + index - 8
    };
    write!(w, "\x1b[{code}m")
}

/// Write SGR sequence for 16-color background.
///
/// Uses codes 40-47 for normal colors, 100-107 for bright colors.
pub fn sgr_bg_16<W: Write>(w: &mut W, index: u8) -> io::Result<()> {
    let code = if index < 8 {
        40 + index
    } else {
        100 + index - 8
    };
    write!(w, "\x1b[{code}m")
}

/// Write SGR default foreground: `CSI 39 m`
pub fn sgr_fg_default<W: Write>(w: &mut W) -> io::Result<()> {
    w.write_all(b"\x1b[39m")
}

/// Write SGR default background: `CSI 49 m`
pub fn sgr_bg_default<W: Write>(w: &mut W) -> io::Result<()> {
    w.write_all(b"\x1b[49m")
}

/// Write SGR for a PackedRgba color as foreground (true color).
///
/// Skips if alpha is 0 (transparent).
pub fn sgr_fg_packed<W: Write>(w: &mut W, color: PackedRgba) -> io::Result<()> {
    if color.a() == 0 {
        return sgr_fg_default(w);
    }
    sgr_fg_rgb(w, color.r(), color.g(), color.b())
}

/// Write SGR for a PackedRgba color as background (true color).
///
/// Skips if alpha is 0 (transparent).
pub fn sgr_bg_packed<W: Write>(w: &mut W, color: PackedRgba) -> io::Result<()> {
    if color.a() == 0 {
        return sgr_bg_default(w);
    }
    sgr_bg_rgb(w, color.r(), color.g(), color.b())
}

// =============================================================================
// Cursor Positioning
// =============================================================================

/// CUP (Cursor Position): `CSI row ; col H` (1-indexed)
///
/// Moves cursor to absolute position. Row and col are 0-indexed input,
/// converted to 1-indexed for ANSI.
pub fn cup<W: Write>(w: &mut W, row: u16, col: u16) -> io::Result<()> {
    write!(
        w,
        "\x1b[{};{}H",
        row.saturating_add(1),
        col.saturating_add(1)
    )
}

/// CUP to column only: `CSI col G` (1-indexed)
///
/// Moves cursor to column on current row.
pub fn cha<W: Write>(w: &mut W, col: u16) -> io::Result<()> {
    write!(w, "\x1b[{}G", col.saturating_add(1))
}

/// Move cursor up: `CSI n A`
pub fn cuu<W: Write>(w: &mut W, n: u16) -> io::Result<()> {
    if n == 0 {
        return Ok(());
    }
    if n == 1 {
        w.write_all(b"\x1b[A")
    } else {
        write!(w, "\x1b[{n}A")
    }
}

/// Move cursor down: `CSI n B`
pub fn cud<W: Write>(w: &mut W, n: u16) -> io::Result<()> {
    if n == 0 {
        return Ok(());
    }
    if n == 1 {
        w.write_all(b"\x1b[B")
    } else {
        write!(w, "\x1b[{n}B")
    }
}

/// Move cursor forward (right): `CSI n C`
pub fn cuf<W: Write>(w: &mut W, n: u16) -> io::Result<()> {
    if n == 0 {
        return Ok(());
    }
    if n == 1 {
        w.write_all(b"\x1b[C")
    } else {
        write!(w, "\x1b[{n}C")
    }
}

/// Move cursor back (left): `CSI n D`
pub fn cub<W: Write>(w: &mut W, n: u16) -> io::Result<()> {
    if n == 0 {
        return Ok(());
    }
    if n == 1 {
        w.write_all(b"\x1b[D")
    } else {
        write!(w, "\x1b[{n}D")
    }
}

/// Move cursor to start of line: `\r` (CR)
#[inline]
pub fn cr<W: Write>(w: &mut W) -> io::Result<()> {
    w.write_all(b"\r")
}

/// Move cursor down one line: `\n` (LF)
///
/// Note: In raw mode (OPOST disabled), this moves y+1 but preserves x.
#[inline]
pub fn lf<W: Write>(w: &mut W) -> io::Result<()> {
    w.write_all(b"\n")
}

/// DEC cursor save: `ESC 7` (DECSC)
pub const CURSOR_SAVE: &[u8] = b"\x1b7";

/// DEC cursor restore: `ESC 8` (DECRC)
pub const CURSOR_RESTORE: &[u8] = b"\x1b8";

/// Write cursor save (DECSC).
#[inline]
pub fn cursor_save<W: Write>(w: &mut W) -> io::Result<()> {
    w.write_all(CURSOR_SAVE)
}

/// Write cursor restore (DECRC).
#[inline]
pub fn cursor_restore<W: Write>(w: &mut W) -> io::Result<()> {
    w.write_all(CURSOR_RESTORE)
}

/// Hide cursor: `CSI ? 25 l`
pub const CURSOR_HIDE: &[u8] = b"\x1b[?25l";

/// Show cursor: `CSI ? 25 h`
pub const CURSOR_SHOW: &[u8] = b"\x1b[?25h";

/// Write hide cursor.
#[inline]
pub fn cursor_hide<W: Write>(w: &mut W) -> io::Result<()> {
    w.write_all(CURSOR_HIDE)
}

/// Write show cursor.
#[inline]
pub fn cursor_show<W: Write>(w: &mut W) -> io::Result<()> {
    w.write_all(CURSOR_SHOW)
}

// =============================================================================
// Erase Operations
// =============================================================================

/// EL (Erase Line) mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EraseLineMode {
    /// Erase from cursor to end of line.
    ToEnd = 0,
    /// Erase from start of line to cursor.
    ToStart = 1,
    /// Erase entire line.
    All = 2,
}

/// EL (Erase Line): `CSI n K`
pub fn erase_line<W: Write>(w: &mut W, mode: EraseLineMode) -> io::Result<()> {
    match mode {
        EraseLineMode::ToEnd => w.write_all(b"\x1b[K"),
        EraseLineMode::ToStart => w.write_all(b"\x1b[1K"),
        EraseLineMode::All => w.write_all(b"\x1b[2K"),
    }
}

/// ED (Erase Display) mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EraseDisplayMode {
    /// Erase from cursor to end of screen.
    ToEnd = 0,
    /// Erase from start of screen to cursor.
    ToStart = 1,
    /// Erase entire screen.
    All = 2,
    /// Erase scrollback buffer (xterm extension).
    Scrollback = 3,
}

/// ED (Erase Display): `CSI n J`
pub fn erase_display<W: Write>(w: &mut W, mode: EraseDisplayMode) -> io::Result<()> {
    match mode {
        EraseDisplayMode::ToEnd => w.write_all(b"\x1b[J"),
        EraseDisplayMode::ToStart => w.write_all(b"\x1b[1J"),
        EraseDisplayMode::All => w.write_all(b"\x1b[2J"),
        EraseDisplayMode::Scrollback => w.write_all(b"\x1b[3J"),
    }
}

// =============================================================================
// Scroll Region
// =============================================================================

/// DECSTBM (Set Top and Bottom Margins): `CSI top ; bottom r`
///
/// Sets the scroll region. Top and bottom are 0-indexed, converted to 1-indexed.
pub fn set_scroll_region<W: Write>(w: &mut W, top: u16, bottom: u16) -> io::Result<()> {
    write!(
        w,
        "\x1b[{};{}r",
        top.saturating_add(1),
        bottom.saturating_add(1)
    )
}

/// Reset scroll region to full screen: `CSI r`
pub const RESET_SCROLL_REGION: &[u8] = b"\x1b[r";

/// Write reset scroll region.
#[inline]
pub fn reset_scroll_region<W: Write>(w: &mut W) -> io::Result<()> {
    w.write_all(RESET_SCROLL_REGION)
}

// =============================================================================
// Synchronized Output (DEC 2026)
// =============================================================================

/// Begin synchronized output: `CSI ? 2026 h`
pub const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";

/// End synchronized output: `CSI ? 2026 l`
pub const SYNC_END: &[u8] = b"\x1b[?2026l";

/// Write synchronized output begin.
#[inline]
pub fn sync_begin<W: Write>(w: &mut W) -> io::Result<()> {
    w.write_all(SYNC_BEGIN)
}

/// Write synchronized output end.
#[inline]
pub fn sync_end<W: Write>(w: &mut W) -> io::Result<()> {
    w.write_all(SYNC_END)
}

// =============================================================================
// OSC 8 Hyperlinks
// =============================================================================

/// Open an OSC 8 hyperlink.
///
/// Format: `OSC 8 ; params ; uri ST`
/// Uses ST (String Terminator) = `ESC \`
pub fn hyperlink_start<W: Write>(w: &mut W, url: &str) -> io::Result<()> {
    write!(w, "\x1b]8;;{url}\x1b\\")
}

/// Close an OSC 8 hyperlink.
///
/// Format: `OSC 8 ; ; ST`
pub fn hyperlink_end<W: Write>(w: &mut W) -> io::Result<()> {
    w.write_all(b"\x1b]8;;\x1b\\")
}

/// Open an OSC 8 hyperlink with an ID parameter.
///
/// The ID allows grouping multiple link spans.
/// Format: `OSC 8 ; id=ID ; uri ST`
pub fn hyperlink_start_with_id<W: Write>(w: &mut W, id: &str, url: &str) -> io::Result<()> {
    write!(w, "\x1b]8;id={id};{url}\x1b\\")
}

// =============================================================================
// Mode Control
// =============================================================================

/// Enable alternate screen: `CSI ? 1049 h`
pub const ALT_SCREEN_ENTER: &[u8] = b"\x1b[?1049h";

/// Disable alternate screen: `CSI ? 1049 l`
pub const ALT_SCREEN_LEAVE: &[u8] = b"\x1b[?1049l";

/// Enable bracketed paste: `CSI ? 2004 h`
pub const BRACKETED_PASTE_ENABLE: &[u8] = b"\x1b[?2004h";

/// Disable bracketed paste: `CSI ? 2004 l`
pub const BRACKETED_PASTE_DISABLE: &[u8] = b"\x1b[?2004l";

/// Enable SGR mouse reporting: `CSI ? 1000;1002;1006 h`
///
/// Enables:
/// - 1000: Normal mouse tracking
/// - 1002: Button event tracking (motion while pressed)
/// - 1006: SGR extended coordinates (supports > 223)
pub const MOUSE_ENABLE: &[u8] = b"\x1b[?1000;1002;1006h";

/// Disable mouse reporting: `CSI ? 1000;1002;1006 l`
pub const MOUSE_DISABLE: &[u8] = b"\x1b[?1000;1002;1006l";

/// Enable focus reporting: `CSI ? 1004 h`
pub const FOCUS_ENABLE: &[u8] = b"\x1b[?1004h";

/// Disable focus reporting: `CSI ? 1004 l`
pub const FOCUS_DISABLE: &[u8] = b"\x1b[?1004l";

/// Write alternate screen enter.
#[inline]
pub fn alt_screen_enter<W: Write>(w: &mut W) -> io::Result<()> {
    w.write_all(ALT_SCREEN_ENTER)
}

/// Write alternate screen leave.
#[inline]
pub fn alt_screen_leave<W: Write>(w: &mut W) -> io::Result<()> {
    w.write_all(ALT_SCREEN_LEAVE)
}

/// Write bracketed paste enable.
#[inline]
pub fn bracketed_paste_enable<W: Write>(w: &mut W) -> io::Result<()> {
    w.write_all(BRACKETED_PASTE_ENABLE)
}

/// Write bracketed paste disable.
#[inline]
pub fn bracketed_paste_disable<W: Write>(w: &mut W) -> io::Result<()> {
    w.write_all(BRACKETED_PASTE_DISABLE)
}

/// Write mouse enable.
#[inline]
pub fn mouse_enable<W: Write>(w: &mut W) -> io::Result<()> {
    w.write_all(MOUSE_ENABLE)
}

/// Write mouse disable.
#[inline]
pub fn mouse_disable<W: Write>(w: &mut W) -> io::Result<()> {
    w.write_all(MOUSE_DISABLE)
}

/// Write focus enable.
#[inline]
pub fn focus_enable<W: Write>(w: &mut W) -> io::Result<()> {
    w.write_all(FOCUS_ENABLE)
}

/// Write focus disable.
#[inline]
pub fn focus_disable<W: Write>(w: &mut W) -> io::Result<()> {
    w.write_all(FOCUS_DISABLE)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn to_bytes<F: FnOnce(&mut Vec<u8>) -> io::Result<()>>(f: F) -> Vec<u8> {
        let mut buf = Vec::new();
        f(&mut buf).unwrap();
        buf
    }

    // SGR Tests

    #[test]
    fn sgr_reset_bytes() {
        assert_eq!(to_bytes(sgr_reset), b"\x1b[0m");
    }

    #[test]
    fn sgr_flags_bold() {
        assert_eq!(to_bytes(|w| sgr_flags(w, StyleFlags::BOLD)), b"\x1b[1m");
    }

    #[test]
    fn sgr_flags_multiple() {
        let flags = StyleFlags::BOLD | StyleFlags::ITALIC | StyleFlags::UNDERLINE;
        assert_eq!(to_bytes(|w| sgr_flags(w, flags)), b"\x1b[1;3;4m");
    }

    #[test]
    fn sgr_flags_empty() {
        assert_eq!(to_bytes(|w| sgr_flags(w, StyleFlags::empty())), b"");
    }

    #[test]
    fn sgr_fg_rgb_bytes() {
        assert_eq!(
            to_bytes(|w| sgr_fg_rgb(w, 255, 128, 0)),
            b"\x1b[38;2;255;128;0m"
        );
    }

    #[test]
    fn sgr_bg_rgb_bytes() {
        assert_eq!(to_bytes(|w| sgr_bg_rgb(w, 0, 0, 0)), b"\x1b[48;2;0;0;0m");
    }

    #[test]
    fn sgr_fg_256_bytes() {
        assert_eq!(to_bytes(|w| sgr_fg_256(w, 196)), b"\x1b[38;5;196m");
    }

    #[test]
    fn sgr_bg_256_bytes() {
        assert_eq!(to_bytes(|w| sgr_bg_256(w, 232)), b"\x1b[48;5;232m");
    }

    #[test]
    fn sgr_fg_16_normal() {
        assert_eq!(to_bytes(|w| sgr_fg_16(w, 1)), b"\x1b[31m"); // Red
        assert_eq!(to_bytes(|w| sgr_fg_16(w, 7)), b"\x1b[37m"); // White
    }

    #[test]
    fn sgr_fg_16_bright() {
        assert_eq!(to_bytes(|w| sgr_fg_16(w, 9)), b"\x1b[91m"); // Bright red
        assert_eq!(to_bytes(|w| sgr_fg_16(w, 15)), b"\x1b[97m"); // Bright white
    }

    #[test]
    fn sgr_bg_16_normal() {
        assert_eq!(to_bytes(|w| sgr_bg_16(w, 0)), b"\x1b[40m"); // Black
        assert_eq!(to_bytes(|w| sgr_bg_16(w, 4)), b"\x1b[44m"); // Blue
    }

    #[test]
    fn sgr_bg_16_bright() {
        assert_eq!(to_bytes(|w| sgr_bg_16(w, 8)), b"\x1b[100m"); // Bright black
        assert_eq!(to_bytes(|w| sgr_bg_16(w, 12)), b"\x1b[104m"); // Bright blue
    }

    #[test]
    fn sgr_default_colors() {
        assert_eq!(to_bytes(sgr_fg_default), b"\x1b[39m");
        assert_eq!(to_bytes(sgr_bg_default), b"\x1b[49m");
    }

    #[test]
    fn sgr_packed_transparent_uses_default() {
        assert_eq!(
            to_bytes(|w| sgr_fg_packed(w, PackedRgba::TRANSPARENT)),
            b"\x1b[39m"
        );
        assert_eq!(
            to_bytes(|w| sgr_bg_packed(w, PackedRgba::TRANSPARENT)),
            b"\x1b[49m"
        );
    }

    #[test]
    fn sgr_packed_opaque() {
        let color = PackedRgba::rgb(10, 20, 30);
        assert_eq!(
            to_bytes(|w| sgr_fg_packed(w, color)),
            b"\x1b[38;2;10;20;30m"
        );
    }

    // Cursor Tests

    #[test]
    fn cup_1_indexed() {
        assert_eq!(to_bytes(|w| cup(w, 0, 0)), b"\x1b[1;1H");
        assert_eq!(to_bytes(|w| cup(w, 23, 79)), b"\x1b[24;80H");
    }

    #[test]
    fn cha_1_indexed() {
        assert_eq!(to_bytes(|w| cha(w, 0)), b"\x1b[1G");
        assert_eq!(to_bytes(|w| cha(w, 79)), b"\x1b[80G");
    }

    #[test]
    fn cursor_relative_moves() {
        assert_eq!(to_bytes(|w| cuu(w, 1)), b"\x1b[A");
        assert_eq!(to_bytes(|w| cuu(w, 5)), b"\x1b[5A");
        assert_eq!(to_bytes(|w| cud(w, 1)), b"\x1b[B");
        assert_eq!(to_bytes(|w| cud(w, 3)), b"\x1b[3B");
        assert_eq!(to_bytes(|w| cuf(w, 1)), b"\x1b[C");
        assert_eq!(to_bytes(|w| cuf(w, 10)), b"\x1b[10C");
        assert_eq!(to_bytes(|w| cub(w, 1)), b"\x1b[D");
        assert_eq!(to_bytes(|w| cub(w, 2)), b"\x1b[2D");
    }

    #[test]
    fn cursor_relative_zero_is_noop() {
        assert_eq!(to_bytes(|w| cuu(w, 0)), b"");
        assert_eq!(to_bytes(|w| cud(w, 0)), b"");
        assert_eq!(to_bytes(|w| cuf(w, 0)), b"");
        assert_eq!(to_bytes(|w| cub(w, 0)), b"");
    }

    #[test]
    fn cursor_save_restore() {
        assert_eq!(to_bytes(cursor_save), b"\x1b7");
        assert_eq!(to_bytes(cursor_restore), b"\x1b8");
    }

    #[test]
    fn cursor_visibility() {
        assert_eq!(to_bytes(cursor_hide), b"\x1b[?25l");
        assert_eq!(to_bytes(cursor_show), b"\x1b[?25h");
    }

    // Erase Tests

    #[test]
    fn erase_line_modes() {
        assert_eq!(to_bytes(|w| erase_line(w, EraseLineMode::ToEnd)), b"\x1b[K");
        assert_eq!(
            to_bytes(|w| erase_line(w, EraseLineMode::ToStart)),
            b"\x1b[1K"
        );
        assert_eq!(to_bytes(|w| erase_line(w, EraseLineMode::All)), b"\x1b[2K");
    }

    #[test]
    fn erase_display_modes() {
        assert_eq!(
            to_bytes(|w| erase_display(w, EraseDisplayMode::ToEnd)),
            b"\x1b[J"
        );
        assert_eq!(
            to_bytes(|w| erase_display(w, EraseDisplayMode::ToStart)),
            b"\x1b[1J"
        );
        assert_eq!(
            to_bytes(|w| erase_display(w, EraseDisplayMode::All)),
            b"\x1b[2J"
        );
        assert_eq!(
            to_bytes(|w| erase_display(w, EraseDisplayMode::Scrollback)),
            b"\x1b[3J"
        );
    }

    // Scroll Region Tests

    #[test]
    fn scroll_region_1_indexed() {
        assert_eq!(to_bytes(|w| set_scroll_region(w, 0, 23)), b"\x1b[1;24r");
        assert_eq!(to_bytes(|w| set_scroll_region(w, 5, 20)), b"\x1b[6;21r");
    }

    #[test]
    fn scroll_region_reset() {
        assert_eq!(to_bytes(reset_scroll_region), b"\x1b[r");
    }

    // Sync Output Tests

    #[test]
    fn sync_output() {
        assert_eq!(to_bytes(sync_begin), b"\x1b[?2026h");
        assert_eq!(to_bytes(sync_end), b"\x1b[?2026l");
    }

    // OSC 8 Hyperlink Tests

    #[test]
    fn hyperlink_basic() {
        assert_eq!(
            to_bytes(|w| hyperlink_start(w, "https://example.com")),
            b"\x1b]8;;https://example.com\x1b\\"
        );
        assert_eq!(to_bytes(hyperlink_end), b"\x1b]8;;\x1b\\");
    }

    #[test]
    fn hyperlink_with_id() {
        assert_eq!(
            to_bytes(|w| hyperlink_start_with_id(w, "link1", "https://example.com")),
            b"\x1b]8;id=link1;https://example.com\x1b\\"
        );
    }

    // Mode Control Tests

    #[test]
    fn alt_screen() {
        assert_eq!(to_bytes(alt_screen_enter), b"\x1b[?1049h");
        assert_eq!(to_bytes(alt_screen_leave), b"\x1b[?1049l");
    }

    #[test]
    fn bracketed_paste() {
        assert_eq!(to_bytes(bracketed_paste_enable), b"\x1b[?2004h");
        assert_eq!(to_bytes(bracketed_paste_disable), b"\x1b[?2004l");
    }

    #[test]
    fn mouse_mode() {
        assert_eq!(to_bytes(mouse_enable), b"\x1b[?1000;1002;1006h");
        assert_eq!(to_bytes(mouse_disable), b"\x1b[?1000;1002;1006l");
    }

    #[test]
    fn focus_mode() {
        assert_eq!(to_bytes(focus_enable), b"\x1b[?1004h");
        assert_eq!(to_bytes(focus_disable), b"\x1b[?1004l");
    }

    // Property tests

    #[test]
    fn all_sequences_are_ascii() {
        // Verify no high bytes in any constant sequences
        for seq in [
            SGR_RESET,
            CURSOR_SAVE,
            CURSOR_RESTORE,
            CURSOR_HIDE,
            CURSOR_SHOW,
            RESET_SCROLL_REGION,
            SYNC_BEGIN,
            SYNC_END,
            ALT_SCREEN_ENTER,
            ALT_SCREEN_LEAVE,
            BRACKETED_PASTE_ENABLE,
            BRACKETED_PASTE_DISABLE,
            MOUSE_ENABLE,
            MOUSE_DISABLE,
            FOCUS_ENABLE,
            FOCUS_DISABLE,
        ] {
            for &byte in seq {
                assert!(byte < 128, "Non-ASCII byte {byte:#x} in sequence");
            }
        }
    }

    #[test]
    fn osc_sequences_are_terminated() {
        // All OSC 8 sequences must end with ST (ESC \)
        let link_start = to_bytes(|w| hyperlink_start(w, "test"));
        assert!(
            link_start.ends_with(b"\x1b\\"),
            "hyperlink_start not terminated with ST"
        );

        let link_end = to_bytes(hyperlink_end);
        assert!(
            link_end.ends_with(b"\x1b\\"),
            "hyperlink_end not terminated with ST"
        );

        let link_id = to_bytes(|w| hyperlink_start_with_id(w, "id", "url"));
        assert!(
            link_id.ends_with(b"\x1b\\"),
            "hyperlink_start_with_id not terminated with ST"
        );
    }
}
