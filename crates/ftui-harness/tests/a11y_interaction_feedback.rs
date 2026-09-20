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
        assert!(
            self.views.get() <= 32,
            "accessibility redraw loop did not terminate"
        );

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

        let mut level =
            A11yNodeInfo::new(8, A11yRole::Slider, Rect::new(0, 1, 20, 1)).with_name("Level");
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
        assert_eq!(
            self.batches.len(),
            self.stage,
            "one callback per transition"
        );
        assert_eq!(
            a11y.dropped, 0,
            "duplicate candidates must not consume the cap"
        );
        assert_eq!(
            a11y.tree.focused_id(),
            Some(if self.stage < 5 { 7 } else { 8 })
        );
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
    assert!(
        !model.watchdog_fired,
        "callback-driven redraw sequence stalled"
    );
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

mod dialog_runtime {
    use super::*;
    use std::cell::RefCell;

    use ftui_core::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};
    use ftui_render::frame::HitId;
    use ftui_widgets::input::TextInput;
    use ftui_widgets::modal::{Dialog, DialogResult, DialogState};
    use ftui_widgets::{StatefulWidget, Widget};

    enum Message {
        Advance,
        Watchdog,
    }

    impl From<Event> for Message {
        fn from(_: Event) -> Self {
            Self::Watchdog
        }
    }

    struct DialogModel {
        dialog: Dialog,
        state: RefCell<DialogState>,
        stage: usize,
        views: Cell<usize>,
        batches: Vec<Vec<ScreenReaderAnnouncement>>,
        focused_ids: Vec<u64>,
        watchdog_fired: bool,
    }

    impl DialogModel {
        fn new() -> Self {
            Self {
                dialog: Dialog::prompt("Profile", "Enter a name").hit_id(HitId::new(700)),
                state: RefCell::new(DialogState::default()),
                stage: 0,
                views: Cell::new(0),
                batches: Vec::new(),
                focused_ids: Vec::new(),
                watchdog_fired: false,
            }
        }
    }

    impl Model for DialogModel {
        type Message = Message;

        fn init(&mut self) -> Cmd<Message> {
            Cmd::tick(Duration::from_secs(5))
        }

        fn update(&mut self, message: Message) -> Cmd<Message> {
            if matches!(message, Message::Watchdog) {
                self.watchdog_fired = true;
                return Cmd::Quit;
            }
            assert!(self.stage < 7, "unexpected extra dialog transition");
            if self.stage == 0 {
                self.state.get_mut().reset();
            } else {
                let code = match self.stage {
                    1 => KeyCode::Char('\u{00e9}'),
                    2 | 3 => KeyCode::Tab,
                    4 | 5 => KeyCode::BackTab,
                    6 => KeyCode::Enter,
                    _ => unreachable!(),
                };
                let result = self.dialog.handle_event(
                    &Event::Key(KeyEvent {
                        code,
                        modifiers: Modifiers::empty(),
                        kind: KeyEventKind::Press,
                    }),
                    self.state.get_mut(),
                    None,
                );
                if self.stage == 6 {
                    assert_eq!(result, Some(DialogResult::Input("\u{00e9}".to_owned())));
                } else {
                    assert!(result.is_none());
                }
            }
            self.stage += 1;
            Cmd::none()
        }

        fn view(&self, frame: &mut Frame) {
            self.views.set(self.views.get() + 1);
            assert!(
                self.views.get() <= 32,
                "dialog redraw loop did not terminate"
            );
            // The application owns focus transfer and restoration. Both tree
            // metadata and focus come from real widgets, never synthetic nodes
            // or direct mutations of the accessibility builder in this test.
            let mut launcher = TextInput::new().with_focused(self.stage == 0 || self.stage == 7);
            launcher.set_value("Launcher");
            launcher.render(Rect::new(0, 0, 20, 1), frame);
            self.dialog
                .render(Rect::new(0, 1, 80, 23), frame, &mut self.state.borrow_mut());
        }

        fn on_accessibility(&mut self, a11y: AccessibilityFrame<'_>) -> Cmd<Message> {
            assert_eq!(self.batches.len(), self.stage);
            assert_eq!(a11y.dropped, 0);
            let focused = a11y.tree.focused().expect("an actual widget owns focus");
            assert_eq!(
                a11y.tree.nodes().filter(|node| node.state.focused).count(),
                1
            );
            if matches!(self.stage, 1..=6) {
                assert_eq!(a11y.tree.node_count(), 5); // caller + dialog + three controls
                let dialog = a11y
                    .tree
                    .node(700)
                    .expect("dialog semantics reached runtime");
                assert_eq!(dialog.role, A11yRole::Dialog);
                assert_eq!(dialog.children.len(), 3);
                assert_eq!(focused.parent, Some(700));
                if matches!(self.stage, 1 | 2 | 6) {
                    assert_eq!(focused.role, A11yRole::TextInput);
                    assert_eq!(focused.name.as_deref(), Some("Profile"));
                    assert_eq!(
                        focused.state.value_text.as_deref(),
                        Some(if self.stage == 1 { "" } else { "\u{00e9}" })
                    );
                } else {
                    assert_eq!(focused.role, A11yRole::Button);
                    assert_eq!(
                        focused.name.as_deref(),
                        Some(if self.stage == 4 { "Cancel" } else { "OK" })
                    );
                }
            } else {
                assert_eq!(a11y.tree.node_count(), 1);
                assert!(a11y.tree.node(700).is_none());
                assert_eq!(focused.role, A11yRole::TextInput);
            }
            self.focused_ids.push(focused.id);
            self.batches.push(a11y.announcements.to_vec());
            if self.stage == 7 {
                Cmd::Quit
            } else {
                // Go through Program's command dispatch and Model::update,
                // rather than mutating dialog state from the render callback.
                Cmd::msg(Message::Advance)
            }
        }
    }

    fn exercise_dialog(max_announcements: usize) {
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
            DialogModel::new(),
            HeadlessEventSource::new(80, 24, features),
            features,
            TerminalPresenter::new(writer),
            config,
        )
        .expect("construct dialog runtime");
        program
            .run()
            .expect("run actual dialog interaction sequence");
        let model = program.model();
        assert!(!model.watchdog_fired, "dialog callback sequence stalled");
        assert_eq!(model.stage, 7);
        assert_eq!(model.batches.len(), 8);
        assert!(!model.state.borrow().is_open());
        assert_eq!(model.focused_ids[0], model.focused_ids[7]);
        assert_eq!(model.focused_ids[1], model.focused_ids[2]);
        assert_eq!(model.focused_ids[1], model.focused_ids[6]);
        assert_eq!(model.focused_ids[3], model.focused_ids[5]);
        assert_ne!(model.focused_ids[3], model.focused_ids[4]);
        for (stage, batch) in model.batches.iter().enumerate() {
            if stage == 2 {
                // A real prompt edit changes the tree and reaches the callback,
                // but must not reread the entire field on every keypress.
                assert!(batch.is_empty());
                continue;
            }
            assert_eq!(batch.len(), 1, "stage {stage}: {batch:?}");
            assert_eq!(batch[0].reason, AnnouncementReason::FocusChanged);
            assert_eq!(batch[0].urgency, LiveRegion::Polite);
            assert_eq!(batch[0].node_id, Some(model.focused_ids[stage]));
            match stage {
                1 => assert_eq!(batch[0].text, "textInput: Profile. Enter a name. focused"),
                3 | 5 => assert_eq!(batch[0].text, "button: OK. focused"),
                4 => assert_eq!(batch[0].text, "button: Cancel. focused"),
                6 => assert_eq!(
                    batch[0].text,
                    "textInput: Profile. Enter a name. focused, value \u{00e9}"
                ),
                _ => {}
            }
        }
        assert_eq!(model.batches[0], model.batches[7]);
    }

    #[test]
    fn runtime_dialog_events_reach_accessibility_and_restore_caller_focus() {
        exercise_dialog(8);
    }

    #[test]
    fn runtime_dialog_focus_feedback_does_not_duplicate_under_one_message_cap() {
        exercise_dialog(1);
    }
}
