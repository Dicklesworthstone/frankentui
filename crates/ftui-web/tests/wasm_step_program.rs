#![cfg(target_arch = "wasm32")]
#![forbid(unsafe_code)]

use core::time::Duration;

use ftui_core::event::{Event, ImeEvent, KeyCode, KeyEvent, KeyEventKind, Modifiers, PasteEvent};
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use ftui_render::frame::Frame;
use ftui_runtime::program::{Cmd, Model};
use ftui_runtime::render_trace::checksum_buffer;
use ftui_web::step_program::StepProgram;
use ftui_web::{WebBackendError, WebEventSource, WebInputLimit};
use wasm_bindgen_test::wasm_bindgen_test;

#[derive(Default)]
struct CounterModel {
    value: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CounterMsg {
    Increment,
    Decrement,
    Noop,
    Quit,
}

impl From<Event> for CounterMsg {
    fn from(event: Event) -> Self {
        match event {
            Event::Key(key) if key.code == KeyCode::Char('+') => Self::Increment,
            Event::Key(key) if key.code == KeyCode::Char('-') => Self::Decrement,
            Event::Key(key) if key.code == KeyCode::Char('q') => Self::Quit,
            Event::Tick => Self::Increment,
            _ => Self::Noop,
        }
    }
}

impl Model for CounterModel {
    type Message = CounterMsg;

    fn init(&mut self) -> Cmd<Self::Message> {
        Cmd::none()
    }

    fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
        match msg {
            CounterMsg::Increment => self.value += 1,
            CounterMsg::Decrement => self.value -= 1,
            CounterMsg::Noop => {}
            CounterMsg::Quit => return Cmd::quit(),
        }
        Cmd::none()
    }

    fn view(&self, frame: &mut Frame) {
        let text = format!("count={}", self.value);
        for (index, ch) in text.chars().enumerate() {
            if (index as u16) >= frame.width() {
                break;
            }
            frame.buffer.set_raw(index as u16, 0, Cell::from_char(ch));
        }
    }
}

fn key_event(ch: char) -> Event {
    Event::Key(KeyEvent {
        code: KeyCode::Char(ch),
        modifiers: Modifiers::empty(),
        kind: KeyEventKind::Press,
    })
}

#[wasm_bindgen_test]
fn model_log_modes_apply_policy_in_actual_wasm() {
    struct LogModel;
    impl Model for LogModel {
        type Message = Event;
        fn init(&mut self) -> Cmd<Event> {
            let payload = "a\x1b[31mb\x1b[2Jc\x1b]0;secret\x07d\n";
            Cmd::sequence(vec![
                Cmd::log(payload),
                Cmd::log_sgr_only(payload),
                Cmd::log_raw(payload),
                Cmd::quit(),
                Cmd::log("unreachable"),
            ])
        }
        fn update(&mut self, _: Event) -> Cmd<Event> {
            Cmd::none()
        }
        fn view(&self, _: &mut Frame) {}
    }
    let mut program = StepProgram::new(LogModel, 80, 24);
    program.init().unwrap();
    assert!(!program.is_running());
    assert_eq!(
        program.take_outputs().logs,
        [
            "abcd\n",
            "a\x1b[31mbcd\n",
            "a\x1b[31mb\x1b[2Jc\x1b]0;secret\x07d\n",
        ]
    );
}

fn buffer_text(buffer: &Buffer) -> String {
    (0..buffer.width())
        .map(|x| {
            buffer
                .get(x, 0)
                .and_then(|cell| cell.content.as_char())
                .unwrap_or(' ')
        })
        .collect()
}

fn scenario_checksums() -> Vec<u64> {
    let mut program = StepProgram::new(CounterModel::default(), 16, 2);
    program.init().expect("initialization should succeed");

    let mut checksums = Vec::new();
    checksums.push(checksum_buffer(
        program
            .outputs()
            .last_buffer
            .as_ref()
            .expect("init should render first frame"),
        program.pool(),
    ));

    program
        .push_event(key_event('+'))
        .expect("key admission should succeed");
    program
        .push_event(key_event('+'))
        .expect("key admission should succeed");
    let step_1 = program.step().expect("step 1 should succeed");
    if step_1.rendered {
        checksums.push(checksum_buffer(
            program
                .outputs()
                .last_buffer
                .as_ref()
                .expect("step 1 should have rendered"),
            program.pool(),
        ));
    }

    program
        .resize(20, 3)
        .expect("resize admission should succeed");
    program.advance_time(Duration::from_millis(17));
    let step_2 = program.step().expect("step 2 should succeed");
    if step_2.rendered {
        checksums.push(checksum_buffer(
            program
                .outputs()
                .last_buffer
                .as_ref()
                .expect("step 2 should have rendered"),
            program.pool(),
        ));
    }
    assert_eq!(program.size(), (20, 3));
    let resized = program
        .outputs()
        .last_buffer
        .as_ref()
        .expect("step 2 should have rendered");
    assert_eq!(resized.width(), 20);
    assert_eq!(resized.height(), 3);

    program
        .push_event(key_event('-'))
        .expect("key admission should succeed");
    program
        .push_event(Event::Tick)
        .expect("tick admission should succeed");
    program.advance_time(Duration::from_millis(17));
    let step_3 = program.step().expect("step 3 should succeed");
    if step_3.rendered {
        checksums.push(checksum_buffer(
            program
                .outputs()
                .last_buffer
                .as_ref()
                .expect("step 3 should have rendered"),
            program.pool(),
        ));
    }

    assert_eq!(checksums.len(), 4);
    checksums
}

#[wasm_bindgen_test]
fn wasm_quit_replays_without_a_frame_and_preserves_the_accepted_tail() {
    use ftui_web::session_record::{SessionRecorder, SessionTrace, replay};

    let mut recorder = SessionRecorder::new(CounterModel::default(), 16, 2, 7);
    recorder.init().unwrap();
    let tail = vec![
        Event::Paste(PasteEvent::bracketed("尾巴 🦀")),
        Event::Resize {
            width: 30,
            height: 4,
        },
    ];
    for event in [key_event('+'), key_event('q')]
        .into_iter()
        .chain(tail.clone())
    {
        recorder.push_event(1, event).unwrap();
    }
    let outcome = recorder.step().unwrap();
    assert!(!outcome.running && !outcome.rendered);
    assert_eq!(outcome.events_processed, 2);
    assert_eq!(outcome.events_pending, 2);
    assert_eq!(recorder.program().model().value, 1);
    assert_eq!(recorder.program().size(), (16, 2));
    let jsonl = recorder.finish().to_jsonl().replace('🦀', r"\ud83e\udd80");
    let trace = SessionTrace::from_jsonl_validated(&jsonl).unwrap();
    let replayed = replay(CounterModel::default(), &trace).unwrap();
    assert!(replayed.ok());
    assert_eq!(replayed.total_steps, 2);
    assert_eq!(replayed.total_frames, 1);
    assert!(!replayed.running);
    assert_eq!(replayed.unprocessed_events, tail);

    let mut program = StepProgram::new(CounterModel::default(), 16, 2);
    program.init().unwrap();
    program.push_event(key_event('q')).unwrap();
    for event in &tail {
        program.push_event(event.clone()).unwrap();
    }
    assert_eq!(program.step().unwrap().events_pending, 2);
    assert_eq!(program.take_pending_events(), tail);
    assert_eq!(program.step().unwrap().events_pending, 0);
}

#[wasm_bindgen_test]
fn wasm_step_program_event_flow_updates_model_and_buffer() {
    let mut program = StepProgram::new(CounterModel::default(), 16, 2);
    program.init().expect("initialization should succeed");

    program
        .push_event(key_event('+'))
        .expect("key admission should succeed");
    program
        .push_event(key_event('+'))
        .expect("key admission should succeed");
    program
        .push_event(key_event('-'))
        .expect("key admission should succeed");
    let result = program.step().expect("step should succeed");

    assert!(result.running);
    assert!(result.rendered);
    assert_eq!(result.events_processed, 3);
    assert_eq!(program.model().value, 1);
    assert_eq!(program.size(), (16, 2));

    let line = buffer_text(
        program
            .outputs()
            .last_buffer
            .as_ref()
            .expect("buffer should exist after render"),
    );
    assert!(line.starts_with("count=1"));
    let outputs = program.outputs();
    assert!(!outputs.last_patches.is_empty());
    let stats = outputs
        .last_patch_stats
        .expect("patch stats should be captured");
    assert!(stats.patch_count >= 1);
    assert!(stats.dirty_cells >= 1);
}

#[wasm_bindgen_test]
fn wasm_step_program_replay_produces_identical_checksums() {
    let run_a = scenario_checksums();
    let run_b = scenario_checksums();

    assert!(!run_a.is_empty());
    assert_eq!(run_a, run_b);
}

#[wasm_bindgen_test]
fn wasm_step_program_overflow_preserves_accepted_input_and_resize_state() {
    let mut program = StepProgram::new(CounterModel::default(), 16, 2);
    program.init().unwrap();
    for _ in 0..WebEventSource::MAX_EVENTS {
        program.push_event(key_event('+')).unwrap();
    }
    assert!(matches!(
        program.push_event(key_event('-')),
        Err(WebBackendError::InputQueueFull {
            limit: WebInputLimit::Events,
            ..
        })
    ));
    assert!(program.resize(20, 3).is_err());
    assert_eq!(program.size(), (16, 2));
    assert_eq!(program.step().unwrap().events_processed, 4096);
    assert_eq!(program.model().value, 4096);
    assert_eq!(program.size(), (16, 2));
    program.push_event(key_event('-')).unwrap();
    program.resize(20, 3).unwrap();
    assert_eq!(program.step().unwrap().events_processed, 2);
    assert_eq!(program.model().value, 4095);
    assert_eq!(program.size(), (20, 3));
    assert!(buffer_text(program.outputs().last_buffer.as_ref().unwrap()).starts_with("count=4095"));
}

#[wasm_bindgen_test]
fn wasm_event_queue_bounds_text_bytes_and_delivers_exact_retry_content() {
    use ftui_backend::BackendEventSource;

    let mut source = WebEventSource::new(16, 2);
    let paste = Event::Paste(PasteEvent::bracketed(
        "界".repeat(WebEventSource::MAX_PAYLOAD_BYTES / 3),
    ));
    source.push_event(paste.clone()).unwrap();
    let tail = Event::Ime(ImeEvent::commit("日本語 👩‍💻"));
    let rejected = source.push_event(tail.clone()).unwrap_err();
    assert!(matches!(
        &rejected,
        WebBackendError::InputQueueFull {
            limit: WebInputLimit::PayloadBytes,
            ..
        }
    ));
    assert_eq!(source.read_event().unwrap(), Some(paste));
    let WebBackendError::InputQueueFull { event, .. } = rejected else {
        unreachable!();
    };
    source.push_event(event).unwrap();
    assert_eq!(source.read_event().unwrap(), Some(tail));
    assert_eq!(source.queued_events(), 0);
    assert_eq!(source.queued_payload_bytes(), 0);
}
