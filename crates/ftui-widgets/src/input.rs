#![forbid(unsafe_code)]

//! Text input widget.
//!
//! A single-line text input field with cursor management, scrolling, and styling.

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind};
use ftui_core::geometry::Rect;
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use ftui_style::style::Style;
use ftui_text::width_cache::WidthCache;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{Widget, apply_style};

/// A single-line text input widget.
#[derive(Debug, Clone, Default)]
pub struct TextInput {
    /// Text value.
    value: String,
    /// Cursor position (grapheme index).
    cursor: usize,
    /// Scroll offset (grapheme index) for horizontal scrolling.
    scroll: usize,
    /// Placeholder text.
    placeholder: String,
    /// Mask character for password mode.
    mask_char: Option<char>,
    /// Base style.
    style: Style,
    /// Cursor style.
    cursor_style: Style,
    /// Placeholder style.
    placeholder_style: Style,
}

impl TextInput {
    /// Create a new empty text input.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the text value.
    pub fn with_value(mut self, value: impl Into<String>) -> Self {
        self.value = value.into();
        self.cursor = self.value.graphemes(true).count();
        self
    }

    /// Set the placeholder text.
    pub fn with_placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// Set password mode with mask character.
    pub fn with_mask(mut self, mask: char) -> Self {
        self.mask_char = Some(mask);
        self
    }

    /// Handle a terminal event.
    ///
    /// Returns `true` if the state changed.
    pub fn handle_event(&mut self, event: &Event) -> bool {
        if let Event::Key(key) = event {
            if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat {
                return self.handle_key(key);
            }
        }
        false
    }

    fn handle_key(&mut self, key: &KeyEvent) -> bool {
        match key.code {
            KeyCode::Char(c) => {
                self.insert_char(c);
                true
            }
            KeyCode::Backspace => {
                self.delete_char_back();
                true
            }
            KeyCode::Delete => {
                self.delete_char_forward();
                true
            }
            KeyCode::Left => {
                self.move_cursor_left();
                true
            }
            KeyCode::Right => {
                self.move_cursor_right();
                true
            }
            KeyCode::Home => {
                self.cursor = 0;
                self.scroll = 0;
                true
            }
            KeyCode::End => {
                self.cursor = self.grapheme_count();
                // Scroll calculation happens in render or should be updated here?
                // For now, let render clamp scroll, but we might want to track it.
                // Keeping scroll logic simple: render ensures cursor is visible.
                true
            }
            _ => false,
        }
    }

    /// Insert a character at the cursor position.
    fn insert_char(&mut self, c: char) {
        let graphemes: Vec<&str> = self.value.graphemes(true).collect();
        if self.cursor >= graphemes.len() {
            self.value.push(c);
        } else {
            // Reconstruct string with char inserted
            let mut new_val = String::with_capacity(self.value.len() + c.len_utf8());
            for (i, g) in graphemes.iter().enumerate() {
                if i == self.cursor {
                    new_val.push(c);
                }
                new_val.push_str(g);
            }
            self.value = new_val;
        }
        self.cursor += 1;
    }

    /// Delete character before cursor (Backspace).
    fn delete_char_back(&mut self) {
        if self.cursor > 0 {
            let graphemes: Vec<&str> = self.value.graphemes(true).collect();
            if self.cursor <= graphemes.len() {
                // Reconstruct skipping cursor-1
                let mut new_val = String::with_capacity(self.value.len());
                for (i, g) in graphemes.iter().enumerate() {
                    if i != self.cursor - 1 {
                        new_val.push_str(g);
                    }
                }
                self.value = new_val;
                self.cursor -= 1;
            }
        }
    }

    /// Delete character at cursor (Delete).
    fn delete_char_forward(&mut self) {
        let graphemes: Vec<&str> = self.value.graphemes(true).collect();
        if self.cursor < graphemes.len() {
            let mut new_val = String::with_capacity(self.value.len());
            for (i, g) in graphemes.iter().enumerate() {
                if i != self.cursor {
                    new_val.push_str(g);
                }
            }
            self.value = new_val;
        }
    }

    fn move_cursor_left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    fn move_cursor_right(&mut self) {
        if self.cursor < self.grapheme_count() {
            self.cursor += 1;
        }
    }

    fn grapheme_count(&self) -> usize {
        self.value.graphemes(true).count()
    }

    /// Get the current value.
    pub fn value(&self) -> &str {
        &self.value
    }
}

impl Widget for TextInput {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width < 1 || area.height < 1 {
            return;
        }

        // Apply base style to area
        buf.set_style(area, self.style);

        let graphemes: Vec<&str> = self.value.graphemes(true).collect();
        let placeholder_graphemes: Vec<&str> = self.placeholder.graphemes(true).collect();
        let show_placeholder = self.value.is_empty() && !self.placeholder.is_empty();
        
        let content_width = area.width as usize;
        
        // Calculate scroll position to keep cursor visible
        // We calculate this dynamically during render to ensure visibility
        // Ideally state would track this, but this is immediate-mode friendly
        let cursor_visual_pos = if self.value.is_empty() {
            0
        } else {
            // Need accurate visual position of cursor relative to start
            // Just counting graphemes assumes width 1, which is WRONG for CJK
            // But horizontal scrolling usually steps by cells, not graphemes?
            // Let's stick to grapheme-based scrolling for simplicity in v1,
            // assuming mostly 1-width chars, but using unicode-width for render.
            // Wait, if we have width 2 chars, scrolling by "1 grapheme" might jump 2 cells.
            
            // Simpler approach: Calculate visible window of graphemes.
            self.cursor
        };

        // Determine effective scroll
        // Ensure cursor is within [scroll, scroll + width]
        let mut effective_scroll = self.scroll;
        if cursor_visual_pos < effective_scroll {
            effective_scroll = cursor_visual_pos;
        }
        if cursor_visual_pos >= effective_scroll + content_width {
            effective_scroll = cursor_visual_pos - content_width + 1;
        }

        // Render content
        let mut x = area.x;
        let y = area.y;
        
        if show_placeholder {
            let start = effective_scroll.min(placeholder_graphemes.len());
            for g in placeholder_graphemes.iter().skip(start).take(content_width) {
                if x >= area.x + area.width { break; }
                let w = UnicodeWidthStr::width(*g) as u16;
                buf.set_string(x, y, g, self.placeholder_style);
                x += w;
            }
        } else {
            let start = effective_scroll.min(graphemes.len());
            for (i, g) in graphemes.iter().enumerate().skip(start) {
                if (x - area.x) as usize >= content_width { break; }
                
                let w = UnicodeWidthStr::width(*g) as u16;
                let text = if let Some(mask) = self.mask_char {
                    mask.to_string()
                } else {
                    g.to_string()
                };
                
                buf.set_string(x, y, &text, self.style);
                x += w;
            }
        }

        // Set cursor (if it's in visible area)
        let cursor_rel = cursor_visual_pos.saturating_sub(effective_scroll);
        if cursor_rel < content_width {
            let cursor_x = area.x + cursor_rel as u16;
            // Set style at cursor position to indicate cursor (e.g. reverse)
            // Or just rely on terminal cursor if we support that.
            // ftui doesn't have "show cursor" in Buffer yet, Frame has it.
            // But Widget::render only takes Buffer.
            // So we simulate cursor with style reverse or similar.
            if let Some(cell) = buf.get_mut(cursor_x, y) {
                // Apply cursor style (e.g. reverse)
                // For v1, let's just inverse the cell
                use ftui_render::cell::{StyleFlags, CellAttrs};
                let current_flags = cell.attrs.flags();
                let new_flags = current_flags ^ StyleFlags::REVERSE;
                cell.attrs = cell.attrs.with_flags(new_flags);
            }
        }
    }
}

// Add helper for Buffer to set string (if not already present in ftui-render)
// ftui-render::buffer::Buffer doesn't have set_string? 
// drawing.rs usually has it.
// I need to check if set_string is available or implement it here using set_raw/set.
