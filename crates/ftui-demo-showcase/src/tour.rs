#![forbid(unsafe_code)]

//! Guided Tour orchestration for the demo showcase.
//!
//! Provides a deterministic, data-driven storyboard that advances across
//! key screens using the Screen Registry metadata.

use std::time::Duration;

use ftui_core::geometry::Rect;

use crate::app::ScreenId;
use crate::screens::{self, ScreenCategory};

const SPEED_MIN: f64 = 0.25;
const SPEED_MAX: f64 = 4.0;
/// Slack a step must leave after its final action, so the screen has time to
/// show the result before the tour moves on.
const ACTION_TAIL_MS: u64 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TourAdvanceReason {
    Auto,
    ManualNext,
    ManualPrev,
    Jump,
}

#[derive(Debug, Clone)]
pub enum TourEvent {
    StepChanged {
        from: ScreenId,
        to: ScreenId,
        reason: TourAdvanceReason,
    },
    Finished {
        last_screen: ScreenId,
    },
}

/// What the tour does to the active screen at a scheduled moment.
///
/// The tour narrates *and* drives: a step that talks about search actually
/// types a query and walks the matches, so the viewer sees the feature work
/// instead of reading a caption about it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TourInput {
    Char(char),
    Enter,
    Esc,
    Tab,
    Backspace,
    Up,
    Down,
    Left,
    Right,
    /// A pointer event, positioned by [`TourPointerAt`].
    Pointer {
        kind: TourPointer,
        at: TourPointerAt,
    },
}

/// Where a tour pointer event lands.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TourPointerAt {
    /// A fraction of the content area, so a step reads the same at 80x24 and
    /// 200x60.
    Fraction { x_pct: f32, y_pct: f32 },
    /// The dashboard's bottom splitter handle, resolved from the live layout
    /// and offset by `dx_cells` columns.
    ///
    /// The handle is a fixed cell offset, not a fraction: measured at
    /// x_pct 0.23 on an 80-column terminal and 0.17 at 160, so any hardcoded
    /// percentage misses it at some size. Asking the screen is exact.
    DashboardSplitter { dx_cells: i16 },
}

/// Which pointer transition a [`TourInput::Pointer`] represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TourPointer {
    Down,
    Drag,
    Up,
    Move,
}

/// Whether an action presses a key or releases it.
///
/// Both halves are scheduled separately because the gap between them is the
/// point: screens that latch a key hold their state until the release arrives.
/// Quake moves forward only while `w` is down, so a press and release in the
/// same tick cancel out and the player never moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TourPhase {
    Press,
    Release,
}

/// A [`TourInput`] scheduled at an offset into its step, in tour time (so it
/// follows the speed multiplier and pauses with the tour).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TourAction {
    pub at_ms: u64,
    pub input: TourInput,
    pub phase: TourPhase,
}

impl TourAction {
    pub const fn new(at_ms: u64, input: TourInput) -> Self {
        Self {
            at_ms,
            input,
            phase: TourPhase::Press,
        }
    }

    pub const fn released(at_ms: u64, input: TourInput) -> Self {
        Self {
            at_ms,
            input,
            phase: TourPhase::Release,
        }
    }
}

/// How long a tapped key stays down. Long enough to be a real press for screens
/// that track key state, short enough to read as a tap.
const TAP_HOLD_MS: u64 = 60;

/// Press and release `input`, holding it for `hold_ms`.
///
/// Use this for keys a screen latches (movement in Quake); [`tap`] is the
/// right call for everything else.
pub fn hold(at_ms: u64, input: TourInput, hold_ms: u64) -> Vec<TourAction> {
    vec![
        TourAction::new(at_ms, input),
        TourAction::released(at_ms + hold_ms.max(1), input),
    ]
}

/// Press and immediately release `input`.
pub fn tap(at_ms: u64, input: TourInput) -> Vec<TourAction> {
    hold(at_ms, input, TAP_HOLD_MS)
}

/// Expand `text` into one keystroke per character, `every_ms` apart, starting
/// at `start_ms`. Used to type search queries and markdown samples.
///
/// A newline becomes [`TourInput::Enter`], the key a typist actually presses,
/// rather than a literal `'\n'` character whose fate depends on each widget's
/// control-character policy.
pub fn typed(start_ms: u64, every_ms: u64, text: &str) -> Vec<TourAction> {
    text.chars()
        .enumerate()
        .flat_map(|(i, ch)| {
            let input = if ch == '\n' {
                TourInput::Enter
            } else {
                TourInput::Char(ch)
            };
            // Hold no longer than the gap to the next character, so a fast
            // typing cadence cannot leave two keys down at once.
            let held = every_ms.saturating_sub(1).clamp(1, TAP_HOLD_MS);
            hold(start_ms + (i as u64) * every_ms, input, held)
        })
        .collect()
}

/// Repeat one input `count` times, `every_ms` apart, starting at `start_ms`.
/// Used to walk search matches and cycle samples.
pub fn repeated(start_ms: u64, every_ms: u64, count: usize, input: TourInput) -> Vec<TourAction> {
    (0..count)
        .flat_map(|i| {
            let held = every_ms.saturating_sub(1).clamp(1, TAP_HOLD_MS);
            hold(start_ms + (i as u64) * every_ms, input, held)
        })
        .collect()
}

/// Drag the pointer horizontally across the content area: press at
/// `(from_x_pct, y_pct)`, move to `to_x_pct` over `steps`, then release.
///
/// Used to drag pane splitters, which is the only honest way to show a
/// resizable workspace working.
pub fn drag_splitter(
    start_ms: u64,
    every_ms: u64,
    to_dx_cells: i16,
    steps: usize,
) -> Vec<TourAction> {
    let steps = steps.max(1);
    let at = |dx: i16| TourInput::Pointer {
        kind: TourPointer::Drag,
        at: TourPointerAt::DashboardSplitter { dx_cells: dx },
    };
    let mut out = vec![TourAction::new(
        start_ms,
        TourInput::Pointer {
            kind: TourPointer::Down,
            at: TourPointerAt::DashboardSplitter { dx_cells: 0 },
        },
    )];
    for i in 1..=steps {
        let dx = (i32::from(to_dx_cells) * i as i32 / steps as i32) as i16;
        out.push(TourAction::new(start_ms + (i as u64) * every_ms, at(dx)));
    }
    out.push(TourAction::new(
        start_ms + ((steps as u64) + 1) * every_ms,
        TourInput::Pointer {
            kind: TourPointer::Up,
            at: TourPointerAt::DashboardSplitter {
                dx_cells: to_dx_cells,
            },
        },
    ));
    out
}

#[derive(Debug, Clone)]
pub struct TourStep {
    pub id: String,
    pub screen: ScreenId,
    pub title: &'static str,
    pub blurb: &'static str,
    pub hint: Option<&'static str>,
    pub duration: Duration,
    pub highlight: Option<TourHighlight>,
    /// Keystrokes this step performs, sorted by `at_ms`.
    pub actions: Vec<TourAction>,
}

#[derive(Debug, Clone, Copy)]
pub struct TourHighlight {
    x_pct: f32,
    y_pct: f32,
    w_pct: f32,
    h_pct: f32,
}

impl TourHighlight {
    pub const fn new_pct(x_pct: f32, y_pct: f32, w_pct: f32, h_pct: f32) -> Self {
        Self {
            x_pct,
            y_pct,
            w_pct,
            h_pct,
        }
    }

    pub fn resolve(self, area: Rect) -> Rect {
        if area.width == 0 || area.height == 0 {
            return Rect::default();
        }
        let width = ((area.width as f32) * self.w_pct).round() as u16;
        let height = ((area.height as f32) * self.h_pct).round() as u16;
        let width = width.clamp(1, area.width);
        let height = height.clamp(1, area.height);
        let x = area.x + ((area.width as f32) * self.x_pct).round() as u16;
        let y = area.y + ((area.height as f32) * self.y_pct).round() as u16;
        let x = x.min(area.right().saturating_sub(width));
        let y = y.min(area.bottom().saturating_sub(height));
        Rect::new(x, y, width, height)
    }
}

#[derive(Debug, Clone)]
pub struct TourOverlayStep<'a> {
    pub index: usize,
    pub title: &'a str,
    pub category: ScreenCategory,
    pub hint: Option<&'a str>,
    pub is_current: bool,
}

#[derive(Debug, Clone)]
pub struct TourOverlayState<'a> {
    pub step_index: usize,
    pub step_count: usize,
    pub screen_title: &'a str,
    pub screen_category: ScreenCategory,
    pub callout_title: &'a str,
    pub callout_body: &'a str,
    pub callout_hint: Option<&'a str>,
    pub paused: bool,
    pub speed: f64,
    pub remaining: Duration,
    pub steps: Vec<TourOverlayStep<'a>>,
    pub highlight: Option<Rect>,
}

#[derive(Debug, Clone)]
pub struct GuidedTourState {
    active: bool,
    paused: bool,
    speed: f64,
    step_index: usize,
    step_elapsed: Duration,
    steps: Vec<TourStep>,
    resume_screen: ScreenId,
    /// How many of the current step's actions have already been handed out.
    /// Actions are sorted by `at_ms`, so this doubles as the cursor into them.
    actions_fired: usize,
}

impl Default for GuidedTourState {
    fn default() -> Self {
        Self::new()
    }
}

impl GuidedTourState {
    pub fn new() -> Self {
        Self {
            active: false,
            paused: false,
            speed: 1.0,
            step_index: 0,
            step_elapsed: Duration::ZERO,
            steps: build_steps(),
            resume_screen: ScreenId::Dashboard,
            actions_fired: 0,
        }
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn speed(&self) -> f64 {
        self.speed
    }

    pub fn set_speed(&mut self, speed: f64) {
        self.speed = normalize_speed(speed);
    }

    pub fn toggle_pause(&mut self) {
        self.paused = !self.paused;
    }

    pub fn pause(&mut self) {
        self.paused = true;
    }

    pub fn resume(&mut self) {
        self.paused = false;
    }

    pub fn start(&mut self, resume_screen: ScreenId, start_step: usize, speed: f64) {
        self.steps = build_steps();
        self.active = true;
        self.paused = false;
        self.speed = normalize_speed(speed);
        self.step_index = start_step.min(self.steps.len().saturating_sub(1));
        self.step_elapsed = Duration::ZERO;
        self.actions_fired = 0;
        self.resume_screen = resume_screen;
    }

    pub fn stop(&mut self, keep_last: bool) -> ScreenId {
        let screen = if keep_last {
            self.active_screen()
        } else {
            self.resume_screen
        };
        self.active = false;
        self.paused = false;
        self.step_elapsed = Duration::ZERO;
        self.actions_fired = 0;
        screen
    }

    pub fn step_index(&self) -> usize {
        self.step_index
    }

    pub fn step_count(&self) -> usize {
        self.steps.len()
    }

    pub fn current_step(&self) -> Option<&TourStep> {
        self.steps.get(self.step_index)
    }

    pub fn active_screen(&self) -> ScreenId {
        self.steps
            .get(self.step_index)
            .map(|step| step.screen)
            .unwrap_or(self.resume_screen)
    }

    pub fn advance(&mut self, delta: Duration) -> Option<TourEvent> {
        if !self.active || self.paused || self.steps.is_empty() {
            return None;
        }

        self.step_elapsed = self
            .step_elapsed
            .checked_add(scale_duration(delta, self.speed))
            .unwrap_or(Duration::MAX);

        let step = self.steps.get(self.step_index)?;
        if self.step_elapsed < step.duration {
            return None;
        }
        self.next_step(TourAdvanceReason::Auto)
    }

    /// Hand back the current step's actions whose scheduled offset has passed,
    /// in order, each exactly once.
    ///
    /// Call after [`Self::advance`] on the same tick. A paused tour yields
    /// nothing, so the demo freezes where the viewer paused it rather than
    /// replaying a burst of keystrokes on resume.
    pub fn take_due_actions(&mut self) -> Vec<TourAction> {
        if !self.active || self.paused {
            return Vec::new();
        }
        let Some(step) = self.steps.get(self.step_index) else {
            return Vec::new();
        };
        let mut due = Vec::new();
        while let Some(action) = step.actions.get(self.actions_fired) {
            if Duration::from_millis(action.at_ms) > self.step_elapsed {
                break;
            }
            due.push(*action);
            self.actions_fired += 1;
        }
        due
    }

    pub fn next_step(&mut self, reason: TourAdvanceReason) -> Option<TourEvent> {
        if !self.active || self.steps.is_empty() {
            return None;
        }
        let from = self.active_screen();
        if self.step_index + 1 >= self.steps.len() {
            self.active = false;
            self.paused = false;
            self.step_elapsed = Duration::ZERO;
            self.actions_fired = 0;
            return Some(TourEvent::Finished { last_screen: from });
        }
        self.step_index += 1;
        self.step_elapsed = Duration::ZERO;
        self.actions_fired = 0;
        let to = self.active_screen();
        Some(TourEvent::StepChanged { from, to, reason })
    }

    pub fn prev_step(&mut self) -> Option<TourEvent> {
        if !self.active || self.steps.is_empty() {
            return None;
        }
        if self.step_index == 0 {
            return None;
        }
        let from = self.active_screen();
        self.step_index = self.step_index.saturating_sub(1);
        self.step_elapsed = Duration::ZERO;
        self.actions_fired = 0;
        let to = self.active_screen();
        Some(TourEvent::StepChanged {
            from,
            to,
            reason: TourAdvanceReason::ManualPrev,
        })
    }

    pub fn jump_to(&mut self, index: usize) -> Option<TourEvent> {
        if !self.active || self.steps.is_empty() {
            return None;
        }
        let idx = index.min(self.steps.len().saturating_sub(1));
        if idx == self.step_index {
            return None;
        }
        let from = self.active_screen();
        self.step_index = idx;
        self.step_elapsed = Duration::ZERO;
        self.actions_fired = 0;
        let to = self.active_screen();
        Some(TourEvent::StepChanged {
            from,
            to,
            reason: TourAdvanceReason::Jump,
        })
    }

    pub fn overlay_state<'a>(
        &'a self,
        content_area: Rect,
        max_steps: usize,
    ) -> Option<TourOverlayState<'a>> {
        if !self.active {
            return None;
        }
        let step = self.steps.get(self.step_index)?;
        let step_count = self.steps.len();
        let highlight = step.highlight.map(|h| h.resolve(content_area));

        let window = max_steps.max(1);
        let start = self.step_index.saturating_sub(1);
        let end = (start + window).min(step_count);
        let steps = self.steps[start..end]
            .iter()
            .enumerate()
            .map(|(offset, step)| {
                let index = start + offset;
                TourOverlayStep {
                    index,
                    title: step.title,
                    category: screens::screen_category(step.screen),
                    hint: step.hint,
                    is_current: index == self.step_index,
                }
            })
            .collect::<Vec<_>>();

        let remaining = step
            .duration
            .saturating_sub(self.step_elapsed.min(step.duration));

        Some(TourOverlayState {
            step_index: self.step_index,
            step_count,
            screen_title: step.title,
            screen_category: screens::screen_category(step.screen),
            callout_title: step.title,
            callout_body: step.blurb,
            callout_hint: step.hint,
            paused: self.paused,
            speed: self.speed,
            remaining,
            steps,
            highlight,
        })
    }
}

pub(crate) fn build_steps() -> Vec<TourStep> {
    #[allow(clippy::too_many_arguments)]
    fn push_step(
        steps: &mut Vec<TourStep>,
        screen: ScreenId,
        suffix: &'static str,
        blurb: &'static str,
        hint: &'static str,
        duration_ms: u64,
        actions: Vec<TourAction>,
    ) {
        let meta = screens::screen_meta(screen);
        let base = slugify(meta.title);
        debug_assert!(
            actions
                .last()
                .is_none_or(|last| last.at_ms + ACTION_TAIL_MS <= duration_ms),
            "tour step {base}:{suffix} schedules an action too close to its end"
        );
        steps.push(TourStep {
            id: format!("step:{base}:{suffix}"),
            screen,
            title: meta.title,
            blurb,
            hint: Some(hint),
            duration: Duration::from_millis(duration_ms),
            highlight: None,
            actions,
        });
    }

    /// Concatenate action groups and sort by scheduled offset, so a step can be
    /// written as independent beats without hand-ordering the result.
    fn beats(groups: impl IntoIterator<Item = Vec<TourAction>>) -> Vec<TourAction> {
        let mut all: Vec<TourAction> = groups.into_iter().flatten().collect();
        all.sort_by_key(|a| a.at_ms);
        all
    }

    /// Press and release one key. Named for what the viewer sees; the release
    /// matters because screens that latch a key stay latched without it.
    fn press(at_ms: u64, input: TourInput) -> Vec<TourAction> {
        tap(at_ms, input)
    }

    use TourInput::{Char, Down, Enter, Esc, Left, Right, Tab, Up};

    let mut steps = Vec::new();

    // The storyboard shows features working rather than describing them: each
    // step types real queries, walks real results, and cycles real samples.
    // Steps are kept short so nothing outstays its welcome; the longer ones are
    // the search beats, which need time to type.

    // ---- Orientation -----------------------------------------------------
    push_step(
        &mut steps,
        ScreenId::Dashboard,
        "overview",
        "Every tile here is live and clickable. This whole UI is one Rust binary.",
        "Click any tile, or press Ctrl+K for the command palette.",
        3400,
        Vec::new(),
    );

    // ---- Text: real search over 5.4 MB of Shakespeare --------------------
    push_step(
        &mut steps,
        ScreenId::Shakespeare,
        "search",
        "Searching the complete works of Shakespeare - 5.4 MB, no index, no lag.",
        "Press / to search, then Enter to walk the matches.",
        7200,
        beats([
            press(250, Char('/')),
            typed(700, 90, "Hamlet"),
            // While the search field holds focus, Enter/Down step matches and
            // `n` would be typed into the query instead.
            repeated(1700, 900, 6, Enter),
        ]),
    );
    push_step(
        &mut steps,
        ScreenId::Shakespeare,
        "modes",
        "The same buffer, re-laid-out on demand: view modes, scrolling, jumps.",
        "m cycles view mode; g and G jump to the ends.",
        4200,
        beats([
            press(200, Esc),
            press(700, Char('m')),
            press(1700, Char('m')),
            press(2600, Char('G')),
            press(3300, Char('g')),
        ]),
    );

    // ---- Text: search a 9 MB C amalgamation ------------------------------
    push_step(
        &mut steps,
        ScreenId::CodeExplorer,
        "search",
        "Same engine over sqlite3.c - 9.2 MB of C, searched and highlighted live.",
        "/ searches; Enter steps through every hit.",
        7000,
        beats([
            press(250, Char('/')),
            typed(700, 90, "sqlite3_open"),
            // Same as Shakespeare: the query field owns plain characters.
            repeated(2000, 900, 5, Enter),
        ]),
    );
    push_step(
        &mut steps,
        ScreenId::CodeExplorer,
        "hotspots",
        "Hotspots and feature spotlights navigate structure, not just text.",
        "] jumps hotspots; f cycles the spotlight.",
        4200,
        beats([
            press(200, Esc),
            press(700, Char(']')),
            press(1600, Char(']')),
            press(2500, Char('f')),
            press(3400, Char('f')),
        ]),
    );

    // ---- Markdown: streaming render + varied input -----------------------
    push_step(
        &mut steps,
        ScreenId::MarkdownRichText,
        "stream",
        "GitHub-flavored markdown streaming in: tables, code, lists, math.",
        "r restarts the stream, w and a change wrap and alignment, f runs it fast.",
        5200,
        beats([
            press(400, Char('r')),
            press(1800, Char('w')),
            press(2700, Char('a')),
            press(3600, Char('f')),
        ]),
    );
    push_step(
        &mut steps,
        ScreenId::MarkdownLiveEditor,
        "typing",
        "Type markdown, see it render. The preview keeps up keystroke by keystroke.",
        "Everything you see is being typed live.",
        6600,
        beats([
            typed(300, 55, "# FrankenTUI\n\n"),
            typed(1600, 45, "- [x] **bold**, `code`, _italic_\n"),
            typed(3300, 45, "- [ ] tables, math, links\n\n"),
            typed(4900, 45, "> Rendered as you type.\n"),
        ]),
    );

    // ---- Diagrams --------------------------------------------------------
    #[cfg(feature = "screen-mermaid")]
    {
        push_step(
            &mut steps,
            ScreenId::MermaidShowcase,
            "samples",
            "Mermaid diagrams laid out in the terminal: flowcharts, sequence, state, ER, Gantt.",
            "j and k walk 29 samples; m shows layout metrics.",
            6800,
            // Three samples with time to lay out and be read, rather than five
            // that flick past mid-layout.
            beats([press(300, Char('m')), repeated(1000, 1900, 3, Char('j'))]),
        );
        push_step(
            &mut steps,
            ScreenId::MermaidShowcase,
            "layout",
            "The layout engine is tunable live: tiers, glyph modes, and render backends.",
            "l toggles layout, t cycles tier, b cycles render mode, f refits the view.",
            4600,
            beats([
                press(300, Char('l')),
                press(1300, Char('t')),
                press(2300, Char('b')),
                press(3300, Char('f')),
            ]),
        );
        push_step(
            &mut steps,
            ScreenId::MermaidMegaShowcase,
            "mega",
            "Stress mode: procedurally generated graphs, re-laid-out on every change.",
            "j walks samples; R reseeds the generator.",
            4800,
            beats([repeated(300, 900, 3, Char('j')), press(3200, Char('R'))]),
        );
    }

    // ---- Visuals ---------------------------------------------------------
    push_step(
        &mut steps,
        ScreenId::VisualEffects,
        "effects",
        "Braille-rasterized effects: reaction-diffusion, metaballs, attractors, fractals.",
        "Arrow keys switch effects; every one is deterministic math.",
        7400,
        // Three effects at ~2.3s each. Five at 1.3s flicked past before any of
        // them resolved into a recognisable pattern.
        beats([repeated(600, 2300, 3, Right)]),
    );
    push_step(
        &mut steps,
        ScreenId::VisualEffects,
        "textfx",
        "Text effects run through the same rasterizer, with easing and combos.",
        "t enters text mode; arrows cycle effects.",
        5600,
        beats([press(300, Char('t')), repeated(1400, 1800, 2, Right)]),
    );
    push_step(
        &mut steps,
        ScreenId::DataViz,
        "charts",
        "Charts, sparklines and gauges - all cell-addressed, no image layer.",
        "Arrows switch panels; d flips bar direction.",
        5600,
        beats([repeated(400, 1400, 3, Right), press(4200, Char('d'))]),
    );

    // ---- Widgets and layout ---------------------------------------------
    push_step(
        &mut steps,
        ScreenId::WidgetGallery,
        "widgets",
        "80+ widgets: inputs, tables, trees, pickers, toasts, palettes.",
        "j and k walk the sections.",
        4600,
        beats([repeated(400, 1000, 4, Char('j'))]),
    );
    push_step(
        &mut steps,
        ScreenId::TableThemeGallery,
        "tables",
        "A dedicated table theme engine: striping, emphasis, borders, selection.",
        "Tab cycles presets; Z and B change zebra and borders.",
        4600,
        beats([
            repeated(300, 900, 3, Tab),
            press(3100, Char('Z')),
            press(3800, Char('B')),
        ]),
    );
    push_step(
        &mut steps,
        ScreenId::LayoutLab,
        "layout",
        "Flex and grid solvers with live constraints.",
        "Arrows resize constraints; d flips direction, a cycles alignment.",
        4600,
        beats([
            repeated(400, 900, 2, Right),
            press(2400, Char('d')),
            press(3400, Char('a')),
        ]),
    );
    push_step(
        &mut steps,
        ScreenId::Dashboard,
        "panes",
        "Pane workspaces are draggable: grab a divider and the layout re-solves live.",
        "Drag the bottom dividers; right-click a pane to cycle its mode.",
        6800,
        // A real pointer drag on the real handle, resolved from the live
        // layout. The handle sits at a fixed cell offset, so its fraction of
        // the screen moves with terminal width (0.23 at 80 columns, 0.17 at
        // 160) and a hardcoded percentage would miss it.
        beats([
            drag_splitter(500, 220, 14, 10),
            drag_splitter(3700, 220, -10, 8),
        ]),
    );
    push_step(
        &mut steps,
        ScreenId::ResponsiveDemo,
        "responsive",
        "Breakpoints in a terminal: the layout restructures as the viewport changes.",
        "- and + resize the simulated viewport; b switches breakpoint sets.",
        7000,
        // Narrow the viewport through the breakpoints and widen it back, which
        // is the whole point of the screen and cannot be seen from a caption.
        beats([
            repeated(400, 300, 6, Char('-')),
            repeated(2600, 300, 6, Char('+')),
            press(4800, Char('b')),
        ]),
    );

    // ---- Interaction -----------------------------------------------------
    push_step(
        &mut steps,
        ScreenId::FormsInput,
        "forms",
        "Real form controls with validation, undo/redo and focus management.",
        "Tab moves between fields; Space toggles checkboxes.",
        5200,
        beats([
            typed(300, 70, "frankentui"),
            press(1500, Tab),
            typed(1900, 70, "demo@example.com"),
            press(3400, Tab),
            press(3900, Char(' ')),
        ]),
    );
    push_step(
        &mut steps,
        ScreenId::VirtualizedSearch,
        "virtualized",
        "A virtualized list with Fenwick-indexed variable heights: O(log n) scrolling.",
        "/ filters; j walks results without re-laying out the world.",
        5600,
        beats([
            press(300, Char('/')),
            typed(800, 80, "render"),
            press(1900, Enter),
            repeated(2600, 650, 4, Char('j')),
        ]),
    );
    push_step(
        &mut steps,
        ScreenId::LogSearch,
        "logs",
        "Live log stream with search, filters and match stepping.",
        "/ searches, n steps matches, Space pauses the stream.",
        5400,
        beats([
            press(300, Char('/')),
            typed(800, 80, "error"),
            press(1800, Enter),
            repeated(2500, 700, 3, Char('n')),
            press(4700, Esc),
        ]),
    );
    push_step(
        &mut steps,
        ScreenId::CommandPaletteLab,
        "palette",
        "The command palette scores matches with a Bayesian evidence ledger.",
        "b runs the query benchmark, m cycles the filter, arrows walk results.",
        5000,
        // This screen drives the palette itself - it has no free-text field, so
        // typing a query here would just hit its single-letter bindings.
        beats([
            press(300, Char('b')),
            repeated(1400, 700, 3, Down),
            press(3600, Char('m')),
            // Leave the benchmark off again rather than letting it keep
            // running queries behind the remaining steps.
            press(4300, Char('b')),
        ]),
    );
    push_step(
        &mut steps,
        ScreenId::KanbanBoard,
        "kanban",
        "Drag-and-drop board with undo: cards move by keyboard or mouse.",
        "h and l change column; L moves the card.",
        4200,
        beats([
            repeated(300, 800, 2, Char('j')),
            press(2000, Char('L')),
            press(2900, Char('u')),
        ]),
    );

    // ---- Theming, i18n, accessibility ------------------------------------
    push_step(
        &mut steps,
        ScreenId::ThemeStudio,
        "theme",
        "Themes are data: edit, preview, and export to JSON or a Ghostty config.",
        "Enter applies a theme; e exports it.",
        4400,
        beats([repeated(300, 800, 3, Char('j')), press(2800, Enter)]),
    );
    push_step(
        &mut steps,
        ScreenId::I18nDemo,
        "i18n",
        "Locale-aware rendering with BiDi: English, French, German, Japanese, Arabic.",
        "L cycles locale; D flips RTL.",
        4400,
        beats([repeated(300, 900, 3, Char('L')), press(3100, Char('D'))]),
    );
    push_step(
        &mut steps,
        ScreenId::AccessibilityPanel,
        "a11y",
        "A live accessibility tree mirrors the widget tree, with WCAG contrast checks.",
        "Announcements are emitted as structured evidence.",
        3400,
        Vec::new(),
    );

    // ---- The kernel story ------------------------------------------------
    push_step(
        &mut steps,
        ScreenId::InlineModeStory,
        "scrollback",
        "Inline mode keeps your scrollback. The UI pins itself; your history stays real.",
        "t appends a burst of log lines; h resizes the pinned UI, m flips the mode.",
        6400,
        // Push real output past the pinned UI: the claim is about what happens
        // to scrollback, so the step has to produce some.
        beats([
            press(400, Char('t')),
            press(1600, Char('t')),
            press(2800, Char('h')),
            press(4200, Char('m')),
        ]),
    );
    push_step(
        &mut steps,
        ScreenId::DeterminismLab,
        "checksums",
        "Determinism is measured, not claimed: identical input, identical checksum.",
        "r runs the scene; ] advances the seed. Same seed, same hash.",
        7000,
        // Run, run again - the hash is identical - then change the seed and run
        // once more so the hash moves. That is the claim, demonstrated.
        beats([
            press(400, Char('r')),
            press(1900, Char('r')),
            press(3400, Char(']')),
            press(4300, Char('r')),
        ]),
    );
    push_step(
        &mut steps,
        ScreenId::SnapshotPlayer,
        "replay",
        "Time travel: record frames, scrub the timeline, diff what changed.",
        "Arrows scrub; diff mode shows the deltas.",
        4400,
        beats([repeated(400, 900, 4, Right)]),
    );
    push_step(
        &mut steps,
        ScreenId::HyperlinkPlayground,
        "links",
        "OSC-8 hyperlinks with real hit regions - hover and click like a GUI.",
        "Arrows move between links, Enter follows one, c copies the URL.",
        5800,
        beats([
            repeated(400, 700, 3, Down),
            press(2800, Enter),
            press(3900, Char('c')),
        ]),
    );
    push_step(
        &mut steps,
        ScreenId::ExplainabilityCockpit,
        "evidence",
        "Every probabilistic decision is logged: diff strategy, budgets, regimes.",
        "1-4 focus a panel; Up walks back through the decision timeline.",
        5600,
        // Up is older, and the timeline starts at the newest row - so Down
        // alone, which is where this step began, moves nothing at all.
        beats([
            press(400, Char('4')),
            repeated(1300, 800, 4, Up),
            press(4400, Char('1')),
        ]),
    );
    push_step(
        &mut steps,
        ScreenId::PerformanceHud,
        "budgets",
        "Frame budgets are enforced, and degradation is deliberate and recoverable.",
        "s piles on load until the budget gives; c lets it recover.",
        7000,
        // Degradation you can watch: stress the frame until quality drops, then
        // cool it down and watch it come back.
        beats([press(500, Char('s')), press(4000, Char('c'))]),
    );

    // ---- Sign-off --------------------------------------------------------
    push_step(
        &mut steps,
        ScreenId::QuakeEasterEgg,
        "quake",
        "And yes - a raycast Quake level, rendered in text cells. Press Tab to explore.",
        "WASD moves, arrows look. Thanks for watching.",
        7200,
        // Movement is latched: forward velocity is set on key-down and cleared
        // on key-up, so the player only moves while `w` is genuinely held.
        beats([
            hold(400, Char('w'), 1800),
            repeated(2500, 260, 5, Left),
            hold(4200, Char('w'), 1200),
            repeated(5700, 260, 4, Right),
        ]),
    );

    steps
}

fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_was_sep = true;
    for ch in input.chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_was_sep = false;
        } else if !last_was_sep {
            out.push('_');
            last_was_sep = true;
        }
    }
    out.trim_matches('_').to_string()
}

fn normalize_speed(speed: f64) -> f64 {
    if speed.is_finite() && speed > 0.0 {
        speed.clamp(SPEED_MIN, SPEED_MAX)
    } else {
        1.0
    }
}

fn scale_duration(delta: Duration, speed: f64) -> Duration {
    let micros = delta.as_micros() as f64 * speed;
    let micros = micros.round().clamp(0.0, u64::MAX as f64) as u64;
    Duration::from_micros(micros)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_step(
        screen: ScreenId,
        title: &'static str,
        duration_ms: u64,
        highlight: Option<TourHighlight>,
    ) -> TourStep {
        TourStep {
            id: format!("step:{title}"),
            screen,
            title,
            blurb: "blurb",
            hint: None,
            duration: Duration::from_millis(duration_ms),
            highlight,
            actions: Vec::new(),
        }
    }

    #[test]
    fn tour_advances_and_finishes() {
        let mut tour = GuidedTourState::new();
        tour.start(ScreenId::Dashboard, 0, 1.0);
        assert!(tour.is_active());
        let steps = tour.step_count();
        assert!(steps > 0);

        // Force-advance until completion.
        for _ in 0..steps {
            let _ = tour.advance(Duration::from_secs(10));
        }
        assert!(!tour.is_active());
    }

    #[test]
    fn tour_pause_blocks_advance() {
        let mut tour = GuidedTourState::new();
        tour.start(ScreenId::Dashboard, 0, 1.0);
        tour.pause();
        let before = tour.step_index();
        let _ = tour.advance(Duration::from_secs(10));
        assert_eq!(before, tour.step_index());
    }

    #[test]
    fn actions_fire_once_in_schedule_order() {
        let mut tour = GuidedTourState::new();
        tour.steps = vec![TourStep {
            id: "s".to_string(),
            screen: ScreenId::Dashboard,
            title: "T",
            blurb: "b",
            hint: None,
            duration: Duration::from_millis(5000),
            highlight: None,
            actions: beats_for_test(),
        }];
        tour.active = true;

        // Nothing is due before its offset.
        let _ = tour.advance(Duration::from_millis(50));
        assert!(tour.take_due_actions().is_empty());

        // Crossing 100ms and 200ms yields both, in order, exactly once.
        let _ = tour.advance(Duration::from_millis(200));
        let due: Vec<TourInput> = tour.take_due_actions().iter().map(|a| a.input).collect();
        assert_eq!(due, vec![TourInput::Char('a'), TourInput::Enter]);
        assert!(tour.take_due_actions().is_empty());

        // A later action still fires on a subsequent tick.
        let _ = tour.advance(Duration::from_millis(1000));
        let later: Vec<TourInput> = tour.take_due_actions().iter().map(|a| a.input).collect();
        assert_eq!(later, vec![TourInput::Down]);
    }

    fn beats_for_test() -> Vec<TourAction> {
        vec![
            TourAction::new(100, TourInput::Char('a')),
            TourAction::new(200, TourInput::Enter),
            TourAction::new(900, TourInput::Down),
        ]
    }

    #[test]
    fn paused_tour_performs_no_actions() {
        let mut tour = GuidedTourState::new();
        tour.start(ScreenId::Dashboard, 0, 1.0);
        tour.pause();
        let _ = tour.advance(Duration::from_secs(5));
        assert!(
            tour.take_due_actions().is_empty(),
            "a paused tour must not drive the screen"
        );
    }

    #[test]
    fn step_change_resets_the_action_cursor() {
        let mut tour = GuidedTourState::new();
        tour.start(ScreenId::Dashboard, 0, 1.0);
        let _ = tour.advance(Duration::from_millis(400));
        let _ = tour.take_due_actions();
        let _ = tour.next_step(TourAdvanceReason::ManualNext);
        assert_eq!(
            tour.actions_fired, 0,
            "the next step must start from its own first action"
        );
    }

    #[test]
    fn typed_sends_enter_for_newlines() {
        let pressed: Vec<TourInput> = typed(0, 10, "a\nb")
            .iter()
            .filter(|a| a.phase == TourPhase::Press)
            .map(|a| a.input)
            .collect();
        assert_eq!(
            pressed,
            vec![TourInput::Char('a'), TourInput::Enter, TourInput::Char('b')]
        );
    }

    #[test]
    fn every_press_is_matched_by_a_later_release() {
        // A press with no release latches key-tracking screens; a release in
        // the same instant cancels the press before it can do anything. Quake
        // moves forward only while `w` is down, which is why hold() schedules
        // the two apart.
        for step in build_steps() {
            let mut down: Vec<(TourInput, u64)> = Vec::new();
            for action in &step.actions {
                match action.phase {
                    TourPhase::Press => down.push((action.input, action.at_ms)),
                    TourPhase::Release => {
                        let idx = down
                            .iter()
                            .position(|(input, _)| *input == action.input)
                            .unwrap_or_else(|| {
                                panic!("{}: release without a press: {:?}", step.id, action.input)
                            });
                        let (_, pressed_at) = down.remove(idx);
                        assert!(
                            action.at_ms > pressed_at,
                            "{}: {:?} released in the same instant it was pressed",
                            step.id,
                            action.input
                        );
                    }
                }
            }
            down.retain(|(input, _)| !matches!(input, TourInput::Pointer { .. }));
            assert!(
                down.is_empty(),
                "{}: keys left held: {:?}",
                step.id,
                down.iter().map(|(i, _)| *i).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn typed_and_repeated_expand_on_schedule() {
        let presses: Vec<(u64, TourInput)> = typed(100, 50, "hi")
            .iter()
            .filter(|a| a.phase == TourPhase::Press)
            .map(|a| (a.at_ms, a.input))
            .collect();
        assert_eq!(
            presses,
            vec![(100, TourInput::Char('h')), (150, TourInput::Char('i'))]
        );
        let walk: Vec<u64> = repeated(0, 10, 3, TourInput::Char('n'))
            .iter()
            .filter(|a| a.phase == TourPhase::Press)
            .map(|a| a.at_ms)
            .collect();
        assert_eq!(walk, vec![0, 10, 20]);
    }

    #[test]
    fn every_step_is_short_and_finishes_its_actions_before_it_ends() {
        for step in build_steps() {
            assert!(
                step.duration <= Duration::from_millis(7500),
                "{} lingers for {:?}",
                step.id,
                step.duration
            );
            let mut last = 0;
            for action in &step.actions {
                assert!(action.at_ms >= last, "{} has out-of-order actions", step.id);
                last = action.at_ms;
            }
            if let Some(final_action) = step.actions.last() {
                assert!(
                    Duration::from_millis(final_action.at_ms + ACTION_TAIL_MS) <= step.duration,
                    "{} presses its last key with no time left to show the result",
                    step.id
                );
            }
        }
    }

    #[test]
    fn tour_covers_the_headline_screens() {
        let screens: Vec<ScreenId> = build_steps().iter().map(|s| s.screen).collect();
        for required in [
            ScreenId::Shakespeare,
            ScreenId::CodeExplorer,
            ScreenId::MarkdownRichText,
            ScreenId::MarkdownLiveEditor,
            ScreenId::VisualEffects,
            ScreenId::DataViz,
            ScreenId::WidgetGallery,
            ScreenId::VirtualizedSearch,
            ScreenId::LogSearch,
        ] {
            assert!(
                screens.contains(&required),
                "the tour never visits {required:?}"
            );
        }
    }

    #[test]
    fn search_steps_actually_type_a_query() {
        let steps = build_steps();
        let shakespeare = steps
            .iter()
            .find(|s| s.id.contains("search") && s.screen == ScreenId::Shakespeare)
            .expect("shakespeare search step");
        assert!(
            shakespeare
                .actions
                .iter()
                .any(|a| a.input == TourInput::Char('/')),
            "the search step must open the search bar"
        );
        let typed_chars = shakespeare
            .actions
            .iter()
            .filter(|a| matches!(a.input, TourInput::Char(c) if c.is_ascii_alphabetic()))
            .count();
        assert!(
            typed_chars >= 5,
            "the search step should type a real query, got {typed_chars} letters"
        );
    }

    #[test]
    fn text_search_steps_walk_matches_without_typing_into_the_query() {
        // Shakespeare and Code Explorer keep focus in the search field after a
        // query, so Enter/Down step matches while a plain letter is appended to
        // the query instead. Driving them with `n` produced "Hamletnnnn" and no
        // matches, which is exactly what this pins.
        for screen in [ScreenId::Shakespeare, ScreenId::CodeExplorer] {
            let steps = build_steps();
            let step = steps
                .iter()
                .find(|s| s.screen == screen && s.id.contains("search"))
                .expect("search step");
            let slash = step
                .actions
                .iter()
                .position(|a| a.input == TourInput::Char('/'))
                .expect("search step opens the field with /");
            // The query is the contiguous run of characters after the slash;
            // letters inside it are the search term, not navigation.
            let after_query = slash
                + 1
                + step.actions[slash + 1..]
                    .iter()
                    .position(|a| !matches!(a.input, TourInput::Char(_)))
                    .expect("search step must do something after typing the query");
            let tail = &step.actions[after_query..];
            let walks_with_enter = tail
                .iter()
                .filter(|a| a.phase == TourPhase::Press)
                .filter(|a| matches!(a.input, TourInput::Enter | TourInput::Down))
                .count();
            assert!(
                walks_with_enter >= 3,
                "{screen:?} should step matches with Enter/Down"
            );
            assert!(
                !tail
                    .iter()
                    .filter(|a| a.phase == TourPhase::Press)
                    .any(|a| matches!(a.input, TourInput::Char(_))),
                "{screen:?} must not press a plain key while the query field has focus"
            );
        }
    }

    #[test]
    fn tour_next_prev_clamps() {
        let mut tour = GuidedTourState::new();
        tour.start(ScreenId::Dashboard, 0, 1.0);
        let first_idx = tour.step_index();
        let first_screen = tour.active_screen();
        let _ = tour.prev_step();
        assert_eq!(tour.step_index(), first_idx);
        assert_eq!(tour.active_screen(), first_screen);
        if tour.step_count() < 2 {
            return;
        }
        let _ = tour.next_step(TourAdvanceReason::ManualNext);
        assert_eq!(tour.step_index(), first_idx + 1);
    }

    #[test]
    fn tour_start_clamps_speed_and_index() {
        let mut tour = GuidedTourState::new();
        let count = tour.step_count();
        assert!(count > 0);

        tour.start(ScreenId::Dashboard, usize::MAX, SPEED_MAX * 2.0);
        assert_eq!(tour.step_index(), count - 1);
        assert!((tour.speed() - SPEED_MAX).abs() < f64::EPSILON);
    }

    #[test]
    fn tour_stop_returns_resume_or_last() {
        let mut tour = GuidedTourState::new();
        tour.start(ScreenId::Dashboard, 0, 1.0);
        let _ = tour.next_step(TourAdvanceReason::ManualNext);
        let last = tour.active_screen();
        let screen = tour.stop(true);
        assert_eq!(screen, last);

        tour.start(ScreenId::MarkdownRichText, 0, 1.0);
        let screen = tour.stop(false);
        assert_eq!(screen, ScreenId::MarkdownRichText);
    }

    #[test]
    fn tour_jump_to_same_index_noop() {
        let mut tour = GuidedTourState::new();
        tour.start(ScreenId::Dashboard, 0, 1.0);
        assert!(tour.jump_to(0).is_none());
    }

    #[test]
    fn tour_jump_to_emits_event() {
        let mut tour = GuidedTourState::new();
        tour.start(ScreenId::Dashboard, 0, 1.0);
        if tour.step_count() < 2 {
            return;
        }
        let from = tour.active_screen();
        let event = tour.jump_to(1).expect("jump to next step");
        match event {
            TourEvent::StepChanged {
                from: seen_from,
                reason,
                ..
            } => {
                assert_eq!(seen_from, from);
                assert_eq!(reason, TourAdvanceReason::Jump);
            }
            _ => panic!("expected step change"),
        }
    }

    #[test]
    fn tour_jump_to_clamps_to_last() {
        let mut tour = GuidedTourState::new();
        tour.active = true;
        tour.steps = vec![
            test_step(ScreenId::Dashboard, "First", 1000, None),
            test_step(ScreenId::MarkdownRichText, "Second", 1000, None),
        ];
        tour.step_index = 0;

        let event = tour.jump_to(99).expect("jump to last step");
        match event {
            TourEvent::StepChanged { to, reason, .. } => {
                assert_eq!(to, ScreenId::MarkdownRichText);
                assert_eq!(reason, TourAdvanceReason::Jump);
            }
            _ => panic!("expected step change"),
        }
        assert_eq!(tour.step_index(), 1);
    }

    #[test]
    fn overlay_state_window_and_highlight() {
        let mut tour = GuidedTourState::new();
        tour.active = true;
        tour.paused = false;
        tour.speed = 1.0;
        tour.step_index = 1;
        tour.step_elapsed = Duration::from_millis(900);
        tour.steps = vec![
            test_step(ScreenId::Dashboard, "First", 3000, None),
            test_step(
                ScreenId::MarkdownRichText,
                "Second",
                2000,
                Some(TourHighlight::new_pct(0.8, 0.8, 0.6, 0.6)),
            ),
            test_step(ScreenId::VisualEffects, "Third", 1000, None),
        ];

        let area = Rect::new(3, 4, 20, 10);
        let overlay = tour.overlay_state(area, 3).expect("overlay state");
        assert_eq!(overlay.step_index, 1);
        assert_eq!(overlay.steps.len(), 3);
        assert!(overlay.steps.iter().any(|step| step.is_current));
        assert_eq!(overlay.remaining, Duration::from_millis(1100));
        let highlight = overlay.highlight.expect("highlight rect");
        assert!(highlight.x >= area.x);
        assert!(highlight.y >= area.y);
        assert!(highlight.right() <= area.right());
        assert!(highlight.bottom() <= area.bottom());
    }

    #[test]
    fn overlay_state_handles_tiny_area() {
        let mut tour = GuidedTourState::new();
        tour.active = true;
        tour.paused = true;
        tour.speed = 1.0;
        tour.step_index = 0;
        tour.step_elapsed = Duration::from_millis(250);
        tour.steps = vec![test_step(
            ScreenId::Dashboard,
            "First",
            1000,
            Some(TourHighlight::new_pct(0.9, 0.9, 0.8, 0.8)),
        )];

        let area = Rect::new(0, 0, 1, 1);
        let overlay = tour.overlay_state(area, 0).expect("overlay state");
        assert_eq!(overlay.steps.len(), 1);
        let highlight = overlay.highlight.expect("highlight rect");
        assert!(highlight.x >= area.x);
        assert!(highlight.y >= area.y);
        assert!(highlight.right() <= area.right());
        assert!(highlight.bottom() <= area.bottom());
    }

    #[test]
    fn overlay_state_handles_large_area() {
        let mut tour = GuidedTourState::new();
        tour.active = true;
        tour.paused = false;
        tour.speed = 1.0;
        tour.step_index = 0;
        tour.step_elapsed = Duration::from_millis(100);
        tour.steps = vec![test_step(
            ScreenId::Dashboard,
            "First",
            1000,
            Some(TourHighlight::new_pct(0.2, 0.2, 0.3, 0.4)),
        )];

        let area = Rect::new(2, 3, 120, 40);
        let overlay = tour.overlay_state(area, 5).expect("overlay state");
        let highlight = overlay.highlight.expect("highlight rect");
        assert!(highlight.x >= area.x);
        assert!(highlight.y >= area.y);
        assert!(highlight.right() <= area.right());
        assert!(highlight.bottom() <= area.bottom());
    }

    #[test]
    fn tour_steps_exclude_guided_tour_screen() {
        let steps = build_steps();
        assert!(!steps.is_empty());
        assert!(steps.iter().all(|step| step.screen != ScreenId::GuidedTour));
        assert!(steps.iter().all(|step| step.hint.is_some()));
    }

    #[test]
    fn highlight_resolves_within_bounds() {
        let highlight = TourHighlight::new_pct(0.95, 0.95, 0.8, 0.8);
        let area = Rect::new(4, 2, 16, 8);
        let rect = highlight.resolve(area);
        assert!(rect.x >= area.x);
        assert!(rect.y >= area.y);
        assert!(rect.width >= 1);
        assert!(rect.height >= 1);
        assert!(rect.right() <= area.right());
        assert!(rect.bottom() <= area.bottom());
    }

    #[test]
    fn highlight_resolve_zero_area_no_panic() {
        let highlight = TourHighlight::new_pct(0.5, 0.5, 0.5, 0.5);
        // Both dimensions zero
        let rect = highlight.resolve(Rect::new(0, 0, 0, 0));
        assert_eq!(rect, Rect::default());
        // Width zero
        let rect = highlight.resolve(Rect::new(5, 5, 0, 10));
        assert_eq!(rect, Rect::default());
        // Height zero
        let rect = highlight.resolve(Rect::new(5, 5, 10, 0));
        assert_eq!(rect, Rect::default());
    }

    #[test]
    fn normalize_speed_handles_bounds() {
        assert!((normalize_speed(0.1) - SPEED_MIN).abs() < f64::EPSILON);
        assert!((normalize_speed(10.0) - SPEED_MAX).abs() < f64::EPSILON);
        assert_eq!(normalize_speed(-1.0), 1.0);
        assert_eq!(normalize_speed(f64::NAN), 1.0);
    }

    #[test]
    fn scale_duration_rounds_and_clamps() {
        let delta = Duration::from_micros(1500);
        assert_eq!(scale_duration(delta, 2.0), Duration::from_micros(3000));
        assert_eq!(scale_duration(delta, 0.0), Duration::ZERO);
    }
}
