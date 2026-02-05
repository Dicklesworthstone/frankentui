#![forbid(unsafe_code)]
//! Snapshot tests for text effects visual regression (bd-3cuk).
//!
//! Run with: cargo test -p ftui-extras --test text_effects_snapshots
//! Update snapshots: BLESS=1 cargo test -p ftui-extras --test text_effects_snapshots

#[cfg(feature = "text-effects")]
use ftui_core::geometry::Rect;
#[cfg(feature = "text-effects")]
use ftui_render::buffer::Buffer;
#[cfg(feature = "text-effects")]
use ftui_render::cell::PackedRgba;
#[cfg(feature = "text-effects")]
use ftui_render::cell::StyleFlags;
#[cfg(feature = "text-effects")]
use ftui_render::frame::Frame;
#[cfg(feature = "text-effects")]
use ftui_render::grapheme_pool::GraphemePool;
#[cfg(feature = "text-effects")]
use ftui_widgets::Widget;
#[cfg(feature = "text-effects")]
use std::fmt::Write as FmtWrite;
#[cfg(feature = "text-effects")]
use std::path::Path;

// Import the text effects module
// Note: Requires the text-effects feature to be enabled
#[cfg(feature = "text-effects")]
use ftui_extras::text_effects::{
    AsciiArtStyle, AsciiArtText, ColorGradient, Direction, StyledText, TextEffect,
};

#[cfg(feature = "text-effects")]
fn buffer_to_ansi(buf: &Buffer) -> String {
    let capacity = (buf.width() as usize + 32) * buf.height() as usize;
    let mut out = String::with_capacity(capacity);

    for y in 0..buf.height() {
        if y > 0 {
            out.push('\n');
        }

        let mut prev_fg = PackedRgba::WHITE;
        let mut prev_bg = PackedRgba::TRANSPARENT;
        let mut prev_flags = StyleFlags::empty();
        let mut style_active = false;

        for x in 0..buf.width() {
            let cell = buf.get(x, y).unwrap();
            if cell.is_continuation() {
                continue;
            }

            let fg = cell.fg;
            let bg = cell.bg;
            let flags = cell.attrs.flags();
            let style_changed = fg != prev_fg || bg != prev_bg || flags != prev_flags;

            if style_changed {
                let has_style =
                    fg != PackedRgba::WHITE || bg != PackedRgba::TRANSPARENT || !flags.is_empty();

                if has_style {
                    if style_active {
                        out.push_str("\x1b[0m");
                    }

                    let mut params: Vec<String> = Vec::new();
                    if !flags.is_empty() {
                        if flags.contains(StyleFlags::BOLD) {
                            params.push("1".to_string());
                        }
                        if flags.contains(StyleFlags::DIM) {
                            params.push("2".to_string());
                        }
                        if flags.contains(StyleFlags::ITALIC) {
                            params.push("3".to_string());
                        }
                        if flags.contains(StyleFlags::UNDERLINE) {
                            params.push("4".to_string());
                        }
                        if flags.contains(StyleFlags::BLINK) {
                            params.push("5".to_string());
                        }
                        if flags.contains(StyleFlags::REVERSE) {
                            params.push("7".to_string());
                        }
                        if flags.contains(StyleFlags::HIDDEN) {
                            params.push("8".to_string());
                        }
                        if flags.contains(StyleFlags::STRIKETHROUGH) {
                            params.push("9".to_string());
                        }
                    }

                    if fg.a() > 0 && fg != PackedRgba::WHITE {
                        params.push(format!("38;2;{};{};{}", fg.r(), fg.g(), fg.b()));
                    }
                    if bg.a() > 0 && bg != PackedRgba::TRANSPARENT {
                        params.push(format!("48;2;{};{};{}", bg.r(), bg.g(), bg.b()));
                    }

                    if params.is_empty() {
                        out.push_str("\x1b[0m");
                    } else {
                        out.push_str("\x1b[");
                        out.push_str(&params.join(";"));
                        out.push('m');
                    }
                    style_active = true;
                } else if style_active {
                    out.push_str("\x1b[0m");
                    style_active = false;
                }

                prev_fg = fg;
                prev_bg = bg;
                prev_flags = flags;
            }

            if let Some(ch) = cell.content.as_char() {
                out.push(ch);
            } else {
                out.push('?');
            }
        }

        if style_active {
            out.push_str("\x1b[0m");
        }
    }

    out
}

#[cfg(feature = "text-effects")]
fn diff_text(expected: &str, actual: &str) -> String {
    let expected_lines: Vec<&str> = expected.lines().collect();
    let actual_lines: Vec<&str> = actual.lines().collect();

    let max_lines = expected_lines.len().max(actual_lines.len());
    let mut out = String::new();
    let mut has_diff = false;

    for i in 0..max_lines {
        let exp = expected_lines.get(i).copied();
        let act = actual_lines.get(i).copied();

        match (exp, act) {
            (Some(e), Some(a)) if e == a => {
                writeln!(out, " {e}").unwrap();
            }
            (Some(e), Some(a)) => {
                writeln!(out, "-{e}").unwrap();
                writeln!(out, "+{a}").unwrap();
                has_diff = true;
            }
            (Some(e), None) => {
                writeln!(out, "-{e}").unwrap();
                has_diff = true;
            }
            (None, Some(a)) => {
                writeln!(out, "+{a}").unwrap();
                has_diff = true;
            }
            (None, None) => {}
        }
    }

    if has_diff { out } else { String::new() }
}

#[cfg(feature = "text-effects")]
fn is_bless() -> bool {
    std::env::var("BLESS").is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

#[cfg(feature = "text-effects")]
fn assert_buffer_snapshot_ansi(name: &str, buf: &Buffer) {
    let base = Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = base
        .join("tests")
        .join("snapshots")
        .join(format!("{name}.ansi.snap"));
    let actual = buffer_to_ansi(buf);

    if is_bless() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("failed to create snapshot directory");
        }
        std::fs::write(&path, &actual).expect("failed to write snapshot");
        return;
    }

    match std::fs::read_to_string(&path) {
        Ok(expected) => {
            if expected != actual {
                let diff = diff_text(&expected, &actual);
                std::panic::panic_any(format!(
                    "=== ANSI snapshot mismatch: '{name}' ===\nFile: {}\nSet BLESS=1 to update.\n\nDiff (- expected, + actual):\n{diff}",
                    path.display()
                ));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::panic::panic_any(format!(
                "=== No ANSI snapshot found: '{name}' ===\nExpected at: {}\nRun with BLESS=1 to create it.\n\nActual output:\n{actual}",
                path.display()
            ));
        }
        Err(e) => {
            std::panic::panic_any(format!("Failed to read snapshot '{}': {e}", path.display()));
        }
    }
}

#[cfg(feature = "text-effects")]
macro_rules! assert_snapshot_ansi {
    ($name:expr, $buf:expr) => {
        assert_buffer_snapshot_ansi($name, $buf)
    };
}

// =============================================================================
// Gradient Snapshots
// =============================================================================

#[test]
#[cfg(feature = "text-effects")]
fn snapshot_rainbow_gradient() {
    let text = StyledText::new("RAINBOW GRADIENT TEST")
        .effect(TextEffect::RainbowGradient { speed: 0.0 })
        .time(0.0);

    let area = Rect::new(0, 0, 30, 1);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(30, 1, &mut pool);
    text.render(area, &mut frame);
    assert_snapshot_ansi!("text_effects_rainbow_gradient", &frame.buffer);
}

#[test]
#[cfg(feature = "text-effects")]
fn snapshot_horizontal_gradient() {
    let gradient = ColorGradient::sunset();
    let text = StyledText::new("SUNSET GRADIENT")
        .effect(TextEffect::HorizontalGradient { gradient })
        .time(0.0);

    let area = Rect::new(0, 0, 20, 1);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(20, 1, &mut pool);
    text.render(area, &mut frame);
    assert_snapshot_ansi!("text_effects_horizontal_gradient", &frame.buffer);
}

#[test]
#[cfg(feature = "text-effects")]
fn snapshot_animated_gradient_frame_0() {
    let gradient = ColorGradient::cyberpunk();
    let text = StyledText::new("CYBERPUNK")
        .effect(TextEffect::AnimatedGradient {
            gradient,
            speed: 1.0,
        })
        .time(0.0);

    let area = Rect::new(0, 0, 15, 1);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(15, 1, &mut pool);
    text.render(area, &mut frame);
    assert_snapshot_ansi!("text_effects_animated_gradient_f0", &frame.buffer);
}

#[test]
#[cfg(feature = "text-effects")]
fn snapshot_animated_gradient_frame_50() {
    let gradient = ColorGradient::cyberpunk();
    let text = StyledText::new("CYBERPUNK")
        .effect(TextEffect::AnimatedGradient {
            gradient,
            speed: 1.0,
        })
        .time(0.5);

    let area = Rect::new(0, 0, 15, 1);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(15, 1, &mut pool);
    text.render(area, &mut frame);
    assert_snapshot_ansi!("text_effects_animated_gradient_f50", &frame.buffer);
}

// =============================================================================
// Wave Effect Snapshots
// =============================================================================

#[test]
#[cfg(feature = "text-effects")]
fn snapshot_wave_frame_0() {
    let text = StyledText::new("WAVE TEXT")
        .effect(TextEffect::Wave {
            amplitude: 1.0,
            wavelength: 5.0,
            speed: 1.0,
            direction: Direction::Down,
        })
        .time(0.0);

    let area = Rect::new(0, 0, 15, 3);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(15, 3, &mut pool);
    text.render(area, &mut frame);
    assert_snapshot_ansi!("text_effects_wave_f0", &frame.buffer);
}

#[test]
#[cfg(feature = "text-effects")]
fn snapshot_wave_frame_25() {
    let text = StyledText::new("WAVE TEXT")
        .effect(TextEffect::Wave {
            amplitude: 1.0,
            wavelength: 5.0,
            speed: 1.0,
            direction: Direction::Down,
        })
        .time(0.25);

    let area = Rect::new(0, 0, 15, 3);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(15, 3, &mut pool);
    text.render(area, &mut frame);
    assert_snapshot_ansi!("text_effects_wave_f25", &frame.buffer);
}

// =============================================================================
// Glow Effect Snapshots
// =============================================================================

#[test]
#[cfg(feature = "text-effects")]
fn snapshot_glow_static() {
    let text = StyledText::new("GLOW")
        .effect(TextEffect::Glow {
            color: PackedRgba::rgb(0, 255, 255),
            intensity: 0.8,
        })
        .base_color(PackedRgba::rgb(255, 255, 255))
        .time(0.0);

    let area = Rect::new(0, 0, 10, 1);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(10, 1, &mut pool);
    text.render(area, &mut frame);
    assert_snapshot_ansi!("text_effects_glow_static", &frame.buffer);
}

#[test]
#[cfg(feature = "text-effects")]
fn snapshot_pulsing_glow() {
    let text = StyledText::new("PULSE")
        .effect(TextEffect::PulsingGlow {
            color: PackedRgba::rgb(255, 0, 128),
            speed: 2.0,
        })
        .time(0.25);

    let area = Rect::new(0, 0, 10, 1);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(10, 1, &mut pool);
    text.render(area, &mut frame);
    assert_snapshot_ansi!("text_effects_pulsing_glow", &frame.buffer);
}

// =============================================================================
// ASCII Art Snapshots
// =============================================================================

#[test]
#[cfg(feature = "text-effects")]
fn snapshot_ascii_art_block() {
    let art = AsciiArtText::new("HI", AsciiArtStyle::Block);
    let lines = art.render_lines();

    // Create a buffer big enough for the ASCII art
    let height = lines.len() as u16;
    let width = lines.iter().map(|l| l.len()).max().unwrap_or(0) as u16;

    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width.max(10), height.max(5), &mut pool);

    // Render each line
    for (y, line) in lines.iter().enumerate() {
        for (x, ch) in line.chars().enumerate() {
            if x < width as usize && y < height as usize {
                frame
                    .buffer
                    .set_raw(x as u16, y as u16, ftui_render::cell::Cell::from_char(ch));
            }
        }
    }

    assert_snapshot_ansi!("text_effects_ascii_art_block", &frame.buffer);
}

#[test]
#[cfg(feature = "text-effects")]
fn snapshot_ascii_art_banner() {
    let art = AsciiArtText::new("AB", AsciiArtStyle::Banner);
    let lines = art.render_lines();

    let height = lines.len() as u16;
    let width = lines.iter().map(|l| l.len()).max().unwrap_or(0) as u16;

    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width.max(10), height.max(5), &mut pool);

    for (y, line) in lines.iter().enumerate() {
        for (x, ch) in line.chars().enumerate() {
            if x < width as usize && y < height as usize {
                frame
                    .buffer
                    .set_raw(x as u16, y as u16, ftui_render::cell::Cell::from_char(ch));
            }
        }
    }

    assert_snapshot_ansi!("text_effects_ascii_art_banner", &frame.buffer);
}

// =============================================================================
// Fade Effect Snapshots
// =============================================================================

#[test]
#[cfg(feature = "text-effects")]
fn snapshot_fade_in_0() {
    let text = StyledText::new("FADE IN")
        .effect(TextEffect::FadeIn { progress: 0.0 })
        .base_color(PackedRgba::rgb(255, 255, 255))
        .time(0.0);

    let area = Rect::new(0, 0, 10, 1);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(10, 1, &mut pool);
    text.render(area, &mut frame);
    assert_snapshot_ansi!("text_effects_fade_in_0", &frame.buffer);
}

#[test]
#[cfg(feature = "text-effects")]
fn snapshot_fade_in_50() {
    let text = StyledText::new("FADE IN")
        .effect(TextEffect::FadeIn { progress: 0.5 })
        .base_color(PackedRgba::rgb(255, 255, 255))
        .time(0.0);

    let area = Rect::new(0, 0, 10, 1);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(10, 1, &mut pool);
    text.render(area, &mut frame);
    assert_snapshot_ansi!("text_effects_fade_in_50", &frame.buffer);
}

#[test]
#[cfg(feature = "text-effects")]
fn snapshot_fade_in_100() {
    let text = StyledText::new("FADE IN")
        .effect(TextEffect::FadeIn { progress: 1.0 })
        .base_color(PackedRgba::rgb(255, 255, 255))
        .time(0.0);

    let area = Rect::new(0, 0, 10, 1);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(10, 1, &mut pool);
    text.render(area, &mut frame);
    assert_snapshot_ansi!("text_effects_fade_in_100", &frame.buffer);
}

// =============================================================================
// Pulse Effect Snapshots
// =============================================================================

#[test]
#[cfg(feature = "text-effects")]
fn snapshot_pulse_min() {
    let text = StyledText::new("PULSE")
        .effect(TextEffect::Pulse {
            speed: 1.0,
            min_alpha: 0.3,
        })
        .base_color(PackedRgba::rgb(255, 100, 100))
        .time(0.5); // At 0.5s with 1Hz, should be near min

    let area = Rect::new(0, 0, 10, 1);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(10, 1, &mut pool);
    text.render(area, &mut frame);
    assert_snapshot_ansi!("text_effects_pulse_min", &frame.buffer);
}

#[test]
#[cfg(feature = "text-effects")]
fn snapshot_pulse_max() {
    let text = StyledText::new("PULSE")
        .effect(TextEffect::Pulse {
            speed: 1.0,
            min_alpha: 0.3,
        })
        .base_color(PackedRgba::rgb(255, 100, 100))
        .time(0.0); // At 0s, should be at max

    let area = Rect::new(0, 0, 10, 1);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(10, 1, &mut pool);
    text.render(area, &mut frame);
    assert_snapshot_ansi!("text_effects_pulse_max", &frame.buffer);
}

// =============================================================================
// Typewriter Effect Snapshots
// =============================================================================

#[test]
#[cfg(feature = "text-effects")]
fn snapshot_typewriter_partial() {
    let text = StyledText::new("TYPEWRITER")
        .effect(TextEffect::Typewriter { visible_chars: 5.0 })
        .time(0.0);

    let area = Rect::new(0, 0, 15, 1);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(15, 1, &mut pool);
    text.render(area, &mut frame);
    assert_snapshot_ansi!("text_effects_typewriter_partial", &frame.buffer);
}

#[test]
#[cfg(feature = "text-effects")]
fn snapshot_typewriter_complete() {
    let text = StyledText::new("TYPEWRITER")
        .effect(TextEffect::Typewriter {
            visible_chars: 10.0,
        })
        .time(0.0);

    let area = Rect::new(0, 0, 15, 1);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(15, 1, &mut pool);
    text.render(area, &mut frame);
    assert_snapshot_ansi!("text_effects_typewriter_complete", &frame.buffer);
}

// =============================================================================
// Effect Chain Snapshots
// =============================================================================

#[test]
#[cfg(feature = "text-effects")]
fn snapshot_effect_chain() {
    let text = StyledText::new("CHAINED")
        .effect(TextEffect::RainbowGradient { speed: 0.0 })
        .effect(TextEffect::Pulse {
            speed: 1.0,
            min_alpha: 0.5,
        })
        .time(0.0);

    let area = Rect::new(0, 0, 12, 1);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(12, 1, &mut pool);
    text.render(area, &mut frame);
    assert_snapshot_ansi!("text_effects_chain", &frame.buffer);
}

// =============================================================================
// Scramble Effect Snapshots
// =============================================================================

#[test]
#[cfg(feature = "text-effects")]
fn snapshot_scramble_start() {
    let text = StyledText::new("SCRAMBLE")
        .effect(TextEffect::Scramble { progress: 0.0 })
        .seed(42)
        .time(0.0);

    let area = Rect::new(0, 0, 12, 1);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(12, 1, &mut pool);
    text.render(area, &mut frame);
    assert_snapshot_ansi!("text_effects_scramble_start", &frame.buffer);
}

#[test]
#[cfg(feature = "text-effects")]
fn snapshot_scramble_end() {
    let text = StyledText::new("SCRAMBLE")
        .effect(TextEffect::Scramble { progress: 1.0 })
        .seed(42)
        .time(0.0);

    let area = Rect::new(0, 0, 12, 1);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(12, 1, &mut pool);
    text.render(area, &mut frame);
    assert_snapshot_ansi!("text_effects_scramble_end", &frame.buffer);
}

// =============================================================================
// Color Wave Snapshots
// =============================================================================

#[test]
#[cfg(feature = "text-effects")]
fn snapshot_color_wave() {
    let text = StyledText::new("COLOR WAVE")
        .effect(TextEffect::ColorWave {
            color1: PackedRgba::rgb(255, 0, 0),
            color2: PackedRgba::rgb(0, 0, 255),
            speed: 1.0,
            wavelength: 5.0,
        })
        .time(0.0);

    let area = Rect::new(0, 0, 15, 1);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(15, 1, &mut pool);
    text.render(area, &mut frame);
    assert_snapshot_ansi!("text_effects_color_wave", &frame.buffer);
}

// =============================================================================
// Glitch Effect Snapshots
// =============================================================================

#[test]
#[cfg(feature = "text-effects")]
fn snapshot_glitch_low() {
    let text = StyledText::new("GLITCH")
        .effect(TextEffect::Glitch { intensity: 0.2 })
        .seed(42)
        .time(0.0);

    let area = Rect::new(0, 0, 10, 1);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(10, 1, &mut pool);
    text.render(area, &mut frame);
    assert_snapshot_ansi!("text_effects_glitch_low", &frame.buffer);
}

#[test]
#[cfg(feature = "text-effects")]
fn snapshot_glitch_high() {
    let text = StyledText::new("GLITCH")
        .effect(TextEffect::Glitch { intensity: 0.8 })
        .seed(42)
        .time(0.0);

    let area = Rect::new(0, 0, 10, 1);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(10, 1, &mut pool);
    text.render(area, &mut frame);
    assert_snapshot_ansi!("text_effects_glitch_high", &frame.buffer);
}

// =============================================================================
// Preset Gradient Snapshots
// =============================================================================

#[test]
#[cfg(feature = "text-effects")]
fn snapshot_preset_fire() {
    let text = StyledText::new("FIRE GRADIENT")
        .effect(TextEffect::HorizontalGradient {
            gradient: ColorGradient::fire(),
        })
        .time(0.0);

    let area = Rect::new(0, 0, 18, 1);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(18, 1, &mut pool);
    text.render(area, &mut frame);
    assert_snapshot_ansi!("text_effects_preset_fire", &frame.buffer);
}

#[test]
#[cfg(feature = "text-effects")]
fn snapshot_preset_ocean() {
    let text = StyledText::new("OCEAN GRADIENT")
        .effect(TextEffect::HorizontalGradient {
            gradient: ColorGradient::ocean(),
        })
        .time(0.0);

    let area = Rect::new(0, 0, 18, 1);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(18, 1, &mut pool);
    text.render(area, &mut frame);
    assert_snapshot_ansi!("text_effects_preset_ocean", &frame.buffer);
}

#[test]
#[cfg(feature = "text-effects")]
fn snapshot_preset_matrix() {
    let text = StyledText::new("MATRIX GRADIENT")
        .effect(TextEffect::HorizontalGradient {
            gradient: ColorGradient::matrix(),
        })
        .time(0.0);

    let area = Rect::new(0, 0, 20, 1);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(20, 1, &mut pool);
    text.render(area, &mut frame);
    assert_snapshot_ansi!("text_effects_preset_matrix", &frame.buffer);
}
