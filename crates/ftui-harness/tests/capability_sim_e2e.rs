#![forbid(unsafe_code)]

//! Capability Simulator E2E Test Suite (bd-k4lj.6)
//!
//! End-to-end validation of terminal capability simulation for testing.
//! Tests profile accuracy, override stacking, degradation behavior,
//! quirk simulation, and cross-profile integration.
//!
//! # Invariants
//!
//! | ID       | Invariant                                              |
//! |----------|--------------------------------------------------------|
//! | PROF-1   | Profile capabilities match specification exactly        |
//! | PROF-2   | Profile identity is preserved through from_profile()    |
//! | OVER-1   | Override stacking applies inner over outer correctly    |
//! | OVER-2   | Override cleanup is guaranteed (RAII)                   |
//! | OVER-3   | Thread-local isolation between concurrent tests         |
//! | DEG-1    | Mux-aware policies degrade features conservatively      |
//! | DEG-2    | Color fallback follows depth ordering                   |
//! | QUIRK-1  | Multiplexer quirks disable passthrough-unsafe features  |
//! | INT-1    | Profile matrix produces deterministic snapshots         |
//! | INT-2    | Cross-profile diffs are consistent                      |
//!
//! # Running
//!
//! ```sh
//! cargo test -p ftui-harness capability_sim_
//! ```
//!
//! # JSONL Logging
//!
//! ```sh
//! CAP_SIM_LOG=1 cargo test -p ftui-harness capability_sim_
//! ```

use ftui_core::capability_override::{
    CapabilityOverride, clear_all_overrides, has_active_overrides, override_depth, push_override,
    with_capability_override,
};
use ftui_core::terminal_capabilities::{TerminalCapabilities, TerminalProfile};
use std::io::Write;

// ============================================================================
// JSONL Logger
// ============================================================================

struct CapSimLogger {
    writer: Option<Box<dyn Write>>,
    run_id: String,
}

impl CapSimLogger {
    fn new(case_name: &str) -> Self {
        let writer = if std::env::var("CAP_SIM_LOG").is_ok() {
            let dir = std::env::temp_dir().join("ftui_cap_sim_e2e");
            let _ = std::fs::create_dir_all(&dir);
            let path = dir.join(format!("{case_name}.jsonl"));
            std::fs::File::create(path)
                .ok()
                .map(|f| Box::new(f) as Box<dyn Write>)
        } else {
            None
        };
        Self {
            writer,
            run_id: format!(
                "{}-{}",
                case_name,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
            ),
        }
    }

    fn log_event(&mut self, event: &str, data: &str) {
        if let Some(ref mut w) = self.writer {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let _ = writeln!(
                w,
                r#"{{"run_id":"{}","event":"{}","ts_ms":{},"data":{}}}"#,
                self.run_id, event, ts, data
            );
        }
    }

    fn log_invariant(&mut self, id: &str, passed: bool, detail: &str) {
        self.log_event(
            "invariant",
            &format!(
                r#"{{"id":"{}","passed":{},"detail":"{}"}}"#,
                id, passed, detail
            ),
        );
    }

    fn log_profile(&mut self, profile: &str, caps_summary: &str) {
        self.log_event(
            "profile",
            &format!(
                r#"{{"profile":"{}","capabilities":"{}"}}"#,
                profile, caps_summary
            ),
        );
    }

    fn log_complete(&mut self, passed: bool, checks: usize) {
        self.log_event(
            "complete",
            &format!(r#"{{"passed":{},"total_checks":{}}}"#, passed, checks),
        );
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Summarize capabilities as a compact string for logging.
fn caps_summary(caps: &TerminalCapabilities) -> String {
    format!(
        "tc={},256={},sync={},hyper={},scroll={},mux={},kitty={},focus={},paste={},mouse={},clip={}",
        caps.true_color as u8,
        caps.colors_256 as u8,
        caps.sync_output as u8,
        caps.osc8_hyperlinks as u8,
        caps.scroll_region as u8,
        caps.in_any_mux() as u8,
        caps.kitty_keyboard as u8,
        caps.focus_events as u8,
        caps.bracketed_paste as u8,
        caps.mouse_sgr as u8,
        caps.osc52_clipboard as u8,
    )
}

/// All profiles that have predefined capability sets.
const ALL_PROFILES: [TerminalProfile; 11] = [
    TerminalProfile::Modern,
    TerminalProfile::Xterm256Color,
    TerminalProfile::Xterm,
    TerminalProfile::Vt100,
    TerminalProfile::Dumb,
    TerminalProfile::Screen,
    TerminalProfile::Tmux,
    TerminalProfile::Zellij,
    TerminalProfile::WindowsConsole,
    TerminalProfile::Kitty,
    TerminalProfile::LinuxConsole,
];

// ============================================================================
// PROF-1: Profile Accuracy
// ============================================================================

#[test]
fn capability_sim_profile_modern() {
    let mut logger = CapSimLogger::new("profile_modern");
    let caps = TerminalCapabilities::modern();

    assert!(caps.true_color, "PROF-1: modern should have true color");
    assert!(caps.colors_256, "PROF-1: modern should have 256 colors");
    assert!(caps.sync_output, "PROF-1: modern should have sync output");
    assert!(
        caps.osc8_hyperlinks,
        "PROF-1: modern should have hyperlinks"
    );
    assert!(
        caps.scroll_region,
        "PROF-1: modern should have scroll region"
    );
    assert!(!caps.in_any_mux(), "PROF-1: modern should not be in mux");
    assert!(
        caps.kitty_keyboard,
        "PROF-1: modern should have kitty keyboard"
    );
    assert!(caps.focus_events, "PROF-1: modern should have focus events");
    assert!(
        caps.bracketed_paste,
        "PROF-1: modern should have bracketed paste"
    );
    assert!(caps.mouse_sgr, "PROF-1: modern should have mouse sgr");
    assert!(caps.osc52_clipboard, "PROF-1: modern should have clipboard");

    logger.log_profile("modern", &caps_summary(&caps));
    logger.log_invariant("PROF-1", true, "modern_all_features");
    logger.log_complete(true, 11);
}

#[test]
fn capability_sim_profile_xterm_256color() {
    let mut logger = CapSimLogger::new("profile_xterm256");
    let caps = TerminalCapabilities::xterm_256color();

    assert!(!caps.true_color, "PROF-1: xterm256 no true color");
    assert!(caps.colors_256, "PROF-1: xterm256 has 256 colors");
    assert!(!caps.sync_output, "PROF-1: xterm256 no sync output");
    assert!(!caps.osc8_hyperlinks, "PROF-1: xterm256 no hyperlinks");
    assert!(caps.scroll_region, "PROF-1: xterm256 has scroll region");
    assert!(!caps.kitty_keyboard, "PROF-1: xterm256 no kitty keyboard");
    assert!(caps.bracketed_paste, "PROF-1: xterm256 has bracketed paste");
    assert!(caps.mouse_sgr, "PROF-1: xterm256 has mouse sgr");

    logger.log_profile("xterm-256color", &caps_summary(&caps));
    logger.log_invariant("PROF-1", true, "xterm256_capabilities");
    logger.log_complete(true, 8);
}

#[test]
fn capability_sim_profile_xterm_16() {
    let mut logger = CapSimLogger::new("profile_xterm16");
    let caps = TerminalCapabilities::xterm();

    assert!(!caps.true_color, "PROF-1: xterm no true color");
    assert!(!caps.colors_256, "PROF-1: xterm no 256 colors");
    assert!(caps.scroll_region, "PROF-1: xterm has scroll region");
    assert!(caps.bracketed_paste, "PROF-1: xterm has bracketed paste");
    assert!(caps.mouse_sgr, "PROF-1: xterm has mouse sgr");
    assert!(!caps.kitty_keyboard, "PROF-1: xterm no kitty keyboard");
    assert!(!caps.focus_events, "PROF-1: xterm no focus events");

    logger.log_profile("xterm", &caps_summary(&caps));
    logger.log_invariant("PROF-1", true, "xterm16_capabilities");
    logger.log_complete(true, 7);
}

#[test]
fn capability_sim_profile_vt100() {
    let mut logger = CapSimLogger::new("profile_vt100");
    let caps = TerminalCapabilities::vt100();

    assert!(!caps.true_color, "PROF-1: vt100 no true color");
    assert!(!caps.colors_256, "PROF-1: vt100 no 256 colors");
    assert!(!caps.has_color(), "PROF-1: vt100 has no color at all");
    assert!(caps.scroll_region, "PROF-1: vt100 has scroll region");
    assert!(!caps.bracketed_paste, "PROF-1: vt100 no bracketed paste");
    assert!(!caps.mouse_sgr, "PROF-1: vt100 no mouse");

    logger.log_profile("vt100", &caps_summary(&caps));
    logger.log_invariant("PROF-1", true, "vt100_minimal");
    logger.log_complete(true, 6);
}

#[test]
fn capability_sim_profile_dumb() {
    let mut logger = CapSimLogger::new("profile_dumb");
    let caps = TerminalCapabilities::dumb();

    // Dumb terminal: absolutely nothing
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
    assert!(!caps.in_any_mux());
    assert!(!caps.has_color());
    assert_eq!(caps.color_depth(), "mono");

    logger.log_profile("dumb", &caps_summary(&caps));
    logger.log_invariant("PROF-1", true, "dumb_nothing_enabled");
    logger.log_complete(true, 13);
}

#[test]
fn capability_sim_profile_kitty() {
    let mut logger = CapSimLogger::new("profile_kitty");
    let caps = TerminalCapabilities::kitty();

    // Kitty should match Modern's features
    assert!(caps.true_color);
    assert!(caps.colors_256);
    assert!(caps.sync_output);
    assert!(caps.osc8_hyperlinks);
    assert!(caps.kitty_keyboard);
    assert!(caps.focus_events);
    assert!(caps.osc52_clipboard);
    assert_eq!(caps.profile(), TerminalProfile::Kitty);

    logger.log_profile("kitty", &caps_summary(&caps));
    logger.log_invariant("PROF-1", true, "kitty_full_features");
    logger.log_complete(true, 8);
}

// ============================================================================
// PROF-2: Profile Identity Preservation
// ============================================================================

#[test]
fn capability_sim_profile_identity_roundtrip() {
    let mut logger = CapSimLogger::new("profile_identity");

    for profile in &ALL_PROFILES {
        let caps = TerminalCapabilities::from_profile(*profile);
        assert_eq!(
            caps.profile(),
            *profile,
            "PROF-2: from_profile({profile:?}) should preserve profile identity"
        );
        logger.log_invariant("PROF-2", true, &format!("{profile:?}"));
    }

    logger.log_complete(true, ALL_PROFILES.len());
}

#[test]
fn capability_sim_profile_from_profile_deterministic() {
    let mut logger = CapSimLogger::new("profile_deterministic");

    for profile in &ALL_PROFILES {
        let caps1 = TerminalCapabilities::from_profile(*profile);
        let caps2 = TerminalCapabilities::from_profile(*profile);
        assert_eq!(
            caps1, caps2,
            "PROF-2: from_profile({profile:?}) should be deterministic"
        );
    }

    logger.log_invariant("PROF-2", true, "deterministic_all_profiles");
    logger.log_complete(true, ALL_PROFILES.len());
}

// ============================================================================
// DEG-2: Color Depth Ordering
// ============================================================================

#[test]
fn capability_sim_color_depth_ordering() {
    let mut logger = CapSimLogger::new("color_depth_ordering");

    // Color depth should follow: mono < 256 < truecolor
    let dumb = TerminalCapabilities::dumb();
    let xterm256 = TerminalCapabilities::xterm_256color();
    let modern = TerminalCapabilities::modern();

    assert_eq!(dumb.color_depth(), "mono");
    assert_eq!(xterm256.color_depth(), "256");
    assert_eq!(modern.color_depth(), "truecolor");

    // has_color() should be false for mono
    assert!(!dumb.has_color());
    assert!(xterm256.has_color());
    assert!(modern.has_color());

    logger.log_invariant("DEG-2", true, "color_depth_ordering");
    logger.log_complete(true, 6);
}

#[test]
fn capability_sim_color_depth_all_profiles() {
    let mut logger = CapSimLogger::new("color_depth_profiles");

    for profile in &ALL_PROFILES {
        let caps = TerminalCapabilities::from_profile(*profile);
        let depth = caps.color_depth();

        // Verify depth is valid
        assert!(
            ["mono", "256", "truecolor"].contains(&depth),
            "PROF-1: {profile:?} has invalid color_depth: {depth}"
        );

        // Verify depth consistency with flags
        match depth {
            "truecolor" => assert!(
                caps.true_color,
                "{profile:?}: truecolor but true_color=false"
            ),
            "256" => {
                assert!(!caps.true_color, "{profile:?}: 256 but true_color=true");
                assert!(caps.colors_256, "{profile:?}: 256 but colors_256=false");
            }
            "mono" => assert!(!caps.has_color(), "{profile:?}: mono but has_color()=true"),
            _ => unreachable!(),
        }

        logger.log_profile(&format!("{profile:?}"), &format!("depth={depth}"));
    }

    logger.log_invariant("DEG-2", true, "color_depth_consistency");
    logger.log_complete(true, ALL_PROFILES.len());
}

// ============================================================================
// OVER-1: Override Stacking
// ============================================================================

#[test]
fn capability_sim_override_apply_single() {
    let mut logger = CapSimLogger::new("override_single");

    let base = TerminalCapabilities::modern();
    let override_cfg = CapabilityOverride::new().true_color(Some(false));
    let result = override_cfg.apply_to(base);

    assert!(
        !result.true_color,
        "OVER-1: override should disable true color"
    );
    // Other fields should remain from base
    assert!(result.colors_256, "OVER-1: unoverridden field preserved");
    assert!(result.sync_output, "OVER-1: unoverridden field preserved");

    logger.log_invariant("OVER-1", true, "single_override");
    logger.log_complete(true, 3);
}

#[test]
fn capability_sim_override_stacking_precedence() {
    let mut logger = CapSimLogger::new("override_stacking");

    let base = TerminalCapabilities::dumb();

    // First override: enable true color
    let over1 = CapabilityOverride::new().true_color(Some(true));
    let after1 = over1.apply_to(base);
    assert!(
        after1.true_color,
        "OVER-1: first override enables true color"
    );

    // Second override: disable it again
    let over2 = CapabilityOverride::new().true_color(Some(false));
    let after2 = over2.apply_to(after1);
    assert!(
        !after2.true_color,
        "OVER-1: second override should win (inner over outer)"
    );

    // Third override: None (no change)
    let over3 = CapabilityOverride::new(); // all None
    let after3 = over3.apply_to(after2);
    assert!(
        !after3.true_color,
        "OVER-1: None override preserves previous value"
    );

    logger.log_invariant("OVER-1", true, "stacking_precedence");
    logger.log_complete(true, 3);
}

#[test]
fn capability_sim_override_dumb_disables_all() {
    let mut logger = CapSimLogger::new("override_dumb");

    let base = TerminalCapabilities::modern();
    let dumb_override = CapabilityOverride::dumb();
    let result = dumb_override.apply_to(base);

    assert!(!result.true_color);
    assert!(!result.colors_256);
    assert!(!result.sync_output);
    assert!(!result.osc8_hyperlinks);
    assert!(!result.scroll_region);
    assert!(!result.kitty_keyboard);
    assert!(!result.focus_events);
    assert!(!result.bracketed_paste);
    assert!(!result.mouse_sgr);
    assert!(!result.osc52_clipboard);
    assert!(!result.in_tmux);
    assert!(!result.in_screen);
    assert!(!result.in_zellij);

    logger.log_invariant("OVER-1", true, "dumb_disables_all");
    logger.log_complete(true, 13);
}

#[test]
fn capability_sim_override_modern_enables_all() {
    let mut logger = CapSimLogger::new("override_modern");

    let base = TerminalCapabilities::dumb();
    let modern_override = CapabilityOverride::modern();
    let result = modern_override.apply_to(base);

    assert!(result.true_color);
    assert!(result.colors_256);
    assert!(result.sync_output);
    assert!(result.osc8_hyperlinks);
    assert!(result.scroll_region);
    assert!(result.kitty_keyboard);
    assert!(result.focus_events);
    assert!(result.bracketed_paste);
    assert!(result.mouse_sgr);
    assert!(result.osc52_clipboard);
    // Modern override explicitly sets mux to false
    assert!(!result.in_tmux);
    assert!(!result.in_screen);
    assert!(!result.in_zellij);

    logger.log_invariant("OVER-1", true, "modern_enables_all");
    logger.log_complete(true, 13);
}

#[test]
fn capability_sim_override_tmux_simulation() {
    let mut logger = CapSimLogger::new("override_tmux");

    let base = TerminalCapabilities::modern();
    let tmux_override = CapabilityOverride::tmux();
    let result = tmux_override.apply_to(base);

    assert!(result.in_tmux, "OVER-1: tmux override enables in_tmux");
    assert!(!result.sync_output, "OVER-1: tmux disables sync output");
    assert!(!result.osc8_hyperlinks, "OVER-1: tmux disables hyperlinks");
    assert!(
        !result.kitty_keyboard,
        "OVER-1: tmux disables kitty keyboard"
    );
    assert!(result.colors_256, "OVER-1: tmux enables 256 colors");
    assert!(result.bracketed_paste, "OVER-1: tmux keeps bracketed paste");
    // true_color is None in tmux override, so inherits from base (modern = true)
    assert!(
        result.true_color,
        "OVER-1: tmux inherits true_color from base"
    );

    logger.log_invariant("OVER-1", true, "tmux_simulation");
    logger.log_complete(true, 7);
}

#[test]
fn capability_sim_override_empty_is_identity() {
    let mut logger = CapSimLogger::new("override_identity");

    let empty = CapabilityOverride::new();
    assert!(empty.is_empty(), "Empty override should report is_empty()");

    for profile in &ALL_PROFILES {
        let base = TerminalCapabilities::from_profile(*profile);
        let result = empty.apply_to(base);
        assert_eq!(
            base, result,
            "OVER-1: empty override should be identity for {profile:?}"
        );
    }

    logger.log_invariant("OVER-1", true, "empty_identity");
    logger.log_complete(true, ALL_PROFILES.len());
}

// ============================================================================
// OVER-2: Override RAII Cleanup
// ============================================================================

#[test]
fn capability_sim_override_guard_cleanup() {
    let mut logger = CapSimLogger::new("guard_cleanup");

    // Ensure clean state
    clear_all_overrides();
    assert!(!has_active_overrides(), "Should start clean");
    assert_eq!(override_depth(), 0);

    {
        let _guard = push_override(CapabilityOverride::dumb());
        assert!(
            has_active_overrides(),
            "OVER-2: guard should activate override"
        );
        assert_eq!(override_depth(), 1, "OVER-2: depth should be 1");
    }
    // Guard dropped

    assert!(
        !has_active_overrides(),
        "OVER-2: override should be cleaned up after guard drop"
    );
    assert_eq!(override_depth(), 0, "OVER-2: depth should return to 0");

    logger.log_invariant("OVER-2", true, "guard_cleanup");
    logger.log_complete(true, 4);
}

#[test]
fn capability_sim_override_nested_guards() {
    let mut logger = CapSimLogger::new("nested_guards");

    clear_all_overrides();

    {
        let _g1 = push_override(CapabilityOverride::new().true_color(Some(true)));
        assert_eq!(override_depth(), 1);

        {
            let _g2 = push_override(CapabilityOverride::new().true_color(Some(false)));
            assert_eq!(override_depth(), 2);

            {
                let _g3 = push_override(CapabilityOverride::new().colors_256(Some(true)));
                assert_eq!(override_depth(), 3, "OVER-2: three nested overrides");
            }
            assert_eq!(override_depth(), 2, "OVER-2: back to 2 after inner drop");
        }
        assert_eq!(override_depth(), 1, "OVER-2: back to 1");
    }
    assert_eq!(override_depth(), 0, "OVER-2: fully cleaned up");

    logger.log_invariant("OVER-2", true, "nested_guards");
    logger.log_complete(true, 4);
}

#[test]
fn capability_sim_override_with_closure() {
    let mut logger = CapSimLogger::new("with_closure");

    clear_all_overrides();

    let result = with_capability_override(CapabilityOverride::dumb(), || {
        assert!(has_active_overrides(), "OVER-2: active inside closure");
        assert_eq!(override_depth(), 1);
        42
    });

    assert_eq!(result, 42, "Closure should return value");
    assert!(!has_active_overrides(), "OVER-2: cleaned up after closure");

    logger.log_invariant("OVER-2", true, "with_closure");
    logger.log_complete(true, 3);
}

#[test]
fn capability_sim_override_clear_all() {
    let mut logger = CapSimLogger::new("clear_all");

    let _g1 = push_override(CapabilityOverride::dumb());
    let _g2 = push_override(CapabilityOverride::modern());
    assert_eq!(override_depth(), 2);

    clear_all_overrides();
    assert_eq!(override_depth(), 0, "OVER-2: clear_all removes everything");
    assert!(!has_active_overrides());

    // Guards dropping after clear should not panic
    drop(_g2);
    drop(_g1);

    logger.log_invariant("OVER-2", true, "clear_all_safe");
    logger.log_complete(true, 3);
}

// ============================================================================
// DEG-1: Mux-Aware Degradation Policies
// ============================================================================

#[test]
fn capability_sim_mux_sync_output_disabled() {
    let mut logger = CapSimLogger::new("mux_sync_output");

    // All multiplexers should disable sync output via use_sync_output()
    let mux_profiles = [
        TerminalProfile::Tmux,
        TerminalProfile::Screen,
        TerminalProfile::Zellij,
    ];

    for profile in &mux_profiles {
        let caps = TerminalCapabilities::from_profile(*profile);
        assert!(
            !caps.use_sync_output(),
            "DEG-1: {profile:?} should disable sync output via policy"
        );
        logger.log_invariant("DEG-1", true, &format!("{profile:?}_no_sync"));
    }

    // Non-mux profiles with sync_output=true should allow it
    let modern = TerminalCapabilities::modern();
    assert!(
        modern.use_sync_output(),
        "DEG-1: modern (non-mux) should allow sync output"
    );

    logger.log_complete(true, mux_profiles.len() + 1);
}

#[test]
fn capability_sim_mux_scroll_region_policy() {
    let mut logger = CapSimLogger::new("mux_scroll_region");

    let mux_profiles = [
        TerminalProfile::Tmux,
        TerminalProfile::Screen,
        TerminalProfile::Zellij,
    ];

    for profile in &mux_profiles {
        let caps = TerminalCapabilities::from_profile(*profile);
        // Mux profiles should disable scroll region via use_scroll_region()
        assert!(
            !caps.use_scroll_region(),
            "DEG-1: {profile:?} should disable scroll region via policy"
        );
    }

    // Modern non-mux should allow scroll region
    let modern = TerminalCapabilities::modern();
    assert!(
        modern.use_scroll_region(),
        "DEG-1: modern should allow scroll region"
    );

    logger.log_invariant("DEG-1", true, "mux_scroll_region_policy");
    logger.log_complete(true, mux_profiles.len() + 1);
}

#[test]
fn capability_sim_mux_detection_flags() {
    let mut logger = CapSimLogger::new("mux_detection");

    let cases = [
        (TerminalProfile::Tmux, true, false, false),
        (TerminalProfile::Screen, false, true, false),
        (TerminalProfile::Zellij, false, false, true),
        (TerminalProfile::Modern, false, false, false),
        (TerminalProfile::Dumb, false, false, false),
    ];

    for (profile, tmux, screen, zellij) in &cases {
        let caps = TerminalCapabilities::from_profile(*profile);
        assert_eq!(caps.in_tmux, *tmux, "QUIRK-1: {profile:?} in_tmux");
        assert_eq!(caps.in_screen, *screen, "QUIRK-1: {profile:?} in_screen");
        assert_eq!(caps.in_zellij, *zellij, "QUIRK-1: {profile:?} in_zellij");
        assert_eq!(
            caps.in_any_mux(),
            *tmux || *screen || *zellij || caps.in_wezterm_mux,
            "QUIRK-1: {profile:?} in_any_mux"
        );
        logger.log_invariant("QUIRK-1", true, &format!("{profile:?}_mux_flags"));
    }

    logger.log_complete(true, cases.len());
}

// ============================================================================
// QUIRK-1: Multiplexer Quirk Simulation
// ============================================================================

#[test]
fn capability_sim_tmux_disables_advanced_features() {
    let mut logger = CapSimLogger::new("tmux_quirks");

    let caps = TerminalCapabilities::tmux();

    // tmux limitations
    assert!(
        !caps.sync_output,
        "QUIRK-1: tmux no sync (passthrough unreliable)"
    );
    assert!(!caps.osc8_hyperlinks, "QUIRK-1: tmux no hyperlinks");
    assert!(!caps.kitty_keyboard, "QUIRK-1: tmux no kitty keyboard");
    assert!(!caps.focus_events, "QUIRK-1: tmux no focus events");
    assert!(!caps.osc52_clipboard, "QUIRK-1: tmux no clipboard");
    assert!(!caps.true_color, "QUIRK-1: tmux no true color by default");

    // tmux does support some features
    assert!(caps.colors_256, "QUIRK-1: tmux has 256 colors");
    assert!(caps.scroll_region, "QUIRK-1: tmux has scroll region (raw)");
    assert!(caps.bracketed_paste, "QUIRK-1: tmux has bracketed paste");
    assert!(caps.mouse_sgr, "QUIRK-1: tmux has mouse sgr");

    logger.log_invariant("QUIRK-1", true, "tmux_quirks_verified");
    logger.log_complete(true, 10);
}

#[test]
fn capability_sim_screen_quirks() {
    let mut logger = CapSimLogger::new("screen_quirks");

    let caps = TerminalCapabilities::screen();

    assert!(caps.in_screen);
    assert!(!caps.sync_output, "QUIRK-1: screen no sync output");
    assert!(!caps.osc8_hyperlinks, "QUIRK-1: screen no hyperlinks");
    assert!(!caps.true_color, "QUIRK-1: screen no true color");
    assert!(caps.colors_256, "QUIRK-1: screen has 256 colors");

    logger.log_invariant("QUIRK-1", true, "screen_quirks");
    logger.log_complete(true, 5);
}

#[test]
fn capability_sim_zellij_quirks() {
    let mut logger = CapSimLogger::new("zellij_quirks");

    let caps = TerminalCapabilities::zellij();

    // Zellij is more capable than tmux/screen
    assert!(caps.in_zellij);
    assert!(caps.true_color, "QUIRK-1: zellij supports true color");
    assert!(caps.focus_events, "QUIRK-1: zellij supports focus events");
    assert!(!caps.sync_output, "QUIRK-1: zellij still no sync output");
    assert!(!caps.osc8_hyperlinks, "QUIRK-1: zellij no hyperlinks");
    assert!(!caps.kitty_keyboard, "QUIRK-1: zellij no kitty keyboard");

    logger.log_invariant("QUIRK-1", true, "zellij_better_than_tmux");
    logger.log_complete(true, 6);
}

#[test]
fn capability_sim_windows_console_quirks() {
    let mut logger = CapSimLogger::new("windows_quirks");

    let caps = TerminalCapabilities::windows_console();

    assert!(caps.true_color, "QUIRK-1: windows has true color");
    assert!(caps.osc8_hyperlinks, "QUIRK-1: windows has hyperlinks");
    assert!(caps.focus_events, "QUIRK-1: windows has focus events");
    assert!(!caps.sync_output, "QUIRK-1: windows no sync output");
    assert!(!caps.kitty_keyboard, "QUIRK-1: windows no kitty keyboard");

    logger.log_invariant("QUIRK-1", true, "windows_console_quirks");
    logger.log_complete(true, 5);
}

#[test]
fn capability_sim_linux_console_quirks() {
    let mut logger = CapSimLogger::new("linux_console_quirks");

    let caps = TerminalCapabilities::linux_console();

    assert!(!caps.true_color, "QUIRK-1: linux console no true color");
    assert!(!caps.colors_256, "QUIRK-1: linux console no 256 colors");
    assert!(!caps.has_color(), "QUIRK-1: linux console no color");
    assert!(
        caps.scroll_region,
        "QUIRK-1: linux console has scroll region"
    );
    assert!(caps.bracketed_paste, "QUIRK-1: linux console has paste");
    assert!(caps.mouse_sgr, "QUIRK-1: linux console has mouse");

    logger.log_invariant("QUIRK-1", true, "linux_console_no_color");
    logger.log_complete(true, 6);
}

// ============================================================================
// INT-1: Profile Matrix Integration
// ============================================================================

#[test]
fn capability_sim_profile_matrix_deterministic() {
    let mut logger = CapSimLogger::new("matrix_deterministic");

    // Generate capability snapshots for all profiles twice
    let snapshots1: Vec<(TerminalProfile, String)> = ALL_PROFILES
        .iter()
        .map(|p| (*p, caps_summary(&TerminalCapabilities::from_profile(*p))))
        .collect();

    let snapshots2: Vec<(TerminalProfile, String)> = ALL_PROFILES
        .iter()
        .map(|p| (*p, caps_summary(&TerminalCapabilities::from_profile(*p))))
        .collect();

    assert_eq!(
        snapshots1, snapshots2,
        "INT-1: profile matrix should be deterministic"
    );

    logger.log_invariant("INT-1", true, "matrix_deterministic");
    logger.log_complete(true, 1);
}

#[test]
fn capability_sim_profile_matrix_uniqueness() {
    let mut logger = CapSimLogger::new("matrix_uniqueness");

    // Most profiles should produce distinct capability sets
    // (some might overlap, but their profile identifiers differ)
    let caps_list: Vec<(TerminalProfile, TerminalCapabilities)> = ALL_PROFILES
        .iter()
        .map(|p| (*p, TerminalCapabilities::from_profile(*p)))
        .collect();

    // Profile identifiers must be unique
    for i in 0..caps_list.len() {
        for j in (i + 1)..caps_list.len() {
            assert_ne!(
                caps_list[i].0, caps_list[j].0,
                "INT-1: profile identifiers should be unique"
            );
        }
    }

    logger.log_invariant("INT-1", true, "profile_uniqueness");
    logger.log_complete(true, 1);
}

// ============================================================================
// INT-2: Cross-Profile Capability Comparison
// ============================================================================

#[test]
fn capability_sim_cross_profile_feature_hierarchy() {
    let mut logger = CapSimLogger::new("feature_hierarchy");

    let dumb = TerminalCapabilities::dumb();
    let vt100 = TerminalCapabilities::vt100();
    let xterm = TerminalCapabilities::xterm();
    let xterm256 = TerminalCapabilities::xterm_256color();
    let modern = TerminalCapabilities::modern();

    // Feature count should generally increase up the hierarchy
    let dumb_features = count_features(&dumb);
    let vt100_features = count_features(&vt100);
    let xterm_features = count_features(&xterm);
    let xterm256_features = count_features(&xterm256);
    let modern_features = count_features(&modern);

    assert!(
        dumb_features <= vt100_features,
        "INT-2: dumb ({dumb_features}) <= vt100 ({vt100_features})"
    );
    assert!(
        vt100_features <= xterm_features,
        "INT-2: vt100 ({vt100_features}) <= xterm ({xterm_features})"
    );
    assert!(
        xterm_features <= xterm256_features,
        "INT-2: xterm ({xterm_features}) <= xterm256 ({xterm256_features})"
    );
    assert!(
        xterm256_features <= modern_features,
        "INT-2: xterm256 ({xterm256_features}) <= modern ({modern_features})"
    );

    logger.log_invariant("INT-2", true, "feature_hierarchy");
    logger.log_complete(true, 4);
}

fn count_features(caps: &TerminalCapabilities) -> u32 {
    let mut count = 0u32;
    if caps.true_color {
        count += 1;
    }
    if caps.colors_256 {
        count += 1;
    }
    if caps.sync_output {
        count += 1;
    }
    if caps.osc8_hyperlinks {
        count += 1;
    }
    if caps.scroll_region {
        count += 1;
    }
    if caps.kitty_keyboard {
        count += 1;
    }
    if caps.focus_events {
        count += 1;
    }
    if caps.bracketed_paste {
        count += 1;
    }
    if caps.mouse_sgr {
        count += 1;
    }
    if caps.osc52_clipboard {
        count += 1;
    }
    count
}

#[test]
fn capability_sim_mux_feature_subset() {
    let mut logger = CapSimLogger::new("mux_feature_subset");

    // Mux profiles should have fewer effective features than modern
    let modern = TerminalCapabilities::modern();
    let modern_effective = count_effective_features(&modern);

    let mux_profiles = [
        TerminalProfile::Tmux,
        TerminalProfile::Screen,
        TerminalProfile::Zellij,
    ];

    for profile in &mux_profiles {
        let caps = TerminalCapabilities::from_profile(*profile);
        let effective = count_effective_features(&caps);
        assert!(
            effective <= modern_effective,
            "INT-2: {profile:?} effective features ({effective}) should <= modern ({modern_effective})"
        );
        logger.log_invariant("INT-2", true, &format!("{profile:?}_effective={effective}"));
    }

    logger.log_complete(true, mux_profiles.len());
}

fn count_effective_features(caps: &TerminalCapabilities) -> u32 {
    let mut count = 0u32;
    if caps.true_color {
        count += 1;
    }
    if caps.colors_256 {
        count += 1;
    }
    if caps.use_sync_output() {
        count += 1;
    }
    if caps.osc8_hyperlinks {
        count += 1;
    }
    if caps.use_scroll_region() {
        count += 1;
    }
    if caps.kitty_keyboard {
        count += 1;
    }
    if caps.focus_events {
        count += 1;
    }
    if caps.bracketed_paste {
        count += 1;
    }
    if caps.mouse_sgr {
        count += 1;
    }
    if caps.osc52_clipboard {
        count += 1;
    }
    count
}

// ============================================================================
// Builder Pattern Tests
// ============================================================================

#[test]
fn capability_sim_builder_custom_profile() {
    let mut logger = CapSimLogger::new("builder_custom");

    let caps = TerminalCapabilities::builder()
        .colors_256(true)
        .true_color(true)
        .mouse_sgr(true)
        .bracketed_paste(true)
        .build();

    assert!(caps.true_color);
    assert!(caps.colors_256);
    assert!(caps.mouse_sgr);
    assert!(caps.bracketed_paste);
    // Everything else should be default (false)
    assert!(!caps.sync_output);
    assert!(!caps.osc8_hyperlinks);
    assert!(!caps.kitty_keyboard);
    assert_eq!(caps.profile(), TerminalProfile::Custom);

    logger.log_invariant("PROF-1", true, "builder_custom");
    logger.log_complete(true, 8);
}

// ============================================================================
// Override + Profile Composition
// ============================================================================

#[test]
fn capability_sim_override_on_each_profile() {
    let mut logger = CapSimLogger::new("override_all_profiles");

    // Apply a selective override on each base profile
    let override_cfg = CapabilityOverride::new()
        .true_color(Some(true))
        .mouse_sgr(Some(false));

    for profile in &ALL_PROFILES {
        let base = TerminalCapabilities::from_profile(*profile);
        let result = override_cfg.apply_to(base);

        assert!(
            result.true_color,
            "Override should enable true_color on {profile:?}"
        );
        assert!(
            !result.mouse_sgr,
            "Override should disable mouse_sgr on {profile:?}"
        );

        // Non-overridden fields should match base
        assert_eq!(
            result.sync_output, base.sync_output,
            "Non-overridden sync_output should match base for {profile:?}"
        );
        assert_eq!(
            result.colors_256, base.colors_256,
            "Non-overridden colors_256 should match base for {profile:?}"
        );

        logger.log_invariant("OVER-1", true, &format!("override_on_{profile:?}"));
    }

    logger.log_complete(true, ALL_PROFILES.len());
}

// ============================================================================
// Mux Override Integration
// ============================================================================

#[test]
fn capability_sim_mux_override_chain() {
    let mut logger = CapSimLogger::new("mux_override_chain");

    // Modern base -> tmux override -> verify policies
    let base = TerminalCapabilities::modern();
    let tmux_over = CapabilityOverride::tmux();
    let result = tmux_over.apply_to(base);

    // Should be in tmux
    assert!(result.in_tmux);

    // Policies should gate features
    assert!(!result.use_sync_output(), "DEG-1: mux disables sync policy");
    assert!(
        !result.use_scroll_region(),
        "DEG-1: mux disables scroll policy"
    );

    // But the raw flags might differ from policy
    // (scroll_region raw = true from tmux override, but policy = false)
    assert!(result.scroll_region, "Raw scroll_region is true");

    logger.log_invariant("DEG-1", true, "mux_override_chain");
    logger.log_complete(true, 4);
}

// ============================================================================
// Suite Summary
// ============================================================================

#[test]
fn capability_sim_suite_summary() {
    let mut logger = CapSimLogger::new("suite_summary");

    let mut total_checks = 0u32;

    // Verify all profiles can be instantiated
    for profile in &ALL_PROFILES {
        let caps = TerminalCapabilities::from_profile(*profile);
        let _summary = caps_summary(&caps);
        total_checks += 1;
    }

    // Verify override round-trip on all profiles
    for profile in &ALL_PROFILES {
        let base = TerminalCapabilities::from_profile(*profile);
        let result = CapabilityOverride::new().apply_to(base);
        assert_eq!(base, result, "Empty override is identity for {profile:?}");
        total_checks += 1;
    }

    // Verify dumb -> modern -> dumb round-trip
    let base = TerminalCapabilities::dumb();
    let upgraded = CapabilityOverride::modern().apply_to(base);
    let downgraded = CapabilityOverride::dumb().apply_to(upgraded);
    // downgraded should match dumb except profile field
    assert!(!downgraded.true_color);
    assert!(!downgraded.has_color());
    total_checks += 2;

    logger.log_event(
        "summary",
        &format!(
            r#"{{"profiles_tested":{},"total_checks":{}}}"#,
            ALL_PROFILES.len(),
            total_checks
        ),
    );
    logger.log_complete(true, total_checks as usize);
}
