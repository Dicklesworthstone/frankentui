#![forbid(unsafe_code)]

//! Dialog presets built on the Modal container.
//!
//! Provides common dialog patterns:
//! - Alert: Message with OK button
//! - Confirm: Message with OK/Cancel
//! - Prompt: Message with text input + OK/Cancel
//! - Custom: Builder for custom dialogs
//!
//! # Example
//!
//! ```ignore
//! let dialog = Dialog::alert("Operation complete", "File saved successfully.");
//! let dialog = Dialog::confirm("Delete file?", "This action cannot be undone.");
//! let dialog = Dialog::prompt("Enter name", "Please enter your username:");
//! ```

use crate::block::{Alignment, Block};
use crate::borders::Borders;
use crate::modal::{Modal, ModalConfig, ModalPosition, ModalSizeConstraints};
use crate::{StatefulWidget, Widget, draw_text_span, set_style_area};
use ftui_core::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ftui_core::geometry::Rect;
use ftui_render::frame::{Frame, HitData, HitId, HitRegion};
use ftui_style::{Style, StyleFlags};
use ftui_text::display_width;

/// Hit region for dialog buttons.
pub const DIALOG_HIT_BUTTON: HitRegion = HitRegion::Custom(10);

/// Result from a dialog interaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogResult {
    /// Dialog was dismissed without action.
    Dismissed,
    /// OK / primary button pressed.
    Ok,
    /// Cancel / secondary button pressed.
    Cancel,
    /// Custom button pressed with its ID.
    Custom(String),
    /// Prompt dialog submitted with input value.
    Input(String),
}

/// A button in a dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogButton {
    /// Display label.
    pub label: String,
    /// Unique identifier.
    pub id: String,
    /// Whether this is the primary/default button.
    pub primary: bool,
}

impl DialogButton {
    /// Create a new dialog button.
    pub fn new(label: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            id: id.into(),
            primary: false,
        }
    }

    /// Mark as primary button.
    pub fn primary(mut self) -> Self {
        self.primary = true;
        self
    }

    /// Display width including brackets.
    pub fn display_width(&self) -> usize {
        // [ label ] = display_width(label) + 4
        display_width(self.label.as_str()) + 4
    }
}

/// Dialog type variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogKind {
    /// Alert: single OK button.
    Alert,
    /// Confirm: OK + Cancel buttons.
    Confirm,
    /// Prompt: input field + OK + Cancel.
    Prompt,
    /// Custom dialog.
    Custom,
}

/// Dialog state for handling input and button focus.
#[derive(Debug, Clone, Default)]
pub struct DialogState {
    /// Currently focused button index.
    pub focused_button: Option<usize>,
    /// Input field value (for Prompt dialogs).
    pub input_value: String,
    /// Whether the input field is focused.
    pub input_focused: bool,
    /// Whether the dialog is open.
    pub open: bool,
    /// Result after interaction.
    pub result: Option<DialogResult>,
}

impl DialogState {
    /// Create a new open dialog state.
    pub fn new() -> Self {
        Self {
            open: true,
            input_focused: true, // Start with input focused for prompts
            ..Default::default()
        }
    }

    /// Check if dialog is open.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Close the dialog with a result.
    pub fn close(&mut self, result: DialogResult) {
        self.open = false;
        self.result = Some(result);
    }

    /// Reset the dialog state to open.
    pub fn reset(&mut self) {
        self.open = true;
        self.result = None;
        self.input_value.clear();
        self.focused_button = None;
        self.input_focused = true;
    }

    /// Get the result if closed.
    pub fn take_result(&mut self) -> Option<DialogResult> {
        self.result.take()
    }
}

/// Dialog configuration.
#[derive(Debug, Clone)]
pub struct DialogConfig {
    /// Modal configuration.
    pub modal_config: ModalConfig,
    /// Dialog kind.
    pub kind: DialogKind,
    /// Button style.
    pub button_style: Style,
    /// Primary button style.
    pub primary_button_style: Style,
    /// Focused button style.
    pub focused_button_style: Style,
    /// Title style.
    pub title_style: Style,
    /// Message style.
    pub message_style: Style,
    /// Input style (for Prompt).
    pub input_style: Style,
}

impl Default for DialogConfig {
    fn default() -> Self {
        Self {
            modal_config: ModalConfig::default()
                .position(ModalPosition::Center)
                .size(ModalSizeConstraints::new().min_width(30).max_width(60)),
            kind: DialogKind::Alert,
            button_style: Style::new(),
            primary_button_style: Style::new().bold(),
            focused_button_style: Style::new().reverse(),
            title_style: Style::new().bold(),
            message_style: Style::new(),
            input_style: Style::new(),
        }
    }
}

/// A dialog widget built on Modal.
///
/// Invariants:
/// - At least one button is always present.
/// - Button focus wraps around (modular arithmetic).
/// - For Prompt dialogs, Tab cycles: input -> buttons -> input.
///
/// Failure modes:
/// - If area is too small, content may be truncated but dialog never panics.
/// - Empty title/message is allowed (renders nothing for that row).
#[derive(Debug, Clone)]
pub struct Dialog {
    /// Dialog title.
    title: String,
    /// Dialog message.
    message: String,
    /// Buttons.
    buttons: Vec<DialogButton>,
    /// Configuration.
    config: DialogConfig,
    /// Hit ID for mouse interaction.
    hit_id: Option<HitId>,
}

impl Dialog {
    /// Create an alert dialog (message + OK).
    pub fn alert(title: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
            buttons: vec![DialogButton::new("OK", "ok").primary()],
            config: DialogConfig {
                kind: DialogKind::Alert,
                ..Default::default()
            },
            hit_id: None,
        }
    }

    /// Create a confirm dialog (message + OK/Cancel).
    pub fn confirm(title: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
            buttons: vec![
                DialogButton::new("OK", "ok").primary(),
                DialogButton::new("Cancel", "cancel"),
            ],
            config: DialogConfig {
                kind: DialogKind::Confirm,
                ..Default::default()
            },
            hit_id: None,
        }
    }

    /// Create a prompt dialog (message + input + OK/Cancel).
    pub fn prompt(title: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
            buttons: vec![
                DialogButton::new("OK", "ok").primary(),
                DialogButton::new("Cancel", "cancel"),
            ],
            config: DialogConfig {
                kind: DialogKind::Prompt,
                ..Default::default()
            },
            hit_id: None,
        }
    }

    /// Create a custom dialog with a builder.
    pub fn custom(title: impl Into<String>, message: impl Into<String>) -> DialogBuilder {
        DialogBuilder {
            title: title.into(),
            message: message.into(),
            buttons: Vec::new(),
            config: DialogConfig {
                kind: DialogKind::Custom,
                ..Default::default()
            },
            hit_id: None,
        }
    }

    /// Set the hit ID for mouse interaction.
    pub fn hit_id(mut self, id: HitId) -> Self {
        self.hit_id = Some(id);
        self.config.modal_config = self.config.modal_config.hit_id(id);
        self
    }

    /// Set the modal configuration.
    pub fn modal_config(mut self, config: ModalConfig) -> Self {
        self.config.modal_config = config;
        self
    }

    /// Set button style.
    pub fn button_style(mut self, style: Style) -> Self {
        self.config.button_style = style;
        self
    }

    /// Set primary button style.
    pub fn primary_button_style(mut self, style: Style) -> Self {
        self.config.primary_button_style = style;
        self
    }

    /// Set focused button style.
    pub fn focused_button_style(mut self, style: Style) -> Self {
        self.config.focused_button_style = style;
        self
    }

    /// Handle an event and potentially update state.
    pub fn handle_event(
        &self,
        event: &Event,
        state: &mut DialogState,
        hit: Option<(HitId, HitRegion, HitData)>,
    ) -> Option<DialogResult> {
        if !state.open {
            return None;
        }

        if self.config.kind != DialogKind::Prompt && state.input_focused {
            state.input_focused = false;
        }

        match event {
            // Escape closes with Dismissed
            Event::Key(KeyEvent {
                code: KeyCode::Escape,
                kind: KeyEventKind::Press,
                ..
            }) if self.config.modal_config.close_on_escape => {
                state.close(DialogResult::Dismissed);
                return Some(DialogResult::Dismissed);
            }

            // Tab cycles focus
            Event::Key(KeyEvent {
                code: KeyCode::Tab,
                kind: KeyEventKind::Press,
                modifiers,
                ..
            }) => {
                let shift = modifiers.contains(Modifiers::SHIFT);
                self.cycle_focus(state, shift);
            }

            // Enter activates focused button
            Event::Key(KeyEvent {
                code: KeyCode::Enter,
                kind: KeyEventKind::Press,
                ..
            }) => {
                return self.activate_button(state);
            }

            // Arrow keys navigate buttons
            Event::Key(KeyEvent {
                code: KeyCode::Left | KeyCode::Right,
                kind: KeyEventKind::Press,
                ..
            }) if !state.input_focused => {
                let forward = matches!(
                    event,
                    Event::Key(KeyEvent {
                        code: KeyCode::Right,
                        ..
                    })
                );
                self.navigate_buttons(state, forward);
            }

            // Mouse click on button
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                ..
            }) => {
                if let (Some((id, region, data)), Some(expected)) = (hit, self.hit_id)
                    && id == expected
                    && region == DIALOG_HIT_BUTTON
                    && let Ok(idx) = usize::try_from(data)
                    && idx < self.buttons.len()
                {
                    state.focused_button = Some(idx);
                    return self.activate_button(state);
                }
            }

            // For prompt dialogs, handle text input
            Event::Key(key_event)
                if self.config.kind == DialogKind::Prompt && state.input_focused =>
            {
                self.handle_input_key(state, key_event);
            }

            _ => {}
        }

        None
    }

    fn cycle_focus(&self, state: &mut DialogState, reverse: bool) {
        let has_input = self.config.kind == DialogKind::Prompt;
        let button_count = self.buttons.len();

        if has_input {
            // Cycle: input -> button 0 -> button 1 -> ... -> input
            if state.input_focused {
                state.input_focused = false;
                state.focused_button = if reverse {
                    Some(button_count.saturating_sub(1))
                } else {
                    Some(0)
                };
            } else if let Some(idx) = state.focused_button {
                if reverse {
                    if idx == 0 {
                        state.input_focused = true;
                        state.focused_button = None;
                    } else {
                        state.focused_button = Some(idx - 1);
                    }
                } else if idx + 1 >= button_count {
                    state.input_focused = true;
                    state.focused_button = None;
                } else {
                    state.focused_button = Some(idx + 1);
                }
            }
        } else {
            // Just cycle buttons
            let current = state.focused_button.unwrap_or(0);
            state.focused_button = if reverse {
                Some(if current == 0 {
                    button_count - 1
                } else {
                    current - 1
                })
            } else {
                Some((current + 1) % button_count)
            };
        }
    }

    fn navigate_buttons(&self, state: &mut DialogState, forward: bool) {
        let count = self.buttons.len();
        if count == 0 {
            return;
        }
        let current = state.focused_button.unwrap_or(0);
        state.focused_button = if forward {
            Some((current + 1) % count)
        } else {
            Some(if current == 0 { count - 1 } else { current - 1 })
        };
    }

    fn activate_button(&self, state: &mut DialogState) -> Option<DialogResult> {
        let idx = state.focused_button.or_else(|| {
            // Default to primary button
            self.buttons.iter().position(|b| b.primary)
        })?;

        let button = self.buttons.get(idx)?;
        let result = match button.id.as_str() {
            "ok" => {
                if self.config.kind == DialogKind::Prompt {
                    DialogResult::Input(state.input_value.clone())
                } else {
                    DialogResult::Ok
                }
            }
            "cancel" => DialogResult::Cancel,
            id => DialogResult::Custom(id.to_string()),
        };

        state.close(result.clone());
        Some(result)
    }

    fn handle_input_key(&self, state: &mut DialogState, key: &KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }

        match key.code {
            KeyCode::Char(c) => {
                state.input_value.push(c);
            }
            KeyCode::Backspace => {
                state.input_value.pop();
            }
            KeyCode::Delete => {
                state.input_value.clear();
            }
            _ => {}
        }
    }

    /// Calculate content height.
    fn content_height(&self) -> u16 {
        let mut height: u16 = 2; // Top and bottom border

        // Title row
        if !self.title.is_empty() {
            height += 1;
        }

        // Message row(s) - simplified: 1 row
        if !self.message.is_empty() {
            height += 1;
        }

        // Spacing
        height += 1;

        // Input row (for Prompt)
        if self.config.kind == DialogKind::Prompt {
            height += 1;
            height += 1; // Spacing
        }

        // Button row
        height += 1;

        height
    }

    /// Render the dialog content.
    fn render_content(&self, area: Rect, frame: &mut Frame, state: &DialogState) {
        if area.is_empty() {
            return;
        }

        // Draw border
        let block = Block::default()
            .borders(Borders::ALL)
            .title(&self.title)
            .title_alignment(Alignment::Center);
        block.render(area, frame);

        let inner = block.inner(area);
        if inner.is_empty() {
            return;
        }

        let mut y = inner.y;

        // Message
        if !self.message.is_empty() && y < inner.bottom() {
            self.draw_centered_text(
                frame,
                inner.x,
                y,
                inner.width,
                &self.message,
                self.config.message_style,
            );
            y += 1;
        }

        // Spacing
        y += 1;

        // Input field (for Prompt)
        if self.config.kind == DialogKind::Prompt && y < inner.bottom() {
            self.render_input(frame, inner.x, y, inner.width, state);
            y += 2; // Input + spacing
        }

        // Buttons
        if y < inner.bottom() {
            self.render_buttons(frame, inner.x, y, inner.width, state);
        }
    }

    fn draw_centered_text(
        &self,
        frame: &mut Frame,
        x: u16,
        y: u16,
        width: u16,
        text: &str,
        style: Style,
    ) {
        let text_width = display_width(text).min(width as usize);
        let offset = (width as usize - text_width) / 2;
        let start_x = x.saturating_add(offset as u16);
        draw_text_span(frame, start_x, y, text, style, x.saturating_add(width));
    }

    fn render_input(&self, frame: &mut Frame, x: u16, y: u16, width: u16, state: &DialogState) {
        // Draw input background
        let input_area = Rect::new(x + 1, y, width.saturating_sub(2), 1);
        let input_style = self.config.input_style;
        set_style_area(&mut frame.buffer, input_area, input_style);

        // Draw input value or placeholder
        let display_text = if state.input_value.is_empty() {
            " "
        } else {
            &state.input_value
        };

        draw_text_span(
            frame,
            input_area.x,
            y,
            display_text,
            input_style,
            input_area.right(),
        );

        // Draw cursor if focused
        if state.input_focused {
            let input_width = display_width(state.input_value.as_str());
            let cursor_x = input_area.x + input_width.min(input_area.width as usize) as u16;
            if cursor_x < input_area.right() {
                frame.cursor_position = Some((cursor_x, y));
                frame.cursor_visible = true;
            }
        }
    }

    fn render_buttons(&self, frame: &mut Frame, x: u16, y: u16, width: u16, state: &DialogState) {
        if self.buttons.is_empty() {
            return;
        }

        // Calculate total button width
        let total_width: usize = self
            .buttons
            .iter()
            .map(|b| b.display_width())
            .sum::<usize>()
            + self.buttons.len().saturating_sub(1) * 2; // Spacing between buttons

        // Center the buttons
        let start_x = x + (width as usize - total_width.min(width as usize)) as u16 / 2;
        let mut bx = start_x;

        for (i, button) in self.buttons.iter().enumerate() {
            let is_focused = state.focused_button == Some(i);

            // Select style
            let mut style = if is_focused {
                self.config.focused_button_style
            } else if button.primary {
                self.config.primary_button_style
            } else {
                self.config.button_style
            };
            if is_focused {
                let has_reverse = style
                    .attrs
                    .is_some_and(|attrs| attrs.contains(StyleFlags::REVERSE));
                if !has_reverse {
                    style = style.reverse();
                }
            }

            // Draw button: [ label ]
            let btn_text = format!("[ {} ]", button.label);
            let btn_width = display_width(btn_text.as_str());
            draw_text_span(frame, bx, y, &btn_text, style, x.saturating_add(width));

            // Register hit region for button
            if let Some(hit_id) = self.hit_id {
                let max_btn_width = width.saturating_sub(bx.saturating_sub(x));
                let btn_area_width = btn_width.min(max_btn_width as usize) as u16;
                if btn_area_width > 0 {
                    let btn_area = Rect::new(bx, y, btn_area_width, 1);
                    frame.register_hit(btn_area, hit_id, DIALOG_HIT_BUTTON, i as u64);
                }
            }

            bx = bx.saturating_add(btn_width as u16 + 2); // Button + spacing
        }
    }
}

impl StatefulWidget for Dialog {
    type State = DialogState;

    fn render(&self, area: Rect, frame: &mut Frame, state: &mut Self::State) {
        if !state.open || area.is_empty() {
            return;
        }

        // Calculate content area
        let content_height = self.content_height();
        let config = self.config.modal_config.clone().size(
            ModalSizeConstraints::new()
                .min_width(30)
                .max_width(60)
                .min_height(content_height)
                .max_height(content_height + 4),
        );

        // Create a wrapper widget for the dialog content
        let content = DialogContent {
            dialog: self,
            state,
        };

        // Render via Modal
        let modal = Modal::new(content).config(config);
        modal.render(area, frame);
    }
}

/// Internal wrapper for rendering dialog content.
struct DialogContent<'a> {
    dialog: &'a Dialog,
    state: &'a DialogState,
}

impl Widget for DialogContent<'_> {
    fn render(&self, area: Rect, frame: &mut Frame) {
        self.dialog.render_content(area, frame, self.state);
    }
}

/// Builder for custom dialogs.
#[derive(Debug, Clone)]
pub struct DialogBuilder {
    title: String,
    message: String,
    buttons: Vec<DialogButton>,
    config: DialogConfig,
    hit_id: Option<HitId>,
}

impl DialogBuilder {
    /// Add a button.
    pub fn button(mut self, button: DialogButton) -> Self {
        self.buttons.push(button);
        self
    }

    /// Add an OK button.
    pub fn ok_button(self) -> Self {
        self.button(DialogButton::new("OK", "ok").primary())
    }

    /// Add a Cancel button.
    pub fn cancel_button(self) -> Self {
        self.button(DialogButton::new("Cancel", "cancel"))
    }

    /// Add a custom button.
    pub fn custom_button(self, label: impl Into<String>, id: impl Into<String>) -> Self {
        self.button(DialogButton::new(label, id))
    }

    /// Set modal configuration.
    pub fn modal_config(mut self, config: ModalConfig) -> Self {
        self.config.modal_config = config;
        self
    }

    /// Set hit ID for mouse interaction.
    pub fn hit_id(mut self, id: HitId) -> Self {
        self.hit_id = Some(id);
        self
    }

    /// Build the dialog.
    pub fn build(self) -> Dialog {
        let mut buttons = self.buttons;
        if buttons.is_empty() {
            buttons.push(DialogButton::new("OK", "ok").primary());
        }

        Dialog {
            title: self.title,
            message: self.message,
            buttons,
            config: self.config,
            hit_id: self.hit_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;

    #[test]
    fn alert_dialog_single_button() {
        let dialog = Dialog::alert("Title", "Message");
        assert_eq!(dialog.buttons.len(), 1);
        assert_eq!(dialog.buttons[0].label, "OK");
        assert!(dialog.buttons[0].primary);
    }

    #[test]
    fn confirm_dialog_two_buttons() {
        let dialog = Dialog::confirm("Title", "Message");
        assert_eq!(dialog.buttons.len(), 2);
        assert_eq!(dialog.buttons[0].label, "OK");
        assert_eq!(dialog.buttons[1].label, "Cancel");
    }

    #[test]
    fn prompt_dialog_has_input() {
        let dialog = Dialog::prompt("Title", "Message");
        assert_eq!(dialog.config.kind, DialogKind::Prompt);
        assert_eq!(dialog.buttons.len(), 2);
    }

    #[test]
    fn custom_dialog_builder() {
        let dialog = Dialog::custom("Custom", "Message")
            .ok_button()
            .cancel_button()
            .custom_button("Help", "help")
            .build();
        assert_eq!(dialog.buttons.len(), 3);
    }

    #[test]
    fn dialog_state_starts_open() {
        let state = DialogState::new();
        assert!(state.is_open());
        assert!(state.result.is_none());
    }

    #[test]
    fn dialog_state_close_sets_result() {
        let mut state = DialogState::new();
        state.close(DialogResult::Ok);
        assert!(!state.is_open());
        assert_eq!(state.result, Some(DialogResult::Ok));
    }

    #[test]
    fn dialog_escape_closes() {
        let dialog = Dialog::alert("Test", "Msg");
        let mut state = DialogState::new();
        let event = Event::Key(KeyEvent {
            code: KeyCode::Escape,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        let result = dialog.handle_event(&event, &mut state, None);
        assert_eq!(result, Some(DialogResult::Dismissed));
        assert!(!state.is_open());
    }

    #[test]
    fn dialog_enter_activates_primary() {
        let dialog = Dialog::alert("Test", "Msg");
        let mut state = DialogState::new();
        state.input_focused = false; // Not on input
        let event = Event::Key(KeyEvent {
            code: KeyCode::Enter,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        let result = dialog.handle_event(&event, &mut state, None);
        assert_eq!(result, Some(DialogResult::Ok));
    }

    #[test]
    fn dialog_tab_cycles_focus() {
        let dialog = Dialog::confirm("Test", "Msg");
        let mut state = DialogState::new();
        state.input_focused = false;
        state.focused_button = Some(0);

        let tab = Event::Key(KeyEvent {
            code: KeyCode::Tab,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });

        dialog.handle_event(&tab, &mut state, None);
        assert_eq!(state.focused_button, Some(1));

        dialog.handle_event(&tab, &mut state, None);
        assert_eq!(state.focused_button, Some(0)); // Wraps around
    }

    #[test]
    fn prompt_enter_returns_input() {
        let dialog = Dialog::prompt("Test", "Enter:");
        let mut state = DialogState::new();
        state.input_value = "hello".to_string();
        state.input_focused = false;
        state.focused_button = Some(0); // OK button

        let enter = Event::Key(KeyEvent {
            code: KeyCode::Enter,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });

        let result = dialog.handle_event(&enter, &mut state, None);
        assert_eq!(result, Some(DialogResult::Input("hello".to_string())));
    }

    #[test]
    fn button_display_width() {
        let button = DialogButton::new("OK", "ok");
        assert_eq!(button.display_width(), 6); // [ OK ]
    }

    #[test]
    fn render_alert_does_not_panic() {
        let dialog = Dialog::alert("Alert", "This is an alert message.");
        let mut state = DialogState::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        dialog.render(Rect::new(0, 0, 80, 24), &mut frame, &mut state);
    }

    #[test]
    fn render_confirm_does_not_panic() {
        let dialog = Dialog::confirm("Confirm", "Are you sure?");
        let mut state = DialogState::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        dialog.render(Rect::new(0, 0, 80, 24), &mut frame, &mut state);
    }

    #[test]
    fn render_prompt_does_not_panic() {
        let dialog = Dialog::prompt("Prompt", "Enter your name:");
        let mut state = DialogState::new();
        state.input_value = "Test User".to_string();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        dialog.render(Rect::new(0, 0, 80, 24), &mut frame, &mut state);
    }

    #[test]
    fn render_tiny_area_does_not_panic() {
        let dialog = Dialog::alert("T", "M");
        let mut state = DialogState::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);
        dialog.render(Rect::new(0, 0, 10, 5), &mut frame, &mut state);
    }

    #[test]
    fn custom_dialog_empty_buttons_gets_default() {
        let dialog = Dialog::custom("Custom", "No buttons").build();
        assert_eq!(dialog.buttons.len(), 1);
        assert_eq!(dialog.buttons[0].label, "OK");
    }

    #[test]
    fn render_unicode_message_does_not_panic() {
        // CJK characters are 2 columns wide each
        let dialog = Dialog::alert("你好", "这是一条消息 🎉");
        let mut state = DialogState::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        dialog.render(Rect::new(0, 0, 80, 24), &mut frame, &mut state);
    }

    #[test]
    fn prompt_with_unicode_input_renders_correctly() {
        let dialog = Dialog::prompt("入力", "名前を入力:");
        let mut state = DialogState::new();
        state.input_value = "田中太郎".to_string(); // CJK input
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        dialog.render(Rect::new(0, 0, 80, 24), &mut frame, &mut state);
    }
}
