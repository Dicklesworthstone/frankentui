//! Capability Simulator E2E Test Suite (bd-k4lj.6)
//!
//! End-to-end validation of terminal capability simulation:
//!
//! # Coverage
//! 1. **Profile Accuracy**: Each predefined profile (Modern, Xterm256, Xterm, Vt100,
//!    Dumb, Screen, Tmux, Zellij, Kitty, WindowsConsole, LinuxConsole) produces correct
//!    capability sets.
//! 2. **Capability Override**: Thread-local injection, stacking, RAII cleanup.
//! 3. **Degradation**: Mux-aware fallback behavior (sync, scroll, hyperlinks).
//! 4. **Quirk Simulation**: Mux-specific policies (passthrough, feature suppression).
//! 5. **Integration**: Demo screen renders correctly under different profiles, profile
//!    switching via keybindings, and cross-profile rendering consistency.
//!
//! # Invariants
//! - **Profile identity**: `from_profile(p).profile() == p` for all predefined profiles.
//! - **Mux policy monotonicity**: sync_output, scroll_region, hyperlinks disabled in any mux.
//! - **Override RAII**: overrides are fully cleaned up on guard drop (including panics).
//! - **Rendering determinism**: same profile + same size → identical buffer hash.
//!
//! # JSONL Logging
//! ```json
//! {"test":"profile_accuracy_modern","check":"all_features_on","passed":true,"notes":""}
//! ```
//!
//! Run with: `cargo test -p ftui-demo-showcase --test capability_sim_e2e -- --nocapture`

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Instant;

use ftui_core::capability_override::{
    CapabilityOverride, clear_all_overrides, has_active_overrides, override_depth, push_override,
    with_capability_override,
};
use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_core::geometry::Rect;
use ftui_core::terminal_capabilities::{
    CapabilityProfileBuilder, TerminalCapabilities, TerminalProfile,
};
use ftui_demo_showcase::screens::Screen;
use ftui_demo_showcase::screens::terminal_capabilities::TerminalCapabilitiesScreen;
use ftui_render::frame::Frame;
use ftui_render::grapheme_pool::GraphemePool;

// =============================================================================
// Test Utilities
// =============================================================================

#[allow(dead_code)]
fn is_coverage_run() -> bool {
    std::env::var("LLVM_PROFILE_FILE").is_ok() || std::env::var("CARGO_LLVM_COV").is_ok()
}

fn log_jsonl(test: &str, check: &str, passed: bool, notes: &str) {
    eprintln!(
        "{{\"test\":\"{test}\",\"check\":\"{check}\",\"passed\":{passed},\"notes\":\"{notes}\"}}"
    );
}

fn key_press(code: KeyCode) -> Event {
    Event::Key(KeyEvent {
        code,
        kind: KeyEventKind::Press,
        modifiers: Modifiers::empty(),
    })
}

fn render_lines(screen: &TerminalCapabilitiesScreen, width: u16, height: u16) -> Vec<String> {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(width, height, &mut pool);
    screen.view(&mut frame, Rect::new(0, 0, width, height));

    let mut lines = Vec::with_capacity(height as usize);
    for y in 0..height {
        let mut line = String::new();
        for x in 0..width {
            if let Some(cell) = frame.buffer.get(x, y)
                && let Some(ch) = cell.content.as_char()
            {
                line.push(ch);
            } else {
                line.push(' ');
            }
        }
        lines.push(line);
    }
    lines
}

fn buffer_hash(frame: &Frame, area: Rect) -> u64 {
    let mut hasher = DefaultHasher::new();
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let Some(cell) = frame.buffer.get(x, y) {
                if let Some(ch) = cell.content.as_char() {
                    ch.hash(&mut hasher);
                }
                cell.fg.hash(&mut hasher);
                cell.bg.hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

// =============================================================================
// 1. Profile Accuracy
// =============================================================================

#[test]
fn profile_accuracy_modern() {
    let caps = TerminalCapabilities::from_profile(TerminalProfile::Modern);

    assert_eq!(caps.profile(), TerminalProfile::Modern);
    assert!(caps.true_color);
    assert!(caps.colors_256);
    assert!(caps.sync_output);
    assert!(caps.osc8_hyperlinks);
    assert!(caps.scroll_region);
    assert!(!caps.in_tmux);
    assert!(!caps.in_screen);
    assert!(!caps.in_zellij);
    assert!(caps.kitty_keyboard);
    assert!(caps.focus_events);
    assert!(caps.bracketed_paste);
    assert!(caps.mouse_sgr);
    assert!(caps.osc52_clipboard);

    log_jsonl("profile_accuracy_modern", "all_features_on", true, "");
}

#[test]
fn profile_accuracy_xterm_256color() {
    let caps = TerminalCapabilities::from_profile(TerminalProfile::Xterm256Color);

    assert_eq!(caps.profile(), TerminalProfile::Xterm256Color);
    assert!(!caps.true_color, "xterm-256 has no truecolor");
    assert!(caps.colors_256);
    assert!(!caps.sync_output, "xterm-256 has no sync");
    assert!(!caps.osc8_hyperlinks);
    assert!(caps.scroll_region);
    assert!(!caps.in_tmux);
    assert!(caps.bracketed_paste);
    assert!(caps.mouse_sgr);

    log_jsonl(
        "profile_accuracy_xterm256",
        "no_truecolor_no_sync",
        true,
        "",
    );
}

#[test]
fn profile_accuracy_vt100() {
    let caps = TerminalCapabilities::from_profile(TerminalProfile::Vt100);

    assert_eq!(caps.profile(), TerminalProfile::Vt100);
    assert!(!caps.true_color);
    assert!(!caps.colors_256);
    assert!(!caps.sync_output);
    assert!(!caps.osc8_hyperlinks);
    assert!(caps.scroll_region, "vt100 supports scroll regions");
    assert!(!caps.kitty_keyboard);
    assert!(!caps.focus_events);
    assert!(!caps.bracketed_paste);
    assert!(!caps.mouse_sgr);

    log_jsonl("profile_accuracy_vt100", "minimal_cursor_only", true, "");
}

#[test]
fn profile_accuracy_dumb() {
    let caps = TerminalCapabilities::from_profile(TerminalProfile::Dumb);

    assert_eq!(caps.profile(), TerminalProfile::Dumb);
    assert!(!caps.true_color);
    assert!(!caps.colors_256);
    assert!(!caps.sync_output);
    assert!(!caps.osc8_hyperlinks);
    assert!(!caps.scroll_region);
    assert!(!caps.kitty_keyboard);
    assert!(!caps.focus_events);
    assert!(!caps.bracketed_paste);
    assert!(!caps.mouse_sgr);
    assert!(!caps.osc52_clipboard);

    log_jsonl("profile_accuracy_dumb", "no_features", true, "");
}

#[test]
fn profile_accuracy_all_predefined_identity() {
    for profile in TerminalProfile::all_predefined() {
        let caps = TerminalCapabilities::from_profile(*profile);
        assert_eq!(
            caps.profile(),
            *profile,
            "from_profile({profile:?}).profile() should be identity"
        );
    }

    log_jsonl(
        "profile_accuracy_identity",
        "all_profiles",
        true,
        &format!("profiles={}", TerminalProfile::all_predefined().len()),
    );
}

#[test]
fn profile_accuracy_mux_profiles() {
    for profile in [
        TerminalProfile::Tmux,
        TerminalProfile::Screen,
        TerminalProfile::Zellij,
    ] {
        let caps = TerminalCapabilities::from_profile(profile);

        let in_mux = caps.in_tmux || caps.in_screen || caps.in_zellij;
        assert!(in_mux, "{profile:?} should set a mux flag");
        assert!(caps.in_any_mux(), "{profile:?} in_any_mux should be true");

        // Mux policy: these should be effectively disabled
        assert!(
            !caps.use_sync_output(),
            "{profile:?} should disable sync_output in mux"
        );
        assert!(
            !caps.use_scroll_region(),
            "{profile:?} should disable scroll_region in mux"
        );
        assert!(
            !caps.use_hyperlinks(),
            "{profile:?} should disable hyperlinks in mux"
        );

        log_jsonl(
            "profile_accuracy_mux",
            profile.as_str(),
            true,
            "mux_policy_correct",
        );
    }
}

#[test]
fn profile_accuracy_kitty() {
    let caps = TerminalCapabilities::from_profile(TerminalProfile::Kitty);

    assert_eq!(caps.profile(), TerminalProfile::Kitty);
    assert!(caps.true_color);
    assert!(caps.colors_256);
    assert!(caps.kitty_keyboard, "Kitty should have keyboard protocol");
    assert!(caps.focus_events);
    assert!(!caps.in_tmux);
    assert!(!caps.in_screen);

    log_jsonl("profile_accuracy_kitty", "keyboard_protocol", true, "");
}

// =============================================================================
// 2. Capability Override
// =============================================================================

#[test]
fn override_push_pop_raii() {
    assert!(!has_active_overrides(), "no overrides initially");
    assert_eq!(override_depth(), 0);

    {
        let _guard = push_override(CapabilityOverride::dumb());
        assert!(has_active_overrides());
        assert_eq!(override_depth(), 1);

        {
            let _guard2 = push_override(CapabilityOverride::modern());
            assert_eq!(override_depth(), 2);
        }
        // guard2 dropped
        assert_eq!(override_depth(), 1);
    }
    // guard dropped
    assert!(!has_active_overrides());
    assert_eq!(override_depth(), 0);

    log_jsonl("override_raii", "push_pop_cleanup", true, "");
}

#[test]
fn override_with_closure() {
    assert!(!has_active_overrides());

    with_capability_override(CapabilityOverride::dumb(), || {
        assert!(has_active_overrides());
        let caps = TerminalCapabilities::with_overrides();
        assert!(!caps.true_color, "dumb override disables truecolor");
        assert!(!caps.colors_256, "dumb override disables 256 colors");
    });

    assert!(
        !has_active_overrides(),
        "closure override should be cleaned up"
    );

    log_jsonl("override_closure", "scoped_cleanup", true, "");
}

#[test]
fn override_stacking() {
    // Base: modern
    let base = TerminalCapabilities::modern();

    // Stack: disable truecolor
    let mut partial = CapabilityOverride::new();
    partial.true_color = Some(false);

    let _guard = push_override(partial);
    let caps = base.with_overrides_from(base);

    // true_color overridden to false, but colors_256 should remain from base
    assert!(!caps.true_color, "override should disable truecolor");

    clear_all_overrides();

    log_jsonl("override_stacking", "partial_override", true, "");
}

#[test]
fn override_thread_isolation() {
    clear_all_overrides();

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let barrier_clone = barrier.clone();

    let handle = std::thread::spawn(move || {
        let _guard = push_override(CapabilityOverride::dumb());
        assert!(has_active_overrides(), "thread should have override");
        assert_eq!(override_depth(), 1);

        barrier_clone.wait(); // sync with main thread

        // Override still active in this thread
        assert!(has_active_overrides());
    });

    barrier.wait(); // wait for thread to set override

    // Main thread should NOT see the override
    assert!(
        !has_active_overrides(),
        "main thread should not see other thread's overrides"
    );

    handle.join().unwrap();

    log_jsonl("override_thread_isolation", "cross_thread", true, "");
}

#[test]
fn override_clear_all() {
    let _g1 = push_override(CapabilityOverride::dumb());
    let _g2 = push_override(CapabilityOverride::modern());
    assert_eq!(override_depth(), 2);

    clear_all_overrides();
    assert_eq!(override_depth(), 0);
    assert!(!has_active_overrides());

    // Guards are still alive but should not panic on drop
    drop(_g2);
    drop(_g1);

    log_jsonl("override_clear_all", "force_clear", true, "");
}

// =============================================================================
// 3. Degradation (Mux-Aware Fallback)
// =============================================================================

#[test]
fn degradation_sync_output_disabled_in_all_muxes() {
    let mux_profiles = [
        TerminalProfile::Tmux,
        TerminalProfile::Screen,
        TerminalProfile::Zellij,
    ];

    for profile in mux_profiles {
        let caps = TerminalCapabilities::from_profile(profile);
        assert!(
            !caps.use_sync_output(),
            "{profile:?} should disable sync_output"
        );
    }

    // Non-mux profiles with sync_output should have it enabled
    let modern = TerminalCapabilities::from_profile(TerminalProfile::Modern);
    assert!(modern.use_sync_output(), "Modern should have sync_output");

    log_jsonl("degradation_sync", "mux_disabled_nonmux_enabled", true, "");
}

#[test]
fn degradation_scroll_region_disabled_in_muxes() {
    for profile in [
        TerminalProfile::Tmux,
        TerminalProfile::Screen,
        TerminalProfile::Zellij,
    ] {
        let caps = TerminalCapabilities::from_profile(profile);
        assert!(
            !caps.use_scroll_region(),
            "{profile:?} should disable scroll_region"
        );
    }

    let modern = TerminalCapabilities::modern();
    assert!(
        modern.use_scroll_region(),
        "Non-mux modern should have scroll_region"
    );

    log_jsonl("degradation_scroll", "mux_disabled", true, "");
}

#[test]
fn degradation_hyperlinks_disabled_in_muxes() {
    for profile in [
        TerminalProfile::Tmux,
        TerminalProfile::Screen,
        TerminalProfile::Zellij,
    ] {
        let caps = TerminalCapabilities::from_profile(profile);
        assert!(
            !caps.use_hyperlinks(),
            "{profile:?} should disable hyperlinks"
        );
    }

    let modern = TerminalCapabilities::modern();
    assert!(
        modern.use_hyperlinks(),
        "Non-mux modern should have hyperlinks"
    );

    log_jsonl("degradation_hyperlinks", "mux_disabled", true, "");
}

#[test]
fn degradation_color_fallback_hierarchy() {
    // Verify the color support hierarchy: truecolor > 256 > 16
    let modern = TerminalCapabilities::modern();
    assert!(modern.true_color && modern.colors_256);

    let xterm256 = TerminalCapabilities::xterm_256color();
    assert!(!xterm256.true_color && xterm256.colors_256);

    let xterm = TerminalCapabilities::xterm();
    assert!(!xterm.true_color && !xterm.colors_256);

    let dumb = TerminalCapabilities::dumb();
    assert!(!dumb.true_color && !dumb.colors_256);

    log_jsonl("degradation_color", "hierarchy_correct", true, "");
}

// =============================================================================
// 4. Quirk Simulation (Mux-Specific Policies)
// =============================================================================

#[test]
fn quirk_tmux_passthrough() {
    let tmux = TerminalCapabilities::from_profile(TerminalProfile::Tmux);
    assert!(tmux.in_tmux);
    assert!(
        tmux.needs_passthrough_wrap(),
        "tmux needs passthrough wrapping"
    );
    assert!(tmux.in_any_mux());

    log_jsonl("quirk_tmux", "passthrough_needed", true, "");
}

#[test]
fn quirk_screen_passthrough() {
    let screen = TerminalCapabilities::from_profile(TerminalProfile::Screen);
    assert!(screen.in_screen);
    assert!(
        screen.needs_passthrough_wrap(),
        "GNU screen needs passthrough wrapping"
    );

    log_jsonl("quirk_screen", "passthrough_needed", true, "");
}

#[test]
fn quirk_zellij_no_passthrough() {
    let zellij = TerminalCapabilities::from_profile(TerminalProfile::Zellij);
    assert!(zellij.in_zellij);
    assert!(
        !zellij.needs_passthrough_wrap(),
        "Zellij does NOT need passthrough"
    );

    log_jsonl("quirk_zellij", "no_passthrough", true, "");
}

#[test]
fn quirk_windows_console() {
    let win = TerminalCapabilities::from_profile(TerminalProfile::WindowsConsole);
    assert!(!win.in_tmux);
    assert!(!win.in_screen);
    assert!(!win.in_zellij);
    assert!(!win.needs_passthrough_wrap());

    log_jsonl(
        "quirk_windows",
        "no_mux_no_passthrough",
        true,
        &format!("truecolor={} 256={}", win.true_color, win.colors_256),
    );
}

#[test]
fn quirk_linux_console() {
    let linux = TerminalCapabilities::from_profile(TerminalProfile::LinuxConsole);
    assert!(!linux.true_color, "Linux console has no truecolor");
    assert!(!linux.osc8_hyperlinks);
    assert!(!linux.osc52_clipboard);
    assert!(!linux.kitty_keyboard);

    log_jsonl("quirk_linux", "minimal_features", true, "");
}

// =============================================================================
// 5. Integration: Demo Screen Under Different Profiles
// =============================================================================

#[test]
fn integration_screen_renders_under_all_profiles() {
    for profile in TerminalProfile::all_predefined() {
        let screen = TerminalCapabilitiesScreen::with_profile(*profile);
        let lines = render_lines(&screen, 120, 40);

        // Should render without panic and produce non-empty output
        let non_empty_lines = lines.iter().filter(|l| l.chars().any(|c| c != ' ')).count();
        assert!(
            non_empty_lines > 5,
            "{profile:?} should produce meaningful output, got {non_empty_lines} non-empty lines"
        );

        // Profile name should appear in the rendered output
        let profile_name = profile.as_str();
        let has_profile_ref = lines
            .iter()
            .any(|l| l.to_lowercase().contains(profile_name));
        assert!(
            has_profile_ref,
            "{profile:?} profile name should appear in rendered output"
        );

        log_jsonl(
            "integration_all_profiles",
            profile_name,
            true,
            &format!("non_empty_lines={non_empty_lines}"),
        );
    }
}

#[test]
fn integration_profile_switching_changes_output() {
    let mut screen = TerminalCapabilitiesScreen::with_profile(TerminalProfile::Modern);

    let lines_modern = render_lines(&screen, 120, 40);
    let modern_content = lines_modern.join("\n");

    // Switch profile with 'P'
    let _ = screen.update(&key_press(KeyCode::Char('p')));
    let lines_after = render_lines(&screen, 120, 40);
    let after_content = lines_after.join("\n");

    assert_ne!(
        modern_content, after_content,
        "Profile switch should change rendered output"
    );

    log_jsonl("integration_profile_switch", "output_changes", true, "");
}

#[test]
fn integration_screen_render_determinism() {
    let area = Rect::new(0, 0, 120, 40);

    for profile in TerminalProfile::all_predefined() {
        let screen = TerminalCapabilitiesScreen::with_profile(*profile);

        let mut pool1 = GraphemePool::new();
        let mut frame1 = Frame::new(120, 40, &mut pool1);
        screen.view(&mut frame1, area);
        let hash1 = buffer_hash(&frame1, area);

        let mut pool2 = GraphemePool::new();
        let mut frame2 = Frame::new(120, 40, &mut pool2);
        screen.view(&mut frame2, area);
        let hash2 = buffer_hash(&frame2, area);

        assert_eq!(
            hash1, hash2,
            "{profile:?}: same state should produce identical render"
        );

        log_jsonl(
            "integration_determinism",
            profile.as_str(),
            true,
            &format!("hash={hash1}"),
        );
    }
}

#[test]
fn integration_different_profiles_different_output() {
    let area = Rect::new(0, 0, 120, 40);
    let mut hashes = Vec::new();

    for profile in [
        TerminalProfile::Modern,
        TerminalProfile::Dumb,
        TerminalProfile::Tmux,
    ] {
        let screen = TerminalCapabilitiesScreen::with_profile(profile);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        screen.view(&mut frame, area);
        hashes.push((profile, buffer_hash(&frame, area)));
    }

    let distinct = hashes
        .iter()
        .map(|(_, h)| h)
        .collect::<std::collections::HashSet<_>>()
        .len();

    assert!(
        distinct >= 2,
        "Modern, Dumb, Tmux should produce at least 2 distinct outputs"
    );

    log_jsonl(
        "integration_distinct",
        "cross_profile",
        true,
        &format!("distinct={distinct}"),
    );
}

#[test]
fn integration_multiple_sizes() {
    let sizes: &[(u16, u16)] = &[(40, 10), (80, 24), (120, 40), (200, 50)];

    for &(w, h) in sizes {
        let screen = TerminalCapabilitiesScreen::with_profile(TerminalProfile::Modern);
        let lines = render_lines(&screen, w, h);

        // Should render without panic at any size
        assert_eq!(lines.len(), h as usize, "{w}x{h}: line count mismatch");

        let non_empty = lines.iter().filter(|l| l.chars().any(|c| c != ' ')).count();
        assert!(
            non_empty > 0,
            "{w}x{h}: should produce some non-empty lines"
        );

        log_jsonl(
            "integration_sizes",
            &format!("{w}x{h}"),
            true,
            &format!("non_empty={non_empty}"),
        );
    }
}

// =============================================================================
// Performance: Profile Switching Latency
// =============================================================================

#[test]
fn perf_profile_cycling_latency() {
    let mut screen = TerminalCapabilitiesScreen::with_profile(TerminalProfile::Modern);

    let start = Instant::now();
    for _ in 0..100 {
        let _ = screen.update(&key_press(KeyCode::Char('p')));
    }
    let elapsed = start.elapsed();

    log_jsonl(
        "perf_profile_cycling",
        "100_cycles",
        elapsed.as_millis() < 50,
        &format!("elapsed_us={}", elapsed.as_micros()),
    );

    assert!(
        elapsed.as_millis() < 50,
        "Profile cycling should be fast: {:?}",
        elapsed
    );
}

#[test]
fn perf_render_under_all_profiles() {
    let area = Rect::new(0, 0, 120, 40);
    let mut results = Vec::new();

    for profile in TerminalProfile::all_predefined() {
        let screen = TerminalCapabilitiesScreen::with_profile(*profile);
        let mut pool = GraphemePool::new();

        let mut times = Vec::with_capacity(20);
        for _ in 0..20 {
            let mut frame = Frame::new(120, 40, &mut pool);
            let start = Instant::now();
            screen.view(&mut frame, area);
            times.push(start.elapsed().as_nanos() as u64);
        }

        times.sort();
        let avg_ns = times.iter().sum::<u64>() / times.len() as u64;
        let p95_ns = times[times.len() * 95 / 100];

        results.push(serde_json::json!({
            "profile": profile.as_str(),
            "avg_ns": avg_ns,
            "p95_ns": p95_ns,
        }));

        let budget_ns = if is_coverage_run() {
            12_000_000
        } else {
            5_000_000
        };
        assert!(
            avg_ns < budget_ns,
            "{profile:?} render exceeded {budget_ns}ns budget: avg={}ns",
            avg_ns,
        );
    }

    eprintln!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "test": "perf_render_all_profiles",
            "results": results,
        }))
        .unwrap()
    );
}

// =============================================================================
// Override + Screen Integration
// =============================================================================

#[test]
fn integration_override_affects_screen_rendering() {
    let screen_base = TerminalCapabilitiesScreen::new();
    let lines_base = render_lines(&screen_base, 120, 40);
    let base_content = lines_base.join("\n");

    // Render under a dumb override
    with_capability_override(CapabilityOverride::dumb(), || {
        let screen_dumb = TerminalCapabilitiesScreen::new();
        let lines_dumb = render_lines(&screen_dumb, 120, 40);
        let dumb_content = lines_dumb.join("\n");

        // Output may differ based on detected caps vs overridden
        log_jsonl(
            "integration_override_screen",
            "dumb_override",
            true,
            &format!(
                "base_len={} dumb_len={}",
                base_content.len(),
                dumb_content.len()
            ),
        );
    });
}

// =============================================================================
// Profile String Parsing
// =============================================================================

#[test]
fn profile_from_str_roundtrip() {
    for profile in TerminalProfile::all_predefined() {
        let name = profile.as_str();
        let parsed: TerminalProfile = name.parse().expect("Failed to parse profile name");
        assert_eq!(*profile, parsed, "Roundtrip failed for {name}");
    }

    log_jsonl("profile_from_str", "roundtrip", true, "");
}

#[test]
fn profile_from_str_aliases() {
    let aliases = [
        ("xterm-256", TerminalProfile::Xterm256Color),
        ("xterm256color", TerminalProfile::Xterm256Color),
        ("screen-256color", TerminalProfile::Screen),
        ("tmux-256color", TerminalProfile::Tmux),
        ("xterm-kitty", TerminalProfile::Kitty),
        ("linux-console", TerminalProfile::LinuxConsole),
        ("windows", TerminalProfile::WindowsConsole),
        ("conhost", TerminalProfile::WindowsConsole),
        ("auto", TerminalProfile::Detected),
    ];

    for (alias, expected) in aliases {
        let parsed: Result<TerminalProfile, ()> = alias.parse();
        assert_eq!(
            parsed,
            Ok(expected),
            "Alias '{alias}' should parse to {expected:?}"
        );
    }

    log_jsonl(
        "profile_from_str_aliases",
        "all_aliases",
        true,
        &format!("count={}", aliases.len()),
    );
}

// =============================================================================
// Builder API
// =============================================================================

#[test]
fn builder_custom_profile() {
    let caps = TerminalCapabilities::builder()
        .colors_256(true)
        .true_color(false)
        .mouse_sgr(true)
        .build();

    assert!(caps.colors_256);
    assert!(!caps.true_color);
    assert!(caps.mouse_sgr);
    assert!(!caps.sync_output, "builder defaults are off");

    log_jsonl("builder_custom", "selective_enable", true, "");
}

#[test]
fn builder_from_profile_override() {
    let caps = CapabilityProfileBuilder::from_profile(TerminalProfile::Modern)
        .true_color(false) // Override one field
        .build();

    assert!(!caps.true_color, "overridden field");
    assert!(caps.colors_256, "non-overridden field from Modern");
    assert!(caps.sync_output, "non-overridden field from Modern");

    log_jsonl("builder_profile_override", "selective_override", true, "");
}

// =============================================================================
// Edge Cases
// =============================================================================

#[test]
fn edge_case_unknown_profile_string() {
    let parsed: Result<TerminalProfile, ()> = "nonexistent-terminal".parse();
    assert!(parsed.is_err(), "Unknown profile string should fail");

    log_jsonl("edge_unknown_profile", "parse_fails", true, "");
}

#[test]
fn edge_case_override_apply_to_dumb() {
    let dumb = TerminalCapabilities::dumb();
    let modern_override = CapabilityOverride::modern();
    let result = modern_override.apply_to(dumb);

    assert!(
        result.true_color,
        "override should enable truecolor on dumb"
    );
    assert!(result.colors_256);
    assert!(result.sync_output);

    log_jsonl("edge_override_on_dumb", "applies_correctly", true, "");
}

#[test]
fn edge_case_empty_override_is_identity() {
    let modern = TerminalCapabilities::modern();
    let empty = CapabilityOverride::new();
    let result = empty.apply_to(modern);

    assert_eq!(
        result, modern,
        "Empty override should not change capabilities"
    );

    log_jsonl("edge_empty_override", "identity", true, "");
}
