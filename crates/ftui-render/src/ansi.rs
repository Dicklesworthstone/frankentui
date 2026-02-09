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

#[inline]
fn write_u8_dec(buf: &mut [u8], n: u8) -> usize {
    if n >= 100 {
        let hundreds = n / 100;
        let tens = (n / 10) % 10;
        let ones = n % 10;
        buf[0] = b'0' + hundreds;
        buf[1] = b'0' + tens;
        buf[2] = b'0' + ones;
        3
    } else if n >= 10 {
        let tens = n / 10;
        let ones = n % 10;
        buf[0] = b'0' + tens;
        buf[1] = b'0' + ones;
        2
    } else {
        buf[0] = b'0' + n;
        1
    }
}

#[inline]
fn write_sgr_code<W: Write>(w: &mut W, code: u8) -> io::Result<()> {
    let mut buf = [0u8; 6];
    buf[0] = 0x1b;
    buf[1] = b'[';
    let len = write_u8_dec(&mut buf[2..], code);
    buf[2 + len] = b'm';
    w.write_all(&buf[..2 + len + 1])
}

/// Write SGR sequence for style flags (all set flags).
///
/// Emits `CSI n ; n ; ... m` for each enabled flag.
/// Does not emit reset first - caller is responsible for state management.
pub fn sgr_flags<W: Write>(w: &mut W, flags: StyleFlags) -> io::Result<()> {
    if flags.is_empty() {
        return Ok(());
    }

    let bits = flags.bits();
    if bits.is_power_of_two()
        && let Some(seq) = sgr_single_flag_seq(bits)
    {
        return w.write_all(seq);
    }

    let mut buf = [0u8; 32];
    let mut idx = 0usize;
    buf[idx] = 0x1b;
    buf[idx + 1] = b'[';
    idx += 2;
    let mut first = true;

    for (flag, codes) in FLAG_TABLE {
        if flags.contains(flag) {
            if !first {
                buf[idx] = b';';
                idx += 1;
            }
            idx += write_u8_dec(&mut buf[idx..], codes.on);
            first = false;
        }
    }

    buf[idx] = b'm';
    idx += 1;
    w.write_all(&buf[..idx])
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

#[inline]
fn sgr_single_flag_seq(bits: u8) -> Option<&'static [u8]> {
    match bits {
        0b0000_0001 => Some(b"\x1b[1m"), // bold
        0b0000_0010 => Some(b"\x1b[2m"), // dim
        0b0000_0100 => Some(b"\x1b[3m"), // italic
        0b0000_1000 => Some(b"\x1b[4m"), // underline
        0b0001_0000 => Some(b"\x1b[5m"), // blink
        0b0010_0000 => Some(b"\x1b[7m"), // reverse
        0b0100_0000 => Some(b"\x1b[9m"), // strikethrough
        0b1000_0000 => Some(b"\x1b[8m"), // hidden
        _ => None,
    }
}

#[inline]
fn sgr_single_flag_off_seq(bits: u8) -> Option<&'static [u8]> {
    match bits {
        0b0000_0001 => Some(b"\x1b[22m"), // bold off
        0b0000_0010 => Some(b"\x1b[22m"), // dim off
        0b0000_0100 => Some(b"\x1b[23m"), // italic off
        0b0000_1000 => Some(b"\x1b[24m"), // underline off
        0b0001_0000 => Some(b"\x1b[25m"), // blink off
        0b0010_0000 => Some(b"\x1b[27m"), // reverse off
        0b0100_0000 => Some(b"\x1b[29m"), // strikethrough off
        0b1000_0000 => Some(b"\x1b[28m"), // hidden off
        _ => None,
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

    let disable_bits = flags_to_disable.bits();
    if disable_bits.is_power_of_two()
        && let Some(seq) = sgr_single_flag_off_seq(disable_bits)
    {
        w.write_all(seq)?;
        if disable_bits == StyleFlags::BOLD.bits() && flags_to_keep.contains(StyleFlags::DIM) {
            return Ok(StyleFlags::DIM);
        }
        if disable_bits == StyleFlags::DIM.bits() && flags_to_keep.contains(StyleFlags::BOLD) {
            return Ok(StyleFlags::BOLD);
        }
        return Ok(StyleFlags::empty());
    }

    let mut collateral = StyleFlags::empty();

    for (flag, codes) in FLAG_TABLE {
        if !flags_to_disable.contains(flag) {
            continue;
        }
        // Emit the off code
        write_sgr_code(w, codes.off)?;
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

    // ---- sgr_flags_off tests ----

    #[test]
    fn sgr_flags_off_empty_is_noop() {
        let bytes = to_bytes(|w| {
            sgr_flags_off(w, StyleFlags::empty(), StyleFlags::empty()).unwrap();
            Ok(())
        });
        assert!(bytes.is_empty(), "disabling no flags should emit nothing");
    }

    #[test]
    fn sgr_flags_off_single_bold() {
        let mut buf = Vec::new();
        let collateral = sgr_flags_off(&mut buf, StyleFlags::BOLD, StyleFlags::empty()).unwrap();
        assert_eq!(buf, b"\x1b[22m");
        assert!(collateral.is_empty(), "no collateral when DIM is not kept");
    }

    #[test]
    fn sgr_flags_off_single_dim() {
        let mut buf = Vec::new();
        let collateral = sgr_flags_off(&mut buf, StyleFlags::DIM, StyleFlags::empty()).unwrap();
        assert_eq!(buf, b"\x1b[22m");
        assert!(collateral.is_empty(), "no collateral when BOLD is not kept");
    }

    #[test]
    fn sgr_flags_off_bold_collateral_dim() {
        // Disabling BOLD while DIM should stay → collateral = DIM
        let mut buf = Vec::new();
        let collateral = sgr_flags_off(&mut buf, StyleFlags::BOLD, StyleFlags::DIM).unwrap();
        assert_eq!(buf, b"\x1b[22m");
        assert_eq!(collateral, StyleFlags::DIM);
    }

    #[test]
    fn sgr_flags_off_dim_collateral_bold() {
        // Disabling DIM while BOLD should stay → collateral = BOLD
        let mut buf = Vec::new();
        let collateral = sgr_flags_off(&mut buf, StyleFlags::DIM, StyleFlags::BOLD).unwrap();
        assert_eq!(buf, b"\x1b[22m");
        assert_eq!(collateral, StyleFlags::BOLD);
    }

    #[test]
    fn sgr_flags_off_italic() {
        let mut buf = Vec::new();
        let collateral = sgr_flags_off(&mut buf, StyleFlags::ITALIC, StyleFlags::empty()).unwrap();
        assert_eq!(buf, b"\x1b[23m");
        assert!(collateral.is_empty());
    }

    #[test]
    fn sgr_flags_off_underline() {
        let mut buf = Vec::new();
        let collateral =
            sgr_flags_off(&mut buf, StyleFlags::UNDERLINE, StyleFlags::empty()).unwrap();
        assert_eq!(buf, b"\x1b[24m");
        assert!(collateral.is_empty());
    }

    #[test]
    fn sgr_flags_off_blink() {
        let mut buf = Vec::new();
        let collateral = sgr_flags_off(&mut buf, StyleFlags::BLINK, StyleFlags::empty()).unwrap();
        assert_eq!(buf, b"\x1b[25m");
        assert!(collateral.is_empty());
    }

    #[test]
    fn sgr_flags_off_reverse() {
        let mut buf = Vec::new();
        let collateral = sgr_flags_off(&mut buf, StyleFlags::REVERSE, StyleFlags::empty()).unwrap();
        assert_eq!(buf, b"\x1b[27m");
        assert!(collateral.is_empty());
    }

    #[test]
    fn sgr_flags_off_hidden() {
        let mut buf = Vec::new();
        let collateral = sgr_flags_off(&mut buf, StyleFlags::HIDDEN, StyleFlags::empty()).unwrap();
        assert_eq!(buf, b"\x1b[28m");
        assert!(collateral.is_empty());
    }

    #[test]
    fn sgr_flags_off_strikethrough() {
        let mut buf = Vec::new();
        let collateral =
            sgr_flags_off(&mut buf, StyleFlags::STRIKETHROUGH, StyleFlags::empty()).unwrap();
        assert_eq!(buf, b"\x1b[29m");
        assert!(collateral.is_empty());
    }

    #[test]
    fn sgr_flags_off_multi_no_bold_dim_overlap() {
        // Disable ITALIC + UNDERLINE (no shared off codes)
        let mut buf = Vec::new();
        let collateral = sgr_flags_off(
            &mut buf,
            StyleFlags::ITALIC | StyleFlags::UNDERLINE,
            StyleFlags::empty(),
        )
        .unwrap();
        // Multi-flag path emits individual off codes
        let bytes = String::from_utf8(buf).unwrap();
        assert!(bytes.contains("\x1b[23m"), "should contain italic off");
        assert!(bytes.contains("\x1b[24m"), "should contain underline off");
        assert!(collateral.is_empty());
    }

    #[test]
    fn sgr_flags_off_bold_and_dim_together() {
        // Disabling both BOLD and DIM: off=22 emitted for each, but no collateral
        // since both are being disabled (neither needs to stay)
        let mut buf = Vec::new();
        let collateral = sgr_flags_off(
            &mut buf,
            StyleFlags::BOLD | StyleFlags::DIM,
            StyleFlags::empty(),
        )
        .unwrap();
        assert!(
            collateral.is_empty(),
            "no collateral when both are disabled"
        );
    }

    #[test]
    fn sgr_flags_off_bold_dim_with_dim_kept() {
        // Disabling BOLD + ITALIC while DIM should stay
        let mut buf = Vec::new();
        let collateral = sgr_flags_off(
            &mut buf,
            StyleFlags::BOLD | StyleFlags::ITALIC,
            StyleFlags::DIM,
        )
        .unwrap();
        assert_eq!(
            collateral,
            StyleFlags::DIM,
            "DIM should be collateral damage from BOLD off (code 22)"
        );
    }

    // ---- sgr_codes_for_flag tests ----

    #[test]
    fn sgr_codes_for_all_single_flags() {
        let cases = [
            (StyleFlags::BOLD, 1, 22),
            (StyleFlags::DIM, 2, 22),
            (StyleFlags::ITALIC, 3, 23),
            (StyleFlags::UNDERLINE, 4, 24),
            (StyleFlags::BLINK, 5, 25),
            (StyleFlags::REVERSE, 7, 27),
            (StyleFlags::HIDDEN, 8, 28),
            (StyleFlags::STRIKETHROUGH, 9, 29),
        ];
        for (flag, expected_on, expected_off) in cases {
            let codes = sgr_codes_for_flag(flag)
                .unwrap_or_else(|| panic!("should return codes for {flag:?}"));
            assert_eq!(codes.on, expected_on, "on code for {flag:?}");
            assert_eq!(codes.off, expected_off, "off code for {flag:?}");
        }
    }

    #[test]
    fn sgr_codes_for_composite_flag_returns_none() {
        let composite = StyleFlags::BOLD | StyleFlags::ITALIC;
        assert!(
            sgr_codes_for_flag(composite).is_none(),
            "composite flags should return None"
        );
    }

    #[test]
    fn sgr_codes_for_empty_flag_returns_none() {
        assert!(
            sgr_codes_for_flag(StyleFlags::empty()).is_none(),
            "empty flags should return None"
        );
    }

    // ---- cr / lf tests ----

    #[test]
    fn cr_emits_carriage_return() {
        assert_eq!(to_bytes(cr), b"\r");
    }

    #[test]
    fn lf_emits_line_feed() {
        assert_eq!(to_bytes(lf), b"\n");
    }

    // ---- sgr_flags individual fast-path verification ----

    #[test]
    fn sgr_flags_each_single_flag_fast_path() {
        let cases: &[(StyleFlags, &[u8])] = &[
            (StyleFlags::BOLD, b"\x1b[1m"),
            (StyleFlags::DIM, b"\x1b[2m"),
            (StyleFlags::ITALIC, b"\x1b[3m"),
            (StyleFlags::UNDERLINE, b"\x1b[4m"),
            (StyleFlags::BLINK, b"\x1b[5m"),
            (StyleFlags::REVERSE, b"\x1b[7m"),
            (StyleFlags::STRIKETHROUGH, b"\x1b[9m"),
            (StyleFlags::HIDDEN, b"\x1b[8m"),
        ];
        for &(flag, expected) in cases {
            assert_eq!(
                to_bytes(|w| sgr_flags(w, flag)),
                expected,
                "single-flag fast path for {flag:?}"
            );
        }
    }

    #[test]
    fn sgr_flags_all_eight() {
        let all = StyleFlags::BOLD
            | StyleFlags::DIM
            | StyleFlags::ITALIC
            | StyleFlags::UNDERLINE
            | StyleFlags::BLINK
            | StyleFlags::REVERSE
            | StyleFlags::HIDDEN
            | StyleFlags::STRIKETHROUGH;
        let bytes = to_bytes(|w| sgr_flags(w, all));
        // Should emit CSI with codes in FLAG_TABLE order: 1;2;3;4;5;7;8;9
        assert_eq!(bytes, b"\x1b[1;2;3;4;5;7;8;9m");
    }

    // ---- write_u8_dec boundary verification (via sgr_code) ----

    #[test]
    fn sgr_code_single_digit() {
        // code=1 → "\x1b[1m" (1 digit)
        let mut buf = Vec::new();
        write_sgr_code(&mut buf, 1).unwrap();
        assert_eq!(buf, b"\x1b[1m");
    }

    #[test]
    fn sgr_code_two_digits() {
        // code=22 → "\x1b[22m" (2 digits)
        let mut buf = Vec::new();
        write_sgr_code(&mut buf, 22).unwrap();
        assert_eq!(buf, b"\x1b[22m");
    }

    #[test]
    fn sgr_code_three_digits() {
        // code=100 → "\x1b[100m" (3 digits)
        let mut buf = Vec::new();
        write_sgr_code(&mut buf, 100).unwrap();
        assert_eq!(buf, b"\x1b[100m");
    }

    #[test]
    fn sgr_code_max_u8() {
        // code=255 → "\x1b[255m"
        let mut buf = Vec::new();
        write_sgr_code(&mut buf, 255).unwrap();
        assert_eq!(buf, b"\x1b[255m");
    }

    #[test]
    fn sgr_code_zero() {
        let mut buf = Vec::new();
        write_sgr_code(&mut buf, 0).unwrap();
        assert_eq!(buf, b"\x1b[0m");
    }

    // ---- 16-color boundary tests ----

    #[test]
    fn sgr_fg_16_boundary_7_to_8() {
        // Index 7 is the last normal color, 8 is first bright
        assert_eq!(to_bytes(|w| sgr_fg_16(w, 7)), b"\x1b[37m");
        assert_eq!(to_bytes(|w| sgr_fg_16(w, 8)), b"\x1b[90m");
    }

    #[test]
    fn sgr_bg_16_boundary_7_to_8() {
        assert_eq!(to_bytes(|w| sgr_bg_16(w, 7)), b"\x1b[47m");
        assert_eq!(to_bytes(|w| sgr_bg_16(w, 8)), b"\x1b[100m");
    }

    #[test]
    fn sgr_fg_16_first_color() {
        assert_eq!(to_bytes(|w| sgr_fg_16(w, 0)), b"\x1b[30m"); // Black
    }

    #[test]
    fn sgr_bg_16_last_bright() {
        assert_eq!(to_bytes(|w| sgr_bg_16(w, 15)), b"\x1b[107m"); // Bright white
    }

    // ---- 256-color boundary tests ----

    #[test]
    fn sgr_fg_256_zero() {
        assert_eq!(to_bytes(|w| sgr_fg_256(w, 0)), b"\x1b[38;5;0m");
    }

    #[test]
    fn sgr_fg_256_max() {
        assert_eq!(to_bytes(|w| sgr_fg_256(w, 255)), b"\x1b[38;5;255m");
    }

    #[test]
    fn sgr_bg_256_zero() {
        assert_eq!(to_bytes(|w| sgr_bg_256(w, 0)), b"\x1b[48;5;0m");
    }

    #[test]
    fn sgr_bg_256_max() {
        assert_eq!(to_bytes(|w| sgr_bg_256(w, 255)), b"\x1b[48;5;255m");
    }

    // ---- cursor positioning edge cases ----

    #[test]
    fn cup_max_u16() {
        // u16::MAX saturating_add(1) wraps correctly
        let bytes = to_bytes(|w| cup(w, u16::MAX, u16::MAX));
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.starts_with("\x1b["));
        assert!(s.ends_with("H"));
    }

    #[test]
    fn cha_max_u16() {
        let bytes = to_bytes(|w| cha(w, u16::MAX));
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.starts_with("\x1b["));
        assert!(s.ends_with("G"));
    }

    #[test]
    fn cursor_up_max() {
        let bytes = to_bytes(|w| cuu(w, u16::MAX));
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.contains("65535"));
        assert!(s.ends_with("A"));
    }

    // ---- scroll region edge cases ----

    #[test]
    fn scroll_region_same_top_bottom() {
        assert_eq!(to_bytes(|w| set_scroll_region(w, 5, 5)), b"\x1b[6;6r");
    }

    // ---- sgr_flags_off single-flag off-seq fast path (all 8 flags) ----

    #[test]
    fn sgr_flags_off_each_single_flag_fast_path() {
        let cases: &[(StyleFlags, &[u8])] = &[
            (StyleFlags::BOLD, b"\x1b[22m"),
            (StyleFlags::DIM, b"\x1b[22m"),
            (StyleFlags::ITALIC, b"\x1b[23m"),
            (StyleFlags::UNDERLINE, b"\x1b[24m"),
            (StyleFlags::BLINK, b"\x1b[25m"),
            (StyleFlags::REVERSE, b"\x1b[27m"),
            (StyleFlags::STRIKETHROUGH, b"\x1b[29m"),
            (StyleFlags::HIDDEN, b"\x1b[28m"),
        ];
        for &(flag, expected) in cases {
            let mut buf = Vec::new();
            let collateral = sgr_flags_off(&mut buf, flag, StyleFlags::empty()).unwrap();
            assert_eq!(buf, expected, "off sequence for {flag:?}");
            assert!(collateral.is_empty(), "no collateral for {flag:?}");
        }
    }

    // ---- sgr_packed with non-zero alpha ----

    #[test]
    fn sgr_bg_packed_opaque() {
        let color = PackedRgba::rgb(100, 200, 50);
        assert_eq!(
            to_bytes(|w| sgr_bg_packed(w, color)),
            b"\x1b[48;2;100;200;50m"
        );
    }

    // ---- hyperlink with empty url/id ----

    #[test]
    fn hyperlink_empty_url() {
        assert_eq!(to_bytes(|w| hyperlink_start(w, "")), b"\x1b]8;;\x1b\\");
    }

    #[test]
    fn hyperlink_with_empty_id() {
        assert_eq!(
            to_bytes(|w| hyperlink_start_with_id(w, "", "https://x.com")),
            b"\x1b]8;id=;https://x.com\x1b\\"
        );
    }

    // ---- all dynamic sequences start with ESC ----

    #[test]
    fn all_dynamic_sequences_start_with_esc() {
        let sequences: Vec<Vec<u8>> = vec![
            to_bytes(sgr_reset),
            to_bytes(|w| sgr_flags(w, StyleFlags::BOLD)),
            to_bytes(|w| sgr_fg_rgb(w, 1, 2, 3)),
            to_bytes(|w| sgr_bg_rgb(w, 1, 2, 3)),
            to_bytes(|w| sgr_fg_256(w, 42)),
            to_bytes(|w| sgr_bg_256(w, 42)),
            to_bytes(|w| sgr_fg_16(w, 5)),
            to_bytes(|w| sgr_bg_16(w, 5)),
            to_bytes(sgr_fg_default),
            to_bytes(sgr_bg_default),
            to_bytes(|w| cup(w, 0, 0)),
            to_bytes(|w| cha(w, 0)),
            to_bytes(|w| cuu(w, 1)),
            to_bytes(|w| cud(w, 1)),
            to_bytes(|w| cuf(w, 1)),
            to_bytes(|w| cub(w, 1)),
            to_bytes(cursor_save),
            to_bytes(cursor_restore),
            to_bytes(cursor_hide),
            to_bytes(cursor_show),
            to_bytes(|w| erase_line(w, EraseLineMode::All)),
            to_bytes(|w| erase_display(w, EraseDisplayMode::All)),
            to_bytes(|w| set_scroll_region(w, 0, 23)),
            to_bytes(reset_scroll_region),
            to_bytes(sync_begin),
            to_bytes(sync_end),
            to_bytes(|w| hyperlink_start(w, "test")),
            to_bytes(hyperlink_end),
            to_bytes(alt_screen_enter),
            to_bytes(alt_screen_leave),
            to_bytes(bracketed_paste_enable),
            to_bytes(bracketed_paste_disable),
            to_bytes(mouse_enable),
            to_bytes(mouse_disable),
            to_bytes(focus_enable),
            to_bytes(focus_disable),
        ];
        for (i, seq) in sequences.iter().enumerate() {
            assert!(
                seq.starts_with(b"\x1b"),
                "sequence {i} should start with ESC, got {seq:?}"
            );
        }
    }
}
