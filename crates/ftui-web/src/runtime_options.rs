//! Runtime option and theming mutation API with atomic validation (bd-2vr05.13.5).
//!
//! # Why this exists
//!
//! Production embedders of the FrankenTermJS web surface need to reconfigure a
//! live terminal **without re-instantiating it** — exactly the `term.options`
//! mutation surface that real `xterm.js` integrators depend on (cursor style,
//! scrollback depth, screen-reader mode, renderer backend, theme palette, …).
//!
//! This module is the **single host-agnostic source of truth** for that option
//! schema and its mutation semantics. It is pure and deterministic: identical
//! `(current options, patch, capability profile)` inputs always produce an
//! identical [`RuntimeOptionUpdate`] outcome and an identical structured-log
//! serialisation, so a fixed capability profile yields reproducible behaviour
//! across the rendering backends introduced by `bd-2vr05.8.1`.
//!
//! ```text
//!  JSON patch ──parse──> RuntimeOptionPatch ─┐
//!  (or typed builder) ───────────────────────┤
//!                                            ├─ apply_patch(caps) ─┬─ Applied  ─> field diffs + repaint/reinit flags
//!  OptionCapabilityProfile ──────────────────┘  (atomic: all or    └─ Rejected ─> per-field errors, options UNCHANGED
//!                                                 nothing commits)
//! ```
//!
//! ## Atomicity & rollback
//!
//! [`RuntimeOptions::apply_patch`] validates the **entire** candidate state
//! (schema range checks + capability gating) before committing anything. If any
//! field is invalid it returns a [`RuntimeOptionUpdate`] carrying *every*
//! offending field's error and leaves `self` byte-for-byte unchanged — there is
//! no partial application. On success it commits atomically and reports the
//! exact set of changed fields plus the renderer/engine synchronisation that the
//! host must perform ([`RuntimeOptionUpdate::requires_repaint`] /
//! [`RuntimeOptionUpdate::requires_renderer_reinit`]).
//!
//! ## Capability / profile gating
//!
//! A value can be *schema-valid* yet *unsupported by the active engine*: e.g. a
//! `WebGpu` renderer request when the host only advertises Canvas, or a
//! scrollback depth beyond the host's memory budget. Those are reported as
//! gating errors distinct from out-of-range schema errors, so a host can tell
//! "you asked for something impossible here" apart from "you asked for nonsense".
//!
//! The colour swaps themselves stay host-side (browser CSS / canvas), keeping
//! this module free of any dependency on `ftui-style` — the same layering rule
//! the accessibility preference policies follow.

use core::fmt;
use ftui_core::terminal_capabilities::ColorDepth;

/// Hard ceiling on scrollback retention, independent of the host's advertised
/// budget. Guards against absurd payloads even when a host reports a very large
/// `max_scrollback_lines`.
pub const SCROLLBACK_HARD_MAX: u32 = 1_000_000;

/// Minimum WCAG contrast ratio expressible by the option, ×100 (`1.0:1`).
pub const MIN_CONTRAST_RATIO_X100: u16 = 100;
/// Maximum WCAG contrast ratio expressible by the option, ×100 (`21:1`, the
/// largest ratio achievable between sRGB black and white).
pub const MAX_CONTRAST_RATIO_X100: u16 = 2100;

/// Minimum supported tab stop width, in columns.
pub const MIN_TAB_WIDTH: u8 = 1;
/// Maximum supported tab stop width, in columns.
pub const MAX_TAB_WIDTH: u8 = 16;

/// Terminal cursor presentation style (parity with `xterm.js` `cursorStyle`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CursorStyle {
    /// Full-cell block cursor.
    Block,
    /// Underline cursor.
    Underline,
    /// Vertical bar cursor.
    Bar,
}

impl CursorStyle {
    /// Stable lowercase token used in serialisation and JSON parsing.
    #[must_use]
    pub const fn as_token(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Underline => "underline",
            Self::Bar => "bar",
        }
    }

    /// Parse from a stable token; `None` for an unknown style.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "block" => Some(Self::Block),
            "underline" => Some(Self::Underline),
            "bar" => Some(Self::Bar),
            _ => None,
        }
    }
}

impl fmt::Display for CursorStyle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_token())
    }
}

/// Rendering backend selection (parity with `xterm.js` renderer addons, plus the
/// WebGPU-primary backend introduced by `bd-2vr05.8.1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RendererType {
    /// DOM-cell renderer (most compatible, slowest).
    Dom,
    /// 2D canvas renderer.
    Canvas,
    /// WebGL renderer.
    WebGl,
    /// WebGPU renderer (primary high-performance backend).
    WebGpu,
}

impl RendererType {
    /// Stable lowercase token used in serialisation and JSON parsing.
    #[must_use]
    pub const fn as_token(self) -> &'static str {
        match self {
            Self::Dom => "dom",
            Self::Canvas => "canvas",
            Self::WebGl => "webgl",
            Self::WebGpu => "webgpu",
        }
    }

    /// Parse from a stable token; `None` for an unknown renderer.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "dom" => Some(Self::Dom),
            "canvas" => Some(Self::Canvas),
            "webgl" => Some(Self::WebGl),
            "webgpu" => Some(Self::WebGpu),
            _ => None,
        }
    }
}

impl fmt::Display for RendererType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_token())
    }
}

/// A packed `0xRRGGBBAA` theme colour. Host-agnostic: the actual swap is applied
/// by the browser/canvas, this is pure intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ThemeColor(pub u32);

impl ThemeColor {
    /// Construct from individual sRGB channels and alpha.
    #[must_use]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self(((r as u32) << 24) | ((g as u32) << 16) | ((b as u32) << 8) | (a as u32))
    }

    /// Construct an opaque colour from sRGB channels.
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self::rgba(r, g, b, 0xFF)
    }

    /// Red channel.
    #[must_use]
    pub const fn r(self) -> u8 {
        (self.0 >> 24) as u8
    }
    /// Green channel.
    #[must_use]
    pub const fn g(self) -> u8 {
        (self.0 >> 16) as u8
    }
    /// Blue channel.
    #[must_use]
    pub const fn b(self) -> u8 {
        (self.0 >> 8) as u8
    }
    /// Alpha channel (`0xFF` = fully opaque).
    #[must_use]
    pub const fn a(self) -> u8 {
        self.0 as u8
    }

    /// `true` when the colour is fully opaque.
    #[must_use]
    pub const fn is_opaque(self) -> bool {
        self.a() == 0xFF
    }

    /// Stable `#rrggbbaa` lowercase hex token used in serialisation.
    #[must_use]
    pub fn as_hex(self) -> String {
        format!("#{:08x}", self.0)
    }

    /// Parse a `#rrggbb` (opaque) or `#rrggbbaa` hex token.
    #[must_use]
    pub fn from_hex(token: &str) -> Option<Self> {
        let body = token.strip_prefix('#')?;
        match body.len() {
            6 => {
                let rgb = u32::from_str_radix(body, 16).ok()?;
                Some(Self((rgb << 8) | 0xFF))
            }
            8 => Some(Self(u32::from_str_radix(body, 16).ok()?)),
            _ => None,
        }
    }
}

/// The full theme palette: anchor colours plus the 16 ANSI entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ThemePalette {
    /// Default foreground colour.
    pub foreground: ThemeColor,
    /// Default background colour.
    pub background: ThemeColor,
    /// Cursor colour.
    pub cursor: ThemeColor,
    /// Cursor glyph (accent) colour.
    pub cursor_accent: ThemeColor,
    /// Selection highlight background.
    pub selection_background: ThemeColor,
    /// The 16 ANSI palette entries (`0..=7` normal, `8..=15` bright).
    pub ansi: [ThemeColor; 16],
}

impl ThemePalette {
    /// The default dark palette (parity with `xterm.js` defaults).
    #[must_use]
    pub const fn dark() -> Self {
        Self {
            foreground: ThemeColor::rgb(0xD0, 0xD0, 0xD0),
            background: ThemeColor::rgb(0x00, 0x00, 0x00),
            cursor: ThemeColor::rgb(0xFF, 0xFF, 0xFF),
            cursor_accent: ThemeColor::rgb(0x00, 0x00, 0x00),
            selection_background: ThemeColor::rgba(0xFF, 0xFF, 0xFF, 0x4D),
            ansi: [
                ThemeColor::rgb(0x2E, 0x34, 0x36), // black
                ThemeColor::rgb(0xCC, 0x00, 0x00), // red
                ThemeColor::rgb(0x4E, 0x9A, 0x06), // green
                ThemeColor::rgb(0xC4, 0xA0, 0x00), // yellow
                ThemeColor::rgb(0x34, 0x65, 0xA4), // blue
                ThemeColor::rgb(0x75, 0x50, 0x7B), // magenta
                ThemeColor::rgb(0x06, 0x98, 0x9A), // cyan
                ThemeColor::rgb(0xD3, 0xD7, 0xCF), // white
                ThemeColor::rgb(0x55, 0x57, 0x53), // bright black
                ThemeColor::rgb(0xEF, 0x29, 0x29), // bright red
                ThemeColor::rgb(0x8A, 0xE2, 0x34), // bright green
                ThemeColor::rgb(0xFC, 0xE9, 0x4F), // bright yellow
                ThemeColor::rgb(0x72, 0x9F, 0xCF), // bright blue
                ThemeColor::rgb(0xAD, 0x7F, 0xA8), // bright magenta
                ThemeColor::rgb(0x34, 0xE2, 0xE2), // bright cyan
                ThemeColor::rgb(0xEE, 0xEE, 0xEC), // bright white
            ],
        }
    }

    /// The anchor colours that must remain fully opaque (a transparent default
    /// foreground/background is meaningless for a terminal grid).
    const fn anchors(self) -> [(&'static str, ThemeColor); 2] {
        [
            ("foreground", self.foreground),
            ("background", self.background),
        ]
    }
}

impl Default for ThemePalette {
    fn default() -> Self {
        Self::dark()
    }
}

/// The complete, validated runtime option state.
///
/// Construct with [`RuntimeOptions::default`] (safe defaults) and mutate via
/// [`RuntimeOptions::apply_patch`]. Direct field assignment is allowed for
/// host-side initialisation but bypasses validation — prefer the patch API for
/// any host-driven mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeOptions {
    /// Cursor presentation style.
    pub cursor_style: CursorStyle,
    /// Whether the cursor blinks.
    pub cursor_blink: bool,
    /// Scrollback retention in lines (`0` = no scrollback).
    pub scrollback_lines: u32,
    /// Tab stop width in columns (`1..=16`).
    pub tab_width: u8,
    /// Whether bare LF is treated as CRLF on output (`convertEol`).
    pub convert_eol: bool,
    /// Whether screen-reader assistive output mode is active.
    pub screen_reader_mode: bool,
    /// Whether bracketed paste is enabled.
    pub bracketed_paste: bool,
    /// Minimum enforced foreground/background contrast ratio, ×100
    /// (`100..=2100`). `100` (1.0:1) disables enforcement.
    pub minimum_contrast_ratio_x100: u16,
    /// Active rendering backend.
    pub renderer: RendererType,
    /// Theme palette.
    pub theme: ThemePalette,
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            cursor_style: CursorStyle::Block,
            cursor_blink: false,
            scrollback_lines: 1_000,
            tab_width: 8,
            convert_eol: false,
            screen_reader_mode: false,
            bracketed_paste: true,
            minimum_contrast_ratio_x100: MIN_CONTRAST_RATIO_X100,
            // DOM is the most compatible backend and is advertised by every
            // capability profile, so a freshly-defaulted terminal validates and
            // boots on any engine; the host upgrades to Canvas/WebGL/WebGPU via
            // an explicit option patch once it knows what the engine supports.
            renderer: RendererType::Dom,
            theme: ThemePalette::dark(),
        }
    }
}

impl RuntimeOptions {
    /// Apply a (sparse) patch atomically against the given capability profile.
    ///
    /// Returns a [`RuntimeOptionUpdate`] describing the outcome. On any
    /// validation or gating failure the update is *rejected*, every offending
    /// field's error is reported, and `self` is left unchanged. On success the
    /// patch is committed atomically and the changed fields plus required
    /// renderer/engine synchronisation are reported.
    ///
    /// `correlation_id` is recorded for structured-log triage only; it does not
    /// affect the decision.
    #[must_use]
    pub fn apply_patch(
        &mut self,
        patch: &RuntimeOptionPatch,
        caps: &OptionCapabilityProfile,
        correlation_id: &str,
    ) -> RuntimeOptionUpdate {
        // Build the candidate by overlaying the patch onto the current state.
        let mut candidate = *self;
        if let Some(v) = patch.cursor_style {
            candidate.cursor_style = v;
        }
        if let Some(v) = patch.cursor_blink {
            candidate.cursor_blink = v;
        }
        if let Some(v) = patch.scrollback_lines {
            candidate.scrollback_lines = v;
        }
        if let Some(v) = patch.tab_width {
            candidate.tab_width = v;
        }
        if let Some(v) = patch.convert_eol {
            candidate.convert_eol = v;
        }
        if let Some(v) = patch.screen_reader_mode {
            candidate.screen_reader_mode = v;
        }
        if let Some(v) = patch.bracketed_paste {
            candidate.bracketed_paste = v;
        }
        if let Some(v) = patch.minimum_contrast_ratio_x100 {
            candidate.minimum_contrast_ratio_x100 = v;
        }
        if let Some(v) = patch.renderer {
            candidate.renderer = v;
        }
        if let Some(v) = patch.theme {
            candidate.theme = v;
        }

        // Validate the whole candidate, collecting EVERY error (deterministic,
        // complete diagnostics) before deciding.
        let errors = candidate.validate(caps);
        if !errors.is_empty() {
            return RuntimeOptionUpdate {
                correlation_id: correlation_id.to_owned(),
                applied: false,
                changes: Vec::new(),
                errors,
                requires_repaint: false,
                requires_renderer_reinit: false,
            };
        }

        // Compute the field-level diff against the (still-current) state.
        let changes = self.diff(&candidate);
        let requires_renderer_reinit = changes.iter().any(|c| c.field == FIELD_RENDERER);
        let requires_repaint = changes.iter().any(|c| {
            matches!(
                c.field,
                FIELD_RENDERER
                    | FIELD_THEME
                    | FIELD_SCROLLBACK
                    | FIELD_TAB_WIDTH
                    | FIELD_CURSOR_STYLE
                    | FIELD_MIN_CONTRAST
            )
        });

        // Commit atomically.
        *self = candidate;

        RuntimeOptionUpdate {
            correlation_id: correlation_id.to_owned(),
            applied: true,
            changes,
            errors: Vec::new(),
            requires_repaint,
            requires_renderer_reinit,
        }
    }

    /// Validate this option state against a capability profile, returning every
    /// violation. An empty vector means fully valid and supported.
    #[must_use]
    pub fn validate(&self, caps: &OptionCapabilityProfile) -> Vec<RuntimeOptionError> {
        let mut errors = Vec::new();

        // ── Schema range checks ────────────────────────────────────────────
        if self.scrollback_lines > SCROLLBACK_HARD_MAX {
            errors.push(RuntimeOptionError::OutOfRange {
                field: FIELD_SCROLLBACK,
                value: u64::from(self.scrollback_lines),
                min: 0,
                max: u64::from(SCROLLBACK_HARD_MAX),
            });
        }
        if self.tab_width < MIN_TAB_WIDTH || self.tab_width > MAX_TAB_WIDTH {
            errors.push(RuntimeOptionError::OutOfRange {
                field: FIELD_TAB_WIDTH,
                value: u64::from(self.tab_width),
                min: u64::from(MIN_TAB_WIDTH),
                max: u64::from(MAX_TAB_WIDTH),
            });
        }
        if self.minimum_contrast_ratio_x100 < MIN_CONTRAST_RATIO_X100
            || self.minimum_contrast_ratio_x100 > MAX_CONTRAST_RATIO_X100
        {
            errors.push(RuntimeOptionError::OutOfRange {
                field: FIELD_MIN_CONTRAST,
                value: u64::from(self.minimum_contrast_ratio_x100),
                min: u64::from(MIN_CONTRAST_RATIO_X100),
                max: u64::from(MAX_CONTRAST_RATIO_X100),
            });
        }

        // Theme anchors must be opaque.
        for (slot, color) in self.theme.anchors() {
            if !color.is_opaque() {
                errors.push(RuntimeOptionError::TransparentAnchorColor {
                    field: FIELD_THEME,
                    slot,
                    alpha: color.a(),
                });
            }
        }

        // ── Capability / profile gating ────────────────────────────────────
        // Gating only applies to otherwise schema-valid values, so a single
        // field never reports both an out-of-range and a gating error (an
        // over-hard-max scrollback is nonsense, not merely unsupported here).
        if self.scrollback_lines <= SCROLLBACK_HARD_MAX
            && self.scrollback_lines > caps.max_scrollback_lines
        {
            errors.push(RuntimeOptionError::CapabilityGated {
                field: FIELD_SCROLLBACK,
                detail: GatingDetail::ScrollbackBudgetExceeded {
                    requested: self.scrollback_lines,
                    budget: caps.max_scrollback_lines,
                },
            });
        }
        if !caps.supports_renderer(self.renderer) {
            errors.push(RuntimeOptionError::CapabilityGated {
                field: FIELD_RENDERER,
                detail: GatingDetail::RendererUnsupported {
                    requested: self.renderer,
                },
            });
        }

        errors
    }

    /// Compute the ordered field-level diff between `self` and `next`.
    fn diff(&self, next: &Self) -> Vec<OptionFieldChange> {
        let mut changes = Vec::new();
        if self.cursor_style != next.cursor_style {
            changes.push(OptionFieldChange::new(
                FIELD_CURSOR_STYLE,
                self.cursor_style.to_string(),
                next.cursor_style.to_string(),
            ));
        }
        if self.cursor_blink != next.cursor_blink {
            changes.push(OptionFieldChange::new(
                FIELD_CURSOR_BLINK,
                self.cursor_blink.to_string(),
                next.cursor_blink.to_string(),
            ));
        }
        if self.scrollback_lines != next.scrollback_lines {
            changes.push(OptionFieldChange::new(
                FIELD_SCROLLBACK,
                self.scrollback_lines.to_string(),
                next.scrollback_lines.to_string(),
            ));
        }
        if self.tab_width != next.tab_width {
            changes.push(OptionFieldChange::new(
                FIELD_TAB_WIDTH,
                self.tab_width.to_string(),
                next.tab_width.to_string(),
            ));
        }
        if self.convert_eol != next.convert_eol {
            changes.push(OptionFieldChange::new(
                FIELD_CONVERT_EOL,
                self.convert_eol.to_string(),
                next.convert_eol.to_string(),
            ));
        }
        if self.screen_reader_mode != next.screen_reader_mode {
            changes.push(OptionFieldChange::new(
                FIELD_SCREEN_READER,
                self.screen_reader_mode.to_string(),
                next.screen_reader_mode.to_string(),
            ));
        }
        if self.bracketed_paste != next.bracketed_paste {
            changes.push(OptionFieldChange::new(
                FIELD_BRACKETED_PASTE,
                self.bracketed_paste.to_string(),
                next.bracketed_paste.to_string(),
            ));
        }
        if self.minimum_contrast_ratio_x100 != next.minimum_contrast_ratio_x100 {
            changes.push(OptionFieldChange::new(
                FIELD_MIN_CONTRAST,
                self.minimum_contrast_ratio_x100.to_string(),
                next.minimum_contrast_ratio_x100.to_string(),
            ));
        }
        if self.renderer != next.renderer {
            changes.push(OptionFieldChange::new(
                FIELD_RENDERER,
                self.renderer.to_string(),
                next.renderer.to_string(),
            ));
        }
        if self.theme != next.theme {
            changes.push(OptionFieldChange::new(
                FIELD_THEME,
                self.theme.foreground.as_hex(),
                next.theme.foreground.as_hex(),
            ));
        }
        changes
    }

    /// Stable, ordered CSS data-attribute pairs for the browser host. Mirrors
    /// the accessibility surface's `web_dataset()` convention so generated DOM
    /// markup is byte-for-byte reproducible.
    #[must_use]
    pub fn web_dataset(&self) -> [(&'static str, String); 3] {
        [
            ("data-ftui-cursor-style", self.cursor_style.to_string()),
            ("data-ftui-renderer", self.renderer.to_string()),
            (
                "data-ftui-screen-reader",
                self.screen_reader_mode.to_string(),
            ),
        ]
    }

    /// Serialise the full option snapshot to a single deterministic JSON line.
    #[must_use]
    pub fn to_jsonl(&self, correlation_id: &str) -> String {
        format!(
            concat!(
                "{{\"event\":\"runtime_options_snapshot\",\"correlation_id\":\"{cid}\",",
                "\"cursor_style\":\"{cs}\",\"cursor_blink\":{cb},\"scrollback_lines\":{sb},",
                "\"tab_width\":{tw},\"convert_eol\":{ce},\"screen_reader_mode\":{srm},",
                "\"bracketed_paste\":{bp},\"minimum_contrast_ratio_x100\":{mcr},",
                "\"renderer\":\"{rt}\",\"theme_foreground\":\"{fg}\",\"theme_background\":\"{bg}\"}}"
            ),
            cid = escape_json(correlation_id),
            cs = self.cursor_style,
            cb = self.cursor_blink,
            sb = self.scrollback_lines,
            tw = self.tab_width,
            ce = self.convert_eol,
            srm = self.screen_reader_mode,
            bp = self.bracketed_paste,
            mcr = self.minimum_contrast_ratio_x100,
            rt = self.renderer,
            fg = self.theme.foreground.as_hex(),
            bg = self.theme.background.as_hex(),
        )
    }
}

// Canonical field identifiers — single source of truth for diffs, errors, and
// serialisation so the strings never drift between producers.
const FIELD_CURSOR_STYLE: &str = "cursor_style";
const FIELD_CURSOR_BLINK: &str = "cursor_blink";
const FIELD_SCROLLBACK: &str = "scrollback_lines";
const FIELD_TAB_WIDTH: &str = "tab_width";
const FIELD_CONVERT_EOL: &str = "convert_eol";
const FIELD_SCREEN_READER: &str = "screen_reader_mode";
const FIELD_BRACKETED_PASTE: &str = "bracketed_paste";
const FIELD_MIN_CONTRAST: &str = "minimum_contrast_ratio_x100";
const FIELD_RENDERER: &str = "renderer";
const FIELD_THEME: &str = "theme";

/// A sparse patch: every field is independently `Some(_)` to mutate or `None` to
/// leave untouched. Mirrors assigning to a subset of `xterm.js` `term.options`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RuntimeOptionPatch {
    /// See [`RuntimeOptions::cursor_style`].
    pub cursor_style: Option<CursorStyle>,
    /// See [`RuntimeOptions::cursor_blink`].
    pub cursor_blink: Option<bool>,
    /// See [`RuntimeOptions::scrollback_lines`].
    pub scrollback_lines: Option<u32>,
    /// See [`RuntimeOptions::tab_width`].
    pub tab_width: Option<u8>,
    /// See [`RuntimeOptions::convert_eol`].
    pub convert_eol: Option<bool>,
    /// See [`RuntimeOptions::screen_reader_mode`].
    pub screen_reader_mode: Option<bool>,
    /// See [`RuntimeOptions::bracketed_paste`].
    pub bracketed_paste: Option<bool>,
    /// See [`RuntimeOptions::minimum_contrast_ratio_x100`].
    pub minimum_contrast_ratio_x100: Option<u16>,
    /// See [`RuntimeOptions::renderer`].
    pub renderer: Option<RendererType>,
    /// See [`RuntimeOptions::theme`].
    pub theme: Option<ThemePalette>,
}

impl RuntimeOptionPatch {
    /// An empty patch (no-op).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            cursor_style: None,
            cursor_blink: None,
            scrollback_lines: None,
            tab_width: None,
            convert_eol: None,
            screen_reader_mode: None,
            bracketed_paste: None,
            minimum_contrast_ratio_x100: None,
            renderer: None,
            theme: None,
        }
    }

    /// `true` when the patch mutates nothing.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.cursor_style.is_none()
            && self.cursor_blink.is_none()
            && self.scrollback_lines.is_none()
            && self.tab_width.is_none()
            && self.convert_eol.is_none()
            && self.screen_reader_mode.is_none()
            && self.bracketed_paste.is_none()
            && self.minimum_contrast_ratio_x100.is_none()
            && self.renderer.is_none()
            && self.theme.is_none()
    }

    /// Builder: set cursor style.
    #[must_use]
    pub const fn with_cursor_style(mut self, v: CursorStyle) -> Self {
        self.cursor_style = Some(v);
        self
    }
    /// Builder: set scrollback retention.
    #[must_use]
    pub const fn with_scrollback_lines(mut self, v: u32) -> Self {
        self.scrollback_lines = Some(v);
        self
    }
    /// Builder: set tab width.
    #[must_use]
    pub const fn with_tab_width(mut self, v: u8) -> Self {
        self.tab_width = Some(v);
        self
    }
    /// Builder: set screen-reader mode.
    #[must_use]
    pub const fn with_screen_reader_mode(mut self, v: bool) -> Self {
        self.screen_reader_mode = Some(v);
        self
    }
    /// Builder: set minimum contrast ratio ×100.
    #[must_use]
    pub const fn with_minimum_contrast_ratio_x100(mut self, v: u16) -> Self {
        self.minimum_contrast_ratio_x100 = Some(v);
        self
    }
    /// Builder: set renderer backend.
    #[must_use]
    pub const fn with_renderer(mut self, v: RendererType) -> Self {
        self.renderer = Some(v);
        self
    }
    /// Builder: set theme palette.
    #[must_use]
    pub const fn with_theme(mut self, v: ThemePalette) -> Self {
        self.theme = Some(v);
        self
    }
}

/// What the active rendering engine / host advertises as supported. A
/// schema-valid option can still be gated against this profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OptionCapabilityProfile {
    /// Whether the DOM renderer is available (effectively always true).
    pub dom: bool,
    /// Whether the 2D canvas renderer is available.
    pub canvas: bool,
    /// Whether the WebGL renderer is available.
    pub webgl: bool,
    /// Whether the WebGPU renderer is available.
    pub webgpu: bool,
    /// Maximum color fidelity (informational; theme colors stay host-downgraded
    /// rather than gated).
    pub color_depth: ColorDepth,
    /// Maximum scrollback the host will retain in memory.
    pub max_scrollback_lines: u32,
}

impl OptionCapabilityProfile {
    /// A conservative default profile: DOM + Canvas only, modest scrollback.
    #[must_use]
    pub const fn minimal() -> Self {
        Self {
            dom: true,
            canvas: true,
            webgl: false,
            webgpu: false,
            color_depth: ColorDepth::Ansi16,
            max_scrollback_lines: 10_000,
        }
    }

    /// A fully-featured profile: every renderer, generous scrollback.
    #[must_use]
    pub const fn full() -> Self {
        Self {
            dom: true,
            canvas: true,
            webgl: true,
            webgpu: true,
            color_depth: ColorDepth::TrueColor,
            max_scrollback_lines: SCROLLBACK_HARD_MAX,
        }
    }

    /// Whether the requested renderer backend is available.
    #[must_use]
    pub const fn supports_renderer(&self, renderer: RendererType) -> bool {
        match renderer {
            RendererType::Dom => self.dom,
            RendererType::Canvas => self.canvas,
            RendererType::WebGl => self.webgl,
            RendererType::WebGpu => self.webgpu,
        }
    }
}

impl Default for OptionCapabilityProfile {
    fn default() -> Self {
        Self::minimal()
    }
}

/// A single field that changed in a committed update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionFieldChange {
    /// Canonical field identifier.
    pub field: &'static str,
    /// Previous value, rendered as a stable token.
    pub old: String,
    /// New value, rendered as a stable token.
    pub new: String,
}

impl OptionFieldChange {
    fn new(field: &'static str, old: String, new: String) -> Self {
        Self { field, old, new }
    }
}

/// Detail for a capability-gating rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatingDetail {
    /// The requested renderer is not advertised by the host.
    RendererUnsupported {
        /// The renderer that was requested.
        requested: RendererType,
    },
    /// The requested scrollback exceeds the host's memory budget.
    ScrollbackBudgetExceeded {
        /// Requested scrollback retention.
        requested: u32,
        /// The host's advertised budget.
        budget: u32,
    },
}

/// A single, explicit option validation/gating failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeOptionError {
    /// A numeric field is outside its allowed schema range.
    OutOfRange {
        /// Canonical field identifier.
        field: &'static str,
        /// The offending value.
        value: u64,
        /// Inclusive minimum.
        min: u64,
        /// Inclusive maximum.
        max: u64,
    },
    /// A theme anchor colour (foreground/background) was not fully opaque.
    TransparentAnchorColor {
        /// Canonical field identifier (`theme`).
        field: &'static str,
        /// The offending palette slot.
        slot: &'static str,
        /// The non-opaque alpha value.
        alpha: u8,
    },
    /// The value is schema-valid but unsupported by the active capability
    /// profile.
    CapabilityGated {
        /// Canonical field identifier.
        field: &'static str,
        /// Specific gating reason.
        detail: GatingDetail,
    },
}

impl RuntimeOptionError {
    /// The canonical field identifier this error concerns.
    #[must_use]
    pub const fn field(&self) -> &'static str {
        match self {
            Self::OutOfRange { field, .. }
            | Self::TransparentAnchorColor { field, .. }
            | Self::CapabilityGated { field, .. } => field,
        }
    }

    /// A stable machine token for the error kind (used in structured logs).
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::OutOfRange { .. } => "out_of_range",
            Self::TransparentAnchorColor { .. } => "transparent_anchor_color",
            Self::CapabilityGated { .. } => "capability_gated",
        }
    }
}

impl fmt::Display for RuntimeOptionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfRange {
                field,
                value,
                min,
                max,
            } => write!(
                f,
                "option `{field}` value {value} is out of range [{min}, {max}]"
            ),
            Self::TransparentAnchorColor { field, slot, alpha } => write!(
                f,
                "option `{field}` anchor colour `{slot}` must be opaque (alpha {alpha} != 255)"
            ),
            Self::CapabilityGated { field, detail } => match detail {
                GatingDetail::RendererUnsupported { requested } => write!(
                    f,
                    "option `{field}` renderer `{requested}` is not supported by the active host"
                ),
                GatingDetail::ScrollbackBudgetExceeded { requested, budget } => write!(
                    f,
                    "option `{field}` scrollback {requested} exceeds host budget {budget}"
                ),
            },
        }
    }
}

impl std::error::Error for RuntimeOptionError {}

/// The outcome of an [`RuntimeOptions::apply_patch`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeOptionUpdate {
    /// Correlation id carried through for structured-log triage.
    pub correlation_id: String,
    /// `true` when the patch was committed; `false` means rejected and rolled
    /// back (options unchanged).
    pub applied: bool,
    /// The fields that actually changed (empty when rejected or a no-op).
    pub changes: Vec<OptionFieldChange>,
    /// Every validation/gating error (empty when applied).
    pub errors: Vec<RuntimeOptionError>,
    /// Whether the host must repaint the grid to reflect the change.
    pub requires_repaint: bool,
    /// Whether the host must tear down and re-create the renderer backend.
    pub requires_renderer_reinit: bool,
}

impl RuntimeOptionUpdate {
    /// `true` when applied but nothing actually changed (idempotent patch).
    #[must_use]
    pub fn is_noop(&self) -> bool {
        self.applied && self.changes.is_empty()
    }

    /// Serialise the outcome to a single deterministic JSON line, including the
    /// full change set and (on rejection) every error. This is the canonical
    /// applied/rejected audit artifact.
    #[must_use]
    pub fn to_jsonl(&self) -> String {
        let mut changes_json = String::from("[");
        for (i, c) in self.changes.iter().enumerate() {
            if i > 0 {
                changes_json.push(',');
            }
            changes_json.push_str(&format!(
                "{{\"field\":\"{}\",\"old\":\"{}\",\"new\":\"{}\"}}",
                c.field,
                escape_json(&c.old),
                escape_json(&c.new),
            ));
        }
        changes_json.push(']');

        let mut errors_json = String::from("[");
        for (i, e) in self.errors.iter().enumerate() {
            if i > 0 {
                errors_json.push(',');
            }
            errors_json.push_str(&format!(
                "{{\"field\":\"{}\",\"kind\":\"{}\",\"detail\":\"{}\"}}",
                e.field(),
                e.kind(),
                escape_json(&e.to_string()),
            ));
        }
        errors_json.push(']');

        format!(
            concat!(
                "{{\"event\":\"runtime_option_update\",\"correlation_id\":\"{cid}\",",
                "\"applied\":{applied},\"requires_repaint\":{rp},",
                "\"requires_renderer_reinit\":{rr},\"change_count\":{cc},",
                "\"error_count\":{ec},\"changes\":{changes},\"errors\":{errors}}}"
            ),
            cid = escape_json(&self.correlation_id),
            applied = self.applied,
            rp = self.requires_repaint,
            rr = self.requires_renderer_reinit,
            cc = self.changes.len(),
            ec = self.errors.len(),
            changes = changes_json,
            errors = errors_json,
        )
    }
}

/// Errors from parsing a JSON option patch (shape/syntax), distinct from
/// validation/gating errors which are surfaced by [`RuntimeOptions::apply_patch`].
#[cfg(feature = "input-parser")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptionPatchParseError {
    /// The payload was not valid JSON or not a JSON object.
    MalformedJson(String),
    /// An unknown option key was present (strict parsing rejects typos).
    UnknownKey(String),
    /// A key held a value of the wrong JSON type.
    TypeMismatch {
        /// The offending key.
        key: String,
        /// The JSON type that was expected.
        expected: &'static str,
    },
    /// An enum-valued field held an unrecognised token.
    InvalidToken {
        /// The offending key.
        key: String,
        /// The unrecognised token.
        token: String,
    },
}

#[cfg(feature = "input-parser")]
impl fmt::Display for OptionPatchParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedJson(msg) => write!(f, "malformed option patch JSON: {msg}"),
            Self::UnknownKey(key) => write!(f, "unknown option key `{key}`"),
            Self::TypeMismatch { key, expected } => {
                write!(f, "option key `{key}` expected a {expected}")
            }
            Self::InvalidToken { key, token } => {
                write!(f, "option key `{key}` has invalid token `{token}`")
            }
        }
    }
}

#[cfg(feature = "input-parser")]
impl std::error::Error for OptionPatchParseError {}

#[cfg(feature = "input-parser")]
impl RuntimeOptionPatch {
    /// Parse a sparse option patch from a JSON object (the exact shape a JS host
    /// produces from a `term.options = {...}` assignment).
    ///
    /// Strict: unknown keys, wrong JSON types, and unrecognised enum tokens are
    /// rejected with an actionable [`OptionPatchParseError`] rather than silently
    /// ignored. Numeric *range* checks are intentionally deferred to
    /// [`RuntimeOptions::apply_patch`] so that parse errors (shape) stay distinct
    /// from validation errors (semantics).
    ///
    /// # Errors
    ///
    /// Returns [`OptionPatchParseError`] when the payload is not a JSON object,
    /// contains an unknown key, has a type mismatch, or carries an invalid enum
    /// token.
    pub fn parse_json(input: &str) -> Result<Self, OptionPatchParseError> {
        use serde_json::Value;

        let value: Value = serde_json::from_str(input)
            .map_err(|e| OptionPatchParseError::MalformedJson(e.to_string()))?;
        let obj = value.as_object().ok_or_else(|| {
            OptionPatchParseError::MalformedJson("top-level value must be an object".to_owned())
        })?;

        let mut patch = Self::new();

        for (key, val) in obj {
            match key.as_str() {
                "cursorStyle" => {
                    let token = expect_str(key, val)?;
                    patch.cursor_style = Some(CursorStyle::from_token(token).ok_or_else(|| {
                        OptionPatchParseError::InvalidToken {
                            key: key.clone(),
                            token: token.to_owned(),
                        }
                    })?);
                }
                "cursorBlink" => patch.cursor_blink = Some(expect_bool(key, val)?),
                "scrollback" => patch.scrollback_lines = Some(expect_u32(key, val)?),
                "tabStopWidth" => patch.tab_width = Some(expect_u8(key, val)?),
                "convertEol" => patch.convert_eol = Some(expect_bool(key, val)?),
                "screenReaderMode" => patch.screen_reader_mode = Some(expect_bool(key, val)?),
                "bracketedPaste" => patch.bracketed_paste = Some(expect_bool(key, val)?),
                "minimumContrastRatioX100" => {
                    patch.minimum_contrast_ratio_x100 = Some(expect_u16(key, val)?);
                }
                "rendererType" => {
                    let token = expect_str(key, val)?;
                    patch.renderer = Some(RendererType::from_token(token).ok_or_else(|| {
                        OptionPatchParseError::InvalidToken {
                            key: key.clone(),
                            token: token.to_owned(),
                        }
                    })?);
                }
                "theme" => patch.theme = Some(parse_theme(key, val)?),
                other => return Err(OptionPatchParseError::UnknownKey(other.to_owned())),
            }
        }

        Ok(patch)
    }
}

#[cfg(feature = "input-parser")]
fn expect_str<'a>(key: &str, val: &'a serde_json::Value) -> Result<&'a str, OptionPatchParseError> {
    val.as_str()
        .ok_or_else(|| OptionPatchParseError::TypeMismatch {
            key: key.to_owned(),
            expected: "string",
        })
}

#[cfg(feature = "input-parser")]
fn expect_bool(key: &str, val: &serde_json::Value) -> Result<bool, OptionPatchParseError> {
    val.as_bool()
        .ok_or_else(|| OptionPatchParseError::TypeMismatch {
            key: key.to_owned(),
            expected: "boolean",
        })
}

#[cfg(feature = "input-parser")]
fn expect_u64(key: &str, val: &serde_json::Value) -> Result<u64, OptionPatchParseError> {
    val.as_u64()
        .ok_or_else(|| OptionPatchParseError::TypeMismatch {
            key: key.to_owned(),
            expected: "non-negative integer",
        })
}

#[cfg(feature = "input-parser")]
fn expect_u32(key: &str, val: &serde_json::Value) -> Result<u32, OptionPatchParseError> {
    // Saturate at u32::MAX so an absurd payload becomes a clean range rejection
    // at apply time rather than a parse-time overflow.
    Ok(u32::try_from(expect_u64(key, val)?).unwrap_or(u32::MAX))
}

#[cfg(feature = "input-parser")]
fn expect_u16(key: &str, val: &serde_json::Value) -> Result<u16, OptionPatchParseError> {
    Ok(u16::try_from(expect_u64(key, val)?).unwrap_or(u16::MAX))
}

#[cfg(feature = "input-parser")]
fn expect_u8(key: &str, val: &serde_json::Value) -> Result<u8, OptionPatchParseError> {
    Ok(u8::try_from(expect_u64(key, val)?).unwrap_or(u8::MAX))
}

#[cfg(feature = "input-parser")]
fn parse_color(key: &str, val: &serde_json::Value) -> Result<ThemeColor, OptionPatchParseError> {
    let token = expect_str(key, val)?;
    ThemeColor::from_hex(token).ok_or_else(|| OptionPatchParseError::InvalidToken {
        key: key.to_owned(),
        token: token.to_owned(),
    })
}

#[cfg(feature = "input-parser")]
fn parse_theme(key: &str, val: &serde_json::Value) -> Result<ThemePalette, OptionPatchParseError> {
    let obj = val
        .as_object()
        .ok_or_else(|| OptionPatchParseError::TypeMismatch {
            key: key.to_owned(),
            expected: "object",
        })?;
    // Start from the default palette; the theme object overlays named slots.
    let mut palette = ThemePalette::dark();
    for (slot, color_val) in obj {
        match slot.as_str() {
            "foreground" => palette.foreground = parse_color(slot, color_val)?,
            "background" => palette.background = parse_color(slot, color_val)?,
            "cursor" => palette.cursor = parse_color(slot, color_val)?,
            "cursorAccent" => palette.cursor_accent = parse_color(slot, color_val)?,
            "selectionBackground" => {
                palette.selection_background = parse_color(slot, color_val)?;
            }
            other => {
                // Accept ansi0..=ansi15; reject anything else.
                if let Some(idx) = other
                    .strip_prefix("ansi")
                    .and_then(|n| n.parse::<usize>().ok())
                    .filter(|i| *i < 16)
                {
                    palette.ansi[idx] = parse_color(other, color_val)?;
                } else {
                    return Err(OptionPatchParseError::UnknownKey(format!("theme.{other}")));
                }
            }
        }
    }
    Ok(palette)
}

/// Escape a string for embedding inside a JSON double-quoted value.
fn escape_json(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 2);
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Defaults & schema construction ──────────────────────────────────────

    #[test]
    fn defaults_are_valid_under_full_caps() {
        let opts = RuntimeOptions::default();
        assert!(opts.validate(&OptionCapabilityProfile::full()).is_empty());
    }

    #[test]
    fn default_renderer_is_valid_under_minimal_caps() {
        // The default renderer (DOM) must be supported by the minimal profile
        // so a fresh terminal always boots.
        let opts = RuntimeOptions::default();
        assert!(
            opts.validate(&OptionCapabilityProfile::minimal())
                .is_empty()
        );
    }

    #[test]
    fn empty_patch_is_noop() {
        let mut opts = RuntimeOptions::default();
        let before = opts;
        let patch = RuntimeOptionPatch::new();
        assert!(patch.is_empty());
        let update = opts.apply_patch(&patch, &OptionCapabilityProfile::full(), "noop");
        assert!(update.applied);
        assert!(update.is_noop());
        assert!(update.changes.is_empty());
        assert_eq!(opts, before);
    }

    // ── Successful atomic apply ─────────────────────────────────────────────

    #[test]
    fn apply_commits_and_reports_changed_fields() {
        let mut opts = RuntimeOptions::default();
        let patch = RuntimeOptionPatch::new()
            .with_cursor_style(CursorStyle::Bar)
            .with_scrollback_lines(5_000)
            .with_screen_reader_mode(true);
        let update = opts.apply_patch(&patch, &OptionCapabilityProfile::full(), "c1");
        assert!(update.applied);
        assert_eq!(update.changes.len(), 3);
        assert_eq!(opts.cursor_style, CursorStyle::Bar);
        assert_eq!(opts.scrollback_lines, 5_000);
        assert!(opts.screen_reader_mode);
        // cursor_style + scrollback both demand a repaint; no renderer change.
        assert!(update.requires_repaint);
        assert!(!update.requires_renderer_reinit);
    }

    #[test]
    fn idempotent_apply_changes_nothing() {
        let mut opts = RuntimeOptions::default();
        let patch = RuntimeOptionPatch::new().with_cursor_style(opts.cursor_style);
        let update = opts.apply_patch(&patch, &OptionCapabilityProfile::full(), "id");
        assert!(update.applied);
        assert!(update.is_noop());
        assert!(!update.requires_repaint);
    }

    #[test]
    fn renderer_change_requires_reinit_and_repaint() {
        let mut opts = RuntimeOptions::default();
        let patch = RuntimeOptionPatch::new().with_renderer(RendererType::WebGpu);
        let update = opts.apply_patch(&patch, &OptionCapabilityProfile::full(), "r1");
        assert!(update.applied);
        assert!(update.requires_renderer_reinit);
        assert!(update.requires_repaint);
        assert_eq!(opts.renderer, RendererType::WebGpu);
    }

    // ── Atomic rollback on invalid payloads ─────────────────────────────────

    #[test]
    fn out_of_range_rejects_and_rolls_back_entire_patch() {
        let mut opts = RuntimeOptions::default();
        let before = opts;
        // tab_width is valid, but minimum_contrast is absurd → whole patch fails.
        let patch = RuntimeOptionPatch::new()
            .with_tab_width(4)
            .with_minimum_contrast_ratio_x100(9_999);
        let update = opts.apply_patch(&patch, &OptionCapabilityProfile::full(), "bad");
        assert!(!update.applied);
        assert_eq!(update.changes.len(), 0);
        assert_eq!(update.errors.len(), 1);
        assert_eq!(update.errors[0].field(), FIELD_MIN_CONTRAST);
        // Rollback: even the *valid* tab_width must NOT have been applied.
        assert_eq!(opts, before);
    }

    #[test]
    fn all_errors_collected_not_just_first() {
        let mut opts = RuntimeOptions::default();
        let patch = RuntimeOptionPatch::new()
            .with_tab_width(0) // below MIN_TAB_WIDTH
            .with_minimum_contrast_ratio_x100(5) // below MIN_CONTRAST
            .with_scrollback_lines(SCROLLBACK_HARD_MAX + 1); // above hard max
        let update = opts.apply_patch(&patch, &OptionCapabilityProfile::full(), "multi");
        assert!(!update.applied);
        assert_eq!(update.errors.len(), 3);
        let fields: Vec<&str> = update
            .errors
            .iter()
            .map(RuntimeOptionError::field)
            .collect();
        assert!(fields.contains(&FIELD_TAB_WIDTH));
        assert!(fields.contains(&FIELD_MIN_CONTRAST));
        assert!(fields.contains(&FIELD_SCROLLBACK));
    }

    #[test]
    fn transparent_anchor_color_is_rejected() {
        let mut opts = RuntimeOptions::default();
        let mut theme = ThemePalette::dark();
        theme.background = ThemeColor::rgba(0x10, 0x10, 0x10, 0x80); // semi-transparent
        let patch = RuntimeOptionPatch::new().with_theme(theme);
        let update = opts.apply_patch(&patch, &OptionCapabilityProfile::full(), "tc");
        assert!(!update.applied);
        assert!(matches!(
            update.errors[0],
            RuntimeOptionError::TransparentAnchorColor { .. }
        ));
    }

    #[test]
    fn transparent_selection_is_allowed() {
        // Only foreground/background anchors must be opaque; selection may be
        // translucent (that is its whole purpose).
        let mut opts = RuntimeOptions::default();
        let mut theme = ThemePalette::dark();
        theme.selection_background = ThemeColor::rgba(0x80, 0x80, 0xFF, 0x40);
        let patch = RuntimeOptionPatch::new().with_theme(theme);
        let update = opts.apply_patch(&patch, &OptionCapabilityProfile::full(), "sel");
        assert!(update.applied);
    }

    // ── Capability / profile gating ─────────────────────────────────────────

    #[test]
    fn renderer_gated_when_unsupported() {
        let mut opts = RuntimeOptions::default();
        let before = opts;
        let patch = RuntimeOptionPatch::new().with_renderer(RendererType::WebGpu);
        let update = opts.apply_patch(&patch, &OptionCapabilityProfile::minimal(), "g1");
        assert!(!update.applied);
        assert!(matches!(
            update.errors[0],
            RuntimeOptionError::CapabilityGated {
                detail: GatingDetail::RendererUnsupported { .. },
                ..
            }
        ));
        assert_eq!(opts, before);
    }

    #[test]
    fn scrollback_gated_by_host_budget() {
        let mut opts = RuntimeOptions::default();
        let caps = OptionCapabilityProfile::minimal(); // budget 10_000
        let patch = RuntimeOptionPatch::new().with_scrollback_lines(50_000);
        let update = opts.apply_patch(&patch, &caps, "g2");
        assert!(!update.applied);
        assert!(matches!(
            update.errors[0],
            RuntimeOptionError::CapabilityGated {
                detail: GatingDetail::ScrollbackBudgetExceeded { .. },
                ..
            }
        ));
    }

    #[test]
    fn same_renderer_under_minimal_caps_is_ok() {
        let mut opts = RuntimeOptions::default();
        let patch = RuntimeOptionPatch::new().with_renderer(RendererType::Dom);
        let update = opts.apply_patch(&patch, &OptionCapabilityProfile::minimal(), "g3");
        assert!(update.applied);
    }

    // ── Determinism & serialisation ─────────────────────────────────────────

    #[test]
    fn apply_is_deterministic() {
        let patch = RuntimeOptionPatch::new()
            .with_cursor_style(CursorStyle::Underline)
            .with_renderer(RendererType::WebGl);
        let caps = OptionCapabilityProfile::full();
        let mut a = RuntimeOptions::default();
        let mut b = RuntimeOptions::default();
        let ua = a.apply_patch(&patch, &caps, "det");
        let ub = b.apply_patch(&patch, &caps, "det");
        assert_eq!(ua, ub);
        assert_eq!(a, b);
        assert_eq!(ua.to_jsonl(), ub.to_jsonl());
    }

    #[test]
    fn update_jsonl_is_single_line_and_complete() {
        let mut opts = RuntimeOptions::default();
        let patch = RuntimeOptionPatch::new().with_cursor_style(CursorStyle::Bar);
        let line = opts
            .apply_patch(&patch, &OptionCapabilityProfile::full(), "j1")
            .to_jsonl();
        assert!(line.starts_with('{') && line.ends_with('}'));
        assert!(!line.contains('\n'));
        assert!(line.contains("\"event\":\"runtime_option_update\""));
        assert!(line.contains("\"correlation_id\":\"j1\""));
        assert!(line.contains("\"applied\":true"));
        assert!(line.contains("\"field\":\"cursor_style\""));
        assert!(line.contains("\"old\":\"block\""));
        assert!(line.contains("\"new\":\"bar\""));
    }

    #[test]
    fn rejected_update_jsonl_carries_errors() {
        let mut opts = RuntimeOptions::default();
        let patch = RuntimeOptionPatch::new().with_renderer(RendererType::WebGpu);
        let line = opts
            .apply_patch(&patch, &OptionCapabilityProfile::minimal(), "j2")
            .to_jsonl();
        assert!(line.contains("\"applied\":false"));
        assert!(line.contains("\"error_count\":1"));
        assert!(line.contains("\"kind\":\"capability_gated\""));
        assert!(line.contains("\"field\":\"renderer\""));
    }

    #[test]
    fn snapshot_jsonl_round_trips_key_fields() {
        let opts = RuntimeOptions::default();
        let line = opts.to_jsonl("snap");
        assert!(line.contains("\"cursor_style\":\"block\""));
        assert!(line.contains("\"renderer\":\"dom\""));
        assert!(line.contains("\"scrollback_lines\":1000"));
        assert!(line.contains("\"theme_background\":\"#000000ff\""));
    }

    #[test]
    fn web_dataset_keys_are_stable() {
        let opts = RuntimeOptions::default();
        let ds = opts.web_dataset();
        assert_eq!(
            ds.clone().map(|(k, _)| k),
            [
                "data-ftui-cursor-style",
                "data-ftui-renderer",
                "data-ftui-screen-reader",
            ]
        );
        assert_eq!(ds[0].1, "block");
    }

    // ── Token & colour helpers ──────────────────────────────────────────────

    #[test]
    fn cursor_and_renderer_token_round_trip() {
        for s in [CursorStyle::Block, CursorStyle::Underline, CursorStyle::Bar] {
            assert_eq!(CursorStyle::from_token(s.as_token()), Some(s));
        }
        for r in [
            RendererType::Dom,
            RendererType::Canvas,
            RendererType::WebGl,
            RendererType::WebGpu,
        ] {
            assert_eq!(RendererType::from_token(r.as_token()), Some(r));
        }
        assert_eq!(CursorStyle::from_token("nope"), None);
        assert_eq!(RendererType::from_token("nope"), None);
    }

    #[test]
    fn theme_color_hex_round_trips() {
        let c = ThemeColor::rgba(0x12, 0x34, 0x56, 0x78);
        assert_eq!(c.as_hex(), "#12345678");
        assert_eq!(ThemeColor::from_hex("#12345678"), Some(c));
        // 6-digit form implies opaque.
        assert_eq!(
            ThemeColor::from_hex("#abcdef"),
            Some(ThemeColor::rgb(0xAB, 0xCD, 0xEF))
        );
        assert_eq!(ThemeColor::from_hex("zzz"), None);
        assert_eq!(ThemeColor::from_hex("#12"), None);
    }

    #[test]
    fn jsonl_escapes_correlation_id() {
        let mut opts = RuntimeOptions::default();
        let update = opts.apply_patch(
            &RuntimeOptionPatch::new(),
            &OptionCapabilityProfile::full(),
            "a\"b\\c",
        );
        let line = update.to_jsonl();
        assert!(line.contains(r#""correlation_id":"a\"b\\c""#));
    }

    // ── JSON patch parsing (feature-gated) ──────────────────────────────────

    #[cfg(feature = "input-parser")]
    mod json {
        use super::*;

        #[test]
        fn parses_full_patch() {
            let json = r##"{
                "cursorStyle":"bar","cursorBlink":true,"scrollback":5000,
                "tabStopWidth":4,"convertEol":true,"screenReaderMode":true,
                "bracketedPaste":false,"minimumContrastRatioX100":450,
                "rendererType":"webgpu",
                "theme":{"foreground":"#c0c0c0","background":"#101010","ansi3":"#ffcc00"}
            }"##;
            let patch = RuntimeOptionPatch::parse_json(json).expect("valid patch");
            assert_eq!(patch.cursor_style, Some(CursorStyle::Bar));
            assert_eq!(patch.scrollback_lines, Some(5000));
            assert_eq!(patch.tab_width, Some(4));
            assert_eq!(patch.renderer, Some(RendererType::WebGpu));
            let theme = patch.theme.expect("theme present");
            assert_eq!(theme.background, ThemeColor::rgb(0x10, 0x10, 0x10));
            assert_eq!(theme.ansi[3], ThemeColor::rgb(0xFF, 0xCC, 0x00));
        }

        #[test]
        fn parse_then_apply_end_to_end() {
            let mut opts = RuntimeOptions::default();
            let patch = RuntimeOptionPatch::parse_json(r#"{"cursorStyle":"underline"}"#).unwrap();
            let update = opts.apply_patch(&patch, &OptionCapabilityProfile::full(), "e2e");
            assert!(update.applied);
            assert_eq!(opts.cursor_style, CursorStyle::Underline);
        }

        #[test]
        fn malformed_json_is_actionable() {
            let err = RuntimeOptionPatch::parse_json(r#"{"cursorStyle":"#).unwrap_err();
            assert!(matches!(err, OptionPatchParseError::MalformedJson(_)));
            assert!(err.to_string().contains("malformed"));
        }

        #[test]
        fn unknown_key_rejected() {
            let err = RuntimeOptionPatch::parse_json(r#"{"bogusKey":1}"#).unwrap_err();
            assert_eq!(
                err,
                OptionPatchParseError::UnknownKey("bogusKey".to_owned())
            );
        }

        #[test]
        fn type_mismatch_rejected() {
            let err = RuntimeOptionPatch::parse_json(r#"{"scrollback":"lots"}"#).unwrap_err();
            assert!(matches!(err, OptionPatchParseError::TypeMismatch { .. }));
        }

        #[test]
        fn invalid_enum_token_rejected() {
            let err = RuntimeOptionPatch::parse_json(r#"{"rendererType":"crayon"}"#).unwrap_err();
            assert!(matches!(
                err,
                OptionPatchParseError::InvalidToken { ref token, .. } if token == "crayon"
            ));
        }

        #[test]
        fn unknown_theme_slot_rejected() {
            let err =
                RuntimeOptionPatch::parse_json(r##"{"theme":{"sparkle":"#fff"}}"##).unwrap_err();
            assert!(
                matches!(err, OptionPatchParseError::UnknownKey(ref k) if k == "theme.sparkle")
            );
        }

        #[test]
        fn absurd_scrollback_saturates_then_range_rejects() {
            // 9e18 overflows u32 → saturates to u32::MAX at parse, then the
            // hard-max range check rejects it at apply time (not a parse panic).
            let patch =
                RuntimeOptionPatch::parse_json(r#"{"scrollback":9000000000000000000}"#).unwrap();
            assert_eq!(patch.scrollback_lines, Some(u32::MAX));
            let mut opts = RuntimeOptions::default();
            let update = opts.apply_patch(&patch, &OptionCapabilityProfile::full(), "sat");
            assert!(!update.applied);
            assert_eq!(update.errors[0].field(), FIELD_SCROLLBACK);
        }
    }
}
