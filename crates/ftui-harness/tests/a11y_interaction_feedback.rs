#![forbid(unsafe_code)]

//! Exercise interaction announcements through Program -> Frame -> tree diff ->
//! Model::on_accessibility, including transitions that must deliver no speech.

use std::cell::Cell;
use std::time::Duration;

use ftui_a11y::node::{A11yNodeInfo, A11yRole, LiveRegion};
use ftui_a11y::tree::{AnnouncementReason, ScreenReaderAnnouncement};
use ftui_core::event::Event;
use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;
use ftui_render::presenter::TerminalCapabilities;
use ftui_runtime::program::{AccessibilityFrame, HeadlessEventSource, Program, ProgramConfig};
use ftui_runtime::{
    BackendFeatures, Cmd, Model, ScreenMode, ScreenReaderPolicy, TerminalPresenter, TerminalWriter,
    UiAnchor,
};

#[derive(Default)]
struct InteractionModel {
    stage: usize,
    views: Cell<usize>,
    batches: Vec<Vec<ScreenReaderAnnouncement>>,
    watchdog_fired: bool,
}

impl Model for InteractionModel {
    type Message = Event;

    fn init(&mut self) -> Cmd<Event> {
        // A regression in callback-driven redraws must fail, not hang the suite.
        // No tick is needed for the successful path through these ten frames.
        Cmd::tick(Duration::from_secs(5))
    }

    fn update(&mut self, _: Event) -> Cmd<Event> {
        self.watchdog_fired = true;
        Cmd::Quit
    }

    fn view(&self, frame: &mut Frame) {
        self.views.set(self.views.get() + 1);
        assert!(self.views.get() <= 32, "accessibility redraw loop did not terminate");

        // These are application-supplied semantics, pushed through the same
        // public Frame API used by widgets. Do not call tree.diff() here.
        let mut choice = A11yNodeInfo::new(
            7,
            A11yRole::Checkbox,
            Rect::new(u16::from(self.stage >= 2), 0, 20, 1),
        )
        .with_name("Choice");
        choice.state.checked = Some(matches!(self.stage, 1 | 2 | 5..));
        choice.state.disabled = self.stage == 4;
        choice.state.required = self.stage == 4;
        if matches!(self.stage, 3 | 4) {
            choice.live_region = Some(LiveRegion::Assertive);
        }

        let mut level = A11yNodeInfo::new(8, A11yRole::Slider, Rect::new(0, 1, 20, 1))
            .with_name("Level");
        level.state.value_now = match self.stage {
            0..=4 => Some(10.0),
            5 => Some(20.0),
            6 => Some(25.0),
            7 | 8 => Some(30.0),
            _ => None,
        };
        if matches!(self.stage, 6 | 7) {
            level.state.value_text = Some("Medium".to_owned());
        }

        // Neither per-node state says focused: the explicit tree focus must
        // survive Frame finalization and remain authoritative for speech.
        assert!(!choice.state.focused && !level.state.focused);
        frame.push_a11y_nodes(vec![choice, level]);
        frame
            .a11y
            .as_deref_mut()
            .expect("accessibility collection enabled")
            .set_focused(Some(if self.stage < 5 { 7 } else { 8 }));
    }

    fn on_accessibility(&mut self, a11y: AccessibilityFrame<'_>) -> Cmd<Event> {
        assert_eq!(self.batches.len(), self.stage, "one callback per transition");
        assert_eq!(a11y.dropped, 0, "duplicate candidates must not consume the cap");
        assert_eq!(a11y.tree.focused_id(), Some(if self.stage < 5 { 7 } else { 8 }));
        assert_eq!(a11y.order, &[7, 8]);
        self.batches.push(a11y.announcements.to_vec());
        if self.stage == 9 {
            Cmd::Quit
        } else {
            self.stage += 1;
            // Changes made by the accessibility hook must appear in the next
            // frame even when its command does not itself request a redraw.
            Cmd::none()
        }
    }
}

fn exercise_interaction_feedback(max_announcements: usize) {
    let features = BackendFeatures::default();
    let writer = TerminalWriter::new(
        Vec::new(),
        ScreenMode::AltScreen,
        UiAnchor::Bottom,
        TerminalCapabilities::default(),
    );
    let config = ProgramConfig::default().with_accessibility(ScreenReaderPolicy {
        max_announcements,
        ..ScreenReaderPolicy::default()
    });
    let mut program = Program::with_event_source(
        InteractionModel::default(),
        HeadlessEventSource::new(40, 4, features),
        features,
        TerminalPresenter::new(writer),
        config,
    )
    .expect("construct runtime");
    program.run().expect("run accessibility feedback sequence");
    let model = program.model();
    assert!(!model.watchdog_fired, "callback-driven redraw sequence stalled");
    assert_eq!(model.batches.len(), 10);
    assert_eq!(model.stage, 9);

    let expected = [
        Some((
            7,
            AnnouncementReason::FocusChanged,
            LiveRegion::Polite,
            "checkbox: Choice. focused, not checked",
        )),
        Some((
            7,
            AnnouncementReason::FocusedStateChanged,
            LiveRegion::Polite,
            "checkbox: Choice. checked",
        )),
        None, // Stage 2 moves bounds; the raw diff changes but speech stays quiet.
        Some((
            7,
            AnnouncementReason::FocusedStateChanged,
            LiveRegion::Assertive,
            "checkbox: Choice. focused, not checked",
        )),
        Some((
            7,
            AnnouncementReason::FocusedStateChanged,
            LiveRegion::Polite,
            "checkbox: Choice. disabled, required",
        )),
        Some((
            8,
            AnnouncementReason::FocusChanged,
            LiveRegion::Polite,
            "slider: Level. focused, value 20",
        )),
        Some((
            8,
            AnnouncementReason::FocusedStateChanged,
            LiveRegion::Polite,
            "slider: Level. value Medium",
        )),
        None, // Stage 7 changes only the number hidden by "Medium".
        Some((
            8,
            AnnouncementReason::FocusedStateChanged,
            LiveRegion::Polite,
            "slider: Level. value 30",
        )),
        Some((
            8,
            AnnouncementReason::FocusedStateChanged,
            LiveRegion::Polite,
            "slider: Level. value unavailable",
        )),
    ];
    for (stage, (actual, expected)) in model.batches.iter().zip(expected).enumerate() {
        if let Some((node_id, reason, urgency, text)) = expected {
            assert_eq!(actual.len(), 1, "stage {stage}");
            assert_eq!(actual[0].node_id, Some(node_id), "stage {stage}");
            assert_eq!(actual[0].reason, reason, "stage {stage}");
            assert_eq!(actual[0].urgency, urgency, "stage {stage}");
            assert_eq!(actual[0].text, text, "stage {stage}");
        } else {
            assert!(actual.is_empty(), "stage {stage}: {actual:?}");
        }
    }
}

#[test]
fn runtime_delivers_focused_interaction_feedback_without_live_region_opt_in() {
    exercise_interaction_feedback(8);
}

#[test]
fn runtime_coalesces_interaction_feedback_before_applying_a_one_message_cap() {
    exercise_interaction_feedback(1);
}
