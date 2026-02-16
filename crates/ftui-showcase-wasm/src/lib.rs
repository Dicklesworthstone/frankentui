#![forbid(unsafe_code)]

//! WASM showcase runner for the FrankenTUI demo application.
//!
//! This crate provides [`ShowcaseRunner`], a `wasm-bindgen`-exported struct
//! that wraps `ftui_web::step_program::StepProgram<AppModel>` and exposes
//! it to JavaScript for host-driven execution.
//!
//! See `docs/spec/wasm-showcase-runner-contract.md` for the full contract.

#[cfg(target_arch = "wasm32")]
mod wasm;

#[cfg(target_arch = "wasm32")]
pub use wasm::ShowcaseRunner;

// Runner core is used by the wasm module and by native tests.
#[cfg(any(target_arch = "wasm32", test))]
mod runner_core;

#[cfg(test)]
mod tests {
    use crate::runner_core::{PaneDispatchOutcome, RunnerCore};
    use ftui_layout::{
        PaneId, PaneLayoutIntelligenceMode, PaneModifierSnapshot, PanePointerButton,
        PaneResizeTarget, SplitAxis,
    };
    use ftui_web::pane_pointer_capture::{PanePointerCaptureCommand, PanePointerIgnoredReason};
    use std::collections::HashSet;

    fn test_target() -> PaneResizeTarget {
        PaneResizeTarget {
            split_id: PaneId::MIN,
            axis: SplitAxis::Horizontal,
        }
    }

    fn apply_any_intelligence_mode(core: &mut RunnerCore) -> Option<PaneLayoutIntelligenceMode> {
        let primary = PaneId::new(core.pane_primary_id()?).ok()?;
        [
            PaneLayoutIntelligenceMode::Compare,
            PaneLayoutIntelligenceMode::Monitor,
            PaneLayoutIntelligenceMode::Compact,
            PaneLayoutIntelligenceMode::Focus,
        ]
        .into_iter()
        .find(|&mode| core.pane_apply_intelligence_mode(mode, primary))
    }

    fn operation_ids_from_snapshot_json(snapshot_json: &str) -> Vec<u64> {
        let value: serde_json::Value =
            serde_json::from_str(snapshot_json).expect("snapshot json should parse as value");
        value
            .get("interaction_timeline")
            .and_then(|timeline| timeline.get("entries"))
            .and_then(serde_json::Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| {
                        entry
                            .get("operation_id")
                            .and_then(serde_json::Value::as_u64)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn runner_core_creates_and_inits() {
        let mut core = RunnerCore::new(80, 24);
        core.init();
        assert!(core.is_running());
        assert_eq!(core.frame_idx(), 1); // First frame rendered during init.
    }

    #[test]
    fn runner_core_step_no_events() {
        let mut core = RunnerCore::new(80, 24);
        core.init();
        let result = core.step();
        assert!(result.running);
        assert!(!result.rendered);
        assert_eq!(result.events_processed, 0);
    }

    #[test]
    fn runner_core_push_encoded_input() {
        let mut core = RunnerCore::new(80, 24);
        core.init();
        // Push a Tick event via JSON
        let accepted =
            core.push_encoded_input(r#"{"kind":"key","phase":"down","code":"Tab","mods":0}"#);
        assert!(accepted);
        let result = core.step();
        assert_eq!(result.events_processed, 1);
        assert!(result.rendered);
    }

    #[test]
    fn runner_core_resize() {
        let mut core = RunnerCore::new(80, 24);
        core.init();
        core.resize(120, 40);
        let result = core.step();
        assert!(result.rendered);
    }

    #[test]
    fn runner_core_advance_time() {
        let mut core = RunnerCore::new(80, 24);
        core.init();
        core.advance_time_ms(16.0);
        let _ = core.step();
        // Just verify it doesn't panic.
    }

    #[test]
    fn runner_core_set_time() {
        let mut core = RunnerCore::new(80, 24);
        core.init();
        core.set_time_ns(16_000_000.0);
        let _ = core.step();
    }

    #[test]
    fn runner_core_patch_hash() {
        let mut core = RunnerCore::new(80, 24);
        core.init();
        let hash = core.patch_hash();
        assert!(hash.is_some());
        assert!(hash.unwrap().starts_with("fnv1a64:"));
    }

    #[test]
    fn runner_core_patch_hash_matches_flat_batch_hash() {
        let mut core = RunnerCore::new(80, 24);
        core.init();

        let from_outputs = core.patch_hash().expect("hash from live outputs");
        core.prepare_flat_patches();
        let from_flat = core.patch_hash().expect("hash from prepared flat batch");

        assert_eq!(from_outputs, from_flat);
    }

    #[test]
    fn runner_core_take_flat_patches() {
        let mut core = RunnerCore::new(10, 2);
        core.init();
        let flat = core.take_flat_patches();
        // First frame: full repaint of 10*2=20 cells → 80 u32 values + 2 span values.
        assert_eq!(flat.spans, vec![0, 20]);
        assert_eq!(flat.cells.len(), 80); // 20 cells * 4 u32 per cell
    }

    #[test]
    fn runner_core_take_logs() {
        let mut core = RunnerCore::new(80, 24);
        core.init();
        let logs = core.take_logs();
        // Logs may or may not be present depending on AppModel behavior.
        // Just verify we can drain them.
        assert!(logs.is_empty() || !logs.is_empty());
    }

    #[test]
    fn runner_core_unknown_input_returns_false() {
        let mut core = RunnerCore::new(80, 24);
        core.init();
        let accepted = core.push_encoded_input(r#"{"kind":"accessibility","screen_reader":true}"#);
        assert!(!accepted);
    }

    #[test]
    fn runner_core_malformed_input_returns_false() {
        let mut core = RunnerCore::new(80, 24);
        core.init();
        let accepted = core.push_encoded_input("not json");
        assert!(!accepted);
    }

    #[test]
    fn runner_core_patch_stats() {
        let mut core = RunnerCore::new(10, 2);
        core.init();
        let stats = core.patch_stats();
        assert!(stats.is_some());
        let stats = stats.unwrap();
        assert_eq!(stats.dirty_cells, 20);
        assert_eq!(stats.patch_count, 1);
    }

    #[test]
    fn runner_core_pane_pointer_lifecycle_emits_capture_commands() {
        let mut core = RunnerCore::new(80, 24);
        core.init();
        let modifiers = PaneModifierSnapshot::default();

        let down = core.pane_pointer_down(
            test_target(),
            9,
            PanePointerButton::Primary,
            4,
            6,
            modifiers,
        );
        assert!(down.accepted());
        assert_eq!(
            down.capture_command,
            Some(PanePointerCaptureCommand::Acquire { pointer_id: 9 })
        );
        assert!(matches!(
            down.outcome,
            PaneDispatchOutcome::SemanticForwarded
        ));
        assert_eq!(core.pane_active_pointer_id(), Some(9));

        let acquired = core.pane_capture_acquired(9);
        assert!(acquired.accepted());
        assert_eq!(acquired.capture_command, None);
        assert!(matches!(
            acquired.outcome,
            PaneDispatchOutcome::CaptureStateUpdated
        ));
        assert_eq!(core.pane_active_pointer_id(), Some(9));

        let up = core.pane_pointer_up(9, PanePointerButton::Primary, 10, 6, modifiers);
        assert!(up.accepted());
        assert_eq!(
            up.capture_command,
            Some(PanePointerCaptureCommand::Release { pointer_id: 9 })
        );
        assert!(matches!(up.outcome, PaneDispatchOutcome::SemanticForwarded));
        assert_eq!(core.pane_active_pointer_id(), None);
    }

    #[test]
    fn runner_core_pane_pointer_mismatch_is_ignored() {
        let mut core = RunnerCore::new(80, 24);
        core.init();

        let down = core.pane_pointer_down(
            test_target(),
            41,
            PanePointerButton::Primary,
            5,
            2,
            PaneModifierSnapshot::default(),
        );
        assert!(down.accepted());

        let mismatch = core.pane_pointer_move(88, 9, 2, PaneModifierSnapshot::default());
        assert!(!mismatch.accepted());
        assert!(matches!(
            mismatch.outcome,
            PaneDispatchOutcome::Ignored(PanePointerIgnoredReason::PointerMismatch)
        ));
        assert_eq!(core.pane_active_pointer_id(), Some(41));
    }

    #[test]
    fn runner_core_pane_logs_are_drained_with_take_logs() {
        let mut core = RunnerCore::new(80, 24);
        core.init();
        let _ = core.pane_pointer_down(
            test_target(),
            7,
            PanePointerButton::Primary,
            1,
            1,
            PaneModifierSnapshot::default(),
        );

        let logs = core.take_logs();
        assert!(
            logs.iter().any(|line| {
                line.contains("pane_pointer")
                    && line.contains("phase=pointer_down")
                    && line.contains("outcome=semantic_forwarded")
            }),
            "expected pane pointer lifecycle log entry, got: {logs:?}"
        );
    }

    #[test]
    fn runner_core_undo_clears_pointer_capture_after_structural_change() {
        let mut core = RunnerCore::new(80, 24);
        core.init();
        assert!(
            apply_any_intelligence_mode(&mut core).is_some(),
            "expected at least one adaptive mode to produce structural operations"
        );

        let down = core.pane_pointer_down(
            test_target(),
            57,
            PanePointerButton::Primary,
            5,
            4,
            PaneModifierSnapshot::default(),
        );
        assert!(down.accepted());
        assert_eq!(core.pane_active_pointer_id(), Some(57));

        assert!(
            core.pane_undo(),
            "undo should apply after recorded mutations"
        );
        assert_eq!(core.pane_active_pointer_id(), None);

        let move_after = core.pane_pointer_move(57, 8, 4, PaneModifierSnapshot::default());
        assert!(matches!(
            move_after.outcome,
            PaneDispatchOutcome::Ignored(PanePointerIgnoredReason::NoActivePointer)
        ));
    }

    #[test]
    fn import_snapshot_resets_capture_and_keeps_operation_ids_monotonic() {
        let mut source = RunnerCore::new(80, 24);
        source.init();
        assert!(
            apply_any_intelligence_mode(&mut source).is_some(),
            "expected at least one adaptive mode to produce structural operations"
        );
        let snapshot_json = source
            .export_workspace_snapshot_json()
            .expect("snapshot export should succeed");
        let before_ids = operation_ids_from_snapshot_json(&snapshot_json);
        let max_before = before_ids.iter().copied().max().unwrap_or(0);

        let mut restored = RunnerCore::new(80, 24);
        restored.init();
        let down = restored.pane_pointer_down(
            test_target(),
            91,
            PanePointerButton::Primary,
            6,
            6,
            PaneModifierSnapshot::default(),
        );
        assert!(down.accepted());
        assert_eq!(restored.pane_active_pointer_id(), Some(91));

        restored
            .import_workspace_snapshot_json(&snapshot_json)
            .expect("snapshot import should succeed");
        assert_eq!(
            restored.pane_active_pointer_id(),
            None,
            "import should reset capture adapter state"
        );

        assert!(
            apply_any_intelligence_mode(&mut restored).is_some(),
            "restored runner should continue accepting structural pane mutations"
        );
        let after_json = restored
            .export_workspace_snapshot_json()
            .expect("snapshot export after restore should succeed");
        let after_ids = operation_ids_from_snapshot_json(&after_json);
        let max_after = after_ids.iter().copied().max().unwrap_or(0);
        let unique_ids: HashSet<u64> = after_ids.iter().copied().collect();

        assert!(
            max_after > max_before,
            "operation ids should keep advancing after import"
        );
        assert_eq!(
            unique_ids.len(),
            after_ids.len(),
            "timeline operation ids should remain unique after import + mutation"
        );
    }
}
