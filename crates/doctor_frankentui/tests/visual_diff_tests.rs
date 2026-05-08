use doctor_frankentui::semantic_contract::TransformationRiskLevel;
use doctor_frankentui::trace::{InteractionTrace, TraceBuilder, TracePayload, Viewport};
use doctor_frankentui::visual_diff::{
    CursorPosition, TerminalCell, TerminalFrame, TerminalOutputRun, TerminalStyle,
    VisualDiffConfig, VisualDiffMode, VisualDiffVerdict, VisualDifferenceKind,
    compare_terminal_runs, compare_trace_render_captures,
};

fn run(run_id: &str, frames: Vec<TerminalFrame>) -> TerminalOutputRun {
    TerminalOutputRun::new(run_id, frames)
        .with_replay_command(format!("doctor_frankentui replay --run-id {run_id}"))
}

fn colored_cell(grapheme: &str, fg: &str, semantic_class: &str) -> TerminalCell {
    TerminalCell::new(grapheme)
        .with_style(TerminalStyle {
            fg: Some(fg.to_string()),
            bg: None,
            attrs: Vec::new(),
        })
        .with_semantic_class(semantic_class)
}

fn viewport() -> Viewport {
    Viewport {
        width: 80,
        height: 24,
    }
}

fn trace_with_render_hash(trace_id: &str, run_id: &str, hash: &str) -> InteractionTrace {
    let mut builder = TraceBuilder::new(trace_id, run_id, viewport()).with_metadata(
        "replay_command",
        format!("doctor_frankentui replay --trace-id {trace_id}"),
    );
    builder.record(
        0,
        TracePayload::RenderCapture {
            frame_index: 7,
            content_hash: hash.to_string(),
        },
    );
    builder.build()
}

#[test]
fn strict_mode_accepts_identical_terminal_output() {
    let source = run(
        "source-run",
        vec![
            TerminalFrame::from_text(0, "status: ok").with_cursor(CursorPosition {
                x: 9,
                y: 0,
                visible: true,
            }),
        ],
    );
    let translated = run(
        "translated-run",
        vec![
            TerminalFrame::from_text(0, "status: ok").with_cursor(CursorPosition {
                x: 9,
                y: 0,
                visible: true,
            }),
        ],
    );

    let report = compare_terminal_runs(&source, &translated, &VisualDiffConfig::strict());

    assert_eq!(report.verdict, VisualDiffVerdict::Equivalent);
    assert!(report.differences.is_empty());
    assert!(report.artifact_bundle.is_none());
    assert!(report.covered_clause_ids.contains(&"VT-001".to_string()));
    assert_eq!(
        report.expected_loss.policy_id.as_deref(),
        Some("visual_diff_validator")
    );
}

#[test]
fn strict_mode_reports_byte_mismatch_even_when_cells_match() {
    let cells = vec![TerminalCell::new("O"), TerminalCell::new("K")];
    let mut source_frame = TerminalFrame::new(0, 2, 1, cells.clone());
    source_frame.raw_bytes = Some("\x1b[32mOK".to_string());
    let mut translated_frame = TerminalFrame::new(0, 2, 1, cells);
    translated_frame.raw_bytes = Some("\x1b[0;32mOK".to_string());

    let source = run("source-run", vec![source_frame]);
    let translated = run("translated-run", vec![translated_frame]);

    let report = compare_terminal_runs(&source, &translated, &VisualDiffConfig::strict());

    assert_eq!(report.verdict, VisualDiffVerdict::Violation);
    assert_eq!(report.risk_level, TransformationRiskLevel::Critical);
    assert_eq!(report.differences.len(), 1);
    assert_eq!(
        report.differences[0].difference_kind,
        VisualDifferenceKind::StrictByteMismatch
    );
    assert_eq!(report.differences[0].region.width, 2);
    assert!(report.violated_clause_ids.contains(&"VT-001".to_string()));

    let bundle = report
        .artifact_bundle
        .expect("failure emits artifact bundle");
    assert!(bundle.replay_command.contains("source-run"));
    assert!(bundle.replay_command.contains("translated-run"));
    assert!(bundle.files.iter().any(|file| file.path == "replay.sh"));
    assert!(bundle.files.iter().any(|file| file.path == "diffs.jsonl"));
}

#[test]
fn tolerance_mode_accepts_small_decorative_color_delta() {
    let source_frame = TerminalFrame::new(
        0,
        1,
        1,
        vec![colored_cell("x", "#ffffff", "decorative_color")],
    );
    let translated_frame = TerminalFrame::new(
        0,
        1,
        1,
        vec![colored_cell("x", "#fefefe", "decorative_color")],
    );

    let report = compare_terminal_runs(
        &run("source-run", vec![source_frame]),
        &run("translated-run", vec![translated_frame]),
        &VisualDiffConfig::tolerance(),
    );

    assert_eq!(report.mode, VisualDiffMode::Tolerance);
    assert_eq!(report.verdict, VisualDiffVerdict::WithinTolerance);
    assert!(report.differences.is_empty());
    assert!(report.covered_clause_ids.contains(&"VT-002".to_string()));
}

#[test]
fn style_mismatch_pinpoints_cell_region_and_style_delta() {
    let source_frame = TerminalFrame::new(
        0,
        2,
        1,
        vec![
            TerminalCell::new("A"),
            colored_cell("B", "#ff0000", "command_output"),
        ],
    );
    let translated_frame = TerminalFrame::new(
        0,
        2,
        1,
        vec![
            TerminalCell::new("A"),
            colored_cell("B", "#00ff00", "command_output"),
        ],
    );

    let report = compare_terminal_runs(
        &run("source-run", vec![source_frame]),
        &run("translated-run", vec![translated_frame]),
        &VisualDiffConfig::strict(),
    );

    assert_eq!(report.verdict, VisualDiffVerdict::Violation);
    assert_eq!(report.differences.len(), 1);
    let diff = &report.differences[0];
    assert_eq!(diff.difference_kind, VisualDifferenceKind::StyleMismatch);
    assert_eq!(diff.region.x, 1);
    assert_eq!(diff.region.y, 0);
    assert_eq!(diff.style_deltas.len(), 1);
    assert_eq!(diff.style_deltas[0].property, "fg");
    assert_eq!(diff.style_deltas[0].x, 1);
    assert_eq!(diff.style_deltas[0].y, 0);
}

#[test]
fn tolerance_mode_reports_excessive_perceptual_delta_against_vt002() {
    let source_frame = TerminalFrame::new(
        0,
        1,
        1,
        vec![colored_cell("x", "#ffffff", "decorative_color")],
    );
    let translated_frame = TerminalFrame::new(
        0,
        1,
        1,
        vec![colored_cell("x", "#000000", "decorative_color")],
    );

    let report = compare_terminal_runs(
        &run("source-run", vec![source_frame]),
        &run("translated-run", vec![translated_frame]),
        &VisualDiffConfig::tolerance(),
    );

    assert_eq!(report.verdict, VisualDiffVerdict::Violation);
    assert_eq!(report.risk_level, TransformationRiskLevel::Medium);
    assert_eq!(
        report.differences[0].difference_kind,
        VisualDifferenceKind::PerceptualDeltaExceeded
    );
    assert_eq!(report.differences[0].clause_ids, vec!["VT-002".to_string()]);
    assert!(report.differences[0].perceptual_delta.unwrap_or_default() > 0.9);
}

#[test]
fn render_capture_hash_diff_uses_trace_replay_artifact() {
    let source = trace_with_render_hash("source-trace", "source-run", "hash-a");
    let translated = trace_with_render_hash("translated-trace", "translated-run", "hash-b");

    let report = compare_trace_render_captures(&source, &translated, &VisualDiffConfig::strict());

    assert_eq!(report.verdict, VisualDiffVerdict::Violation);
    assert_eq!(
        report.differences[0].difference_kind,
        VisualDifferenceKind::FrameHashMismatch
    );
    assert_eq!(report.differences[0].frame_index, 7);
    assert_eq!(report.differences[0].region.width, 80);
    assert_eq!(report.differences[0].region.height, 24);

    let bundle = report
        .artifact_bundle
        .expect("hash mismatch emits replay bundle");
    assert!(bundle.replay_command.contains("source-trace"));
    assert!(bundle.replay_command.contains("translated-trace"));
    assert!(bundle.bundle_id.starts_with("visual-diff-"));
}
