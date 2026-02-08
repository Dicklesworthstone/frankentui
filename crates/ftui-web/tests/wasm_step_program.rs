#![cfg(target_arch = "wasm32")]
#![forbid(unsafe_code)]

use core::time::Duration;

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use ftui_render::frame::Frame;
use ftui_runtime::program::{Cmd, Model};
use ftui_runtime::render_trace::checksum_buffer;
use ftui_web::step_program::StepProgram;
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
}

impl From<Event> for CounterMsg {
    fn from(event: Event) -> Self {
        match event {
            Event::Key(key) if key.code == KeyCode::Char('+') => Self::Increment,
            Event::Key(key) if key.code == KeyCode::Char('-') => Self::Decrement,
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

    program.push_event(key_event('+'));
    program.push_event(key_event('+'));
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

    program.resize(20, 3);
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

    program.push_event(key_event('-'));
    program.push_event(Event::Tick);
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
fn wasm_step_program_event_flow_updates_model_and_buffer() {
    let mut program = StepProgram::new(CounterModel::default(), 16, 2);
    program.init().expect("initialization should succeed");

    program.push_event(key_event('+'));
    program.push_event(key_event('+'));
    program.push_event(key_event('-'));
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
}

#[wasm_bindgen_test]
fn wasm_step_program_replay_produces_identical_checksums() {
    let run_a = scenario_checksums();
    let run_b = scenario_checksums();

    assert!(!run_a.is_empty());
    assert_eq!(run_a, run_b);
}
