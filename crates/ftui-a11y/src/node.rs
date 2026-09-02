#![allow(clippy::module_name_repetitions)]

//! Core accessibility node types.
//!
//! Defines the ARIA-like role taxonomy, state flags, and node info
//! structures that form the accessibility tree. These are terminal-
//! oriented: roles map to common TUI widget patterns rather than
//! full web ARIA, keeping the surface small and discoverable.

use ftui_core::geometry::Rect;

// ── Roles ──────────────────────────────────────────────────────────────

/// ARIA-like role for accessibility nodes.
///
/// Each variant maps to a common TUI widget archetype. Screen readers
/// use the role to decide announcement strategy and keyboard behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum A11yRole {
    /// Top-level application window.
    Window,
    /// Modal or non-modal dialog overlay.
    Dialog,
    /// Clickable button.
    Button,
    /// Editable text field (single or multi-line).
    TextInput,
    /// Static text label (read-only).
    Label,
    /// Ordered or unordered list container.
    List,
    /// Single item within a list.
    ListItem,
    /// Table / grid container.
    Table,
    /// Row within a table.
    TableRow,
    /// Cell within a table row.
    TableCell,
    /// Two-state checkbox.
    Checkbox,
    /// One-of-many radio button.
    RadioButton,
    /// Determinate or indeterminate progress indicator.
    ProgressBar,
    /// Range slider.
    Slider,
    /// Tab header within a tab bar.
    Tab,
    /// Content panel associated with a tab.
    TabPanel,
    /// Drop-down or pop-up menu container.
    Menu,
    /// Single item within a menu.
    MenuItem,
    /// Toolbar grouping actions.
    Toolbar,
    /// Scrollbar track.
    ScrollBar,
    /// Visual separator (horizontal rule, divider).
    Separator,
    /// Generic grouping container.
    Group,
    /// Decorative / presentational element -- skipped by screen readers.
    Presentation,
}

impl A11yRole {
    /// Returns `true` if this role is interactive (can receive focus).
    #[inline]
    pub const fn is_interactive(&self) -> bool {
        matches!(
            self,
            Self::Button
                | Self::TextInput
                | Self::Checkbox
                | Self::RadioButton
                | Self::Slider
                | Self::Tab
                | Self::MenuItem
        )
    }
}

impl std::fmt::Display for A11yRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Window => "window",
            Self::Dialog => "dialog",
            Self::Button => "button",
            Self::TextInput => "textInput",
            Self::Label => "label",
            Self::List => "list",
            Self::ListItem => "listItem",
            Self::Table => "table",
            Self::TableRow => "tableRow",
            Self::TableCell => "tableCell",
            Self::Checkbox => "checkbox",
            Self::RadioButton => "radioButton",
            Self::ProgressBar => "progressBar",
            Self::Slider => "slider",
            Self::Tab => "tab",
            Self::TabPanel => "tabPanel",
            Self::Menu => "menu",
            Self::MenuItem => "menuItem",
            Self::Toolbar => "toolbar",
            Self::ScrollBar => "scrollBar",
            Self::Separator => "separator",
            Self::Group => "group",
            Self::Presentation => "presentation",
        };
        f.write_str(name)
    }
}

// ── State flags ────────────────────────────────────────────────────────

/// Accessibility state flags for a node.
///
/// All fields default to their "unset" or "false" values via `Default`.
/// Optional fields use `Option` to distinguish "not applicable" from
/// "explicitly false".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct A11yState {
    /// Whether this node currently has keyboard focus.
    pub focused: bool,
    /// Whether this node is disabled / non-interactive.
    pub disabled: bool,
    /// Checked state for checkboxes and radio buttons.
    /// `None` = not a checkable widget.
    pub checked: Option<bool>,
    /// Expanded state for collapsible sections.
    /// `None` = not expandable.
    pub expanded: Option<bool>,
    /// Whether this node is selected (e.g. list item, tab).
    pub selected: bool,
    /// Whether the content is read-only.
    pub readonly: bool,
    /// Whether user input is required.
    pub required: bool,
    /// Whether the node is in a busy / loading state.
    pub busy: bool,
    /// Current numeric value (sliders, progress bars).
    pub value_now: Option<f64>,
    /// Minimum numeric value.
    pub value_min: Option<f64>,
    /// Maximum numeric value.
    pub value_max: Option<f64>,
    /// Human-readable value description (e.g. "50%", "Medium").
    pub value_text: Option<String>,
}

// ── Live region ────────────────────────────────────────────────────────

/// Live region announcement urgency.
///
/// Matches the ARIA `aria-live` attribute semantics. Screen readers use
/// this to decide whether to interrupt the user or wait for a pause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LiveRegion {
    /// Announce at the next graceful opportunity (does not interrupt).
    Polite,
    /// Announce immediately, interrupting current speech.
    Assertive,
}

impl std::fmt::Display for LiveRegion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Polite => f.write_str("polite"),
            Self::Assertive => f.write_str("assertive"),
        }
    }
}

// ── Node info ──────────────────────────────────────────────────────────

/// A single accessibility node describing one widget or region.
///
/// Nodes form a tree via `parent` / `children` ID references. The tree
/// is built once per render pass by [`crate::tree::A11yTreeBuilder`] and
/// frozen into an immutable [`crate::tree::A11yTree`] snapshot.
#[derive(Debug, Clone)]
pub struct A11yNodeInfo {
    /// Unique identifier for this node within a single tree snapshot.
    pub id: u64,
    /// Semantic role.
    pub role: A11yRole,
    /// Human-readable accessible name (equivalent to `aria-label`).
    pub name: Option<String>,
    /// Extended description (equivalent to `aria-description`).
    pub description: Option<String>,
    /// Bounding rectangle in terminal cell coordinates.
    pub bounds: Rect,
    /// IDs of child nodes.
    pub children: Vec<u64>,
    /// ID of the parent node, if any (root has `None`).
    pub parent: Option<u64>,
    /// Keyboard shortcut hint (e.g. "Ctrl+S").
    pub shortcut: Option<String>,
    /// Current state flags.
    pub state: A11yState,
    /// Live-region announcement policy.
    pub live_region: Option<LiveRegion>,
}

impl A11yNodeInfo {
    /// Create a minimal node with required fields; everything else defaults.
    #[inline]
    /// A deterministic node id derived from the role and the rendered
    /// bounds, for widgets that have no hit id or widget id of their own.
    ///
    /// Stable across frames as long as the widget keeps its role and area,
    /// which is what the frame-to-frame diff and the announcements need. The
    /// caveat is inherent: a widget that moves or resizes shows up as a
    /// remove plus an add rather than a change.
    #[must_use]
    pub fn stable_id_for(role: A11yRole, bounds: Rect) -> u64 {
        use std::hash::{BuildHasher, Hash, Hasher};
        let mut hasher =
            ahash::RandomState::with_seeds(0x4131_3179, 0x5354_4142, 0x4c45_4944, 0x2e30_3031)
                .build_hasher();
        role.hash(&mut hasher);
        (bounds.x, bounds.y, bounds.width, bounds.height).hash(&mut hasher);
        hasher.finish()
    }

    pub fn new(id: u64, role: A11yRole, bounds: Rect) -> Self {
        Self {
            id,
            role,
            name: None,
            description: None,
            bounds,
            children: Vec::new(),
            parent: None,
            shortcut: None,
            state: A11yState::default(),
            live_region: None,
        }
    }

    /// Builder-style setter for the accessible name.
    #[inline]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Builder-style setter for the description.
    #[inline]
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    /// Builder-style setter for the keyboard shortcut hint.
    #[inline]
    pub fn with_shortcut(mut self, shortcut: impl Into<String>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    /// Builder-style setter for a live region policy.
    #[inline]
    pub fn with_live_region(mut self, live: LiveRegion) -> Self {
        self.live_region = Some(live);
        self
    }

    /// Builder-style setter for child IDs.
    #[inline]
    pub fn with_children(mut self, children: Vec<u64>) -> Self {
        self.children = children;
        self
    }

    /// Builder-style setter for the parent ID.
    #[inline]
    pub fn with_parent(mut self, parent: u64) -> Self {
        self.parent = Some(parent);
        self
    }

    /// Builder-style setter for the state flags.
    #[inline]
    pub fn with_state(mut self, state: A11yState) -> Self {
        self.state = state;
        self
    }
}
