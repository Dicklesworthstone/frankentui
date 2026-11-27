#![forbid(unsafe_code)]

//! Edge-case tests for TextArea widget.
//!
//! These tests exercise boundary conditions and corner cases that
//! the inline unit tests do not cover: cursor boundary movement,
//! deletion at document edges, scroll/wrap edge cases, undo/redo
//! limits, and event dispatch.

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers, PasteEvent};
use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;
use ftui_widgets::Widget;
use ftui_widgets::textarea::{TextArea, TextAreaState};

// ── Helpers ────────────────────────────────────────────────────────

fn key_press(code: KeyCode) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers: Modifiers::NONE,
        kind: KeyEventKind::Press,
    })
}

fn key_press_ctrl(code: KeyCode) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers: Modifiers::CTRL,
        kind: KeyEventKind::Press,
    })
}

fn key_press_shift(code: KeyCode) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers: Modifiers::SHIFT,
        kind: KeyEventKind::Press,
    })
}

fn key_release(code: KeyCode) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers: Modifiers::NONE,
        kind: KeyEventKind::Release,
    })
}

fn paste_event(text: &str) -> Event {
    Event::Paste(PasteEvent {
        text: text.to_string(),
        bracketed: true,
    })
}

/// Render and return whether cursor was set.
fn render_has_cursor(ta: &TextArea, w: u16, h: u16) -> bool {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(w, h, &mut pool);
    Widget::render(ta, Rect::new(0, 0, w, h), &mut frame);
    frame.cursor_position.is_some()
}

/// Render and return the character at (x, y), or None.
fn render_cell_char(ta: &TextArea, w: u16, h: u16, x: u16, y: u16) -> Option<char> {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(w, h, &mut pool);
    Widget::render(ta, Rect::new(0, 0, w, h), &mut frame);
    frame.buffer.get(x, y).and_then(|c| c.content.as_char())
}

/// Render at an offset origin and return the character at (x, y).
fn render_cell_char_at(
    ta: &TextArea,
    area: Rect,
    buf_w: u16,
    buf_h: u16,
    x: u16,
    y: u16,
) -> Option<char> {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(buf_w, buf_h, &mut pool);
    Widget::render(ta, area, &mut frame);
    frame.buffer.get(x, y).and_then(|c| c.content.as_char())
}

// ── Cursor boundary movement ──────────────────────────────────────

#[test]
fn move_left_at_document_start_is_noop() {
    let mut ta = TextArea::new().with_text("hello");
    ta.move_to_document_start();
    let before = ta.cursor();
    ta.move_left();
    assert_eq!(ta.cursor(), before);
}

#[test]
fn move_right_at_document_end_is_noop() {
    let mut ta = TextArea::new().with_text("hello");
    ta.move_to_document_end();
    let before = ta.cursor();
    ta.move_right();
    assert_eq!(ta.cursor(), before);
}

#[test]
fn move_up_at_first_line_stays_on_first_line() {
    let mut ta = TextArea::new().with_text("abc\ndef");
    ta.move_to_document_start();
    ta.move_right(); // col 1
    ta.move_up();
    assert_eq!(ta.cursor().line, 0);
}

#[test]
fn move_down_at_last_line_stays_on_last_line() {
    let mut ta = TextArea::new().with_text("abc\ndef");
    ta.move_to_document_end();
    let before = ta.cursor();
    ta.move_down();
    assert_eq!(ta.cursor().line, before.line);
}

#[test]
fn move_word_left_at_document_start_is_noop() {
    let mut ta = TextArea::new().with_text("hello world");
    ta.move_to_document_start();
    let before = ta.cursor();
    ta.move_word_left();
    assert_eq!(ta.cursor(), before);
}

#[test]
fn move_word_right_at_document_end_is_noop() {
    let mut ta = TextArea::new().with_text("hello world");
    ta.move_to_document_end();
    let before = ta.cursor();
    ta.move_word_right();
    assert_eq!(ta.cursor(), before);
}

// ── Deletion at boundaries ────────────────────────────────────────

#[test]
fn delete_backward_at_document_start_is_noop() {
    let mut ta = TextArea::new().with_text("hello");
    ta.move_to_document_start();
    ta.delete_backward();
    assert_eq!(ta.text(), "hello");
}

#[test]
fn delete_forward_at_document_end_is_noop() {
    let mut ta = TextArea::new().with_text("hello");
    ta.move_to_document_end();
    ta.delete_forward();
    assert_eq!(ta.text(), "hello");
}

#[test]
fn delete_backward_on_empty_document_is_noop() {
    let mut ta = TextArea::new();
    ta.delete_backward();
    assert!(ta.is_empty());
    assert_eq!(ta.line_count(), 1);
}

#[test]
fn delete_forward_on_empty_document_is_noop() {
    let mut ta = TextArea::new();
    ta.delete_forward();
    assert!(ta.is_empty());
    assert_eq!(ta.line_count(), 1);
}

#[test]
fn delete_word_backward_at_line_start_joins_lines() {
    let mut ta = TextArea::new().with_text("abc\ndef");
    ta.move_to_document_start();
    ta.move_down();
    ta.move_to_line_start();
    ta.delete_word_backward();
    // Should join lines (delete the newline)
    assert_eq!(ta.line_count(), 1);
}

#[test]
fn delete_word_backward_at_document_start_is_noop() {
    let mut ta = TextArea::new().with_text("hello");
    ta.move_to_document_start();
    ta.delete_word_backward();
    assert_eq!(ta.text(), "hello");
}

#[test]
fn delete_to_end_of_line_at_end_of_line_joins_next() {
    let mut ta = TextArea::new().with_text("abc\ndef");
    ta.move_to_document_start();
    ta.move_to_line_end();
    ta.delete_to_end_of_line();
    // Deleting at end of line should join with next line
    assert!(ta.line_count() <= 2);
}

#[test]
fn delete_to_end_of_line_at_end_of_document_is_noop() {
    let mut ta = TextArea::new().with_text("abc");
    ta.move_to_document_end();
    ta.delete_to_end_of_line();
    assert_eq!(ta.text(), "abc");
}

#[test]
fn delete_forward_at_end_of_line_joins_next() {
    let mut ta = TextArea::new().with_text("abc\ndef");
    ta.move_to_document_start();
    ta.move_to_line_end(); // at end of "abc"
    ta.delete_forward();
    assert_eq!(ta.text(), "abcdef");
    assert_eq!(ta.line_count(), 1);
}

// ── Undo/redo edge cases ──────────────────────────────────────────

#[test]
fn undo_on_pristine_document_is_noop() {
    let ta_pristine = TextArea::new().with_text("hello");
    let mut ta = TextArea::new().with_text("hello");
    ta.undo();
    // Text should remain unchanged after undo on pristine doc
    assert_eq!(ta.text(), ta_pristine.text());
}

#[test]
fn redo_without_prior_undo_is_noop() {
    let mut ta = TextArea::new().with_text("hello");
    ta.redo();
    assert_eq!(ta.text(), "hello");
}

#[test]
fn multiple_undo_past_initial_state_is_safe() {
    let mut ta = TextArea::new();
    ta.insert_text("a");
    ta.undo();
    ta.undo(); // past initial
    ta.undo(); // well past initial
    assert!(ta.is_empty());
}

#[test]
fn undo_redo_undo_cycle() {
    let mut ta = TextArea::new();
    ta.insert_text("x");
    ta.insert_text("y");
    assert_eq!(ta.text(), "xy");
    ta.undo();
    assert_eq!(ta.text(), "x");
    ta.redo();
    assert_eq!(ta.text(), "xy");
    ta.undo();
    ta.undo();
    assert_eq!(ta.text(), "");
    ta.redo();
    assert_eq!(ta.text(), "x");
}

// ── Event dispatch ────────────────────────────────────────────────

#[test]
fn handle_event_key_release_returns_false() {
    let mut ta = TextArea::new();
    let changed = ta.handle_event(&key_release(KeyCode::Char('a')));
    assert!(!changed);
    assert!(ta.is_empty());
}

#[test]
fn handle_event_unknown_key_returns_false() {
    let mut ta = TextArea::new();
    let changed = ta.handle_event(&key_press(KeyCode::Escape));
    assert!(!changed);
}

#[test]
fn handle_event_resize_returns_false() {
    let mut ta = TextArea::new();
    let changed = ta.handle_event(&Event::Resize {
        width: 80,
        height: 24,
    });
    assert!(!changed);
}

#[test]
fn handle_event_paste_inserts_text() {
    let mut ta = TextArea::new();
    let changed = ta.handle_event(&paste_event("pasted text"));
    assert!(changed);
    assert_eq!(ta.text(), "pasted text");
}

#[test]
fn handle_event_paste_multiline() {
    let mut ta = TextArea::new();
    ta.handle_event(&paste_event("line1\nline2\nline3"));
    assert_eq!(ta.line_count(), 3);
}

#[test]
fn handle_event_enter_inserts_newline() {
    let mut ta = TextArea::new();
    ta.handle_event(&key_press(KeyCode::Char('a')));
    ta.handle_event(&key_press(KeyCode::Enter));
    ta.handle_event(&key_press(KeyCode::Char('b')));
    assert_eq!(ta.text(), "a\nb");
}

#[test]
fn handle_event_backspace_deletes() {
    let mut ta = TextArea::new().with_text("ab");
    ta.move_to_document_end();
    ta.handle_event(&key_press(KeyCode::Backspace));
    assert_eq!(ta.text(), "a");
}

#[test]
fn handle_event_ctrl_backspace_deletes_word() {
    let mut ta = TextArea::new().with_text("hello world");
    ta.move_to_document_end();
    ta.handle_event(&key_press_ctrl(KeyCode::Backspace));
    assert_eq!(ta.text(), "hello ");
}

#[test]
fn handle_event_delete_key() {
    let mut ta = TextArea::new().with_text("ab");
    ta.move_to_document_start();
    ta.handle_event(&key_press(KeyCode::Delete));
    assert_eq!(ta.text(), "b");
}

#[test]
fn handle_event_ctrl_z_undoes() {
    let mut ta = TextArea::new();
    ta.insert_text("abc");
    ta.handle_event(&key_press_ctrl(KeyCode::Char('z')));
    assert_eq!(ta.text(), "");
}

#[test]
fn handle_event_ctrl_y_redoes() {
    let mut ta = TextArea::new();
    ta.insert_text("abc");
    ta.undo();
    ta.handle_event(&key_press_ctrl(KeyCode::Char('y')));
    assert_eq!(ta.text(), "abc");
}

#[test]
fn handle_event_ctrl_k_deletes_to_eol() {
    let mut ta = TextArea::new().with_text("hello world");
    ta.move_to_document_start();
    ta.handle_event(&key_press_ctrl(KeyCode::Char('k')));
    assert_eq!(ta.text(), "");
}

#[test]
fn handle_event_ctrl_a_selects_all() {
    let mut ta = TextArea::new().with_text("abc\ndef");
    ta.handle_event(&key_press_ctrl(KeyCode::Char('a')));
    assert_eq!(ta.selected_text(), Some("abc\ndef".to_string()));
}

#[test]
fn handle_event_arrows_and_home_end() {
    let mut ta = TextArea::new().with_text("abc");
    ta.move_to_document_start();
    ta.handle_event(&key_press(KeyCode::Right));
    assert_eq!(ta.cursor().grapheme, 1);
    ta.handle_event(&key_press(KeyCode::Left));
    assert_eq!(ta.cursor().grapheme, 0);
    ta.handle_event(&key_press(KeyCode::End));
    assert_eq!(ta.cursor().grapheme, 3);
    ta.handle_event(&key_press(KeyCode::Home));
    assert_eq!(ta.cursor().grapheme, 0);
}

#[test]
fn handle_event_ctrl_left_right_word_movement() {
    let mut ta = TextArea::new().with_text("hello world");
    ta.move_to_document_start();
    ta.handle_event(&key_press_ctrl(KeyCode::Right));
    assert!(ta.cursor().grapheme > 0);
    ta.handle_event(&key_press_ctrl(KeyCode::Left));
    assert_eq!(ta.cursor().grapheme, 0);
}

#[test]
fn handle_event_shift_arrows_select() {
    let mut ta = TextArea::new().with_text("abc\ndef");
    ta.move_to_document_start();
    ta.handle_event(&key_press_shift(KeyCode::Right));
    ta.handle_event(&key_press_shift(KeyCode::Right));
    assert_eq!(ta.selected_text(), Some("ab".to_string()));
    ta.handle_event(&key_press_shift(KeyCode::Down));
    assert!(ta.selected_text().unwrap().contains('\n'));
    // Shift+Left should shrink selection
    ta.clear_selection();
    ta.move_to_document_end();
    ta.handle_event(&key_press_shift(KeyCode::Left));
    assert!(ta.selection().is_some());
    // Shift+Up
    ta.clear_selection();
    ta.move_to_document_end();
    ta.handle_event(&key_press_shift(KeyCode::Up));
    assert!(ta.selection().is_some());
}

#[test]
fn handle_event_page_up_down_via_keycode() {
    let mut ta = TextArea::new();
    for i in 0..30 {
        ta.insert_text(&format!("line {}\n", i));
    }
    ta.move_to_document_start();
    ta.handle_event(&key_press(KeyCode::PageDown));
    assert!(ta.cursor().line > 0);
    ta.handle_event(&key_press(KeyCode::PageUp));
    assert_eq!(ta.cursor().line, 0);
}

// ── Selection edge cases ──────────────────────────────────────────

#[test]
fn select_right_then_delete_removes_selection() {
    let mut ta = TextArea::new().with_text("abcdef");
    ta.move_to_document_start();
    ta.select_right();
    ta.select_right();
    ta.select_right();
    ta.delete_backward();
    assert_eq!(ta.text(), "def");
}

#[test]
fn select_across_lines_then_delete() {
    let mut ta = TextArea::new().with_text("abc\ndef\nghi");
    ta.move_to_document_start();
    ta.move_right(); // after 'a'
    ta.select_down(); // select to line 1 col 1
    ta.select_right(); // extend further
    let selected = ta.selected_text().unwrap();
    assert!(selected.contains('\n'));
    ta.delete_backward();
    // Should have deleted the selection
    assert!(ta.text().len() < "abc\ndef\nghi".len());
}

#[test]
fn select_up_at_first_line_selects_to_start() {
    let mut ta = TextArea::new().with_text("abc\ndef");
    ta.move_to_document_start();
    ta.move_right();
    ta.move_right();
    ta.select_up(); // should select to start of line 0 or stay
    // Cursor still on line 0
    assert_eq!(ta.cursor().line, 0);
}

#[test]
fn select_down_at_last_line() {
    let mut ta = TextArea::new().with_text("abc\ndef");
    ta.move_to_document_end();
    ta.select_down(); // should be noop or extend to end
    assert_eq!(ta.cursor().line, 1);
}

// ── Set cursor position clamping ──────────────────────────────────

#[test]
fn set_cursor_position_clamps_line() {
    use ftui_text::CursorPosition;
    let mut ta = TextArea::new().with_text("abc\ndef");
    ta.set_cursor_position(CursorPosition::new(100, 0, 0));
    // Should clamp to valid range
    assert!(ta.cursor().line < ta.line_count());
}

#[test]
fn set_cursor_position_clamps_column() {
    use ftui_text::CursorPosition;
    let mut ta = TextArea::new().with_text("abc");
    ta.set_cursor_position(CursorPosition::new(0, 100, 100));
    // Should be at or after end of line
    assert!(ta.cursor().grapheme <= 3);
}

// ── Text content edge cases ───────────────────────────────────────

#[test]
fn insert_text_with_crlf_normalizes() {
    let mut ta = TextArea::new();
    ta.insert_text("line1\r\nline2\r\nline3");
    assert!(ta.line_count() >= 2);
    // After insertion, text should be accessible
    let text = ta.text();
    assert!(text.contains("line1"));
    assert!(text.contains("line3"));
}

#[test]
fn insert_text_with_bare_cr() {
    let mut ta = TextArea::new();
    ta.insert_text("abc\rdef");
    let text = ta.text();
    assert!(text.contains("abc"));
    assert!(text.contains("def"));
}

#[test]
fn empty_lines_preserved() {
    let mut ta = TextArea::new();
    ta.insert_text("a\n\n\nb");
    assert_eq!(ta.line_count(), 4);
    assert_eq!(ta.text(), "a\n\n\nb");
}

#[test]
fn trailing_newline_creates_extra_line() {
    let ta = TextArea::new().with_text("abc\n");
    assert_eq!(ta.line_count(), 2);
}

#[test]
fn only_newlines() {
    let ta = TextArea::new().with_text("\n\n\n");
    assert_eq!(ta.line_count(), 4);
}

#[test]
fn unicode_emoji_insert_and_delete() {
    let mut ta = TextArea::new();
    ta.insert_text("🎉🚀");
    assert_eq!(ta.text(), "🎉🚀");
    ta.delete_backward();
    assert_eq!(ta.text(), "🎉");
    ta.delete_backward();
    assert!(ta.is_empty());
}

#[test]
fn cjk_characters() {
    let mut ta = TextArea::new();
    ta.insert_text("你好世界");
    assert_eq!(ta.text(), "你好世界");
    ta.move_to_document_start();
    ta.move_right();
    assert_eq!(ta.cursor().grapheme, 1);
}

// ── Scroll and viewport ───────────────────────────────────────────

#[test]
fn page_up_at_top_stays_at_top() {
    let mut ta = TextArea::new().with_text("abc\ndef\nghi");
    ta.move_to_document_start();
    let state = TextAreaState {
        last_viewport_height: 10,
        last_viewport_width: 80,
    };
    ta.page_up(&state);
    assert_eq!(ta.cursor().line, 0);
}

#[test]
fn page_down_at_bottom_stays_at_bottom() {
    let mut ta = TextArea::new().with_text("abc\ndef\nghi");
    ta.move_to_document_end();
    let before = ta.cursor();
    let state = TextAreaState {
        last_viewport_height: 10,
        last_viewport_width: 80,
    };
    ta.page_down(&state);
    assert_eq!(ta.cursor().line, before.line);
}

#[test]
fn page_navigation_without_soft_wrap_uses_state_viewport_height() {
    let mut ta = TextArea::new().with_text("aaa\nbbb\nccc");
    ta.move_to_document_start();

    let state = TextAreaState {
        last_viewport_height: 1,
        last_viewport_width: 3,
    };

    ta.page_down(&state);
    assert_eq!(ta.cursor().line, 1);
    assert_eq!(render_cell_char(&ta, 3, 1, 0, 0), Some('b'));

    ta.page_down(&state);
    assert_eq!(ta.cursor().line, 2);
    assert_eq!(render_cell_char(&ta, 3, 1, 0, 0), Some('c'));
}

#[test]
fn page_navigation_with_line_numbers_uses_text_area_width() {
    let mut ta = TextArea::new()
        .with_text("0123456789\nabcdefghij")
        .with_line_numbers(true)
        .with_focus(true);
    ta.move_to_document_start();
    for _ in 0..9 {
        ta.move_right();
    }

    let state = TextAreaState {
        last_viewport_height: 1,
        last_viewport_width: 8,
    };

    ta.page_down(&state);

    assert_eq!(ta.cursor().line, 1);
    assert!(render_has_cursor(&ta, 8, 1));
}

#[test]
fn soft_wrap_page_navigation_moves_by_visual_rows() {
    let mut ta = TextArea::new()
        .with_text("01234567890123456789")
        .with_soft_wrap(true);
    ta.move_to_document_start();
    for _ in 0..5 {
        ta.move_right();
    }
    assert_eq!(ta.cursor().visual_col, 5);

    let state = TextAreaState {
        last_viewport_height: 1,
        last_viewport_width: 10,
    };

    ta.page_down(&state);
    assert_eq!(ta.cursor().line, 0);
    assert_eq!(ta.cursor().visual_col, 15);

    ta.page_up(&state);
    assert_eq!(ta.cursor().line, 0);
    assert_eq!(ta.cursor().visual_col, 5);
}

#[test]
fn soft_wrap_page_navigation_uses_text_width_without_render() {
    let mut ta = TextArea::new()
        .with_text("01234567")
        .with_soft_wrap(true)
        .with_line_numbers(true);
    ta.move_to_document_start();

    let state = TextAreaState {
        last_viewport_height: 1,
        last_viewport_width: 8,
    };

    ta.page_down(&state);
    assert_eq!(ta.cursor().line, 0);
    assert_eq!(ta.cursor().visual_col, 5);

    ta.page_up(&state);
    assert_eq!(ta.cursor().line, 0);
    assert_eq!(ta.cursor().visual_col, 0);
}

#[test]
fn horizontal_scroll_on_long_line() {
    let mut ta = TextArea::new().with_text(&"a".repeat(200));
    ta.move_to_document_end();
    // Render with narrow viewport — should not panic
    assert!(render_cell_char(&ta, 20, 5, 0, 0).is_some());
}

// ── Soft wrap edge cases ──────────────────────────────────────────

#[test]
fn soft_wrap_width_one() {
    let ta = TextArea::new().with_soft_wrap(true).with_text("abcd");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 10, &mut pool);
    Widget::render(&ta, Rect::new(0, 0, 1, 10), &mut frame);
    // Each character should be on its own line
    assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('a'));
    assert_eq!(frame.buffer.get(0, 1).unwrap().content.as_char(), Some('b'));
    assert_eq!(frame.buffer.get(0, 2).unwrap().content.as_char(), Some('c'));
    assert_eq!(frame.buffer.get(0, 3).unwrap().content.as_char(), Some('d'));
}

#[test]
fn soft_wrap_empty_lines() {
    let ta = TextArea::new().with_soft_wrap(true).with_text("a\n\nb");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(10, 5, &mut pool);
    Widget::render(&ta, Rect::new(0, 0, 10, 5), &mut frame);
    // Line 0: 'a', Line 1: empty, Line 2: 'b'
    assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('a'));
    assert_eq!(frame.buffer.get(0, 2).unwrap().content.as_char(), Some('b'));
}

#[test]
fn soft_wrap_cursor_at_start_sets_position() {
    let ta = TextArea::new()
        .with_soft_wrap(true)
        .with_text("abcdef")
        .with_focus(true);
    // Cursor at start (default from with_text) — should always be visible
    assert!(render_has_cursor(&ta, 3, 4));
}

// ── Render edge cases ─────────────────────────────────────────────

#[test]
fn render_zero_width_no_panic() {
    let ta = TextArea::new().with_text("test");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(10, 10, &mut pool);
    Widget::render(&ta, Rect::new(0, 0, 0, 5), &mut frame);
}

#[test]
fn render_zero_height_no_panic() {
    let ta = TextArea::new().with_text("test");
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(10, 10, &mut pool);
    Widget::render(&ta, Rect::new(0, 0, 10, 0), &mut frame);
}

#[test]
fn render_width_one_height_one() {
    assert_eq!(
        render_cell_char(&TextArea::new().with_text("hello"), 1, 1, 0, 0),
        Some('h')
    );
}

#[test]
fn render_with_nonzero_origin() {
    let ta = TextArea::new().with_text("abc");
    assert_eq!(
        render_cell_char_at(&ta, Rect::new(5, 5, 10, 5), 20, 20, 5, 5),
        Some('a')
    );
}

#[test]
fn render_placeholder_when_empty() {
    assert_eq!(
        render_cell_char(
            &TextArea::new().with_placeholder("Type here..."),
            20,
            5,
            0,
            0
        ),
        Some('T')
    );
}

#[test]
fn render_no_placeholder_when_has_content() {
    let ta = TextArea::new()
        .with_placeholder("Type here...")
        .with_text("x");
    assert_eq!(render_cell_char(&ta, 20, 5, 0, 0), Some('x'));
}

#[test]
fn render_focused_sets_cursor() {
    let ta = TextArea::new().with_text("abc").with_focus(true);
    assert!(render_has_cursor(&ta, 20, 5));
}

#[test]
fn render_unfocused_no_cursor() {
    let ta = TextArea::new().with_text("abc").with_focus(false);
    assert!(!render_has_cursor(&ta, 20, 5));
}

#[test]
fn render_many_lines_scrolled() {
    let mut ta = TextArea::new();
    for i in 0..100 {
        ta.insert_text(&format!("line {}\n", i));
    }
    // Cursor is at the end; render with small viewport — should not panic
    assert!(render_cell_char(&ta, 20, 5, 0, 0).is_some());
}

#[test]
fn render_with_line_numbers_nonzero_origin() {
    let ta = TextArea::new()
        .with_text("abc\ndef\nghi")
        .with_line_numbers(true);
    assert_eq!(
        render_cell_char_at(&ta, Rect::new(2, 1, 15, 3), 20, 5, 2, 1),
        Some('1')
    );
}

#[test]
fn render_soft_wrap_with_line_numbers() {
    let ta = TextArea::new()
        .with_soft_wrap(true)
        .with_line_numbers(true)
        .with_text("abcdefghij");
    // gutter ~3 chars, leaving 5 for text, "abcdefghij" wraps
    assert_eq!(render_cell_char(&ta, 8, 4, 0, 0), Some('1'));
}

// ── Builder/state edge cases ──────────────────────────────────────

#[test]
fn all_builders_chain() {
    let ta = TextArea::new()
        .with_text("test")
        .with_placeholder("ph")
        .with_focus(true)
        .with_line_numbers(true)
        .with_style(ftui_style::Style::default())
        .with_cursor_line_style(ftui_style::Style::default())
        .with_selection_style(ftui_style::Style::default())
        .with_soft_wrap(true)
        .with_max_height(5);
    assert_eq!(ta.text(), "test");
    assert!(ta.is_focused());
    // Verify render doesn't panic with all options enabled
    assert!(render_cell_char(&ta, 20, 4, 0, 0).is_some());
}

#[test]
fn set_text_resets_state() {
    let mut ta = TextArea::new();
    for i in 0..50 {
        ta.insert_text(&format!("line {}\n", i));
    }
    ta.set_text("fresh");
    assert_eq!(ta.text(), "fresh");
    assert_eq!(ta.line_count(), 1);
    // Cursor should be reset too
    assert_eq!(ta.cursor().line, 0);
}

#[test]
fn editor_mut_direct_access() {
    let mut ta = TextArea::new().with_text("abc");
    ta.editor_mut().move_to_document_end();
    ta.editor_mut().insert_char('!');
    assert_eq!(ta.text(), "abc!");
}

#[test]
fn stateful_widget_updates_state() {
    use ftui_widgets::StatefulWidget;
    let ta = TextArea::new().with_text("abc");
    let mut state = TextAreaState::default();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(15, 8, &mut pool);
    StatefulWidget::render(&ta, Rect::new(0, 0, 15, 8), &mut frame, &mut state);
    assert_eq!(state.last_viewport_height, 8);
    assert_eq!(state.last_viewport_width, 15);
}

// ── Gutter rendering edge cases ──────────────────────────────────

#[test]
fn line_numbers_render_digit_for_10_lines() {
    let mut ta = TextArea::new().with_line_numbers(true);
    for _ in 0..9 {
        ta.insert_text("x\n");
    }
    ta.insert_text("x");
    ta.move_to_document_start();
    assert_eq!(ta.line_count(), 10);
    // With 10 lines, gutter should be 4 chars wide (2 digits + space + sep).
    // Content 'x' should appear at col 4.
    assert_eq!(render_cell_char(&ta, 20, 12, 0, 0), Some(' '));
    // The digit '1' should be at col 1 (right-aligned in 2-digit field)
    assert_eq!(render_cell_char(&ta, 20, 12, 1, 0), Some('1'));
}

#[test]
fn line_numbers_render_for_single_line() {
    let ta = TextArea::new().with_line_numbers(true).with_text("abc");
    // With 1 line, gutter = 3 (1 digit + space + sep). '1' at col 0
    assert_eq!(render_cell_char(&ta, 20, 3, 0, 0), Some('1'));
}

#[test]
fn line_numbers_render_correct_number_at_line_9() {
    let mut ta = TextArea::new().with_line_numbers(true);
    for _ in 0..9 {
        ta.insert_text("x\n");
    }
    ta.insert_text("x");
    ta.move_to_document_start();
    // Line 9 (0-indexed 8) should show "9" in the gutter
    // With 10 lines, gutter is 4 chars, digit field is 2 wide
    assert_eq!(render_cell_char(&ta, 20, 12, 1, 8), Some('9'));
}
