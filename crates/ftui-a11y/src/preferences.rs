//! Accessibility preference flags and the deterministic behavior policies
//! derived from them (reduced motion, high contrast, forced colors).
//!
//! # Why this exists
//!
//! Assistive-technology hosts (the FrankenTermJS web surface, the native
//! terminal surface, and the demo showcase) all need to honour the same
//! operating-system / browser accessibility signals:
//!
//! - **reduced motion** (`prefers-reduced-motion`): collapse animation and
//!   suppress "motion-like" assistive-tech churn (e.g. a progress bar that
//!   updates 60×/second should not interrupt the screen reader 60×/second).
//! - **high contrast** (`prefers-contrast: more`): raise the minimum colour
//!   contrast the host honours and drop dim/faint attributes.
//! - **forced colors** (`forced-colors: active`): ignore arbitrary theme
//!   colours and defer entirely to the system palette.
//!
//! This module is the **single host-agnostic source of truth** for those
//! flags. It is pure and deterministic: identical inputs always produce
//! identical [`MotionProfile`] / [`ContrastProfile`] derivations and identical
//! host serialisations, so behaviour is reproducible for a fixed capability
//! profile (the invariant `bd-2vr05.7.4` requires).
//!
//! ```text
//!  prefers-reduced-motion ─┐
//!  prefers-contrast        ├─> AccessibilityPreferences ─┬─> MotionProfile  ─> announcement coalescing + animation scale
//!  forced-colors           ─┘                            ├─> ContrastProfile ─> min contrast / drop-dim / system-colors
//!                                                        ├─> web_dataset()   ─> CSS data-attributes (browser host)
//!                                                        └─> to_jsonl()      ─> deterministic structured-log line
//! ```
//!
//! The motion policy feeds back into the *state* channel too:
//! [`MotionProfile::filter_announcements`] applies reduced-motion coalescing to
//! the bounded screen-reader announcement batch produced by
//! [`crate::tree::A11yTreeDiff::screen_reader_announcements`].

use crate::node::LiveRegion;
use crate::tree::{AnnouncementReason, ScreenReaderAnnouncement, ScreenReaderAnnouncements};

/// Host-agnostic accessibility mode flags.
///
/// All fields default to `false` (no special accommodation), matching the
/// browser default when no media query matches. Construct from raw media-query
/// booleans via [`from_media_queries`](Self::from_media_queries) so the mapping
/// stays in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct AccessibilityPreferences {
    /// `prefers-reduced-motion: reduce` — collapse animation and coalesce
    /// motion-like assistive-tech announcements.
    pub reduced_motion: bool,
    /// `prefers-contrast: more` — raise the minimum colour contrast and drop
    /// dim/faint attributes.
    pub high_contrast: bool,
    /// `forced-colors: active` — ignore arbitrary theme colours and use the
    /// system palette only. Implies high-contrast behaviour for the visual
    /// channel.
    pub forced_colors: bool,
}

impl AccessibilityPreferences {
    /// No accommodations requested (all flags `false`).
    #[inline]
    #[must_use]
    pub const fn none() -> Self {
        Self {
            reduced_motion: false,
            high_contrast: false,
            forced_colors: false,
        }
    }

    /// Every accommodation enabled.
    #[inline]
    #[must_use]
    pub const fn all() -> Self {
        Self {
            reduced_motion: true,
            high_contrast: true,
            forced_colors: true,
        }
    }

    /// Resolve preferences from raw browser/OS media-query booleans.
    ///
    /// This is the canonical, deterministic mapping a host should use after
    /// reading `window.matchMedia(...)`. `forced_colors` is treated as a
    /// strict superset of `high_contrast` for the visual channel, so a host
    /// that only reports `forced-colors: active` still gets high-contrast
    /// behaviour via the derived [`ContrastProfile`].
    #[inline]
    #[must_use]
    pub const fn from_media_queries(
        prefers_reduced_motion: bool,
        prefers_contrast_more: bool,
        forced_colors_active: bool,
    ) -> Self {
        Self {
            reduced_motion: prefers_reduced_motion,
            high_contrast: prefers_contrast_more,
            forced_colors: forced_colors_active,
        }
    }

    /// Builder-style setter for reduced motion.
    #[inline]
    #[must_use]
    pub const fn with_reduced_motion(mut self, on: bool) -> Self {
        self.reduced_motion = on;
        self
    }

    /// Builder-style setter for high contrast.
    #[inline]
    #[must_use]
    pub const fn with_high_contrast(mut self, on: bool) -> Self {
        self.high_contrast = on;
        self
    }

    /// Builder-style setter for forced colors.
    #[inline]
    #[must_use]
    pub const fn with_forced_colors(mut self, on: bool) -> Self {
        self.forced_colors = on;
        self
    }

    /// `true` if any accommodation is active.
    #[inline]
    #[must_use]
    pub const fn any(self) -> bool {
        self.reduced_motion || self.high_contrast || self.forced_colors
    }

    /// Derive the deterministic motion policy.
    #[inline]
    #[must_use]
    pub const fn motion_profile(self) -> MotionProfile {
        MotionProfile::from_preferences(self)
    }

    /// Derive the deterministic contrast policy.
    #[inline]
    #[must_use]
    pub const fn contrast_profile(self) -> ContrastProfile {
        ContrastProfile::from_preferences(self)
    }

    /// Stable, ordered CSS data-attribute pairs for the browser host.
    ///
    /// The order is fixed (`reduced-motion`, `high-contrast`, `forced-colors`)
    /// so generated DOM markup is byte-for-byte reproducible. A host applies
    /// each pair as `element.dataset[...] = "true" | "false"` and selects on
    /// `[data-ftui-reduced-motion="true"]` in CSS. This mirrors the pane
    /// surface's `accessibility_dataset()` convention.
    #[inline]
    #[must_use]
    pub const fn web_dataset(self) -> [(&'static str, bool); 3] {
        [
            ("data-ftui-reduced-motion", self.reduced_motion),
            ("data-ftui-high-contrast", self.high_contrast),
            ("data-ftui-forced-colors", self.forced_colors),
        ]
    }

    /// Serialise this preference set and its derived policies to a single,
    /// deterministic JSON line for structured-log triage.
    ///
    /// `correlation_id` is escaped for JSON safety. No allocation-heavy
    /// serialisation framework is used so the output is stable across builds.
    #[must_use]
    pub fn to_jsonl(self, correlation_id: &str) -> String {
        let motion = self.motion_profile();
        let contrast = self.contrast_profile();
        format!(
            concat!(
                "{{\"event\":\"a11y_preferences\",\"correlation_id\":\"{cid}\",",
                "\"reduced_motion\":{rm},\"high_contrast\":{hc},\"forced_colors\":{fc},",
                "\"animation_scale_bps\":{asb},\"announcement_coalescing\":{ac},",
                "\"suppress_transient\":{st},\"min_contrast_ratio_x100\":{mcr},",
                "\"drop_dim\":{dd},\"system_colors_only\":{sco}}}"
            ),
            cid = escape_json(correlation_id),
            rm = self.reduced_motion,
            hc = self.high_contrast,
            fc = self.forced_colors,
            asb = motion.animation_scale_bps,
            ac = motion.announcement_coalescing,
            st = motion.suppress_transient,
            mcr = contrast.min_contrast_ratio_x100,
            dd = contrast.drop_dim,
            sco = contrast.system_colors_only,
        )
    }
}

/// Deterministic motion policy derived from [`AccessibilityPreferences`].
///
/// Carries both the *visual* lever (animation scale) and the *assistive-tech*
/// lever (announcement coalescing) so a single policy object covers both
/// rendering channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MotionProfile {
    /// Whether reduced motion is active.
    pub reduced_motion: bool,
    /// Animation duration scale in basis points (`10_000` = full duration,
    /// `0` = instant). Hosts multiply their base durations by this / 10_000.
    pub animation_scale_bps: u16,
    /// Whether repeated live-content announcements on the same node should be
    /// coalesced to the latest value (so motion-like churn does not flood the
    /// screen reader).
    pub announcement_coalescing: bool,
    /// Whether transient (interrupting) live-content announcements should be
    /// downgraded to polite so motion does not interrupt the user.
    pub suppress_transient: bool,
}

impl MotionProfile {
    /// Full motion: no scaling, no coalescing, no downgrading.
    pub const FULL: Self = Self {
        reduced_motion: false,
        animation_scale_bps: 10_000,
        announcement_coalescing: false,
        suppress_transient: false,
    };

    /// Derive from a preference set.
    #[inline]
    #[must_use]
    pub const fn from_preferences(prefs: AccessibilityPreferences) -> Self {
        if prefs.reduced_motion {
            Self {
                reduced_motion: true,
                animation_scale_bps: 0,
                announcement_coalescing: true,
                suppress_transient: true,
            }
        } else {
            Self::FULL
        }
    }

    /// Scale a base animation duration (milliseconds) by the motion policy,
    /// saturating. Returns `0` when reduced motion is active.
    #[inline]
    #[must_use]
    pub const fn scaled_duration_ms(self, base_ms: u32) -> u32 {
        ((base_ms as u64 * self.animation_scale_bps as u64) / 10_000) as u32
    }

    /// Apply reduced-motion coalescing to a bounded announcement batch.
    ///
    /// When [`announcement_coalescing`](Self::announcement_coalescing) is off
    /// the input is returned verbatim (zero coalesced / downgraded). When on:
    ///
    /// - **Focus** and **live-region-policy** announcements are preserved as-is
    ///   (they are intentional, not motion).
    /// - **Live-content** / **live-region-added** announcements (the
    ///   motion-like churn) are coalesced per node ID, keeping only the *latest*
    ///   occurrence, and — when
    ///   [`suppress_transient`](Self::suppress_transient) is on — any
    ///   assertive urgency is downgraded to polite.
    ///
    /// Output order is preserved from the (already deterministic) input, so the
    /// result is itself deterministic.
    #[must_use]
    pub fn filter_announcements(
        self,
        input: &ScreenReaderAnnouncements,
    ) -> MotionFilteredAnnouncements {
        if !self.announcement_coalescing {
            return MotionFilteredAnnouncements {
                announcements: input.announcements.clone(),
                dropped_count: input.dropped_count,
                coalesced_count: 0,
                downgraded_count: 0,
            };
        }

        // Determine, for each coalescable node ID, the index of its LAST
        // motion-like announcement. Earlier occurrences are dropped.
        let mut last_index_for_node: std::collections::HashMap<u64, usize> =
            std::collections::HashMap::new();
        for (idx, ann) in input.announcements.iter().enumerate() {
            if is_motion_like(ann.reason)
                && let Some(node_id) = ann.node_id
            {
                last_index_for_node.insert(node_id, idx);
            }
        }

        let mut announcements = Vec::with_capacity(input.announcements.len());
        let mut coalesced_count = 0usize;
        let mut downgraded_count = 0usize;

        for (idx, ann) in input.announcements.iter().enumerate() {
            if is_motion_like(ann.reason) {
                if let Some(node_id) = ann.node_id {
                    // Keep only the latest motion-like announcement per node.
                    if last_index_for_node.get(&node_id) != Some(&idx) {
                        coalesced_count += 1;
                        continue;
                    }
                }
                let mut kept = ann.clone();
                if self.suppress_transient && kept.urgency == LiveRegion::Assertive {
                    kept.urgency = LiveRegion::Polite;
                    downgraded_count += 1;
                }
                announcements.push(kept);
            } else {
                announcements.push(ann.clone());
            }
        }

        MotionFilteredAnnouncements {
            announcements,
            dropped_count: input.dropped_count,
            coalesced_count,
            downgraded_count,
        }
    }
}

/// Result of applying a [`MotionProfile`] to a screen-reader announcement
/// batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MotionFilteredAnnouncements {
    /// Announcements retained after motion filtering.
    pub announcements: Vec<ScreenReaderAnnouncement>,
    /// Announcements already dropped by the upstream batch cap (carried
    /// through unchanged).
    pub dropped_count: usize,
    /// Motion-like announcements removed by reduced-motion coalescing.
    pub coalesced_count: usize,
    /// Announcements whose urgency was downgraded assertive → polite.
    pub downgraded_count: usize,
}

/// `true` for announcement reasons that represent motion-like content churn
/// (subject to reduced-motion coalescing).
#[inline]
const fn is_motion_like(reason: AnnouncementReason) -> bool {
    matches!(
        reason,
        AnnouncementReason::LiveContentChanged | AnnouncementReason::LiveRegionAdded
    )
}

/// Deterministic contrast policy derived from [`AccessibilityPreferences`].
///
/// All values are host-agnostic *intent*; the actual colour swaps happen in
/// the host (terminal ANSI or browser CSS), which keeps `ftui-a11y` free of any
/// dependency on `ftui-style`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContrastProfile {
    /// Whether any elevated-contrast mode is active.
    pub high_contrast: bool,
    /// Whether system colours must be used exclusively (`forced-colors`).
    pub forced_colors: bool,
    /// Minimum WCAG contrast ratio the host should honour, ×100
    /// (`450` = 4.5:1 AA, `700` = 7.0:1 AAA).
    pub min_contrast_ratio_x100: u16,
    /// Whether dim / faint text attributes should be dropped (they reduce
    /// contrast).
    pub drop_dim: bool,
    /// Whether arbitrary theme colours must be ignored in favour of the system
    /// palette.
    pub system_colors_only: bool,
}

impl ContrastProfile {
    /// WCAG 2.x AA minimum contrast for normal text, ×100.
    pub const WCAG_AA_X100: u16 = 450;
    /// WCAG 2.x AAA minimum contrast for normal text, ×100.
    pub const WCAG_AAA_X100: u16 = 700;

    /// Default (no elevated contrast): AA baseline, keep dim, keep theme.
    pub const DEFAULT: Self = Self {
        high_contrast: false,
        forced_colors: false,
        min_contrast_ratio_x100: Self::WCAG_AA_X100,
        drop_dim: false,
        system_colors_only: false,
    };

    /// Derive from a preference set.
    #[inline]
    #[must_use]
    pub const fn from_preferences(prefs: AccessibilityPreferences) -> Self {
        let elevated = prefs.high_contrast || prefs.forced_colors;
        Self {
            high_contrast: elevated,
            forced_colors: prefs.forced_colors,
            min_contrast_ratio_x100: if elevated {
                Self::WCAG_AAA_X100
            } else {
                Self::WCAG_AA_X100
            },
            drop_dim: elevated,
            system_colors_only: prefs.forced_colors,
        }
    }
}

/// Escape a string for embedding inside a JSON double-quoted value.
fn escape_json(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 2);
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::AnnouncementReason;

    fn ann(
        node_id: u64,
        urgency: LiveRegion,
        reason: AnnouncementReason,
        text: &str,
    ) -> ScreenReaderAnnouncement {
        ScreenReaderAnnouncement {
            node_id: Some(node_id),
            urgency,
            reason,
            text: text.to_owned(),
        }
    }

    // ── Preference construction & builders ──────────────────────────────

    #[test]
    fn none_is_all_false() {
        let p = AccessibilityPreferences::none();
        assert!(!p.reduced_motion);
        assert!(!p.high_contrast);
        assert!(!p.forced_colors);
        assert!(!p.any());
        assert_eq!(p, AccessibilityPreferences::default());
    }

    #[test]
    fn all_is_all_true() {
        let p = AccessibilityPreferences::all();
        assert!(p.reduced_motion && p.high_contrast && p.forced_colors);
        assert!(p.any());
    }

    #[test]
    fn builders_compose() {
        let p = AccessibilityPreferences::none()
            .with_reduced_motion(true)
            .with_high_contrast(true)
            .with_forced_colors(false);
        assert!(p.reduced_motion);
        assert!(p.high_contrast);
        assert!(!p.forced_colors);
        assert!(p.any());
    }

    #[test]
    fn from_media_queries_maps_directly() {
        let p = AccessibilityPreferences::from_media_queries(true, false, true);
        assert!(p.reduced_motion);
        assert!(!p.high_contrast);
        assert!(p.forced_colors);
    }

    // ── Motion profile derivation ───────────────────────────────────────

    #[test]
    fn full_motion_when_not_reduced() {
        let m = AccessibilityPreferences::none().motion_profile();
        assert_eq!(m, MotionProfile::FULL);
        assert!(!m.reduced_motion);
        assert_eq!(m.animation_scale_bps, 10_000);
        assert!(!m.announcement_coalescing);
        assert!(!m.suppress_transient);
    }

    #[test]
    fn reduced_motion_collapses_everything() {
        let m = AccessibilityPreferences::none()
            .with_reduced_motion(true)
            .motion_profile();
        assert!(m.reduced_motion);
        assert_eq!(m.animation_scale_bps, 0);
        assert!(m.announcement_coalescing);
        assert!(m.suppress_transient);
    }

    #[test]
    fn high_contrast_alone_does_not_change_motion() {
        let m = AccessibilityPreferences::none()
            .with_high_contrast(true)
            .with_forced_colors(true)
            .motion_profile();
        assert_eq!(m, MotionProfile::FULL);
    }

    #[test]
    fn scaled_duration_is_instant_under_reduced_motion() {
        let m = MotionProfile::from_preferences(
            AccessibilityPreferences::none().with_reduced_motion(true),
        );
        assert_eq!(m.scaled_duration_ms(250), 0);
        assert_eq!(m.scaled_duration_ms(0), 0);
        assert_eq!(m.scaled_duration_ms(u32::MAX), 0);
    }

    #[test]
    fn scaled_duration_is_identity_under_full_motion() {
        let m = MotionProfile::FULL;
        assert_eq!(m.scaled_duration_ms(250), 250);
        assert_eq!(m.scaled_duration_ms(0), 0);
        // No overflow at the high end (uses u64 intermediate).
        assert_eq!(m.scaled_duration_ms(1_000_000), 1_000_000);
    }

    // ── Contrast profile derivation ─────────────────────────────────────

    #[test]
    fn default_contrast_is_aa_baseline() {
        let c = AccessibilityPreferences::none().contrast_profile();
        assert_eq!(c, ContrastProfile::DEFAULT);
        assert!(!c.high_contrast);
        assert_eq!(c.min_contrast_ratio_x100, ContrastProfile::WCAG_AA_X100);
        assert!(!c.drop_dim);
        assert!(!c.system_colors_only);
    }

    #[test]
    fn high_contrast_raises_to_aaa_and_drops_dim() {
        let c = AccessibilityPreferences::none()
            .with_high_contrast(true)
            .contrast_profile();
        assert!(c.high_contrast);
        assert_eq!(c.min_contrast_ratio_x100, ContrastProfile::WCAG_AAA_X100);
        assert!(c.drop_dim);
        assert!(!c.system_colors_only); // high contrast != forced colors
    }

    #[test]
    fn forced_colors_implies_high_contrast_and_system_palette() {
        let c = AccessibilityPreferences::none()
            .with_forced_colors(true)
            .contrast_profile();
        assert!(c.high_contrast); // forced colors is a superset
        assert!(c.forced_colors);
        assert!(c.system_colors_only);
        assert!(c.drop_dim);
        assert_eq!(c.min_contrast_ratio_x100, ContrastProfile::WCAG_AAA_X100);
    }

    #[test]
    fn reduced_motion_alone_does_not_change_contrast() {
        let c = AccessibilityPreferences::none()
            .with_reduced_motion(true)
            .contrast_profile();
        assert_eq!(c, ContrastProfile::DEFAULT);
    }

    // ── Web dataset ─────────────────────────────────────────────────────

    #[test]
    fn web_dataset_is_stable_and_ordered() {
        let ds = AccessibilityPreferences::all().web_dataset();
        assert_eq!(
            ds,
            [
                ("data-ftui-reduced-motion", true),
                ("data-ftui-high-contrast", true),
                ("data-ftui-forced-colors", true),
            ]
        );
        let ds_none = AccessibilityPreferences::none().web_dataset();
        assert!(ds_none.iter().all(|(_, v)| !v));
        // Keys never change regardless of values.
        for ((k_all, _), (k_none, _)) in ds.iter().zip(ds_none.iter()) {
            assert_eq!(k_all, k_none);
        }
    }

    // ── Structured logging ──────────────────────────────────────────────

    #[test]
    fn jsonl_is_deterministic_and_complete() {
        let p = AccessibilityPreferences::all();
        let line = p.to_jsonl("corr-1");
        // Same inputs → identical line.
        assert_eq!(line, p.to_jsonl("corr-1"));
        assert!(line.starts_with('{') && line.ends_with('}'));
        assert!(line.contains("\"correlation_id\":\"corr-1\""));
        assert!(line.contains("\"reduced_motion\":true"));
        assert!(line.contains("\"animation_scale_bps\":0"));
        assert!(line.contains("\"min_contrast_ratio_x100\":700"));
        assert!(line.contains("\"system_colors_only\":true"));
        assert!(!line.contains('\n'));
    }

    #[test]
    fn jsonl_escapes_correlation_id() {
        let line = AccessibilityPreferences::none().to_jsonl("a\"b\\c\nd");
        assert!(line.contains(r#""correlation_id":"a\"b\\c\nd""#));
        assert!(!line[1..].contains('\n'));
    }

    // ── Announcement filtering ──────────────────────────────────────────

    #[test]
    fn full_motion_passes_announcements_through() {
        let batch = ScreenReaderAnnouncements {
            announcements: vec![
                ann(
                    1,
                    LiveRegion::Polite,
                    AnnouncementReason::FocusChanged,
                    "button: ok",
                ),
                ann(
                    2,
                    LiveRegion::Assertive,
                    AnnouncementReason::LiveContentChanged,
                    "progress 10%",
                ),
            ],
            dropped_count: 3,
        };
        let out = MotionProfile::FULL.filter_announcements(&batch);
        assert_eq!(out.announcements, batch.announcements);
        assert_eq!(out.dropped_count, 3);
        assert_eq!(out.coalesced_count, 0);
        assert_eq!(out.downgraded_count, 0);
    }

    #[test]
    fn reduced_motion_coalesces_same_node_to_latest() {
        let m = AccessibilityPreferences::none()
            .with_reduced_motion(true)
            .motion_profile();
        let batch = ScreenReaderAnnouncements {
            announcements: vec![
                ann(
                    5,
                    LiveRegion::Polite,
                    AnnouncementReason::LiveContentChanged,
                    "progress 10%",
                ),
                ann(
                    5,
                    LiveRegion::Polite,
                    AnnouncementReason::LiveContentChanged,
                    "progress 50%",
                ),
                ann(
                    5,
                    LiveRegion::Polite,
                    AnnouncementReason::LiveContentChanged,
                    "progress 90%",
                ),
            ],
            dropped_count: 0,
        };
        let out = m.filter_announcements(&batch);
        assert_eq!(out.announcements.len(), 1);
        assert_eq!(out.announcements[0].text, "progress 90%"); // latest wins
        assert_eq!(out.coalesced_count, 2);
    }

    #[test]
    fn reduced_motion_downgrades_assertive_churn_to_polite() {
        let m = AccessibilityPreferences::none()
            .with_reduced_motion(true)
            .motion_profile();
        let batch = ScreenReaderAnnouncements {
            announcements: vec![ann(
                7,
                LiveRegion::Assertive,
                AnnouncementReason::LiveContentChanged,
                "loading…",
            )],
            dropped_count: 0,
        };
        let out = m.filter_announcements(&batch);
        assert_eq!(out.announcements.len(), 1);
        assert_eq!(out.announcements[0].urgency, LiveRegion::Polite);
        assert_eq!(out.downgraded_count, 1);
        assert_eq!(out.coalesced_count, 0);
    }

    #[test]
    fn reduced_motion_preserves_focus_and_region_policy_announcements() {
        let m = AccessibilityPreferences::none()
            .with_reduced_motion(true)
            .motion_profile();
        let batch = ScreenReaderAnnouncements {
            announcements: vec![
                ann(
                    1,
                    LiveRegion::Assertive,
                    AnnouncementReason::FocusChanged,
                    "textInput: name",
                ),
                ann(
                    2,
                    LiveRegion::Assertive,
                    AnnouncementReason::LiveRegionChanged,
                    "alert region",
                ),
                ann(
                    3,
                    LiveRegion::Assertive,
                    AnnouncementReason::LiveContentChanged,
                    "tick 1",
                ),
                ann(
                    3,
                    LiveRegion::Assertive,
                    AnnouncementReason::LiveContentChanged,
                    "tick 2",
                ),
            ],
            dropped_count: 0,
        };
        let out = m.filter_announcements(&batch);
        // Focus + region-policy kept verbatim (assertive preserved); the two
        // content ticks coalesce to the latest and downgrade to polite.
        assert_eq!(out.announcements.len(), 3);
        assert_eq!(
            out.announcements[0].reason,
            AnnouncementReason::FocusChanged
        );
        assert_eq!(out.announcements[0].urgency, LiveRegion::Assertive);
        assert_eq!(
            out.announcements[1].reason,
            AnnouncementReason::LiveRegionChanged
        );
        assert_eq!(out.announcements[1].urgency, LiveRegion::Assertive);
        assert_eq!(out.announcements[2].text, "tick 2");
        assert_eq!(out.announcements[2].urgency, LiveRegion::Polite);
        assert_eq!(out.coalesced_count, 1);
        assert_eq!(out.downgraded_count, 1);
    }

    #[test]
    fn coalescing_keeps_distinct_nodes_separate() {
        let m = AccessibilityPreferences::all().motion_profile();
        let batch = ScreenReaderAnnouncements {
            announcements: vec![
                ann(
                    10,
                    LiveRegion::Polite,
                    AnnouncementReason::LiveContentChanged,
                    "a1",
                ),
                ann(
                    11,
                    LiveRegion::Polite,
                    AnnouncementReason::LiveContentChanged,
                    "b1",
                ),
                ann(
                    10,
                    LiveRegion::Polite,
                    AnnouncementReason::LiveContentChanged,
                    "a2",
                ),
            ],
            dropped_count: 0,
        };
        let out = m.filter_announcements(&batch);
        // node 11 kept; node 10 coalesced to latest "a2".
        let texts: Vec<&str> = out.announcements.iter().map(|a| a.text.as_str()).collect();
        assert_eq!(texts, vec!["b1", "a2"]);
        assert_eq!(out.coalesced_count, 1);
    }

    #[test]
    fn filtering_is_deterministic() {
        let m = AccessibilityPreferences::all().motion_profile();
        let batch = ScreenReaderAnnouncements {
            announcements: vec![
                ann(
                    1,
                    LiveRegion::Assertive,
                    AnnouncementReason::LiveContentChanged,
                    "x1",
                ),
                ann(
                    1,
                    LiveRegion::Assertive,
                    AnnouncementReason::LiveContentChanged,
                    "x2",
                ),
            ],
            dropped_count: 1,
        };
        assert_eq!(
            m.filter_announcements(&batch),
            m.filter_announcements(&batch)
        );
    }
}
