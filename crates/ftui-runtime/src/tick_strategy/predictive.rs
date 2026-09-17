//! [`Predictive`] strategy: Markov-chain-driven tick allocation.
//!
//! The crown jewel of the tick strategy system. Uses a [`MarkovPredictor`] to
//! learn screen transition patterns and a [`TickAllocation`] to convert
//! predicted probabilities into tick divisors. Screens the user is likely to
//! switch to get ticked more frequently; unlikely screens get throttled.
//!
//! Gracefully degrades to uniform behavior during cold start (insufficient
//! data) via confidence-weighted blending in [`MarkovPredictor`].

use std::collections::HashMap;
#[cfg(feature = "state-persistence")]
use std::path::PathBuf;

use tracing::{debug, info, trace};

use super::{MarkovPredictor, TickAllocation, TickDecision, TickStrategy, TransitionCounter};

/// Configuration for the [`Predictive`] tick strategy.
#[derive(Debug, Clone)]
pub struct PredictiveStrategyConfig {
    /// How probabilities map to tick divisors.
    pub allocation: TickAllocation,
    /// Divisor used for unknown screens (not in the predictor's vocabulary).
    pub fallback_divisor: u64,
    /// Minimum observations before predictions are fully trusted.
    pub min_observations: u64,
    /// Temporal decay factor applied during maintenance (0.0..1.0).
    pub decay_factor: f64,
    /// How many ticks between decay cycles.
    pub decay_interval: u64,
    /// How many ticks between auto-save cycles (0 = disabled).
    pub auto_save_interval: u64,
}

impl Default for PredictiveStrategyConfig {
    fn default() -> Self {
        Self {
            allocation: TickAllocation::default(),
            fallback_divisor: 5,
            min_observations: 20,
            decay_factor: 0.85,
            decay_interval: 500,
            auto_save_interval: 3000,
        }
    }
}

/// Markov-chain-driven tick strategy.
///
/// Learns screen transition patterns and allocates tick budget proportionally
/// to transition probability. High-probability next screens tick at near-full
/// rate; low-probability screens are aggressively throttled.
///
/// See module-level docs for architecture details.
#[derive(Debug, Clone)]
pub struct Predictive {
    predictor: MarkovPredictor<String>,
    allocation: TickAllocation,
    fallback_divisor: u64,
    decay_factor: f64,
    decay_interval: u64,
    auto_save_interval: u64,
    /// Cache: recomputed only when active screen changes.
    cached_divisors: HashMap<String, u64>,
    /// The active screen the cache was computed for.
    cached_for_screen: Option<String>,
    /// Tick counter for maintenance scheduling.
    ticks_since_decay: u64,
    /// Tick counter for auto-save scheduling.
    ticks_since_save: u64,
    /// Whether there's unsaved data (transitions recorded since last save).
    dirty: bool,
    /// Path for periodic auto-save. `None` disables auto-save I/O.
    #[cfg(feature = "state-persistence")]
    persistence_path: Option<PathBuf>,
}

impl Predictive {
    /// Create a new predictive strategy with the given config.
    #[must_use]
    pub fn new(config: PredictiveStrategyConfig) -> Self {
        info!(
            strategy = "Predictive",
            fallback_divisor = config.fallback_divisor,
            min_observations = config.min_observations,
            decay_factor = config.decay_factor,
            decay_interval = config.decay_interval,
            "tick_strategy.init"
        );
        Self {
            predictor: MarkovPredictor::with_min_observations(config.min_observations),
            allocation: config.allocation,
            fallback_divisor: config.fallback_divisor.max(1),
            decay_factor: config.decay_factor,
            decay_interval: config.decay_interval.max(1),
            auto_save_interval: config.auto_save_interval,
            cached_divisors: HashMap::new(),
            cached_for_screen: None,
            ticks_since_decay: 0,
            ticks_since_save: 0,
            dirty: false,
            #[cfg(feature = "state-persistence")]
            persistence_path: None,
        }
    }

    /// Create with pre-loaded transition data (e.g., from persistence).
    #[must_use]
    pub fn with_counter(
        config: PredictiveStrategyConfig,
        counter: TransitionCounter<String>,
    ) -> Self {
        info!(
            strategy = "Predictive",
            fallback_divisor = config.fallback_divisor,
            min_observations = config.min_observations,
            loaded_transitions = %counter.total(),
            known_screens = counter.state_ids().len(),
            "tick_strategy.init (with pre-loaded data)"
        );
        Self {
            predictor: MarkovPredictor::with_counter(counter, config.min_observations),
            allocation: config.allocation,
            fallback_divisor: config.fallback_divisor.max(1),
            decay_factor: config.decay_factor,
            decay_interval: config.decay_interval.max(1),
            auto_save_interval: config.auto_save_interval,
            cached_divisors: HashMap::new(),
            cached_for_screen: None,
            ticks_since_decay: 0,
            ticks_since_save: 0,
            dirty: false,
            #[cfg(feature = "state-persistence")]
            persistence_path: None,
        }
    }

    /// Create with persistence: load historical transitions from a file.
    ///
    /// - **Missing file**: cold start (empty counter), no error.
    /// - **Corrupted file**: logs a warning, falls back to cold start.
    /// - **Successful load**: applies a single decay (`load_decay_factor`)
    ///   to prevent historical data from permanently dominating.
    #[cfg(feature = "state-persistence")]
    #[must_use]
    pub fn with_persistence(
        config: PredictiveStrategyConfig,
        path: &std::path::Path,
        load_decay_factor: f64,
    ) -> Self {
        let counter = match super::persistence::load_transitions(path) {
            Ok(c) => {
                info!(
                    path = %path.display(),
                    loaded_transitions = %c.total(),
                    known_screens = c.state_ids().len(),
                    "tick_strategy.persistence_loaded"
                );
                c
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "tick_strategy.persistence_load_failed (falling back to cold start)"
                );
                TransitionCounter::new()
            }
        };

        let mut strategy = Self::with_counter(config, counter);

        // Apply load decay to down-weight historical data.
        let factor = load_decay_factor.clamp(0.0, 1.0);
        if factor < 1.0 {
            let total_before = strategy.predictor.counter().total();
            strategy.predictor.counter_mut().decay(factor);
            let total_after = strategy.predictor.counter().total();
            if (total_after - total_before).abs() > f64::EPSILON {
                strategy.dirty = true;
            }
            info!(
                load_decay_factor = factor,
                remaining_total = %total_after,
                "tick_strategy.load_decay_applied"
            );
        }

        // Remember the path for periodic auto-save.
        strategy.persistence_path = Some(path.to_path_buf());
        strategy
    }

    /// Access the underlying predictor.
    #[must_use]
    pub fn predictor(&self) -> &MarkovPredictor<String> {
        &self.predictor
    }

    /// Access the underlying transition counter.
    #[must_use]
    pub fn counter(&self) -> &TransitionCounter<String> {
        self.predictor.counter()
    }

    /// Whether there is unsaved transition data.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Save transitions to disk if dirty and a persistence path is configured.
    ///
    /// IO errors are logged and swallowed — auto-save must never crash the
    /// runtime.
    #[cfg(feature = "state-persistence")]
    fn save_if_dirty(&mut self) {
        if !self.dirty {
            return;
        }
        let Some(path) = self.persistence_path.as_deref() else {
            return;
        };
        match super::persistence::save_transitions(self.predictor.counter(), path) {
            Ok(()) => {
                self.dirty = false;
                self.ticks_since_save = 0;
                info!(
                    path = %path.display(),
                    total = %self.predictor.counter().total(),
                    "tick_strategy.auto_save"
                );
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "tick_strategy.auto_save_failed"
                );
            }
        }
    }

    /// Recompute the cached divisor map for predictions from `active`.
    fn refresh_cache(&mut self, active: &str) {
        if self.cached_for_screen.as_deref() == Some(active) {
            return;
        }

        self.cached_divisors.clear();
        let predictions = self.predictor.predict(&active.to_owned());
        let is_cold = self.predictor.is_cold_start(&active.to_owned());

        if is_cold {
            let obs = self.predictor.counter().total_from(&active.to_owned()) as u64;
            info!(
                screen = active,
                observations = obs,
                min_required = self.predictor.min_observations(),
                using_fallback = true,
                "tick_strategy.cold_start"
            );
        }

        for p in &predictions {
            let divisor = self.allocation.divisor_for(p.probability);
            trace!(
                screen = %p.screen,
                divisor,
                probability = %p.probability,
                confidence = %p.confidence,
                "tick_strategy.screen_divisor"
            );
            self.cached_divisors.insert(p.screen.clone(), divisor);
        }

        debug!(
            strategy = "Predictive",
            active_screen = active,
            num_screens = predictions.len(),
            cold_start = is_cold,
            "tick_strategy.cache_refresh"
        );

        self.cached_for_screen = Some(active.to_owned());
    }
}

impl TickStrategy for Predictive {
    fn should_tick(
        &mut self,
        screen_id: &str,
        tick_count: u64,
        active_screen: &str,
    ) -> TickDecision {
        // Ensure cache is fresh for current active screen.
        self.refresh_cache(active_screen);

        let divisor = self
            .cached_divisors
            .get(screen_id)
            .copied()
            .unwrap_or(self.fallback_divisor);

        if tick_count.is_multiple_of(divisor) {
            TickDecision::Tick
        } else {
            TickDecision::Skip
        }
    }

    fn on_screen_transition(&mut self, from: &str, to: &str) {
        self.predictor
            .record_transition(from.to_owned(), to.to_owned());
        self.dirty = true;
        debug!(
            from,
            to,
            total_transitions = %self.predictor.counter().total(),
            "tick_strategy.transition"
        );
        // Force cache refresh for the new active screen.
        self.cached_for_screen = None;
        self.refresh_cache(to);
    }

    fn maintenance_tick(&mut self, _tick_count: u64) {
        self.ticks_since_decay += 1;
        self.ticks_since_save += 1;

        // Periodic decay.
        if self.ticks_since_decay >= self.decay_interval {
            let entries_before = self.predictor.counter().state_ids().len();
            let total_before = self.predictor.counter().total();
            self.predictor.counter_mut().decay(self.decay_factor);
            let entries_after = self.predictor.counter().state_ids().len();
            let total_after = self.predictor.counter().total();
            debug!(
                factor = self.decay_factor,
                entries_before,
                entries_after,
                total_before = %total_before,
                total_after = %total_after,
                pruned = entries_before.saturating_sub(entries_after),
                "tick_strategy.decay"
            );
            self.ticks_since_decay = 0;
            // Invalidate cache since probabilities changed.
            self.cached_for_screen = None;
            if entries_after != entries_before || (total_after - total_before).abs() > f64::EPSILON
            {
                self.dirty = true;
            }
        }

        // Periodic auto-save.
        if self.auto_save_interval > 0 && self.ticks_since_save >= self.auto_save_interval {
            #[cfg(feature = "state-persistence")]
            self.save_if_dirty();
            #[cfg(not(feature = "state-persistence"))]
            {
                self.ticks_since_save = 0;
            }
        }
    }

    fn shutdown(&mut self) {
        #[cfg(feature = "state-persistence")]
        self.save_if_dirty();
    }

    fn name(&self) -> &str {
        "Predictive"
    }

    fn debug_stats(&self) -> Vec<(String, String)> {
        let confidence = self
            .cached_for_screen
            .as_ref()
            .map(|s| self.predictor.confidence(s))
            .unwrap_or(0.0);

        // Build top prediction string if cache is populated.
        let top_prediction = self
            .cached_for_screen
            .as_ref()
            .and_then(|screen| {
                let preds = self.predictor.predict(screen);
                preds.first().map(|p| {
                    let divisor = self
                        .cached_divisors
                        .get(&p.screen)
                        .copied()
                        .unwrap_or(self.fallback_divisor);
                    format!("{}:{:.2}/div={}", p.screen, p.probability, divisor)
                })
            })
            .unwrap_or_else(|| "(none)".to_owned());

        let decay_next_at = self.decay_interval.saturating_sub(self.ticks_since_decay);

        vec![
            ("strategy".into(), "Predictive".into()),
            (
                "total_transitions".into(),
                format!("{:.0}", self.predictor.counter().total()),
            ),
            (
                "known_screens".into(),
                self.predictor.counter().state_ids().len().to_string(),
            ),
            (
                "cached_divisors".into(),
                self.cached_divisors.len().to_string(),
            ),
            (
                "active_screen".into(),
                self.cached_for_screen
                    .as_deref()
                    .unwrap_or("(none)")
                    .to_owned(),
            ),
            ("confidence".into(), format!("{confidence:.2}")),
            ("top_prediction".into(), top_prediction),
            ("fallback_divisor".into(), self.fallback_divisor.to_string()),
            ("decay_factor".into(), format!("{:.2}", self.decay_factor)),
            ("decay_next_at".into(), decay_next_at.to_string()),
            ("dirty".into(), self.dirty.to_string()),
        ]
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> PredictiveStrategyConfig {
        PredictiveStrategyConfig {
            min_observations: 5, // low threshold for test speed
            fallback_divisor: 10,
            decay_interval: 100,
            ..PredictiveStrategyConfig::default()
        }
    }

    #[test]
    fn cold_start_uses_fallback_divisor() {
        let mut s = Predictive::new(default_config());
        // No transition data → unknown screen → fallback divisor
        assert_eq!(s.should_tick("x", 0, "a"), TickDecision::Tick); // 0 % 10 == 0
        assert_eq!(s.should_tick("x", 1, "a"), TickDecision::Skip);
        assert_eq!(s.should_tick("x", 9, "a"), TickDecision::Skip);
        assert_eq!(s.should_tick("x", 10, "a"), TickDecision::Tick); // 10 % 10 == 0
    }

    #[test]
    fn learns_and_adjusts_divisors() {
        let config = PredictiveStrategyConfig {
            min_observations: 5,
            fallback_divisor: 20,
            ..PredictiveStrategyConfig::default()
        };
        let mut s = Predictive::new(config);

        // Record many a→b transitions
        for _ in 0..20 {
            s.on_screen_transition("a", "b");
        }
        // Record few a→c transitions
        for _ in 0..2 {
            s.on_screen_transition("a", "c");
        }

        // Now when active is "a", "b" should have a lower divisor than "c"
        s.refresh_cache("a");
        let b_div = s.cached_divisors.get("b").copied().unwrap_or(99);
        let c_div = s.cached_divisors.get("c").copied().unwrap_or(99);
        assert!(
            b_div < c_div,
            "b should tick more: b_div={b_div}, c_div={c_div}"
        );
    }

    #[test]
    fn cache_refreshes_on_screen_transition() {
        let mut s = Predictive::new(default_config());
        s.on_screen_transition("a", "b");
        assert_eq!(s.cached_for_screen.as_deref(), Some("b"));

        s.on_screen_transition("b", "c");
        assert_eq!(s.cached_for_screen.as_deref(), Some("c"));
    }

    #[test]
    fn cache_reused_for_same_screen() {
        let mut s = Predictive::new(default_config());
        s.on_screen_transition("a", "b");

        // First call refreshes cache
        s.should_tick("x", 1, "b");
        let cached = s.cached_for_screen.clone();

        // Second call reuses cache (same active screen)
        s.should_tick("x", 2, "b");
        assert_eq!(s.cached_for_screen, cached);
    }

    #[test]
    fn unknown_screen_uses_fallback() {
        let mut s = Predictive::new(default_config());
        s.on_screen_transition("a", "b");

        // "unknown" not in any prediction → fallback_divisor
        let div = s
            .cached_divisors
            .get("unknown")
            .copied()
            .unwrap_or(s.fallback_divisor);
        assert_eq!(div, 10); // default_config fallback
    }

    #[test]
    fn decay_triggers_at_interval() {
        let config = PredictiveStrategyConfig {
            decay_interval: 10,
            decay_factor: 0.5,
            min_observations: 5,
            ..PredictiveStrategyConfig::default()
        };
        let mut s = Predictive::new(config);
        s.on_screen_transition("a", "b");
        let before = s.predictor.counter().total();

        // Simulate maintenance ticks
        for _ in 0..10 {
            s.maintenance_tick(0);
        }

        let after = s.predictor.counter().total();
        assert!(
            after < before,
            "decay should reduce total: {after} < {before}"
        );
    }

    #[test]
    fn dirty_flag_set_on_transition() {
        let mut s = Predictive::new(default_config());
        assert!(!s.is_dirty());

        s.on_screen_transition("a", "b");
        assert!(s.is_dirty());
    }

    #[test]
    fn name_is_stable() {
        let s = Predictive::new(default_config());
        assert_eq!(s.name(), "Predictive");
    }

    #[test]
    fn debug_stats_populated() {
        let mut s = Predictive::new(default_config());
        s.on_screen_transition("a", "b");

        let stats = s.debug_stats();
        assert!(!stats.is_empty());
        assert!(stats.iter().any(|(k, _)| k == "strategy"));
        assert!(stats.iter().any(|(k, _)| k == "total_transitions"));
        assert!(stats.iter().any(|(k, _)| k == "confidence"));
        assert!(stats.iter().any(|(k, _)| k == "top_prediction"));
        assert!(stats.iter().any(|(k, _)| k == "decay_factor"));
        assert!(stats.iter().any(|(k, _)| k == "decay_next_at"));
    }

    #[test]
    fn with_counter_preloads_data() {
        let mut counter = TransitionCounter::new();
        for _ in 0..50 {
            counter.record("a".to_owned(), "b".to_owned());
        }

        let s = Predictive::with_counter(default_config(), counter);
        assert!(!s.predictor().is_cold_start(&"a".to_owned()));
    }

    #[test]
    fn high_probability_screen_ticks_more() {
        let config = PredictiveStrategyConfig {
            min_observations: 5,
            fallback_divisor: 20,
            ..PredictiveStrategyConfig::default()
        };
        let mut s = Predictive::new(config);

        // Build strong signal: a→b is very likely
        for _ in 0..30 {
            s.on_screen_transition("a", "b");
        }
        s.on_screen_transition("a", "c");

        // Count ticks over 100 frames for each screen
        let mut b_ticks = 0u64;
        let mut c_ticks = 0u64;
        for tick in 0..100 {
            if s.should_tick("b", tick, "a") == TickDecision::Tick {
                b_ticks += 1;
            }
            if s.should_tick("c", tick, "a") == TickDecision::Tick {
                c_ticks += 1;
            }
        }

        assert!(
            b_ticks > c_ticks,
            "b should tick more than c: b={b_ticks}, c={c_ticks}"
        );
    }

    // ========================================================================
    // Persistence integration tests (E.2 coverage)
    // ========================================================================

    #[cfg(feature = "state-persistence")]
    mod persistence_tests {
        use super::*;

        fn retained_fixture_dir() -> tempfile::TempDir {
            tempfile::Builder::new()
                .prefix("ftui-tick-predictive-")
                .disable_cleanup(true)
                .tempdir()
                .expect("create retained predictive fixture")
        }

        #[test]
        fn with_persistence_loads_from_file() {
            use crate::tick_strategy::persistence::save_transitions;

            let dir = retained_fixture_dir();
            let path = dir.path().join("transitions.json");

            // Create and save historical data
            let mut counter = TransitionCounter::new();
            for _ in 0..50 {
                counter.record("a".to_owned(), "b".to_owned());
            }
            for _ in 0..20 {
                counter.record("a".to_owned(), "c".to_owned());
            }
            save_transitions(&counter, &path).unwrap();

            // Load with no decay
            let s = Predictive::with_persistence(default_config(), &path, 1.0);
            assert!(!s.predictor().is_cold_start(&"a".to_owned()));
            assert_eq!(s.counter().total(), 70.0);
        }

        #[test]
        fn with_persistence_applies_load_decay() {
            use crate::tick_strategy::persistence::save_transitions;

            let dir = retained_fixture_dir();
            let path = dir.path().join("transitions.json");

            let mut counter = TransitionCounter::new();
            for _ in 0..100 {
                counter.record("a".to_owned(), "b".to_owned());
            }
            save_transitions(&counter, &path).unwrap();

            // Load with 0.5 decay
            let s = Predictive::with_persistence(default_config(), &path, 0.5);
            let total = s.counter().total();
            eprintln!("total after load_decay(0.5): {total}");
            assert!(
                (total - 50.0).abs() < 1e-9,
                "expected ~50.0 after 0.5 decay, got {total}"
            );
        }

        #[test]
        fn with_persistence_missing_file_is_cold_start() {
            let dir = retained_fixture_dir();
            let path = dir.path().join("nonexistent.json");

            let s = Predictive::with_persistence(default_config(), &path, 0.9);
            assert_eq!(s.counter().total(), 0.0);
            assert!(s.predictor().is_cold_start(&"a".to_owned()));
        }

        #[test]
        fn with_persistence_corrupted_file_is_cold_start() {
            let dir = retained_fixture_dir();
            let path = dir.path().join("bad.json");
            std::fs::write(&path, "not valid json {{{").unwrap();

            let s = Predictive::with_persistence(default_config(), &path, 0.9);
            assert_eq!(s.counter().total(), 0.0);
        }

        // ====================================================================
        // Auto-save tests (E.3 coverage)
        // ====================================================================

        #[test]
        fn auto_save_fires_at_interval() {
            let dir = retained_fixture_dir();
            let path = dir.path().join("auto.json");

            let config = PredictiveStrategyConfig {
                auto_save_interval: 10,
                min_observations: 5,
                decay_interval: 9999, // disable decay
                ..PredictiveStrategyConfig::default()
            };
            let mut s = Predictive::with_persistence(config, &path, 1.0);

            // Record a transition to make it dirty.
            s.on_screen_transition("a", "b");
            assert!(s.is_dirty());
            assert!(!path.exists());

            // Pump maintenance ticks up to the interval.
            for _ in 0..10 {
                s.maintenance_tick(0);
            }

            // File should now exist and dirty flag should be cleared.
            assert!(path.exists(), "auto-save should have written the file");
            assert!(!s.is_dirty(), "dirty flag should be cleared after save");
        }

        #[test]
        fn auto_save_writes_valid_json() {
            use crate::tick_strategy::persistence::load_transitions;

            let dir = retained_fixture_dir();
            let path = dir.path().join("valid.json");

            let config = PredictiveStrategyConfig {
                auto_save_interval: 5,
                min_observations: 5,
                decay_interval: 9999,
                ..PredictiveStrategyConfig::default()
            };
            let mut s = Predictive::with_persistence(config, &path, 1.0);

            for _ in 0..10 {
                s.on_screen_transition("x", "y");
            }

            // Trigger auto-save.
            for _ in 0..5 {
                s.maintenance_tick(0);
            }

            // Load the file and verify contents.
            let loaded = load_transitions(&path).unwrap();
            assert_eq!(loaded.count(&"x".to_owned(), &"y".to_owned()), 10.0);
        }

        #[test]
        fn auto_save_skips_when_not_dirty() {
            let dir = retained_fixture_dir();
            let path = dir.path().join("nodirty.json");

            let config = PredictiveStrategyConfig {
                auto_save_interval: 5,
                min_observations: 5,
                decay_interval: 9999,
                ..PredictiveStrategyConfig::default()
            };
            let mut s = Predictive::with_persistence(config, &path, 1.0);

            // No transitions → not dirty. Pump past the interval.
            for _ in 0..10 {
                s.maintenance_tick(0);
            }

            assert!(!path.exists(), "no file should be written when not dirty");
        }

        #[test]
        fn shutdown_triggers_save() {
            let dir = retained_fixture_dir();
            let path = dir.path().join("shutdown.json");

            let config = PredictiveStrategyConfig {
                auto_save_interval: 99999, // won't fire during test
                min_observations: 5,
                decay_interval: 9999,
                ..PredictiveStrategyConfig::default()
            };
            let mut s = Predictive::with_persistence(config, &path, 1.0);

            s.on_screen_transition("p", "q");
            assert!(s.is_dirty());

            s.shutdown();

            assert!(path.exists(), "shutdown should trigger a save");
            assert!(!s.is_dirty(), "dirty flag cleared after shutdown save");
        }

        #[test]
        fn auto_save_no_path_is_noop() {
            // Strategy created without persistence → no crash, no save.
            let config = PredictiveStrategyConfig {
                auto_save_interval: 1,
                min_observations: 5,
                decay_interval: 9999,
                ..PredictiveStrategyConfig::default()
            };
            let mut s = Predictive::new(config);

            s.on_screen_transition("a", "b");
            assert!(s.is_dirty());

            // Pump past interval — should not panic.
            for _ in 0..5 {
                s.maintenance_tick(0);
            }
            s.shutdown();

            // Still dirty because no path was configured.
            assert!(s.is_dirty());
        }

        #[test]
        fn auto_save_bad_path_does_not_crash() {
            let config = PredictiveStrategyConfig {
                auto_save_interval: 5,
                min_observations: 5,
                decay_interval: 9999,
                ..PredictiveStrategyConfig::default()
            };
            let dir = retained_fixture_dir();
            // Point at a child of a directory that does not exist.
            let bad_path = dir.path().join("missing-parent").join("transitions.json");
            let mut s = Predictive::with_persistence(config, &bad_path, 1.0);

            s.on_screen_transition("a", "b");

            // Trigger auto-save — should log error but not panic.
            for _ in 0..5 {
                s.maintenance_tick(0);
            }

            // dirty remains true because save failed.
            assert!(s.is_dirty());
        }

        #[test]
        fn maintenance_decay_marks_strategy_dirty_and_persists_on_shutdown() {
            use crate::tick_strategy::persistence::load_transitions;

            let dir = retained_fixture_dir();
            let path = dir.path().join("decayed-on-shutdown.json");

            let config = PredictiveStrategyConfig {
                auto_save_interval: 99999,
                min_observations: 5,
                decay_interval: 1,
                decay_factor: 0.5,
                ..PredictiveStrategyConfig::default()
            };
            let mut s = Predictive::with_persistence(config, &path, 1.0);

            for _ in 0..20 {
                s.on_screen_transition("a", "b");
            }
            s.shutdown();
            assert!(
                !s.is_dirty(),
                "initial shutdown should flush transition data"
            );

            s.maintenance_tick(0);
            assert!(
                s.is_dirty(),
                "maintenance decay mutates persisted state and must mark it dirty"
            );

            s.shutdown();
            let loaded = load_transitions(&path).unwrap();
            assert!(
                (loaded.total() - 10.0).abs() < 1e-9,
                "expected decayed total to persist after shutdown, got {}",
                loaded.total()
            );
        }

        #[test]
        fn load_decay_marks_strategy_dirty_until_persisted() {
            use crate::tick_strategy::persistence::{load_transitions, save_transitions};

            let dir = retained_fixture_dir();
            let path = dir.path().join("load-decay.json");

            let mut counter = TransitionCounter::new();
            for _ in 0..12 {
                counter.record("a".to_owned(), "b".to_owned());
            }
            save_transitions(&counter, &path).unwrap();

            let config = PredictiveStrategyConfig {
                auto_save_interval: 99999,
                min_observations: 5,
                decay_interval: 9999,
                ..PredictiveStrategyConfig::default()
            };
            let mut s = Predictive::with_persistence(config, &path, 0.5);
            assert!(
                s.is_dirty(),
                "load-time decay changes the in-memory counter and should be persisted"
            );

            s.shutdown();
            let loaded = load_transitions(&path).unwrap();
            assert!(
                (loaded.total() - 6.0).abs() < 1e-9,
                "expected shutdown to persist the decayed total, got {}",
                loaded.total()
            );
        }
    }
}
