#![forbid(unsafe_code)]

//! Contextual help system with tooltips and guided tours.
//!
//! This module provides tooltip widgets and a guided tour system that
//! integrate with the focus system to display contextual help.
//!
//! # Features
//!
//! - **Tooltips**: Floating help text near focused widgets with configurable
//!   positioning, delay, and auto-dismiss behavior.
//!
//! - **Guided Tours**: Step-by-step onboarding walkthroughs with spotlight
//!   highlighting, progress tracking, and completion persistence.
//!
//! # Example
//!
//! ```ignore
//! use ftui_extras::help::{Tour, TourStep, TourState, Spotlight};
//!
//! // Define a tour
//! let tour = Tour::new("onboarding")
//!     .add_step(TourStep::new("Welcome").content("Let's get started!"))
//!     .add_step(TourStep::new("Search").content("Find items here.").target_widget(1));
//!
//! // Start the tour
//! let mut state = TourState::new();
//! state.start(tour);
//!
//! // Render spotlight for current step
//! if let Some(step) = state.current_step() {
//!     let spotlight = Spotlight::new()
//!         .title(&step.title)
//!         .content(&step.content);
//!     // spotlight.render(...)
//! }
//! ```

mod spotlight;
mod tooltip;
mod tour;

pub use spotlight::{PanelPosition, Spotlight, SpotlightConfig};
pub use tooltip::{Tooltip, TooltipConfig, TooltipPosition, TooltipState};
pub use tour::{
    CompletionStatus, Tour, TourAction, TourCompletion, TourEvent, TourState, TourStep,
};

/// Cell content for one grapheme cluster `width` cells wide.
///
/// A cluster wider than one cell or made of several scalars is interned
/// whole, as `ftui_widgets`' own text drawing does. Storing only its first
/// scalar dropped combining marks, ZWJ parts and skin-tone modifiers.
fn grapheme_content(
    frame: &mut ftui_render::frame::Frame,
    grapheme: &str,
    width: usize,
) -> ftui_render::cell::CellContent {
    use ftui_render::cell::CellContent;
    let mut chars = grapheme.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if width <= 1 => CellContent::from_char(c),
        _ => CellContent::from_grapheme(
            frame.intern_with_width(grapheme, u8::try_from(width).unwrap_or(u8::MAX)),
        ),
    }
}
