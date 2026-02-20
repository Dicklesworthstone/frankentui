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
        let entries = value
            .get("interaction_timeline")
            .unwrap_or_else(|| panic!("snapshot missing interaction_timeline: {value}"))
            .get("entries")
            .and_then(serde_json::Value::as_array)
            .unwrap_or_else(|| panic!("snapshot timeline missing entries array: {value}"));
        entries
            .iter()
            .enumerate()
            .map(|(idx, entry)| {
                entry
                    .get("operation_id")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or_else(|| {
                        panic!("timeline entry {idx} missing u64 operation_id: {entry}")
                    })
            })
            .collect()
    }

    fn timeline_baseline_node_ids_from_snapshot_json(snapshot_json: &str) -> Vec<u64> {
        let value: serde_json::Value =
            serde_json::from_str(snapshot_json).expect("snapshot json should parse as value");
        let nodes = value
            .get("interaction_timeline")
            .unwrap_or_else(|| panic!("snapshot missing interaction_timeline: {value}"))
            .get("baseline")
            .unwrap_or_else(|| panic!("snapshot timeline missing baseline: {value}"))
            .get("nodes")
            .and_then(serde_json::Value::as_array)
            .unwrap_or_else(|| panic!("timeline baseline missing nodes array: {value}"));
        nodes
            .iter()
            .enumerate()
            .map(|(idx, node)| {
                node.get("id")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or_else(|| panic!("baseline node {idx} missing u64 id: {node}"))
            })
            .collect()
    }

    fn find_splitter_hit_for_size(
        core: &mut RunnerCore,
        cols: u16,
        rows: u16,
        pointer_id: u32,
    ) -> Option<(i32, i32)> {
        let modifiers = PaneModifierSnapshot::default();
        for y in 0..i32::from(rows.max(1)) {
            for x in 0..i32::from(cols.max(1)) {
                let down = core.pane_pointer_down_at(
                    pointer_id,
                    PanePointerButton::Primary,
                    x,
                    y,
                    modifiers,
                );
                if down.accepted() {
                    let cancel = core.pane_pointer_cancel(Some(pointer_id));
                    assert!(
                        cancel.accepted(),
                        "probe cancel must clear active pointer after accepted probe down"
                    );
                    assert_eq!(
                        core.pane_active_pointer_id(),
                        None,
                        "probe cancel must clear active pointer"
                    );
                    return Some((x, y));
                }
            }
        }
        None
    }

    #[test]
    fn runner_core_creates_and_inits() {
        let mut core = RunnerCore::new(80, 24);
        core.init();
        assert!(core.is_running());
        assert_eq!(core.frame_idx(), 1); // First frame rendered during init.
    }

    #[test]
    fn runner_core_new_clamps_zero_dimensions() {
        let mut core = RunnerCore::new(0, 0);
        core.init();
        assert!(core.is_running());
        assert_eq!(core.frame_idx(), 1);
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

    fn push_key(core: &mut RunnerCore, code: &str, mods: u8) -> bool {
        let payload = format!(
            r#"{{"kind":"key","phase":"down","code":"{code}","mods":{mods},"repeat":false}}"#
        );
        core.push_encoded_input(&payload)
    }

    fn push_key_with_key(core: &mut RunnerCore, key: &str, code: &str, mods: u8) -> bool {
        let payload = format!(
            r#"{{"kind":"key","phase":"down","key":"{key}","code":"{code}","mods":{mods},"repeat":false}}"#
        );
        core.push_encoded_input(&payload)
    }

    fn push_mouse_move(core: &mut RunnerCore, button: i32, x: i32, y: i32) -> bool {
        let payload = format!(
            r#"{{"kind":"mouse","phase":"move","button":{button},"x":{x},"y":{y},"mods":0}}"#
        );
        core.push_encoded_input(&payload)
    }

    #[test]
    fn runner_core_files_screen_render_loop_does_not_panic() {
        let mut core = RunnerCore::new(120, 40);
        core.init();

        // Global shortcut: Digit9 -> FileBrowser.
        assert!(push_key(&mut core, "Digit9", 0));
        let step = core.step();
        assert!(step.rendered);
        core.prepare_flat_patches();
        assert!(!core.flat_spans().is_empty());

        // Exercise a few deterministic frame-loop iterations on Files.
        for _ in 0..8 {
            core.advance_time_ms(16.0);
            let _ = core.step();
            core.prepare_flat_patches();
            let _ = core.patch_hash();
            let _ = core.patch_stats();
        }
    }

    #[test]
    fn runner_core_shift_l_screen_cycle_and_patch_flatten_is_stable() {
        let mut core = RunnerCore::new(120, 40);
        core.init();

        // Shift+L is app-level "next screen"; cycle broadly to cover many screens.
        for _ in 0..64 {
            assert!(
                push_key_with_key(&mut core, "L", "KeyL", 1),
                "Shift+L event should be accepted",
            );
            let _ = core.step();
            core.prepare_flat_patches();
            core.advance_time_ms(16.0);
            let _ = core.step();
            core.prepare_flat_patches();
        }
    }

    #[test]
    fn runner_core_files_shortcut_with_resize_and_input_churn_is_stable() {
        let mut core = RunnerCore::new(80, 24);
        core.init();

        // Global shortcut: Digit9 -> FileBrowser.
        assert!(push_key(&mut core, "Digit9", 0));
        let first = core.step();
        assert!(first.running);

        for i in 0..96u16 {
            assert!(push_mouse_move(
                &mut core,
                if i % 3 == 0 { -1 } else { 0 },
                i as i32 % 5 - 1,
                i as i32 % 4 - 1,
            ));

            if i % 8 == 0 {
                assert!(push_key(&mut core, "PageDown", 0));
            }
            if i % 11 == 0 {
                assert!(push_key(&mut core, "PageUp", 0));
            }

            if i % 7 == 0 {
                core.resize(0, 0);
            } else {
                core.resize(24 + (i % 32), 6 + (i % 12));
            }
            core.advance_time_ms(16.0);

            let result = core.step();
            assert!(result.running);
            core.prepare_flat_patches();
            let _ = core.patch_hash();
            let _ = core.patch_stats();
            let _ = core.take_logs();
        }
    }

    #[test]
    fn runner_core_tiny_geometry_screen_cycle_with_resize_churn_is_stable() {
        let mut core = RunnerCore::new(1, 1);
        core.init();

        for i in 0..180u16 {
            // Shift+L = next screen. This traverses Files, Sizing, and every other screen.
            if i % 2 == 0 {
                assert!(push_key_with_key(&mut core, "L", "KeyL", 1));
            }

            assert!(push_mouse_move(
                &mut core,
                if i % 4 == 0 { -1 } else { 0 },
                i as i32 % 3,
                i as i32 % 2,
            ));

            match i % 5 {
                0 => core.resize(0, 0),
                1 => core.resize(1, 1),
                2 => core.resize(2, 1),
                3 => core.resize(2, 2),
                _ => core.resize(3, 2),
            }

            core.advance_time_ms(16.0);
            let result = core.step();
            assert!(result.running);
            core.prepare_flat_patches();
            let _ = core.patch_hash();
            let _ = core.patch_stats();
        }
    }

    #[test]
    fn runner_core_files_and_sizing_cycle_with_pane_state_queries_is_stable() {
        let mut core = RunnerCore::new(120, 40);
        core.init();

        // Jump to Files first (Digit9 shortcut), then cycle with Shift+L.
        assert!(push_key(&mut core, "Digit9", 0));
        let first = core.step();
        assert!(first.running);
        core.prepare_flat_patches();

        for i in 0..240u16 {
            if i % 10 == 0 {
                assert!(push_key_with_key(&mut core, "L", "KeyL", 1));
            }

            assert!(push_mouse_move(
                &mut core,
                if i % 4 == 0 { -1 } else { 0 },
                i as i32 % 140 - 10,
                i as i32 % 60 - 10,
            ));

            match i % 6 {
                0 => core.resize(0, 0),
                1 => core.resize(80, 24),
                2 => core.resize(120, 40),
                3 => core.resize(150, 45),
                4 => core.resize(90, 28),
                _ => core.resize(64, 20),
            }

            core.advance_time_ms(16.0);
            let result = core.step();
            assert!(result.running);

            core.prepare_flat_patches();
            let _ = core.patch_hash();
            let _ = core.patch_stats();

            // Mirrors host pane overlay polling (`paneLayoutState` path).
            let _ = core.pane_preview_state();
            let _ = core.pane_timeline_status();
            let _ = core.pane_layout_hash();
            let _ = core.pane_selected_ids();
            let _ = core.pane_primary_id();
        }
    }

    #[test]
    fn runner_core_mixed_screen_and_interrupt_churn_is_stable() {
        let mut core = RunnerCore::new(120, 40);
        core.init();

        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next_u32 = || {
            // Deterministic xorshift64* PRNG for reproducible stress traces.
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            state = state.wrapping_mul(0x2545_f491_4f6c_dd1d);
            (state >> 16) as u32
        };

        let screen_codes = [
            "Digit1", "Digit2", "Digit3", "Digit4", "Digit5", "Digit6", "Digit7", "Digit8",
            "Digit9", "Digit0",
        ];

        let mut pointer_id_seed = 800u32;
        for i in 0..420u16 {
            match next_u32() % 15 {
                0 => {
                    let _ = push_key_with_key(&mut core, "L", "KeyL", 1);
                }
                1 => {
                    let code = screen_codes[(next_u32() as usize) % screen_codes.len()];
                    let _ = push_key(&mut core, code, 0);
                }
                2 => {
                    let _ = push_key(
                        &mut core,
                        if next_u32() & 1 == 0 {
                            "PageDown"
                        } else {
                            "PageUp"
                        },
                        0,
                    );
                }
                3 => {
                    let x = (next_u32() % 220) as i32 - 40;
                    let y = (next_u32() % 120) as i32 - 30;
                    let _ = push_mouse_move(&mut core, -1, x, y);
                }
                4 => match next_u32() % 7 {
                    0 => core.resize(0, 0),
                    1 => core.resize(1, 1),
                    2 => core.resize(80, 24),
                    3 => core.resize(120, 40),
                    4 => core.resize(150, 45),
                    5 => core.resize(90, 28),
                    _ => core.resize(64, 20),
                },
                5 => {
                    pointer_id_seed = pointer_id_seed.saturating_add(1);
                    let x = (next_u32() % 160) as i32;
                    let y = (next_u32() % 70) as i32;
                    let down = core.pane_pointer_down_at(
                        pointer_id_seed,
                        PanePointerButton::Primary,
                        x,
                        y,
                        PaneModifierSnapshot::default(),
                    );
                    if matches!(
                        down.capture_command,
                        Some(PanePointerCaptureCommand::Acquire { .. })
                    ) && next_u32() & 1 == 0
                    {
                        let _ = core.pane_capture_acquired(pointer_id_seed);
                    }
                }
                6 => {
                    let pid = core.pane_active_pointer_id().unwrap_or(pointer_id_seed);
                    let _ = core.pane_pointer_move_at(
                        pid,
                        (next_u32() % 160) as i32,
                        (next_u32() % 70) as i32,
                        PaneModifierSnapshot::default(),
                    );
                }
                7 => {
                    let pid = core.pane_active_pointer_id().unwrap_or(pointer_id_seed);
                    let _ = core.pane_pointer_up_at(
                        pid,
                        PanePointerButton::Primary,
                        (next_u32() % 160) as i32,
                        (next_u32() % 70) as i32,
                        PaneModifierSnapshot::default(),
                    );
                }
                8 => {
                    let _ = core.pane_pointer_cancel(if next_u32() & 1 == 0 {
                        None
                    } else {
                        Some(pointer_id_seed)
                    });
                }
                9 => {
                    let _ = core.pane_pointer_leave(pointer_id_seed);
                }
                10 => {
                    let _ = core.pane_blur();
                }
                11 => {
                    let _ = core.pane_visibility_hidden();
                }
                12 => {
                    let _ = core.pane_lost_pointer_capture(pointer_id_seed);
                }
                13 => {
                    let _ = core.pane_undo();
                    let _ = core.pane_redo();
                }
                _ => {
                    let _ = apply_any_intelligence_mode(&mut core);
                }
            }

            if i % 23 == 0 {
                let _ = core.push_encoded_input(
                    r#"{"kind":"mouse","phase":"move","x":-11,"y":-7,"mods":0}"#,
                );
            }

            core.advance_time_ms(8.0 + f64::from((next_u32() % 24) as u16));
            let result = core.step();
            assert!(
                result.running,
                "runner exited unexpectedly at iteration {i}"
            );

            core.prepare_flat_patches();
            let _ = core.patch_hash();
            let _ = core.patch_stats();
            let _ = core.take_logs();
            let _ = core.pane_preview_state();
            let _ = core.pane_timeline_status();
            let _ = core.pane_layout_hash();
            let _ = core.pane_selected_ids();
            let _ = core.pane_primary_id();
        }

        // End-state cleanup must be safe even if no pointer is active.
        let _ = core.pane_blur();
        let _ = core.pane_visibility_hidden();
        assert_eq!(core.pane_active_pointer_id(), None);
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
    fn runner_core_resize_clamps_zero_dimensions() {
        let mut core = RunnerCore::new(80, 24);
        core.init();
        core.resize(0, 0);
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
    fn runner_core_advance_time_ignores_invalid_inputs() {
        let mut core = RunnerCore::new(80, 24);
        core.init();
        core.advance_time_ms(f64::NAN);
        core.advance_time_ms(f64::INFINITY);
        core.advance_time_ms(-1.0);
        let _ = core.step();
    }

    #[test]
    fn runner_core_set_time() {
        let mut core = RunnerCore::new(80, 24);
        core.init();
        core.set_time_ns(16_000_000.0);
        let _ = core.step();
    }

    #[test]
    fn runner_core_set_time_handles_invalid_inputs() {
        let mut core = RunnerCore::new(80, 24);
        core.init();
        core.set_time_ns(f64::NAN);
        core.set_time_ns(f64::NEG_INFINITY);
        core.set_time_ns(-123.0);
        core.set_time_ns(f64::INFINITY);
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
    fn runner_core_pane_pointer_down_at_remains_hittable_after_resize_churn() {
        let mut core = RunnerCore::new(120, 40);
        core.init();

        let sizes = [
            (120u16, 40u16),
            (96u16, 30u16),
            (80u16, 24u16),
            (72u16, 22u16),
            (64u16, 20u16),
            (56u16, 18u16),
            (48u16, 16u16),
        ];
        let mut pointer_id = 400u32;

        for (cols, rows) in sizes.into_iter().cycle().take(16) {
            core.resize(cols, rows);
            let stepped = core.step();
            assert!(stepped.running);

            let probe_hit = find_splitter_hit_for_size(&mut core, cols, rows, pointer_id)
                .unwrap_or_else(|| {
                    panic!(
                        "expected at least one splitter hit target after resize for {cols}x{rows}"
                    )
                });
            pointer_id = pointer_id.saturating_add(1);

            let down = core.pane_pointer_down_at(
                pointer_id,
                PanePointerButton::Primary,
                probe_hit.0,
                probe_hit.1,
                PaneModifierSnapshot::default(),
            );
            assert!(
                down.accepted(),
                "pane pointer down should remain hittable at {}x{} for probe {:?}",
                cols,
                rows,
                probe_hit
            );
            if matches!(
                down.capture_command,
                Some(PanePointerCaptureCommand::Acquire { .. })
            ) {
                let acquired = core.pane_capture_acquired(pointer_id);
                assert!(
                    acquired.accepted(),
                    "capture ack should succeed for accepted pointer down at {cols}x{rows}"
                );
            }

            let up = core.pane_pointer_up_at(
                pointer_id,
                PanePointerButton::Primary,
                probe_hit.0,
                probe_hit.1,
                PaneModifierSnapshot::default(),
            );
            assert!(
                up.accepted(),
                "pane pointer up should succeed at {}x{} for probe {:?}",
                cols,
                rows,
                probe_hit
            );
            assert_eq!(
                core.pane_active_pointer_id(),
                None,
                "active pointer must clear after pointer-up at {cols}x{rows}"
            );
            pointer_id = pointer_id.saturating_add(1);
        }
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
    fn runner_core_pane_blur_releases_active_capture() {
        let mut core = RunnerCore::new(80, 24);
        core.init();
        let modifiers = PaneModifierSnapshot::default();

        let down = core.pane_pointer_down(
            test_target(),
            52,
            PanePointerButton::Primary,
            3,
            4,
            modifiers,
        );
        assert!(down.accepted());
        let acquired = core.pane_capture_acquired(52);
        assert!(acquired.accepted());

        let blur = core.pane_blur();
        assert!(blur.accepted());
        assert!(matches!(
            blur.outcome,
            PaneDispatchOutcome::SemanticForwarded
        ));
        assert_eq!(
            blur.capture_command,
            Some(PanePointerCaptureCommand::Release { pointer_id: 52 })
        );
        assert_eq!(core.pane_active_pointer_id(), None);
    }

    #[test]
    fn runner_core_pane_visibility_hidden_releases_active_capture() {
        let mut core = RunnerCore::new(80, 24);
        core.init();
        let modifiers = PaneModifierSnapshot::default();

        let down = core.pane_pointer_down(
            test_target(),
            66,
            PanePointerButton::Primary,
            6,
            6,
            modifiers,
        );
        assert!(down.accepted());
        let acquired = core.pane_capture_acquired(66);
        assert!(acquired.accepted());

        let hidden = core.pane_visibility_hidden();
        assert!(hidden.accepted());
        assert!(matches!(
            hidden.outcome,
            PaneDispatchOutcome::SemanticForwarded
        ));
        assert_eq!(
            hidden.capture_command,
            Some(PanePointerCaptureCommand::Release { pointer_id: 66 })
        );
        assert_eq!(core.pane_active_pointer_id(), None);
    }

    #[test]
    fn runner_core_pane_leave_before_capture_ack_cancels_active_pointer() {
        let mut core = RunnerCore::new(80, 24);
        core.init();
        let modifiers = PaneModifierSnapshot::default();

        let down = core.pane_pointer_down(
            test_target(),
            79,
            PanePointerButton::Primary,
            8,
            7,
            modifiers,
        );
        assert!(down.accepted());
        assert_eq!(core.pane_active_pointer_id(), Some(79));

        let leave = core.pane_pointer_leave(79);
        assert!(leave.accepted());
        assert!(matches!(
            leave.outcome,
            PaneDispatchOutcome::SemanticForwarded
        ));
        assert_eq!(leave.capture_command, None);
        assert_eq!(core.pane_active_pointer_id(), None);
    }

    #[test]
    fn runner_core_pane_leave_after_capture_ack_is_ignored() {
        let mut core = RunnerCore::new(80, 24);
        core.init();
        let modifiers = PaneModifierSnapshot::default();

        let down = core.pane_pointer_down(
            test_target(),
            81,
            PanePointerButton::Primary,
            8,
            8,
            modifiers,
        );
        assert!(down.accepted());
        let acquired = core.pane_capture_acquired(81);
        assert!(acquired.accepted());

        let leave = core.pane_pointer_leave(81);
        assert!(!leave.accepted());
        assert!(matches!(
            leave.outcome,
            PaneDispatchOutcome::Ignored(PanePointerIgnoredReason::LeaveWhileCaptured)
        ));
        assert_eq!(core.pane_active_pointer_id(), Some(81));
    }

    #[test]
    fn runner_core_pane_lost_pointer_capture_cancels_without_release_command() {
        let mut core = RunnerCore::new(80, 24);
        core.init();
        let modifiers = PaneModifierSnapshot::default();

        let down = core.pane_pointer_down(
            test_target(),
            95,
            PanePointerButton::Primary,
            10,
            9,
            modifiers,
        );
        assert!(down.accepted());
        let acquired = core.pane_capture_acquired(95);
        assert!(acquired.accepted());

        let lost = core.pane_lost_pointer_capture(95);
        assert!(lost.accepted());
        assert!(matches!(
            lost.outcome,
            PaneDispatchOutcome::SemanticForwarded
        ));
        assert_eq!(lost.capture_command, None);
        assert_eq!(core.pane_active_pointer_id(), None);
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

    #[test]
    fn import_snapshot_canonicalizes_timeline_baseline_nodes() {
        let mut source = RunnerCore::new(80, 24);
        source.init();
        assert!(
            apply_any_intelligence_mode(&mut source).is_some(),
            "expected at least one adaptive mode to produce structural operations"
        );
        let snapshot_json = source
            .export_workspace_snapshot_json()
            .expect("snapshot export should succeed");

        let mut mutated: serde_json::Value =
            serde_json::from_str(&snapshot_json).expect("snapshot json should parse as value");
        mutated["interaction_timeline"]["baseline"]["nodes"]
            .as_array_mut()
            .expect("timeline baseline nodes should be present")
            .reverse();
        let mutated_json =
            serde_json::to_string(&mutated).expect("mutated snapshot json should encode");

        let mut restored = RunnerCore::new(80, 24);
        restored.init();
        restored
            .import_workspace_snapshot_json(&mutated_json)
            .expect("snapshot import should succeed");
        let exported = restored
            .export_workspace_snapshot_json()
            .expect("snapshot export after import should succeed");
        let baseline_ids = timeline_baseline_node_ids_from_snapshot_json(&exported);

        assert!(
            baseline_ids.windows(2).all(|ids| ids[0] <= ids[1]),
            "timeline baseline node ids should be canonicalized, got: {baseline_ids:?}"
        );
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct PaneTraceSignature {
        layout_hash: u64,
        selected_ids: Vec<u64>,
        operation_ids: Vec<u64>,
        baseline_ids: Vec<u64>,
    }

    fn run_pane_trace_signature() -> PaneTraceSignature {
        let mut core = RunnerCore::new(120, 40);
        core.init();
        let modifiers = PaneModifierSnapshot::default();

        let down = core.pane_pointer_down(
            test_target(),
            77,
            PanePointerButton::Primary,
            6,
            6,
            modifiers,
        );
        assert!(down.accepted(), "pointer down should be accepted");
        let acquired = core.pane_capture_acquired(77);
        assert!(acquired.accepted(), "capture should be acknowledged");
        let mv = core.pane_pointer_move(77, 14, 6, modifiers);
        assert!(mv.accepted(), "pointer move should be accepted");
        let up = core.pane_pointer_up(77, PanePointerButton::Primary, 14, 6, modifiers);
        assert!(up.accepted(), "pointer up should be accepted");
        assert_eq!(
            core.pane_active_pointer_id(),
            None,
            "pointer must be released after up"
        );

        assert!(
            apply_any_intelligence_mode(&mut core).is_some(),
            "adaptive intelligence mode should produce structural operations"
        );
        assert!(core.pane_undo(), "pane undo should apply");
        assert!(core.pane_redo(), "pane redo should apply");
        assert!(core.pane_replay(), "pane replay should apply");

        let snapshot_json = core
            .export_workspace_snapshot_json()
            .expect("pane snapshot export should succeed");
        let operation_ids = operation_ids_from_snapshot_json(&snapshot_json);
        let baseline_ids = timeline_baseline_node_ids_from_snapshot_json(&snapshot_json);

        let mut restored = RunnerCore::new(120, 40);
        restored.init();
        restored
            .import_workspace_snapshot_json(&snapshot_json)
            .expect("pane snapshot import should succeed");

        PaneTraceSignature {
            layout_hash: restored.pane_layout_hash(),
            selected_ids: restored.pane_selected_ids(),
            operation_ids,
            baseline_ids,
        }
    }

    #[test]
    fn pane_interaction_trace_is_deterministic() {
        let sig_a = run_pane_trace_signature();
        let sig_b = run_pane_trace_signature();
        assert_eq!(sig_a, sig_b, "pane interaction signature should be stable");
        assert!(
            !sig_a.operation_ids.is_empty(),
            "pane trace should include timeline operations"
        );
    }
}
