#![forbid(unsafe_code)]

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEvent, MouseEventKind};
use ftui_demo_showcase::app::AppModel;
use ftui_demo_showcase::screens;
use ftui_runtime::ProgramSimulator;
use ftui_runtime::render_trace::checksum_buffer;
use ftui_web::step_program::StepProgram;

#[derive(Debug, Clone, PartialEq, Eq)]
struct FrameParitySignature {
    label: String,
    cols: u16,
    rows: u16,
    terminal_hash: u64,
    web_hash: u64,
}

#[derive(Debug, Clone)]
enum ParityAction {
    Event(Event),
    Resize(u16, u16),
}

fn key_event(code: KeyCode) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers: Modifiers::empty(),
        kind: KeyEventKind::Press,
    })
}

fn mouse_move(x: u16, y: u16) -> Event {
    Event::Mouse(MouseEvent::new(MouseEventKind::Moved, x, y))
}

fn mouse_down(x: u16, y: u16) -> Event {
    Event::Mouse(MouseEvent::new(MouseEventKind::Down(MouseButton::Left), x, y))
}

fn apply_deterministic_profile(
    model: &mut AppModel,
    screen: ftui_demo_showcase::app::ScreenId,
) -> bool {
    #[cfg(feature = "screen-mermaid")]
    {
        match screen {
            ftui_demo_showcase::app::ScreenId::MermaidShowcase => {
                model.screens.mermaid_showcase.stabilize_metrics_for_snapshot();
                true
            }
            ftui_demo_showcase::app::ScreenId::MermaidMegaShowcase => {
                model.screens.mermaid_mega_showcase.stabilize_for_snapshot();
                false
            }
            _ => false,
        }
    }
    #[cfg(not(feature = "screen-mermaid"))]
    {
        let _ = model;
        let _ = screen;
        false
    }
}

fn capture_signature(
    terminal: &mut ProgramSimulator<AppModel>,
    web: &StepProgram<AppModel>,
    cols: u16,
    rows: u16,
    label: String,
) -> FrameParitySignature {
    let terminal_buffer = terminal.capture_frame(cols, rows).clone();
    let terminal_hash = checksum_buffer(&terminal_buffer, terminal.pool());
    let web_buffer = web
        .outputs()
        .last_buffer
        .as_ref()
        .expect("web step program should have a last buffer after init");
    let web_hash = checksum_buffer(web_buffer, web.pool());
    FrameParitySignature {
        label,
        cols,
        rows,
        terminal_hash,
        web_hash,
    }
}

fn assert_signature_parity(signature: &FrameParitySignature) {
    assert_eq!(
        signature.terminal_hash,
        signature.web_hash,
        "terminal/web parity mismatch at {} ({}x{}): terminal=0x{:016x} web=0x{:016x}",
        signature.label,
        signature.cols,
        signature.rows,
        signature.terminal_hash,
        signature.web_hash
    );
}

fn run_screen_sweep_parity(cols: u16, rows: u16) -> Vec<FrameParitySignature> {
    let mut terminal = ProgramSimulator::new(AppModel::new());
    terminal.init();

    let mut web = StepProgram::new(AppModel::new(), cols, rows);
    web.init().expect("web program init should succeed");

    let mut signatures = Vec::new();
    for &screen in screens::screen_ids() {
        terminal.model_mut().current_screen = screen;
        web.model_mut().current_screen = screen;
        let terminal_toggle_mermaid_metrics = apply_deterministic_profile(terminal.model_mut(), screen);
        let web_toggle_mermaid_metrics = apply_deterministic_profile(web.model_mut(), screen);
        assert_eq!(
            terminal_toggle_mermaid_metrics, web_toggle_mermaid_metrics,
            "deterministic profile branch mismatch for {}",
            screen.title()
        );
        if terminal_toggle_mermaid_metrics {
            let mermaid_toggle = key_event(KeyCode::Char('m'));
            terminal.inject_event(mermaid_toggle.clone());
            web.push_event(mermaid_toggle);
        }

        terminal.inject_event(Event::Tick);
        web.push_event(Event::Tick);
        let step = web.step().expect("web step should succeed during sweep");
        assert!(step.rendered, "screen sweep should render {}", screen.title());

        let signature = capture_signature(
            &mut terminal,
            &web,
            cols,
            rows,
            format!("screen-sweep:{}:{}", screen.title(), step.frame_idx),
        );
        assert_signature_parity(&signature);
        signatures.push(signature);
    }

    signatures
}

fn run_interaction_trace_parity(cols: u16, rows: u16) -> Vec<FrameParitySignature> {
    let mut terminal = ProgramSimulator::new(AppModel::new());
    terminal.init();

    let mut web = StepProgram::new(AppModel::new(), cols, rows);
    web.init().expect("web program init should succeed");

    let mut current_cols = cols;
    let mut current_rows = rows;
    let script = vec![
        ParityAction::Event(Event::Tick),
        ParityAction::Event(key_event(KeyCode::Tab)),
        ParityAction::Event(Event::Tick),
        ParityAction::Event(key_event(KeyCode::BackTab)),
        ParityAction::Event(mouse_move(14, 0)),
        ParityAction::Event(mouse_down(28, 0)),
        ParityAction::Event(Event::Tick),
        ParityAction::Resize(100, 30),
        ParityAction::Event(Event::Tick),
        ParityAction::Event(key_event(KeyCode::Char('?'))),
        ParityAction::Event(Event::Tick),
        ParityAction::Event(key_event(KeyCode::Escape)),
        ParityAction::Resize(cols, rows),
        ParityAction::Event(Event::Tick),
    ];

    let mut signatures = Vec::with_capacity(script.len());
    for (idx, action) in script.into_iter().enumerate() {
        let step_label = match action {
            ParityAction::Event(event) => {
                terminal.inject_event(event.clone());
                web.push_event(event.clone());
                let step = web.step().expect("web step should succeed in interaction trace");
                format!("interaction-step-{idx}:event:{event:?}:frame{}", step.frame_idx)
            }
            ParityAction::Resize(next_cols, next_rows) => {
                current_cols = next_cols;
                current_rows = next_rows;
                let resize_event = Event::Resize {
                    width: next_cols,
                    height: next_rows,
                };
                terminal.inject_event(resize_event);
                web.resize(next_cols, next_rows);
                let step = web
                    .step()
                    .expect("web step should succeed after resize in interaction trace");
                format!("interaction-step-{idx}:resize:{next_cols}x{next_rows}:frame{}", step.frame_idx)
            }
        };

        let signature = capture_signature(
            &mut terminal,
            &web,
            current_cols,
            current_rows,
            step_label,
        );
        assert_signature_parity(&signature);
        signatures.push(signature);
    }

    signatures
}

#[test]
fn terminal_web_screen_sweep_parity_all_screens() {
    let sweep_small_a = run_screen_sweep_parity(80, 24);
    let sweep_small_b = run_screen_sweep_parity(80, 24);
    assert_eq!(
        sweep_small_a, sweep_small_b,
        "screen sweep signatures must be deterministic at 80x24"
    );

    let sweep_large_a = run_screen_sweep_parity(120, 40);
    let sweep_large_b = run_screen_sweep_parity(120, 40);
    assert_eq!(
        sweep_large_a, sweep_large_b,
        "screen sweep signatures must be deterministic at 120x40"
    );
}

#[test]
fn terminal_web_interaction_trace_parity() {
    let trace_a = run_interaction_trace_parity(120, 40);
    let trace_b = run_interaction_trace_parity(120, 40);
    assert_eq!(
        trace_a, trace_b,
        "interaction trace signatures must be deterministic at 120x40"
    );
}
