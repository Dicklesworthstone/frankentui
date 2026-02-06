//! Integration tests for the embedded terminal widget.
//!
//! Tests the full pipeline: PTY -> Parser -> State -> Widget -> Frame
//!
//! # Test Categories
//!
//! 1. **PTY Management**: Spawn/kill shell, environment inheritance
//! 2. **ANSI Rendering**: Colors, cursor, scrollback, wrapping
//! 3. **Input Forwarding**: Key sequences, modifiers, bracketed paste
//! 4. **Widget Rendering**: StatefulWidget render, cell conversion

#![cfg(all(feature = "terminal-widget", unix))]

use std::time::Duration;

use ftui_core::geometry::Rect;
use ftui_extras::terminal::{
    AnsiHandler, AnsiParser, CellAttrs, ClearRegion, TerminalEmulator, TerminalEmulatorState,
    TerminalModes, TerminalState,
};
use ftui_pty::input_forwarding::{BracketedPaste, Key, KeyEvent, Modifiers, key_to_sequence};
use ftui_pty::{PtyConfig, spawn_command};
use ftui_render::cell::StyleFlags;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_style::Color;
use ftui_widgets::StatefulWidget;
use portable_pty::CommandBuilder;

// ============================================================================
// PTY Management Tests
// ============================================================================

#[test]
fn pty_spawn_shell_success() {
    let config = PtyConfig::default()
        .with_size(80, 24)
        .with_test_name("pty_spawn_shell")
        .logging(false);

    let mut cmd = CommandBuilder::new("sh");
    cmd.args(["-c", "echo 'hello from shell'"]);

    let mut session = spawn_command(config, cmd).expect("spawn should succeed");
    let status = session.wait().expect("wait should succeed");

    assert!(status.success(), "shell should exit successfully");

    let output = session
        .read_until(b"hello from shell", Duration::from_secs(2))
        .expect("should capture output");
    assert!(
        output
            .windows(b"hello from shell".len())
            .any(|w| w == b"hello from shell"),
        "output should contain expected text"
    );
}

#[test]
fn pty_environment_inheritance() {
    let config = PtyConfig::default()
        .with_size(80, 24)
        .with_env("FTUI_TEST_VAR", "test_value_12345")
        .logging(false);

    let mut cmd = CommandBuilder::new("sh");
    cmd.args(["-c", "echo $FTUI_TEST_VAR"]);

    let mut session = spawn_command(config, cmd).expect("spawn should succeed");
    let _ = session.wait().expect("wait should succeed");

    let output = session
        .read_until(b"test_value_12345", Duration::from_secs(2))
        .expect("should capture env output");
    assert!(
        output
            .windows(b"test_value_12345".len())
            .any(|w| w == b"test_value_12345"),
        "environment variable should be inherited"
    );
}

#[test]
fn pty_term_variable_set() {
    let config = PtyConfig::default()
        .with_size(80, 24)
        .with_term("xterm-256color")
        .logging(false);

    let mut cmd = CommandBuilder::new("sh");
    cmd.args(["-c", "echo $TERM"]);

    let mut session = spawn_command(config, cmd).expect("spawn should succeed");
    let _ = session.wait().expect("wait should succeed");

    let output = session
        .read_until(b"xterm-256color", Duration::from_secs(2))
        .expect("should capture TERM output");
    assert!(
        output
            .windows(b"xterm-256color".len())
            .any(|w| w == b"xterm-256color"),
        "TERM should be set correctly"
    );
}

// ============================================================================
// ANSI Parser Integration Tests
// ============================================================================

struct TestHandler {
    state: TerminalState,
}

impl TestHandler {
    fn new(width: u16, height: u16) -> Self {
        Self {
            state: TerminalState::new(width, height),
        }
    }
}

impl AnsiHandler for TestHandler {
    fn print(&mut self, ch: char) {
        self.state.put_char(ch);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x08 => self.state.move_cursor_relative(-1, 0), // BS
            0x09 => {
                // Tab: move to next 8-col stop
                let x = self.state.cursor().x;
                let next = ((x / 8) + 1) * 8;
                self.state.move_cursor(next, self.state.cursor().y);
            }
            0x0A..=0x0C => {
                // LF, VT, FF
                let cursor = self.state.cursor();
                if cursor.y + 1 >= self.state.height() {
                    self.state.scroll_up(1);
                } else {
                    self.state.move_cursor_relative(0, 1);
                }
            }
            0x0D => {
                // CR
                self.state.move_cursor(0, self.state.cursor().y);
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &[i64], intermediates: &[u8], action: char) {
        match (action, intermediates) {
            ('H', []) | ('f', []) => {
                let row = params.first().copied().unwrap_or(1).max(1) as u16;
                let col = params.get(1).copied().unwrap_or(1).max(1) as u16;
                self.state
                    .move_cursor(col.saturating_sub(1), row.saturating_sub(1));
            }
            ('A', []) => {
                let n = params.first().copied().unwrap_or(1).max(1) as i16;
                self.state.move_cursor_relative(0, -n);
            }
            ('B', []) => {
                let n = params.first().copied().unwrap_or(1).max(1) as i16;
                self.state.move_cursor_relative(0, n);
            }
            ('C', []) => {
                let n = params.first().copied().unwrap_or(1).max(1) as i16;
                self.state.move_cursor_relative(n, 0);
            }
            ('D', []) => {
                let n = params.first().copied().unwrap_or(1).max(1) as i16;
                self.state.move_cursor_relative(-n, 0);
            }
            ('J', []) => {
                let mode = params.first().copied().unwrap_or(0);
                match mode {
                    0 => self.state.clear_region(ClearRegion::CursorToEnd),
                    1 => self.state.clear_region(ClearRegion::StartToCursor),
                    2 | 3 => self.state.clear_region(ClearRegion::All),
                    _ => {}
                }
            }
            ('K', []) => {
                let mode = params.first().copied().unwrap_or(0);
                match mode {
                    0 => self.state.clear_region(ClearRegion::LineFromCursor),
                    1 => self.state.clear_region(ClearRegion::LineToCursor),
                    2 => self.state.clear_region(ClearRegion::Line),
                    _ => {}
                }
            }
            ('m', []) => {
                let mut iter = params.iter().copied().peekable();
                if params.is_empty() {
                    self.state.pen_mut().reset();
                    return;
                }
                while let Some(code) = iter.next() {
                    match code {
                        0 => self.state.pen_mut().reset(),
                        1 => {
                            self.state.pen_mut().attrs =
                                self.state.pen_mut().attrs.with(CellAttrs::BOLD)
                        }
                        2 => {
                            self.state.pen_mut().attrs =
                                self.state.pen_mut().attrs.with(CellAttrs::DIM)
                        }
                        3 => {
                            self.state.pen_mut().attrs =
                                self.state.pen_mut().attrs.with(CellAttrs::ITALIC)
                        }
                        4 => {
                            self.state.pen_mut().attrs =
                                self.state.pen_mut().attrs.with(CellAttrs::UNDERLINE)
                        }
                        5 => {
                            self.state.pen_mut().attrs =
                                self.state.pen_mut().attrs.with(CellAttrs::BLINK)
                        }
                        7 => {
                            self.state.pen_mut().attrs =
                                self.state.pen_mut().attrs.with(CellAttrs::REVERSE)
                        }
                        8 => {
                            self.state.pen_mut().attrs =
                                self.state.pen_mut().attrs.with(CellAttrs::HIDDEN)
                        }
                        9 => {
                            self.state.pen_mut().attrs =
                                self.state.pen_mut().attrs.with(CellAttrs::STRIKETHROUGH)
                        }
                        22 => {
                            self.state.pen_mut().attrs = self
                                .state
                                .pen_mut()
                                .attrs
                                .without(CellAttrs::BOLD)
                                .without(CellAttrs::DIM);
                        }
                        23 => {
                            self.state.pen_mut().attrs =
                                self.state.pen_mut().attrs.without(CellAttrs::ITALIC)
                        }
                        24 => {
                            self.state.pen_mut().attrs =
                                self.state.pen_mut().attrs.without(CellAttrs::UNDERLINE)
                        }
                        25 => {
                            self.state.pen_mut().attrs =
                                self.state.pen_mut().attrs.without(CellAttrs::BLINK)
                        }
                        27 => {
                            self.state.pen_mut().attrs =
                                self.state.pen_mut().attrs.without(CellAttrs::REVERSE)
                        }
                        28 => {
                            self.state.pen_mut().attrs =
                                self.state.pen_mut().attrs.without(CellAttrs::HIDDEN)
                        }
                        29 => {
                            self.state.pen_mut().attrs =
                                self.state.pen_mut().attrs.without(CellAttrs::STRIKETHROUGH)
                        }
                        30..=37 => {
                            self.state.pen_mut().fg = Some(ansi_color_to_color(code - 30));
                        }
                        38 => {
                            if let Some(color) = parse_extended_color(&mut iter) {
                                self.state.pen_mut().fg = Some(color);
                            }
                        }
                        39 => self.state.pen_mut().fg = None,
                        40..=47 => {
                            self.state.pen_mut().bg = Some(ansi_color_to_color(code - 40));
                        }
                        48 => {
                            if let Some(color) = parse_extended_color(&mut iter) {
                                self.state.pen_mut().bg = Some(color);
                            }
                        }
                        49 => self.state.pen_mut().bg = None,
                        90..=97 => {
                            self.state.pen_mut().fg = Some(ansi_bright_color(code - 90));
                        }
                        100..=107 => {
                            self.state.pen_mut().bg = Some(ansi_bright_color(code - 100));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, _params: &[&[u8]]) {
        // OSC sequences not needed for these tests
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], _c: char) {
        // ESC sequences not needed for these tests
    }
}

fn ansi_color_to_color(index: i64) -> Color {
    match index {
        0 => Color::rgb(0, 0, 0),       // Black
        1 => Color::rgb(187, 0, 0),     // Red
        2 => Color::rgb(0, 187, 0),     // Green
        3 => Color::rgb(187, 187, 0),   // Yellow
        4 => Color::rgb(0, 0, 187),     // Blue
        5 => Color::rgb(187, 0, 187),   // Magenta
        6 => Color::rgb(0, 187, 187),   // Cyan
        7 => Color::rgb(187, 187, 187), // White
        _ => Color::rgb(187, 187, 187),
    }
}

fn ansi_bright_color(index: i64) -> Color {
    match index {
        0 => Color::rgb(85, 85, 85),    // Bright Black
        1 => Color::rgb(255, 85, 85),   // Bright Red
        2 => Color::rgb(85, 255, 85),   // Bright Green
        3 => Color::rgb(255, 255, 85),  // Bright Yellow
        4 => Color::rgb(85, 85, 255),   // Bright Blue
        5 => Color::rgb(255, 85, 255),  // Bright Magenta
        6 => Color::rgb(85, 255, 255),  // Bright Cyan
        7 => Color::rgb(255, 255, 255), // Bright White
        _ => Color::rgb(255, 255, 255),
    }
}

fn parse_extended_color(
    iter: &mut std::iter::Peekable<impl Iterator<Item = i64>>,
) -> Option<Color> {
    match iter.next() {
        Some(5) => {
            // 256-color mode
            let idx = iter.next()? as u8;
            Some(color_from_256(idx))
        }
        Some(2) => {
            // RGB mode
            let r = iter.next()? as u8;
            let g = iter.next()? as u8;
            let b = iter.next()? as u8;
            Some(Color::rgb(r, g, b))
        }
        _ => None,
    }
}

fn color_from_256(idx: u8) -> Color {
    match idx {
        0..=7 => ansi_color_to_color(idx as i64),
        8..=15 => ansi_bright_color((idx - 8) as i64),
        16..=231 => {
            // 6x6x6 color cube
            let idx = idx - 16;
            let r = (idx / 36) % 6;
            let g = (idx / 6) % 6;
            let b = idx % 6;
            let to_rgb = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Color::rgb(to_rgb(r), to_rgb(g), to_rgb(b))
        }
        232..=255 => {
            // Grayscale
            let gray = 8 + (idx - 232) * 10;
            Color::rgb(gray, gray, gray)
        }
    }
}

#[test]
fn parser_to_state_basic_text() {
    let mut handler = TestHandler::new(80, 24);
    let mut parser = AnsiParser::new();

    parser.parse(b"Hello, World!", &mut handler);

    // Check that text was written
    for (i, ch) in "Hello, World!".chars().enumerate() {
        let cell = handler.state.cell(i as u16, 0).expect("cell should exist");
        assert_eq!(cell.ch, ch, "character at position {}", i);
    }
}

#[test]
fn parser_to_state_cursor_movement() {
    let mut handler = TestHandler::new(80, 24);
    let mut parser = AnsiParser::new();

    // Move cursor to row 5, column 10 (1-indexed in ANSI)
    parser.parse(b"\x1b[5;10H", &mut handler);

    let cursor = handler.state.cursor();
    assert_eq!(cursor.x, 9, "cursor x should be 9 (0-indexed)");
    assert_eq!(cursor.y, 4, "cursor y should be 4 (0-indexed)");
}

#[test]
fn parser_to_state_sgr_colors() {
    let mut handler = TestHandler::new(80, 24);
    let mut parser = AnsiParser::new();

    // Set red foreground, green background
    parser.parse(b"\x1b[31;42mX", &mut handler);

    let cell = handler.state.cell(0, 0).expect("cell should exist");
    assert_eq!(cell.ch, 'X');
    assert!(cell.fg.is_some(), "foreground should be set");
    assert!(cell.bg.is_some(), "background should be set");
}

#[test]
fn parser_to_state_256_colors() {
    let mut handler = TestHandler::new(80, 24);
    let mut parser = AnsiParser::new();

    // Set 256-color foreground (color 196 = bright red)
    parser.parse(b"\x1b[38;5;196mR", &mut handler);

    let cell = handler.state.cell(0, 0).expect("cell should exist");
    assert_eq!(cell.ch, 'R');
    assert!(cell.fg.is_some(), "256-color foreground should be set");
}

#[test]
fn parser_to_state_rgb_colors() {
    let mut handler = TestHandler::new(80, 24);
    let mut parser = AnsiParser::new();

    // Set RGB foreground (100, 150, 200)
    parser.parse(b"\x1b[38;2;100;150;200mB", &mut handler);

    let cell = handler.state.cell(0, 0).expect("cell should exist");
    assert_eq!(cell.ch, 'B');

    let fg = cell.fg.expect("RGB foreground should be set");
    let rgb = fg.to_rgb();
    assert_eq!(rgb.r, 100);
    assert_eq!(rgb.g, 150);
    assert_eq!(rgb.b, 200);
}

#[test]
fn parser_to_state_text_attributes() {
    let mut handler = TestHandler::new(80, 24);
    let mut parser = AnsiParser::new();

    // Bold + Italic
    parser.parse(b"\x1b[1;3mB", &mut handler);

    let cell = handler.state.cell(0, 0).expect("cell should exist");
    assert_eq!(cell.ch, 'B');
    assert!(cell.attrs.contains(CellAttrs::BOLD), "should be bold");
    assert!(cell.attrs.contains(CellAttrs::ITALIC), "should be italic");
}

#[test]
fn parser_to_state_clear_screen() {
    let mut handler = TestHandler::new(80, 24);
    let mut parser = AnsiParser::new();

    // Write some text
    parser.parse(b"Hello", &mut handler);

    // Clear screen
    parser.parse(b"\x1b[2J", &mut handler);

    // All cells should be empty
    for x in 0..5 {
        let cell = handler.state.cell(x, 0).expect("cell should exist");
        assert_eq!(cell.ch, ' ', "cell at {} should be cleared", x);
    }
}

#[test]
fn parser_to_state_line_wrapping() {
    let mut handler = TestHandler::new(10, 5);
    let mut parser = AnsiParser::new();

    // Enable wrap mode
    handler.state.set_mode(TerminalModes::WRAP, true);

    // Write more than one line's worth
    parser.parse(b"1234567890ABC", &mut handler);

    // First 10 chars on line 0
    for (i, ch) in "1234567890".chars().enumerate() {
        let cell = handler.state.cell(i as u16, 0).expect("cell should exist");
        assert_eq!(cell.ch, ch, "line 0 position {}", i);
    }

    // Next chars on line 1
    for (i, ch) in "ABC".chars().enumerate() {
        let cell = handler.state.cell(i as u16, 1).expect("cell should exist");
        assert_eq!(cell.ch, ch, "line 1 position {}", i);
    }
}

// ============================================================================
// Input Forwarding Tests
// ============================================================================

#[test]
fn input_forward_simple_key() {
    let event = KeyEvent::plain(Key::Char('a'));
    let seq = key_to_sequence(event);
    assert_eq!(seq, b"a");
}

#[test]
fn input_forward_ctrl_c() {
    let event = KeyEvent::new(Key::Char('c'), Modifiers::CTRL);
    let seq = key_to_sequence(event);
    assert_eq!(seq, &[0x03]); // ETX
}

#[test]
fn input_forward_arrow_keys() {
    assert_eq!(key_to_sequence(KeyEvent::plain(Key::Up)), b"\x1b[A");
    assert_eq!(key_to_sequence(KeyEvent::plain(Key::Down)), b"\x1b[B");
    assert_eq!(key_to_sequence(KeyEvent::plain(Key::Right)), b"\x1b[C");
    assert_eq!(key_to_sequence(KeyEvent::plain(Key::Left)), b"\x1b[D");
}

#[test]
fn input_forward_function_keys() {
    assert_eq!(key_to_sequence(KeyEvent::plain(Key::F(1))), b"\x1bOP");
    assert_eq!(key_to_sequence(KeyEvent::plain(Key::F(5))), b"\x1b[15~");
    assert_eq!(key_to_sequence(KeyEvent::plain(Key::F(12))), b"\x1b[24~");
}

#[test]
fn input_forward_enter_and_tab() {
    assert_eq!(key_to_sequence(KeyEvent::plain(Key::Enter)), b"\r");
    assert_eq!(key_to_sequence(KeyEvent::plain(Key::Tab)), b"\t");
}

#[test]
fn input_forward_alt_modifier() {
    let event = KeyEvent::new(Key::Char('x'), Modifiers::ALT);
    let seq = key_to_sequence(event);
    assert_eq!(seq, b"\x1bx"); // ESC + x
}

#[test]
fn input_forward_bracketed_paste() {
    let mut paste = BracketedPaste::new();
    paste.enable();
    let seq = paste.wrap(b"Hello, World!");

    assert!(seq.starts_with(b"\x1b[200~"));
    assert!(seq.ends_with(b"\x1b[201~"));
    assert!(
        seq.windows(b"Hello, World!".len())
            .any(|w| w == b"Hello, World!")
    );
}

// ============================================================================
// Widget Rendering Tests
// ============================================================================

fn create_test_frame(width: u16, height: u16) -> (Frame<'static>, Box<GraphemePool>) {
    // SAFETY: We leak the pool to get a 'static lifetime for testing.
    // This is acceptable in tests where memory is cleaned up at process exit.
    let pool = Box::new(GraphemePool::default());
    let pool_ref: &'static mut GraphemePool = Box::leak(pool);
    let frame = Frame::new(width, height, pool_ref);
    (frame, Box::new(GraphemePool::default()))
}

#[test]
fn widget_renders_text() {
    let mut state = TerminalEmulatorState::new(80, 24);

    // Write some text to the terminal state
    for (i, ch) in "Hello!".chars().enumerate() {
        state.terminal.move_cursor(i as u16, 0);
        state.terminal.put_char(ch);
    }

    let widget = TerminalEmulator::new();
    let area = Rect::new(0, 0, 80, 24);
    let (mut frame, _pool) = create_test_frame(80, 24);

    StatefulWidget::render(&widget, area, &mut frame, &mut state);

    // Verify the buffer contains our text
    for (i, expected) in "Hello!".chars().enumerate() {
        let cell = frame.buffer.get(i as u16, 0).expect("cell should exist");
        assert_eq!(cell.content.as_char(), Some(expected), "position {}", i);
    }
}

#[test]
fn widget_renders_colors() {
    let mut state = TerminalEmulatorState::new(80, 24);

    // Set pen color and write
    state.terminal.pen_mut().fg = Some(Color::rgb(255, 0, 0));
    state.terminal.pen_mut().bg = Some(Color::rgb(0, 255, 0));
    state.terminal.move_cursor(0, 0);
    state.terminal.put_char('R');

    let widget = TerminalEmulator::new();
    let area = Rect::new(0, 0, 80, 24);
    let (mut frame, _pool) = create_test_frame(80, 24);

    StatefulWidget::render(&widget, area, &mut frame, &mut state);

    let cell = frame.buffer.get(0, 0).expect("cell should exist");
    assert_eq!(cell.content.as_char(), Some('R'));
    // Check that colors were applied (non-transparent)
    assert_ne!(cell.fg.0, 0, "foreground should be set");
    assert_ne!(cell.bg.0, 0, "background should be set");
}

#[test]
fn widget_renders_cursor() {
    let mut state = TerminalEmulatorState::new(80, 24);
    state.terminal.move_cursor(5, 3);

    // The cursor cell should get REVERSE style
    state.terminal.set_cursor_visible(true);

    let widget = TerminalEmulator::new().show_cursor(true).cursor_phase(true);
    let area = Rect::new(0, 0, 80, 24);
    let (mut frame, _pool) = create_test_frame(80, 24);

    StatefulWidget::render(&widget, area, &mut frame, &mut state);

    let cursor_cell = frame.buffer.get(5, 3).expect("cursor cell should exist");
    assert!(
        cursor_cell.attrs.flags().contains(StyleFlags::REVERSE),
        "cursor should have REVERSE style"
    );
}

#[test]
fn widget_cursor_hidden_when_disabled() {
    let mut state = TerminalEmulatorState::new(80, 24);
    state.terminal.move_cursor(5, 3);
    state.terminal.set_cursor_visible(true);

    // Disable cursor rendering
    let widget = TerminalEmulator::new().show_cursor(false);
    let area = Rect::new(0, 0, 80, 24);
    let (mut frame, _pool) = create_test_frame(80, 24);

    StatefulWidget::render(&widget, area, &mut frame, &mut state);

    let cursor_cell = frame.buffer.get(5, 3).expect("cursor cell should exist");
    assert!(
        !cursor_cell.attrs.flags().contains(StyleFlags::REVERSE),
        "cursor should NOT have REVERSE style when disabled"
    );
}

#[test]
fn widget_scroll_offset() {
    let mut state = TerminalEmulatorState::with_scrollback(10, 5, 100);

    // Add content to scrollback by scrolling
    for _ in 0..3 {
        state.terminal.scroll_up(1);
    }

    // Scroll the view up
    state.scroll_up(2);
    assert_eq!(state.scroll_offset, 2);

    // Scroll back down
    state.scroll_down(1);
    assert_eq!(state.scroll_offset, 1);

    // Reset
    state.reset_scroll();
    assert_eq!(state.scroll_offset, 0);
}

#[test]
fn widget_resize() {
    let mut state = TerminalEmulatorState::new(80, 24);

    state.resize(120, 40);

    assert_eq!(state.terminal.width(), 120);
    assert_eq!(state.terminal.height(), 40);
}

// ============================================================================
// Full Pipeline Integration Tests
// ============================================================================

#[test]
fn full_pipeline_pty_to_widget() {
    // Spawn a simple command
    let config = PtyConfig::default().with_size(40, 10).logging(false);

    let mut cmd = CommandBuilder::new("sh");
    cmd.args(["-c", "printf 'TESTOUTPUT'"]);

    let mut session = spawn_command(config, cmd).expect("spawn should succeed");
    let _ = session.wait().expect("wait should succeed");

    let output = session
        .read_until(b"TESTOUTPUT", Duration::from_secs(2))
        .expect("should capture output");

    // Parse through ANSI parser into terminal state
    let mut handler = TestHandler::new(40, 10);
    let mut parser = AnsiParser::new();
    parser.parse(&output, &mut handler);

    // Create widget and render
    let mut state = TerminalEmulatorState::new(40, 10);
    state.terminal = handler.state;

    let widget = TerminalEmulator::new();
    let area = Rect::new(0, 0, 40, 10);
    let (mut frame, _pool) = create_test_frame(40, 10);

    StatefulWidget::render(&widget, area, &mut frame, &mut state);

    // Verify output appears in rendered buffer (somewhere)
    let mut found = false;
    for y in 0..10 {
        let mut line = String::new();
        for x in 0..40 {
            if let Some(cell) = frame.buffer.get(x, y)
                && let Some(c) = cell.content.as_char()
            {
                line.push(c);
            }
        }
        if line.contains("TESTOUTPUT") {
            found = true;
            break;
        }
    }
    assert!(found, "TESTOUTPUT should appear in rendered frame");
}

#[test]
fn input_forwarding_roundtrip() {
    let config = PtyConfig::default().with_size(80, 24).logging(false);

    // Use a command that terminates after consuming a single line so we don't rely on tty
    // line discipline or EOF semantics to end the process (important for CI/coverage stability).
    let mut cmd = CommandBuilder::new("sh");
    cmd.args(["-c", "IFS= read -r line; printf '%s' \"$line\""]);

    let mut session = spawn_command(config, cmd).expect("spawn should succeed");

    // Send input using the input forwarding module: 'X' + Enter.
    session
        .send_input(&key_to_sequence(KeyEvent::plain(Key::Char('X'))))
        .expect("send should succeed");
    session
        .send_input(&key_to_sequence(KeyEvent::plain(Key::Enter)))
        .expect("send should succeed");

    // Wait for the echoed character.
    let output = session
        .read_until(b"X", Duration::from_secs(2))
        .expect("should receive echo");

    let status = session.wait().expect("wait should succeed");
    assert!(status.success(), "child should exit successfully");

    assert!(
        output.windows(1).any(|w| w == b"X"),
        "output should contain echoed character"
    );
}

// ============================================================================
// Edge Cases and Error Handling
// ============================================================================

#[test]
fn empty_area_render_does_not_panic() {
    let mut state = TerminalEmulatorState::new(80, 24);
    let widget = TerminalEmulator::new();

    // Empty area (width=0)
    let area = Rect::new(0, 0, 0, 24);
    let (mut frame, _pool) = create_test_frame(80, 24);
    StatefulWidget::render(&widget, area, &mut frame, &mut state);

    // Empty area (height=0)
    let area = Rect::new(0, 0, 80, 0);
    StatefulWidget::render(&widget, area, &mut frame, &mut state);
}

#[test]
fn terminal_smaller_than_area() {
    // Terminal is 40x10 but we render in 80x24 area
    let mut state = TerminalEmulatorState::new(40, 10);
    state.terminal.move_cursor(0, 0);
    state.terminal.put_char('X');

    let widget = TerminalEmulator::new();
    let area = Rect::new(0, 0, 80, 24);
    let (mut frame, _pool) = create_test_frame(80, 24);

    StatefulWidget::render(&widget, area, &mut frame, &mut state);

    // Content should appear at (0,0)
    let cell = frame.buffer.get(0, 0).expect("cell should exist");
    assert_eq!(cell.content.as_char(), Some('X'));
}

#[test]
fn area_offset_respected() {
    let mut state = TerminalEmulatorState::new(10, 5);
    state.terminal.move_cursor(0, 0);
    state.terminal.put_char('Z');

    let widget = TerminalEmulator::new();
    // Render at offset (5, 3)
    let area = Rect::new(5, 3, 10, 5);
    let (mut frame, _pool) = create_test_frame(80, 24);

    StatefulWidget::render(&widget, area, &mut frame, &mut state);

    // Content should appear at (5,3)
    let cell = frame.buffer.get(5, 3).expect("cell should exist");
    assert_eq!(cell.content.as_char(), Some('Z'));
}
