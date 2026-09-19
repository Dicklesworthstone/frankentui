#![forbid(unsafe_code)]

//! Locale context provider for runtime-wide internationalization.
//!
//! The [`LocaleContext`] owns the current locale and exposes scoped overrides
//! for widget subtrees. Locale changes are versioned so the runtime can
//! trigger re-renders when the active locale changes.

use crate::reactive::{Observable, Subscription};
pub use ftui_i18n::catalog::Locale;
use std::cell::RefCell;
use std::env;
use std::rc::Rc;

thread_local! {
    static GLOBAL_CONTEXT: LocaleContext = LocaleContext::system();
}

/// Text flow direction for the runtime locale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TextDirection {
    /// Left-to-right text flow.
    #[default]
    Ltr,
    /// Right-to-left text flow.
    Rtl,
}

impl TextDirection {
    /// Whether this direction is left-to-right.
    #[inline]
    #[must_use]
    pub const fn is_ltr(self) -> bool {
        matches!(self, Self::Ltr)
    }

    /// Whether this direction is right-to-left.
    #[inline]
    #[must_use]
    pub const fn is_rtl(self) -> bool {
        matches!(self, Self::Rtl)
    }

    /// Infer text direction from a locale tag.
    ///
    /// Checks base language (before '-' or '_', lowercase):
    /// `["ar", "he", "fa", "ur", "yi", "ps", "sd", "ug", "dv", "ckb"]` -> `Rtl`.
    /// Also checks for script subtags `Arab` or `Hebr` (e.g. `ku-Arab`) -> `Rtl`.
    #[must_use]
    pub fn for_locale(locale: &str) -> Self {
        let tag = locale.trim();
        if tag.is_empty() {
            return Self::Ltr;
        }

        let parts: Vec<&str> = tag.split(['-', '_']).collect();
        if let Some(base) = parts.first() {
            let base_lower = base.to_ascii_lowercase();
            const RTL_LANGS: &[&str] =
                &["ar", "he", "fa", "ur", "yi", "ps", "sd", "ug", "dv", "ckb"];
            if RTL_LANGS.contains(&base_lower.as_str()) {
                return Self::Rtl;
            }
        }

        for part in &parts[1..] {
            if part.eq_ignore_ascii_case("arab") || part.eq_ignore_ascii_case("hebr") {
                return Self::Rtl;
            }
        }

        Self::Ltr
    }
}

impl From<TextDirection> for ftui_render::TextDirection {
    fn from(dir: TextDirection) -> Self {
        match dir {
            TextDirection::Ltr => ftui_render::TextDirection::Ltr,
            TextDirection::Rtl => ftui_render::TextDirection::Rtl,
        }
    }
}

impl From<ftui_render::TextDirection> for TextDirection {
    fn from(dir: ftui_render::TextDirection) -> Self {
        match dir {
            ftui_render::TextDirection::Ltr => TextDirection::Ltr,
            ftui_render::TextDirection::Rtl => TextDirection::Rtl,
        }
    }
}

/// Runtime locale context with scoped overrides.
#[derive(Clone, Debug)]
pub struct LocaleContext {
    current: Observable<Locale>,
    overrides: Rc<RefCell<Vec<Locale>>>,
    direction_override: Observable<Option<TextDirection>>,
}

impl LocaleContext {
    /// Create a new locale context with the provided locale.
    #[must_use]
    pub fn new(locale: impl Into<Locale>) -> Self {
        let locale = normalize_locale(locale.into());
        Self {
            current: Observable::new(locale),
            overrides: Rc::new(RefCell::new(Vec::new())),
            direction_override: Observable::new(None),
        }
    }

    /// Create a locale context initialized from system locale detection.
    #[must_use]
    pub fn system() -> Self {
        Self::new(detect_system_locale())
    }

    /// Access the global locale context (thread-local).
    #[must_use]
    pub fn global() -> Self {
        GLOBAL_CONTEXT.with(Clone::clone)
    }

    /// Get the active locale, honoring any scoped override.
    #[must_use]
    pub fn current_locale(&self) -> Locale {
        if let Some(locale) = self.overrides.borrow().last() {
            locale.clone()
        } else {
            self.current.get()
        }
    }

    /// Get the base locale without considering overrides.
    #[must_use]
    pub fn base_locale(&self) -> Locale {
        self.current.get()
    }

    /// Set the base locale.
    pub fn set_locale(&self, locale: impl Into<Locale>) {
        let old_dir = self.direction();
        let locale = normalize_locale(locale.into());
        self.current.set(locale);
        let new_dir = self.direction();
        if new_dir != old_dir {
            tracing::info!(
                target: crate::telemetry_schema::TARGET_LOCALE,
                locale = %self.current_locale(),
                direction = ?new_dir,
                source = "locale",
                "text direction changed"
            );
        }
    }

    /// Get the active text direction (from current locale unless overridden).
    #[must_use]
    pub fn direction(&self) -> TextDirection {
        if let Some(dir) = self.direction_override.get() {
            dir
        } else {
            TextDirection::for_locale(&self.current_locale())
        }
    }

    /// Set an explicit text direction override (or None to infer from locale).
    pub fn set_direction(&self, direction: Option<TextDirection>) {
        let old_dir = self.direction();
        self.direction_override.set(direction);
        let new_dir = self.direction();
        if new_dir != old_dir {
            tracing::info!(
                target: crate::telemetry_schema::TARGET_LOCALE,
                locale = %self.current_locale(),
                direction = ?new_dir,
                source = "override",
                "text direction changed"
            );
        }
    }

    /// Subscribe to base locale and direction changes.
    pub fn subscribe(&self, callback: impl Fn(&Locale) + 'static) -> Subscription {
        let rc_cb = Rc::new(callback);
        let cb1 = Rc::clone(&rc_cb);
        let sub1 = self.current.subscribe(move |loc| cb1(loc));
        let current = self.current.clone();
        let cb2 = Rc::clone(&rc_cb);
        let sub2 = self.direction_override.subscribe(move |_| {
            let loc = current.get();
            cb2(&loc);
        });
        Subscription::composite(vec![sub1, sub2])
    }

    /// Push a scoped locale override. Dropping the guard restores the prior locale.
    #[must_use = "dropping this guard clears the locale override"]
    pub fn push_override(&self, locale: impl Into<Locale>) -> LocaleOverride {
        let locale = normalize_locale(locale.into());
        self.overrides.borrow_mut().push(locale.clone());
        LocaleOverride {
            stack: Rc::clone(&self.overrides),
            locale,
        }
    }

    /// Current version counter for the base locale and direction.
    #[must_use]
    pub fn version(&self) -> u64 {
        self.current
            .version()
            .saturating_add(self.direction_override.version())
    }
}

/// RAII guard for scoped locale overrides.
#[must_use = "dropping this guard clears the locale override"]
pub struct LocaleOverride {
    stack: Rc<RefCell<Vec<Locale>>>,
    locale: Locale,
}

impl Drop for LocaleOverride {
    fn drop(&mut self) {
        let popped = self.stack.borrow_mut().pop();
        if let Some(popped) = popped {
            debug_assert_eq!(popped, self.locale);
        }
    }
}

/// Detect the system locale from environment variables.
///
/// Preference order: `LC_ALL`, then `LANG`. Falls back to `"en"` when unknown.
#[must_use]
pub fn detect_system_locale() -> Locale {
    let lc_all = env::var("LC_ALL").ok();
    let lang = env::var("LANG").ok();
    detect_system_locale_from(lc_all.as_deref(), lang.as_deref())
}

/// Convenience: set the global locale.
pub fn set_locale(locale: impl Into<Locale>) {
    LocaleContext::global().set_locale(locale);
}

/// Convenience: get the global locale.
#[must_use]
pub fn current_locale() -> Locale {
    LocaleContext::global().current_locale()
}

/// Convenience: get the global text direction.
#[must_use]
pub fn direction() -> TextDirection {
    LocaleContext::global().direction()
}

/// Convenience: set the global text direction override.
pub fn set_direction(direction: Option<TextDirection>) {
    LocaleContext::global().set_direction(direction);
}

fn normalize_locale(mut locale: Locale) -> Locale {
    normalize_locale_raw(&locale).unwrap_or_else(|| {
        locale.clear();
        locale.push_str("en");
        locale
    })
}

fn detect_system_locale_from(lc_all: Option<&str>, lang: Option<&str>) -> Locale {
    lc_all
        .and_then(normalize_locale_raw)
        .or_else(|| lang.and_then(normalize_locale_raw))
        .unwrap_or_else(|| "en".to_string())
}

fn normalize_locale_raw(raw: &str) -> Option<Locale> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let raw = raw.split('@').next().unwrap_or(raw);
    let raw = raw.split('.').next().unwrap_or(raw);
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let mut normalized = raw.replace('_', "-");
    if normalized.eq_ignore_ascii_case("c") || normalized.eq_ignore_ascii_case("posix") {
        normalized.clear();
        normalized.push_str("en");
    }
    Some(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::cell::Cell;

    // ---------------------------------------------------------------------
    // Invariants (Alien Artifact)
    // ---------------------------------------------------------------------
    // 1. Normalized locales contain no '_' '.' or '@' suffixes.
    // 2. Locale overrides are LIFO and never mutate the base locale.
    // 3. Locale versions only advance on base locale changes.
    //
    // Failure Modes:
    // | Scenario                     | Expected Behavior                 |
    // |-----------------------------|-----------------------------------|
    // | Empty / whitespace locale   | Falls back to "en"                |
    // | "C"/"POSIX" locale          | Normalized to "en"                |
    // | Override drop out of order  | Debug assert (in dev builds)      |

    #[test]
    fn detect_system_locale_prefers_lc_all() {
        let locale = detect_system_locale_from(Some("fr_FR.UTF-8"), Some("en_US.UTF-8"));
        assert_eq!(locale, "fr-FR");
    }

    #[test]
    fn detect_system_locale_uses_lang_when_lc_all_missing() {
        let locale = detect_system_locale_from(None, Some("en_US.UTF-8"));
        assert_eq!(locale, "en-US");
    }

    #[test]
    fn detect_system_locale_defaults_to_en() {
        let locale = detect_system_locale_from(None, None);
        assert_eq!(locale, "en");
    }

    #[test]
    fn locale_context_switching_updates_version() {
        let ctx = LocaleContext::new("en");
        let v0 = ctx.version();
        ctx.set_locale("en");
        assert_eq!(ctx.version(), v0);
        ctx.set_locale("es");
        assert!(ctx.version() > v0);
        assert_eq!(ctx.current_locale(), "es");
    }

    #[test]
    fn locale_override_is_scoped() {
        let ctx = LocaleContext::new("en");
        assert_eq!(ctx.current_locale(), "en");
        let guard = ctx.push_override("fr");
        assert_eq!(ctx.current_locale(), "fr");
        drop(guard);
        assert_eq!(ctx.current_locale(), "en");
    }

    #[test]
    fn locale_override_is_lifo() {
        let ctx = LocaleContext::new("en");
        let _outer = ctx.push_override("fr");
        assert_eq!(ctx.current_locale(), "fr");
        {
            let _inner = ctx.push_override("es");
            assert_eq!(ctx.current_locale(), "es");
        }
        assert_eq!(ctx.current_locale(), "fr");
    }

    #[test]
    fn normalize_locale_handles_c_and_posix() {
        let c_locale = normalize_locale_raw("C");
        let posix_locale = normalize_locale_raw("POSIX");
        assert_eq!(c_locale.as_deref(), Some("en"));
        assert_eq!(posix_locale.as_deref(), Some("en"));
    }

    #[test]
    fn normalize_locale_strips_codeset_and_modifier() {
        let locale = normalize_locale_raw("en_US.UTF-8@latin");
        assert_eq!(locale.as_deref(), Some("en-US"));
    }

    #[test]
    fn locale_override_does_not_mutate_base_locale() {
        let ctx = LocaleContext::new("en");
        let v0 = ctx.version();
        let _guard = ctx.push_override("fr");
        assert_eq!(ctx.base_locale(), "en");
        assert_eq!(ctx.version(), v0);
    }

    #[test]
    fn normalize_empty_falls_back_to_en() {
        let locale = normalize_locale("".to_string());
        assert_eq!(locale, "en");
    }

    #[test]
    fn normalize_whitespace_only_falls_back_to_en() {
        let locale = normalize_locale("   ".to_string());
        assert_eq!(locale, "en");
    }

    #[test]
    fn subscribe_fires_on_change() {
        use std::cell::Cell;
        use std::rc::Rc;

        let ctx = LocaleContext::new("en");
        let fired = Rc::new(Cell::new(false));
        let fired_clone = Rc::clone(&fired);
        let _sub = ctx.subscribe(move |_| {
            fired_clone.set(true);
        });
        ctx.set_locale("de");
        assert!(fired.get());
    }

    #[test]
    fn detect_system_locale_empty_lc_all_uses_lang() {
        let locale = detect_system_locale_from(Some(""), Some("ja_JP.UTF-8"));
        assert_eq!(locale, "ja-JP");
    }

    proptest! {
        #[test]
        fn normalize_locale_raw_sanitizes_segments(raw in "[A-Za-z0-9_@.\\-]{1,32}") {
            let normalized = normalize_locale_raw(&raw);
            if let Some(locale) = normalized {
                prop_assert!(!locale.trim().is_empty());
                prop_assert!(!locale.contains('@'));
                prop_assert!(!locale.contains('.'));
                prop_assert!(!locale.contains('_'));
            }
        }

        #[test]
        fn overrides_are_lifo(locales in proptest::collection::vec("[a-z]{2}(-[A-Z]{2})?", 1..6)) {
            let ctx = LocaleContext::new("en");
            let mut guards = Vec::new();
            for locale in &locales {
                guards.push(ctx.push_override(locale));
            }
            prop_assert_eq!(ctx.current_locale(), locales.last().unwrap().as_str());
            guards.pop();
            if locales.len() >= 2 {
                let prev = &locales[locales.len() - 2];
                prop_assert_eq!(ctx.current_locale(), prev.as_str());
            } else {
                prop_assert_eq!(ctx.current_locale(), "en");
            }
            // Drop remaining guards in LIFO order (Vec drops front-to-back,
            // but the override stack expects last-pushed-first-dropped).
            while guards.pop().is_some() {}
        }
    }

    #[test]
    fn text_direction_for_locale_inferences() {
        assert_eq!(TextDirection::for_locale("ar-EG"), TextDirection::Rtl);
        assert_eq!(TextDirection::for_locale("en-US"), TextDirection::Ltr);
        assert_eq!(TextDirection::for_locale("ku-Arab"), TextDirection::Rtl);
        assert_eq!(TextDirection::for_locale("he"), TextDirection::Rtl);
        assert_eq!(TextDirection::for_locale("fa_IR"), TextDirection::Rtl);
        assert_eq!(TextDirection::for_locale("ur"), TextDirection::Rtl);
        assert_eq!(TextDirection::for_locale("de-DE"), TextDirection::Ltr);
        assert_eq!(TextDirection::for_locale(""), TextDirection::Ltr);
    }

    #[test]
    fn locale_context_direction_override_and_version() {
        let ctx = LocaleContext::new("en");
        assert_eq!(ctx.direction(), TextDirection::Ltr);
        let v0 = ctx.version();

        ctx.set_direction(Some(TextDirection::Rtl));
        assert_eq!(ctx.direction(), TextDirection::Rtl);
        assert!(ctx.version() > v0);
        let v1 = ctx.version();

        ctx.set_direction(None);
        assert_eq!(ctx.direction(), TextDirection::Ltr);
        assert!(ctx.version() > v1);

        ctx.set_locale("ar");
        assert_eq!(ctx.direction(), TextDirection::Rtl);
    }

    #[test]
    fn subscribe_fires_on_direction_change() {
        let ctx = LocaleContext::new("en");
        let fired = Rc::new(Cell::new(false));
        let fired_clone = Rc::clone(&fired);
        let _sub = ctx.subscribe(move |_| {
            fired_clone.set(true);
        });
        ctx.set_direction(Some(TextDirection::Rtl));
        assert!(fired.get());
    }

    #[test]
    fn render_text_direction_conversion() {
        assert_eq!(
            ftui_render::TextDirection::from(TextDirection::Ltr),
            ftui_render::TextDirection::Ltr
        );
        assert_eq!(
            ftui_render::TextDirection::from(TextDirection::Rtl),
            ftui_render::TextDirection::Rtl
        );
        assert_eq!(
            TextDirection::from(ftui_render::TextDirection::Ltr),
            TextDirection::Ltr
        );
        assert_eq!(
            TextDirection::from(ftui_render::TextDirection::Rtl),
            TextDirection::Rtl
        );
    }
}
