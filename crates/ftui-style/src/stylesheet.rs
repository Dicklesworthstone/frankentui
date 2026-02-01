#![forbid(unsafe_code)]

//! StyleSheet registry for named styles.
//!
//! StyleSheet provides named style registration similar to CSS classes.
//! This enables themeable applications and consistent style reuse without
//! hardcoding colors.
//!
//! # Example
//! ```
//! use ftui_style::{Style, StyleSheet, StyleId};
//! use ftui_render::cell::PackedRgba;
//!
//! let mut sheet = StyleSheet::new();
//!
//! // Define named styles
//! sheet.define("error", Style::new().fg(PackedRgba::rgb(255, 0, 0)).bold());
//! sheet.define("warning", Style::new().fg(PackedRgba::rgb(255, 165, 0)));
//!
//! // Look up by name
//! let error_style = sheet.get("error").unwrap();
//!
//! // Compose multiple styles (later ones take precedence)
//! let composed = sheet.compose(&["base", "error"]);
//! ```

use crate::style::Style;
use ftui_render::cell::PackedRgba;
use std::collections::HashMap;
use std::sync::RwLock;

/// Identifier for a named style in a StyleSheet.
///
/// StyleId is a simple string-based identifier for named styles.
/// We use `&str` for lookups and `String` for storage to balance
/// ergonomics and performance.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StyleId(pub String);

impl StyleId {
    /// Create a new StyleId from a string.
    #[inline]
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// Get the name as a string slice.
    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for StyleId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for StyleId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl AsRef<str> for StyleId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// A registry of named styles for consistent theming.
///
/// StyleSheet allows defining styles by name and looking them up later.
/// This decouples visual appearance from widget logic, allowing themes
/// to override the stylesheet without changing widget code.
///
/// # Thread Safety
///
/// StyleSheet uses an internal RwLock for thread-safe read access
/// after initialization. Multiple readers can access styles concurrently.
#[derive(Debug, Default)]
pub struct StyleSheet {
    styles: RwLock<HashMap<String, Style>>,
}

impl StyleSheet {
    /// Create a new empty StyleSheet.
    #[inline]
    pub fn new() -> Self {
        Self {
            styles: RwLock::new(HashMap::new()),
        }
    }

    /// Create a StyleSheet with default semantic styles.
    ///
    /// This provides a base set of commonly-used style names:
    /// - `error`: Red, bold
    /// - `warning`: Orange/yellow
    /// - `info`: Blue
    /// - `success`: Green
    /// - `muted`: Gray/dim
    /// - `highlight`: Yellow background
    /// - `link`: Blue, underline
    #[must_use]
    pub fn with_defaults() -> Self {
        let sheet = Self::new();

        // Error: Red, bold
        sheet.define(
            "error",
            Style::new().fg(PackedRgba::rgb(255, 85, 85)).bold(),
        );

        // Warning: Orange/yellow
        sheet.define("warning", Style::new().fg(PackedRgba::rgb(255, 170, 0)));

        // Info: Blue
        sheet.define("info", Style::new().fg(PackedRgba::rgb(85, 170, 255)));

        // Success: Green
        sheet.define("success", Style::new().fg(PackedRgba::rgb(85, 255, 85)));

        // Muted: Gray, dim
        sheet.define(
            "muted",
            Style::new().fg(PackedRgba::rgb(128, 128, 128)).dim(),
        );

        // Highlight: Yellow background
        sheet.define(
            "highlight",
            Style::new()
                .bg(PackedRgba::rgb(255, 255, 0))
                .fg(PackedRgba::rgb(0, 0, 0)),
        );

        // Link: Blue, underline
        sheet.define(
            "link",
            Style::new().fg(PackedRgba::rgb(85, 170, 255)).underline(),
        );

        sheet
    }

    /// Define a named style.
    ///
    /// If a style with this name already exists, it is replaced.
    pub fn define(&self, name: impl Into<String>, style: Style) {
        let name = name.into();
        let mut styles = self.styles.write().expect("StyleSheet lock poisoned");
        styles.insert(name, style);
    }

    /// Remove a named style.
    ///
    /// Returns the removed style if it existed.
    pub fn remove(&self, name: &str) -> Option<Style> {
        let mut styles = self.styles.write().expect("StyleSheet lock poisoned");
        styles.remove(name)
    }

    /// Get a named style.
    ///
    /// Returns `None` if the style is not defined.
    pub fn get(&self, name: &str) -> Option<Style> {
        let styles = self.styles.read().expect("StyleSheet lock poisoned");
        styles.get(name).copied()
    }

    /// Get a named style, returning a default if not found.
    pub fn get_or_default(&self, name: &str) -> Style {
        self.get(name).unwrap_or_default()
    }

    /// Check if a style with the given name exists.
    pub fn contains(&self, name: &str) -> bool {
        let styles = self.styles.read().expect("StyleSheet lock poisoned");
        styles.contains_key(name)
    }

    /// Get the number of defined styles.
    pub fn len(&self) -> usize {
        let styles = self.styles.read().expect("StyleSheet lock poisoned");
        styles.len()
    }

    /// Check if the stylesheet is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get all style names.
    pub fn names(&self) -> Vec<String> {
        let styles = self.styles.read().expect("StyleSheet lock poisoned");
        styles.keys().cloned().collect()
    }

    /// Compose multiple styles by name, merging them in order.
    ///
    /// Styles are merged left-to-right, with later styles taking
    /// precedence over earlier ones for conflicting properties.
    ///
    /// Missing style names are silently ignored.
    ///
    /// # Example
    /// ```
    /// use ftui_style::{Style, StyleSheet};
    /// use ftui_render::cell::PackedRgba;
    ///
    /// let sheet = StyleSheet::new();
    /// sheet.define("base", Style::new().fg(PackedRgba::WHITE));
    /// sheet.define("bold", Style::new().bold());
    ///
    /// // Compose: base + bold = white text that's bold
    /// let composed = sheet.compose(&["base", "bold"]);
    /// ```
    pub fn compose(&self, names: &[&str]) -> Style {
        let styles = self.styles.read().expect("StyleSheet lock poisoned");
        let mut result = Style::default();

        for name in names {
            if let Some(style) = styles.get(*name) {
                result = style.merge(&result);
            }
        }

        result
    }

    /// Compose styles with fallback for missing names.
    ///
    /// Like `compose`, but returns `None` if any named style is missing.
    pub fn compose_strict(&self, names: &[&str]) -> Option<Style> {
        let styles = self.styles.read().expect("StyleSheet lock poisoned");
        let mut result = Style::default();

        for name in names {
            match styles.get(*name) {
                Some(style) => result = style.merge(&result),
                None => return None,
            }
        }

        Some(result)
    }

    /// Extend this stylesheet with styles from another.
    ///
    /// Styles from `other` override styles with the same name in `self`.
    pub fn extend(&self, other: &StyleSheet) {
        let other_styles = other.styles.read().expect("StyleSheet lock poisoned");
        let mut self_styles = self.styles.write().expect("StyleSheet lock poisoned");

        for (name, style) in other_styles.iter() {
            self_styles.insert(name.clone(), *style);
        }
    }

    /// Clear all styles from the stylesheet.
    pub fn clear(&self) {
        let mut styles = self.styles.write().expect("StyleSheet lock poisoned");
        styles.clear();
    }
}

impl Clone for StyleSheet {
    fn clone(&self) -> Self {
        let styles = self.styles.read().expect("StyleSheet lock poisoned");
        Self {
            styles: RwLock::new(styles.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::StyleFlags;

    #[test]
    fn new_stylesheet_is_empty() {
        let sheet = StyleSheet::new();
        assert!(sheet.is_empty());
        assert_eq!(sheet.len(), 0);
    }

    #[test]
    fn define_and_get_style() {
        let sheet = StyleSheet::new();
        let style = Style::new().fg(PackedRgba::rgb(255, 0, 0)).bold();

        sheet.define("error", style);

        assert!(!sheet.is_empty());
        assert_eq!(sheet.len(), 1);
        assert!(sheet.contains("error"));

        let retrieved = sheet.get("error").unwrap();
        assert_eq!(retrieved, style);
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let sheet = StyleSheet::new();
        assert!(sheet.get("nonexistent").is_none());
    }

    #[test]
    fn get_or_default_returns_default_for_missing() {
        let sheet = StyleSheet::new();
        let style = sheet.get_or_default("missing");
        assert!(style.is_empty());
    }

    #[test]
    fn define_replaces_existing() {
        let sheet = StyleSheet::new();

        sheet.define("test", Style::new().bold());
        assert!(sheet.get("test").unwrap().has_attr(StyleFlags::BOLD));

        sheet.define("test", Style::new().italic());
        let style = sheet.get("test").unwrap();
        assert!(!style.has_attr(StyleFlags::BOLD));
        assert!(style.has_attr(StyleFlags::ITALIC));
    }

    #[test]
    fn remove_style() {
        let sheet = StyleSheet::new();
        sheet.define("test", Style::new().bold());

        let removed = sheet.remove("test");
        assert!(removed.is_some());
        assert!(!sheet.contains("test"));

        let removed_again = sheet.remove("test");
        assert!(removed_again.is_none());
    }

    #[test]
    fn names_returns_all_style_names() {
        let sheet = StyleSheet::new();
        sheet.define("a", Style::new());
        sheet.define("b", Style::new());
        sheet.define("c", Style::new());

        let names = sheet.names();
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"a".to_string()));
        assert!(names.contains(&"b".to_string()));
        assert!(names.contains(&"c".to_string()));
    }

    #[test]
    fn compose_merges_styles() {
        let sheet = StyleSheet::new();
        sheet.define("base", Style::new().fg(PackedRgba::WHITE));
        sheet.define("bold", Style::new().bold());

        let composed = sheet.compose(&["base", "bold"]);

        assert_eq!(composed.fg, Some(PackedRgba::WHITE));
        assert!(composed.has_attr(StyleFlags::BOLD));
    }

    #[test]
    fn compose_later_wins_on_conflict() {
        let sheet = StyleSheet::new();
        let red = PackedRgba::rgb(255, 0, 0);
        let blue = PackedRgba::rgb(0, 0, 255);

        sheet.define("red", Style::new().fg(red));
        sheet.define("blue", Style::new().fg(blue));

        let composed = sheet.compose(&["red", "blue"]);
        assert_eq!(composed.fg, Some(blue));
    }

    #[test]
    fn compose_ignores_missing() {
        let sheet = StyleSheet::new();
        sheet.define("exists", Style::new().bold());

        let composed = sheet.compose(&["missing", "exists"]);
        assert!(composed.has_attr(StyleFlags::BOLD));
    }

    #[test]
    fn compose_strict_fails_on_missing() {
        let sheet = StyleSheet::new();
        sheet.define("exists", Style::new().bold());

        let result = sheet.compose_strict(&["exists", "missing"]);
        assert!(result.is_none());
    }

    #[test]
    fn compose_strict_succeeds_when_all_present() {
        let sheet = StyleSheet::new();
        sheet.define("a", Style::new().bold());
        sheet.define("b", Style::new().italic());

        let result = sheet.compose_strict(&["a", "b"]);
        assert!(result.is_some());

        let style = result.unwrap();
        assert!(style.has_attr(StyleFlags::BOLD));
        assert!(style.has_attr(StyleFlags::ITALIC));
    }

    #[test]
    fn with_defaults_has_semantic_styles() {
        let sheet = StyleSheet::with_defaults();

        assert!(sheet.contains("error"));
        assert!(sheet.contains("warning"));
        assert!(sheet.contains("info"));
        assert!(sheet.contains("success"));
        assert!(sheet.contains("muted"));
        assert!(sheet.contains("highlight"));
        assert!(sheet.contains("link"));

        // Check error is red and bold
        let error = sheet.get("error").unwrap();
        assert!(error.has_attr(StyleFlags::BOLD));
        assert!(error.fg.is_some());
    }

    #[test]
    fn extend_merges_stylesheets() {
        let sheet1 = StyleSheet::new();
        sheet1.define("a", Style::new().bold());

        let sheet2 = StyleSheet::new();
        sheet2.define("b", Style::new().italic());

        sheet1.extend(&sheet2);

        assert!(sheet1.contains("a"));
        assert!(sheet1.contains("b"));
    }

    #[test]
    fn extend_overrides_existing() {
        let sheet1 = StyleSheet::new();
        sheet1.define("test", Style::new().bold());

        let sheet2 = StyleSheet::new();
        sheet2.define("test", Style::new().italic());

        sheet1.extend(&sheet2);

        let style = sheet1.get("test").unwrap();
        assert!(!style.has_attr(StyleFlags::BOLD));
        assert!(style.has_attr(StyleFlags::ITALIC));
    }

    #[test]
    fn clear_removes_all_styles() {
        let sheet = StyleSheet::with_defaults();
        assert!(!sheet.is_empty());

        sheet.clear();
        assert!(sheet.is_empty());
    }

    #[test]
    fn clone_creates_independent_copy() {
        let sheet1 = StyleSheet::new();
        sheet1.define("test", Style::new().bold());

        let sheet2 = sheet1.clone();
        sheet1.define("other", Style::new());

        assert!(sheet1.contains("other"));
        assert!(!sheet2.contains("other"));
    }

    #[test]
    fn style_id_from_str() {
        let id: StyleId = "error".into();
        assert_eq!(id.as_str(), "error");
    }

    #[test]
    fn style_id_from_string() {
        let id: StyleId = String::from("error").into();
        assert_eq!(id.as_str(), "error");
    }

    #[test]
    fn style_id_equality() {
        let id1 = StyleId::new("error");
        let id2 = StyleId::new("error");
        let id3 = StyleId::new("warning");

        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
    }

    #[test]
    fn stylesheet_thread_safe_reads() {
        use std::sync::Arc;
        use std::thread;

        let sheet = Arc::new(StyleSheet::new());
        sheet.define("test", Style::new().bold());

        let handles: Vec<_> = (0..4)
            .map(|_| {
                let sheet = Arc::clone(&sheet);
                thread::spawn(move || {
                    for _ in 0..100 {
                        let _ = sheet.get("test");
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }
    }
}
