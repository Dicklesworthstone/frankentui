#![forbid(unsafe_code)]

//! Screen modules for the demo showcase.
//!
//! Each screen implements the [`Screen`] trait and can be navigated to via the
//! tab bar or number keys.

pub mod action_timeline;
pub mod advanced_features;
pub mod advanced_text_editor;
pub mod code_explorer;
pub mod dashboard;
pub mod data_viz;
pub mod file_browser;
pub mod form_validation;
pub mod forms_input;
pub mod intrinsic_sizing;
pub mod layout_lab;
pub mod log_search;
pub mod macro_recorder;
pub mod markdown_rich_text;
pub mod mouse_playground;
pub mod notifications;
pub mod performance;
pub mod responsive_demo;
pub mod shakespeare;
pub mod visual_effects;
pub mod widget_gallery;

use ftui_core::event::Event;
use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;

/// A help entry describing a keybinding.
pub struct HelpEntry {
    /// Key label (e.g. "Tab", "Ctrl+F").
    pub key: &'static str,
    /// Description of what the key does.
    pub action: &'static str,
}

/// Trait for demo screens.
///
/// Each screen manages its own state, handles its own messages, and renders
/// into the content area provided by the main layout.
pub trait Screen {
    /// Message type for this screen (will be wrapped by the top-level Msg enum).
    type Message: Send + 'static;

    /// Handle a screen-specific event, returning a command.
    fn update(&mut self, event: &Event) -> Cmd<Self::Message>;

    /// Render the screen into the given area.
    fn view(&self, frame: &mut Frame, area: Rect);

    /// Return keybindings specific to this screen for the help overlay.
    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![]
    }

    /// Called on each application tick (100ms interval) with the global tick count.
    fn tick(&mut self, _tick_count: u64) {}

    /// Title shown in the tab bar.
    fn title(&self) -> &'static str;

    /// Short name for tab display (max ~12 chars).
    fn tab_label(&self) -> &'static str {
        self.title()
    }
}
