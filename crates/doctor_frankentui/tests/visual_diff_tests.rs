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
    assert_eq!(
        bundle.replay_command,
        "doctor_frankentui visual-diff --input replay-input.json"
    );
    let archive: serde_json::Value = serde_json::from_str(
        &bundle
            .files
            .iter()
            .find(|f| f.path == "replay-input.json")
            .expect("archived inputs")
            .content,
    )
    .expect("archive JSON");
    assert_eq!(archive["source_run"]["run_id"], "source-run");
    assert_eq!(archive["translated_run"]["run_id"], "translated-run");
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
    let archive: serde_json::Value = serde_json::from_str(
        &bundle
            .files
            .iter()
            .find(|f| f.path == "replay-input.json")
            .expect("archived inputs")
            .content,
    )
    .expect("archive JSON");
    assert_eq!(archive["source_run"]["trace_id"], "source-trace");
    assert_eq!(archive["translated_run"]["trace_id"], "translated-trace");
    assert!(bundle.bundle_id.starts_with("visual-diff-"));
}

#[cfg(unix)]
#[test]
fn archived_visual_bundle_relocates_and_executes_real_comparator() {
    use doctor_frankentui::semantic_contract::load_builtin_semantic_contract;
    use doctor_frankentui::visual_diff::compare_terminal_runs_with_contract;
    use std::{fs, process::Command};

    // Controlled archived frames test comparison replay, not a new terminal capture.
    let source = run(
        "source-run",
        vec![TerminalFrame::from_text(0, "source frame")],
    );
    let translated = run(
        "translated-run",
        vec![TerminalFrame::from_text(0, "changed frame")],
    );
    let mut config = VisualDiffConfig::strict();
    config.max_perceptual_delta = 0.0125;
    let mut contract = load_builtin_semantic_contract().expect("contract");
    contract.contract_id = "archived-custom-contract".to_string();
    let report = compare_terminal_runs_with_contract(&source, &translated, &config, &contract);
    assert_eq!(report.verdict, VisualDiffVerdict::Violation);
    let bundle = report
        .artifact_bundle
        .as_ref()
        .expect("real producer bundle");
    let temp = tempfile::tempdir().expect("tempdir");
    let original = temp.path().join("original");
    std::fs::create_dir(&original).expect("artifact directory");
    for file in &bundle.files {
        fs::write(original.join(&file.path), &file.content).expect("materialize producer artifact");
    }
    let relocated = temp.path().join("relocated archive with spaces");
    fs::rename(&original, &relocated).expect("relocate actual artifacts");
    let binary = env!("CARGO_BIN_EXE_doctor_frankentui");
    let output = Command::new("bash")
        .arg(relocated.join("replay.sh"))
        .env("DOCTOR_FRANKENTUI_BIN", binary)
        .current_dir(temp.path())
        .output()
        .expect("execute generated replay");
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("real CLI report");
    assert_eq!(payload["replay_scope"], "archived_comparison");
    assert_eq!(
        output.stdout,
        format!(
            "{}\n",
            serde_json::json!({"replay_scope": "archived_comparison", "report": report})
        )
        .into_bytes()
    );

    let archive_path = relocated.join("replay-input.json");
    let mut archive: serde_json::Value =
        serde_json::from_slice(&fs::read(&archive_path).expect("archive")).expect("archive JSON");
    // Deserialize into the actual f32-bearing types before exact equality;
    // widening native f32 to Value's f64 changes its decimal representation.
    assert_eq!(
        serde_json::from_value::<VisualDiffConfig>(archive["config"].clone())
            .expect("archived config"),
        config
    );
    assert_eq!(
        serde_json::from_value::<doctor_frankentui::semantic_contract::SemanticEquivalenceContract>(
            archive["contract"].clone()
        )
        .expect("archived custom contract"),
        contract
    );
    assert_eq!(
        archive["source_run"]["replay_command"],
        source.replay_command.as_deref().expect("provenance")
    );
    archive["translated_run"] = archive["source_run"].clone();
    let equivalent = compare_terminal_runs_with_contract(&source, &source, &config, &contract);
    assert_eq!(equivalent.verdict, VisualDiffVerdict::Equivalent);
    fs::write(
        &archive_path,
        serde_json::to_vec(&archive).expect("archive JSON"),
    )
    .expect("positive archive");
    let output = Command::new("bash")
        .arg(relocated.join("replay.sh"))
        .env("DOCTOR_FRANKENTUI_BIN", binary)
        .current_dir(temp.path())
        .output()
        .expect("positive generated comparison");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        format!(
            "{}\n",
            serde_json::json!({"replay_scope": "archived_comparison", "report": equivalent})
        )
        .into_bytes()
    );

    let mut wrong_schema = archive.clone();
    wrong_schema["schema_version"] = serde_json::json!(999);
    let mut empty = archive;
    empty["source_run"]["frames"] = serde_json::json!([]);
    empty["translated_run"]["frames"] = serde_json::json!([]);
    for invalid in [serde_json::Value::Null, wrong_schema, empty] {
        fs::write(
            &archive_path,
            serde_json::to_vec(&invalid).expect("invalid JSON input"),
        )
        .expect("negative archive");
        let output = Command::new(binary)
            .args(["visual-diff", "--input"])
            .arg(&archive_path)
            .output()
            .expect("negative CLI");
        assert!(
            !output.status.success(),
            "invalid archive accepted: {invalid}"
        );
        assert!(
            output.stdout.is_empty(),
            "invalid archive must not produce a report"
        );
    }
}
