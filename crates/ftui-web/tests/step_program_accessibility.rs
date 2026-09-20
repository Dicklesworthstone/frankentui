#![forbid(unsafe_code)]

//! Exercise the host-driven render/callback/drain path, not announcement helpers.

use std::cell::Cell;

use ftui_a11y::node::{A11yNodeInfo, A11yRole, LiveRegion};
use ftui_a11y::tree::{AnnouncementReason, ScreenReaderAnnouncement, ScreenReaderPolicy};
use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_render::cell::Cell as RenderCell;
use ftui_render::frame::Frame;
use ftui_runtime::program::{AccessibilityFrame, Cmd, Model};
use ftui_web::WebBackend;
use ftui_web::step_program::StepProgram;

const PRIVATE_TEXT: &str = "PRIVATE_A11Y_VALUE_\u{00e9}\u{754c}";

#[derive(Clone, Copy, Default)]
enum Hook {
    #[default]
    Observe,
    Mutate,
    Message,
    Quit,
    InitQuit,
}

#[derive(Debug)]
struct ObservedFrame {
    index: u64,
    focus: Option<u64>,
    order: Vec<u64>,
    announcements: Vec<ScreenReaderAnnouncement>,
    dropped: usize,
}

#[derive(Default)]
struct Controls {
    disabled: bool,
    moved: bool,
    edited: bool,
    status_focused: bool,
    live: bool,
    explicit_focus: Option<u64>,
    hook: Hook,
    frames: Vec<ObservedFrame>,
    views: Cell<usize>,
    collection_seen: Cell<bool>,
}

enum Message {
    Disable,
    FocusStatus,
    Move,
    Edit,
    Noop,
}

impl From<Event> for Message {
    fn from(event: Event) -> Self {
        match event {
            Event::Key(key) => match key.code {
                KeyCode::Char('d') => Self::Disable,
                KeyCode::Char('f') => Self::FocusStatus,
                KeyCode::Char('m') => Self::Move,
                KeyCode::Char('e') => Self::Edit,
                _ => Self::Noop,
            },
            _ => Self::Noop,
        }
    }
}

impl Model for Controls {
    type Message = Message;

    fn init(&mut self) -> Cmd<Message> {
        if matches!(self.hook, Hook::InitQuit) {
            Cmd::Quit
        } else {
            Cmd::none()
        }
    }

    fn update(&mut self, message: Message) -> Cmd<Message> {
        match message {
            Message::Disable => self.disabled = !self.disabled,
            Message::FocusStatus => {
                self.status_focused = true;
                self.live = true;
            }
            Message::Move => self.moved = !self.moved,
            Message::Edit => self.edited = !self.edited,
            Message::Noop => {}
        }
        Cmd::none()
    }

    fn view(&self, frame: &mut Frame) {
        self.views.set(self.views.get() + 1);
        self.collection_seen.set(frame.a11y_enabled());
        frame.buffer.set_raw(
            0,
            0,
            RenderCell::from_char(if self.disabled { 'D' } else { 'E' }),
        );
        let mut input = A11yNodeInfo::new(
            7,
            A11yRole::TextInput,
            Rect::new(1 + u16::from(self.moved), 1, 20, 1),
        )
        .with_name("Email")
        .with_description("Help");
        input.state.focused = !self.status_focused;
        input.state.disabled = self.disabled;
        input.state.value_text = Some(if self.edited {
            format!("{PRIVATE_TEXT} edited")
        } else {
            PRIVATE_TEXT.to_owned()
        });
        let mut status =
            A11yNodeInfo::new(8, A11yRole::Label, Rect::new(1, 2, 20, 1)).with_name("Done");
        status.state.focused = self.status_focused;
        if self.live {
            status.live_region = Some(LiveRegion::Assertive);
        }
        frame.with_a11y_scope(
            A11yNodeInfo::new(100, A11yRole::Group, frame.area()).with_name("Root"),
            |frame| frame.push_a11y_nodes([input, status]),
        );
        if let Some(id) = self.explicit_focus
            && let Some(builder) = frame.a11y.as_deref_mut()
        {
            builder.set_focused(Some(id));
        }
    }

    fn on_accessibility(&mut self, frame: AccessibilityFrame<'_>) -> Cmd<Message> {
        assert_eq!(frame.tree.node_count(), 3);
        assert_eq!(frame.tree.root_id(), Some(100));
        assert_eq!(frame.tree.node(100).unwrap().children, [7, 8]);
        assert_eq!(frame.tree.node(7).unwrap().parent, Some(100));
        self.frames.push(ObservedFrame {
            index: frame.frame_idx,
            focus: frame.tree.focused_id(),
            order: frame.order.to_vec(),
            announcements: frame.announcements.to_vec(),
            dropped: frame.dropped,
        });
        if self.frames.len() == 1 {
            match self.hook {
                Hook::Mutate => self.disabled = true,
                Hook::Message => return Cmd::msg(Message::Disable),
                Hook::Quit => return Cmd::Quit,
                Hook::Observe | Hook::InitQuit => {}
            }
        }
        Cmd::none()
    }
}

fn event(code: char) -> Event {
    Event::Key(KeyEvent {
        code: KeyCode::Char(code),
        modifiers: Modifiers::empty(),
        kind: KeyEventKind::Press,
    })
}

fn program(model: Controls) -> StepProgram<Controls> {
    StepProgram::new(model, 40, 4).with_accessibility(ScreenReaderPolicy::default())
}

fn first_cell(runner: &StepProgram<Controls>) -> Option<char> {
    runner
        .outputs()
        .last_buffer
        .as_ref()
        .unwrap()
        .get(0, 0)
        .unwrap()
        .content
        .as_char()
}

fn input_disabled(runner: &StepProgram<Controls>) -> bool {
    runner
        .accessibility_tree()
        .unwrap()
        .node(7)
        .unwrap()
        .state
        .disabled
}

fn assert_empty_drain(runner: &mut StepProgram<Controls>) {
    let batch = runner.take_accessibility_announcements();
    assert!(batch.announcements.is_empty());
    assert_eq!(batch.dropped_count, 0);
}

#[test]
fn both_constructors_leave_accessibility_disabled_by_default() {
    for mut runner in [
        StepProgram::new(Controls::default(), 40, 4),
        StepProgram::with_backend(Controls::default(), WebBackend::new(40, 4)),
    ] {
        runner.init().unwrap();
        runner.push_event(event('d')).unwrap();
        assert!(runner.step().unwrap().rendered);
        assert!(!runner.model().collection_seen.get());
        assert!(runner.model().frames.is_empty());
        assert!(runner.accessibility_tree().is_none());
        assert!(runner.accessibility_mirror().is_none());
        assert!(runner.accessibility_order().is_empty());
        assert_empty_drain(&mut runner);
        assert!(!runner.step().unwrap().rendered);
    }
}

#[test]
fn initial_frame_finalizes_scopes_and_state_or_explicit_focus() {
    for explicit_focus in [None, Some(8)] {
        let model = Controls {
            explicit_focus,
            ..Controls::default()
        };
        let mut runner = StepProgram::with_backend(model, WebBackend::new(40, 4))
            .with_accessibility(ScreenReaderPolicy::default());
        runner.init().unwrap();
        assert!(runner.model().collection_seen.get());
        assert_eq!(runner.model().frames.len(), 1);
        let observed = &runner.model().frames[0];
        assert_eq!(observed.index, 0);
        assert_eq!(observed.focus, Some(explicit_focus.unwrap_or(7)));
        assert_eq!(observed.order, [100, 7, 8]);
        assert_eq!(runner.accessibility_order(), observed.order);
        assert_eq!(
            runner.accessibility_tree().unwrap().focused_id(),
            observed.focus
        );
        assert_eq!(runner.accessibility_announcements(), observed.announcements);
        let mirror = runner.accessibility_mirror().unwrap();
        assert_eq!(mirror.lines.len(), 3);
        assert_eq!(mirror.lines[0], "group: Root");
        assert_eq!(mirror.omitted_nodes, 0);
    }
}

#[test]
fn speech_and_visual_drains_are_independent_and_do_not_replay() {
    let mut runner = program(Controls::default());
    runner.init().unwrap();
    let expected = runner.model().frames[0].announcements.clone();
    assert_eq!(expected.len(), 1);
    assert!(runner.take_outputs().last_buffer.is_some());
    assert_eq!(runner.accessibility_announcements(), expected);
    let batch = runner.take_accessibility_announcements();
    assert_eq!(batch.announcements, expected);
    assert_eq!(batch.dropped_count, 0);
    assert_empty_drain(&mut runner);
    assert!(runner.accessibility_tree().is_some());

    // Exactly one follow-up render for callback mutations, then the runner
    // settles without an event or tick. The stable tree does not notify again.
    assert!(runner.step().unwrap().rendered);
    assert_eq!(runner.model().frames.len(), 1);
    assert!(runner.accessibility_announcements().is_empty());
    assert!(!runner.step().unwrap().rendered);
    assert_eq!(runner.model().views.get(), 2);
}

#[test]
fn callback_mutations_and_commands_render_on_the_next_host_step() {
    for hook in [Hook::Mutate, Hook::Message] {
        let mut runner = program(Controls {
            hook,
            ..Controls::default()
        });
        runner.init().unwrap();
        assert!(runner.model().disabled);
        // The published tree and visual buffer describe the completed frame,
        // not state mutated by the callback after that frame was presented.
        assert!(!input_disabled(&runner));
        assert_eq!(first_cell(&runner), Some('E'));
        runner.take_accessibility_announcements();
        let step = runner.step().unwrap();
        assert!(step.rendered);
        assert_eq!(step.events_processed, 0);
        assert_eq!(runner.model().frames.len(), 2);
        assert_eq!(runner.model().frames[1].index, 1);
        assert!(input_disabled(&runner));
        assert_eq!(first_cell(&runner), Some('D'));
        let batch = runner.take_accessibility_announcements();
        assert_eq!(batch.announcements.len(), 1);
        assert_eq!(
            batch.announcements[0].reason,
            AnnouncementReason::FocusedStateChanged
        );
        assert_eq!(batch.announcements[0].text, "textInput: Email. disabled");
        assert!(runner.step().unwrap().rendered);
        assert!(!runner.step().unwrap().rendered);
        assert_eq!(runner.model().frames.len(), 2);
    }
}

#[test]
fn retained_state_changes_arrive_but_layout_and_typing_stay_silent() {
    let mut runner = program(Controls::default());
    runner.init().unwrap();
    runner.take_accessibility_announcements();
    runner.push_event(event('d')).unwrap();
    runner.step().unwrap();
    let batch = runner.take_accessibility_announcements();
    assert_eq!(batch.announcements.len(), 1);
    assert_eq!(batch.announcements[0].node_id, Some(7));
    assert_eq!(batch.announcements[0].text, "textInput: Email. disabled");
    for code in ['m', 'e'] {
        let callbacks = runner.model().frames.len();
        runner.push_event(event(code)).unwrap();
        runner.step().unwrap();
        assert_eq!(runner.model().frames.len(), callbacks + 1);
        assert_empty_drain(&mut runner);
        assert_eq!(runner.accessibility_tree().unwrap().focused_id(), Some(7));
    }
}

#[test]
fn arriving_focus_and_live_updates_coalesce_before_the_web_batch_cap() {
    let policy = ScreenReaderPolicy {
        max_announcements: 1,
        ..ScreenReaderPolicy::default()
    };
    let mut runner = StepProgram::new(Controls::default(), 40, 4).with_accessibility(policy);
    runner.init().unwrap();
    runner.take_accessibility_announcements();
    runner.push_event(event('f')).unwrap();
    runner.step().unwrap();
    let batch = runner.take_accessibility_announcements();
    assert_eq!(batch.announcements.len(), 1);
    assert_eq!(batch.dropped_count, 0);
    assert_eq!(
        batch.announcements[0].reason,
        AnnouncementReason::FocusChanged
    );
    assert_eq!(batch.announcements[0].urgency, LiveRegion::Assertive);
    assert_eq!(batch.announcements[0].text, "label: Done. focused");
    assert_eq!(runner.model().frames.last().unwrap().dropped, 0);
}

#[test]
fn limits_and_drop_counts_apply_before_callback_and_drain() {
    for cap in [0, 1] {
        let policy = ScreenReaderPolicy {
            max_announcements: cap,
            max_mirror_nodes: cap,
            max_text_chars: 12,
        };
        let mut runner = StepProgram::new(
            Controls {
                live: true,
                ..Controls::default()
            },
            40,
            4,
        )
        .with_accessibility(policy);
        runner.init().unwrap();
        assert_eq!(runner.model().frames.len(), 1);
        assert_eq!(runner.model().frames[0].dropped, 2 - cap);
        let batch = runner.take_accessibility_announcements();
        assert_eq!(batch.announcements.len(), cap);
        assert_eq!(batch.dropped_count, 2 - cap);
        assert!(
            batch
                .announcements
                .iter()
                .all(|a| a.text.chars().count() <= 12)
        );
        let mirror = runner.accessibility_mirror().unwrap();
        assert_eq!(mirror.lines.len(), cap);
        assert_eq!(mirror.omitted_nodes, 3 - cap);
        assert!(mirror.lines.iter().all(|line| line.chars().count() <= 12));
        assert_empty_drain(&mut runner);
    }
}

#[test]
fn policy_changes_preserve_focus_baseline_and_disabling_releases_local_content() {
    let mut runner = StepProgram::new(Controls::default(), 40, 4);
    runner.init().unwrap();
    let policy = ScreenReaderPolicy::default();
    runner.set_accessibility_policy(Some(policy));
    assert!(runner.step().unwrap().rendered);
    assert_eq!(runner.model().frames.len(), 1);
    let expected = runner.accessibility_announcements().to_vec();
    runner.set_accessibility_policy(Some(policy));
    assert_eq!(
        runner.take_accessibility_announcements().announcements,
        expected
    );

    runner.set_accessibility_policy(Some(ScreenReaderPolicy {
        max_announcements: 1,
        ..policy
    }));
    assert!(runner.step().unwrap().rendered);
    assert_eq!(runner.model().frames.len(), 1);
    assert_empty_drain(&mut runner);
    runner.push_event(event('d')).unwrap();
    runner.step().unwrap();
    assert_eq!(runner.accessibility_announcements().len(), 1);
    runner.set_accessibility_policy(None);
    assert!(runner.accessibility_tree().is_none());
    assert!(runner.accessibility_mirror().is_none());
    assert!(runner.accessibility_order().is_empty());
    assert_empty_drain(&mut runner);
    let callbacks = runner.model().frames.len();
    runner.step().unwrap();
    assert!(!runner.model().collection_seen.get());
    assert_eq!(runner.model().frames.len(), callbacks);
    runner.set_accessibility_policy(Some(policy));
    runner.step().unwrap();
    assert_eq!(runner.model().frames.len(), callbacks + 1);
    let batch = runner.take_accessibility_announcements();
    assert_eq!(batch.announcements.len(), 1);
    assert_eq!(
        batch.announcements[0].reason,
        AnnouncementReason::FocusChanged
    );
    assert!(batch.announcements[0].text.contains("disabled"));
}

#[test]
fn callback_quit_keeps_the_completed_frame_without_more_rendering() {
    let mut runner = program(Controls {
        hook: Hook::Quit,
        ..Controls::default()
    });
    runner.init().unwrap();
    assert!(!runner.is_running());
    assert_eq!(runner.frame_idx(), 1);
    assert!(runner.take_outputs().last_buffer.is_some());
    assert_eq!(
        runner
            .take_accessibility_announcements()
            .announcements
            .len(),
        1
    );
    assert!(!runner.step().unwrap().rendered);
    assert_eq!(runner.model().frames.len(), 1);
    assert_empty_drain(&mut runner);

    let mut no_frame = program(Controls {
        hook: Hook::InitQuit,
        ..Controls::default()
    });
    no_frame.init().unwrap();
    assert_eq!(no_frame.frame_idx(), 0);
    assert!(no_frame.model().frames.is_empty());
    assert!(no_frame.outputs().last_buffer.is_none());
    assert!(no_frame.accessibility_tree().unwrap().is_empty());
}

#[test]
fn visual_output_and_geometry_logs_do_not_capture_accessibility_text() {
    let mut plain = StepProgram::new(Controls::default(), 40, 4);
    let mut accessible = program(Controls::default());
    plain.init().unwrap();
    accessible.init().unwrap();
    let mirror = accessible.accessibility_mirror().unwrap();
    assert!(mirror.text().contains(PRIVATE_TEXT));
    assert!(
        accessible.model().frames[0].announcements[0]
            .text
            .contains(PRIVATE_TEXT)
    );
    for code in ['d', 'm', 'e'] {
        plain.push_event(event(code)).unwrap();
        accessible.push_event(event(code)).unwrap();
        plain.step().unwrap();
        accessible.step().unwrap();
        assert_eq!(first_cell(&plain), first_cell(&accessible));
        let a = plain.take_outputs();
        let b = accessible.take_outputs();
        assert_eq!(a.logs, b.logs);
        assert!(b.logs.is_empty());
    }
    let callbacks = accessible.model().frames.len();
    accessible.resize(40, 4).unwrap();
    accessible.step().unwrap();
    assert_eq!(accessible.model().frames.len(), callbacks);
    assert_empty_drain(&mut accessible);
    let output = accessible.take_outputs();
    assert!(output.last_full_repaint_hint);
    assert!(!output.logs.is_empty());
    assert!(output.logs.iter().all(|line| !line.contains(PRIVATE_TEXT)));
}

#[test]
fn unread_frames_replace_the_batch_instead_of_growing_a_speech_queue() {
    let mut runner = program(Controls::default());
    runner.init().unwrap();
    for _ in 0..100 {
        runner.push_event(event('d')).unwrap();
        runner.step().unwrap();
        assert_eq!(runner.accessibility_announcements().len(), 1);
    }
    let batch = runner.take_accessibility_announcements();
    assert_eq!(batch.announcements.len(), 1);
    assert_eq!(batch.announcements[0].text, "textInput: Email. enabled");
    assert_eq!(batch.dropped_count, 0);
    assert_empty_drain(&mut runner);
}
