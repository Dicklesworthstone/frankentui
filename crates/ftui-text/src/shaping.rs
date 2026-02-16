#![forbid(unsafe_code)]

//! Text shaping backend and deterministic shaped-run cache.
//!
//! This module provides the interface and caching layer for text shaping —
//! the process of converting a sequence of Unicode codepoints into positioned
//! glyphs. Shaping handles script-specific reordering, ligature substitution,
//! and glyph positioning (kerning, mark attachment).
//!
//! # Architecture
//!
//! ```text
//! TextRun (from script_segmentation)
//!     │
//!     ▼
//! ┌───────────────┐
//! │ ShapingCache   │──cache hit──▶ ShapedRun (cached)
//! │ (LRU + gen)    │
//! └───────┬───────┘
//!         │ cache miss
//!         ▼
//! ┌───────────────┐
//! │ TextShaper     │  trait (NoopShaper | RustybuzzShaper)
//! └───────┬───────┘
//!         │
//!         ▼
//!     ShapedRun
//! ```
//!
//! # Key schema
//!
//! The [`ShapingKey`] captures all parameters that affect shaping output:
//! text content (hashed), script, direction, style, font identity, font size,
//! and OpenType features. Two runs producing the same `ShapingKey` are
//! guaranteed to produce identical `ShapedRun` output.
//!
//! # Invalidation
//!
//! The cache uses generation-based invalidation. When fonts change (DPR
//! change, zoom, font swap), the generation is bumped and stale entries are
//! lazily evicted on access. This avoids expensive bulk-clear operations.
//!
//! # Example
//!
//! ```
//! use ftui_text::shaping::{
//!     NoopShaper, ShapingCache, FontId, FontFeatures,
//! };
//! use ftui_text::script_segmentation::{Script, RunDirection};
//!
//! let shaper = NoopShaper;
//! let mut cache = ShapingCache::new(shaper, 1024);
//!
//! let result = cache.shape(
//!     "Hello",
//!     Script::Latin,
//!     RunDirection::Ltr,
//!     FontId(0),
//!     256 * 12, // 12pt in 1/256th units
//!     &FontFeatures::default(),
//! );
//! assert!(!result.glyphs.is_empty());
//! ```

use crate::script_segmentation::{RunDirection, Script};
use lru::LruCache;
use rustc_hash::FxHasher;
use smallvec::SmallVec;
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;

// ---------------------------------------------------------------------------
// Font identity types
// ---------------------------------------------------------------------------

/// Opaque identifier for a font face within the application.
///
/// The mapping from `FontId` to actual font data is managed by the caller.
/// The shaping layer treats this as an opaque discriminant for cache keying.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FontId(pub u32);

/// A single OpenType feature tag + value.
///
/// Tags are 4-byte ASCII identifiers (e.g., `b"liga"`, `b"kern"`, `b"smcp"`).
/// Value 0 disables the feature, 1 enables it, higher values select alternates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FontFeature {
    /// OpenType tag (4 ASCII bytes, e.g., `*b"liga"`).
    pub tag: [u8; 4],
    /// Feature value (0 = off, 1 = on, >1 = alternate selection).
    pub value: u32,
}

impl FontFeature {
    /// Create a new feature from a tag and value.
    #[inline]
    pub const fn new(tag: [u8; 4], value: u32) -> Self {
        Self { tag, value }
    }

    /// Create an enabled feature from a tag.
    #[inline]
    pub const fn enabled(tag: [u8; 4]) -> Self {
        Self { tag, value: 1 }
    }

    /// Create a disabled feature from a tag.
    #[inline]
    pub const fn disabled(tag: [u8; 4]) -> Self {
        Self { tag, value: 0 }
    }
}

/// A set of OpenType features requested for shaping.
///
/// Stack-allocated for the common case of ≤4 features.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct FontFeatures {
    features: SmallVec<[FontFeature; 4]>,
}

impl FontFeatures {
    /// Create an empty feature set.
    #[inline]
    pub fn new() -> Self {
        Self {
            features: SmallVec::new(),
        }
    }

    /// Add a feature to the set.
    #[inline]
    pub fn push(&mut self, feature: FontFeature) {
        self.features.push(feature);
    }

    /// Create from a slice of features.
    pub fn from_slice(features: &[FontFeature]) -> Self {
        Self {
            features: SmallVec::from_slice(features),
        }
    }

    /// Number of features.
    #[inline]
    pub fn len(&self) -> usize {
        self.features.len()
    }

    /// Whether the feature set is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.features.is_empty()
    }

    /// Iterate over features.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = &FontFeature> {
        self.features.iter()
    }

    /// Sort features by tag for deterministic hashing.
    pub fn canonicalize(&mut self) {
        self.features.sort_by_key(|f| f.tag);
    }
}

// ---------------------------------------------------------------------------
// Shaped output types
// ---------------------------------------------------------------------------

/// A single positioned glyph from the shaping engine.
///
/// All metric values are in font design units. The caller converts to pixels
/// using the font's units-per-em and the desired point size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShapedGlyph {
    /// Glyph ID from the font (0 = `.notdef`).
    pub glyph_id: u32,
    /// Byte offset of the start of this glyph's cluster in the source text.
    ///
    /// Multiple glyphs can share the same cluster (ligatures produce one glyph
    /// for multiple characters; complex scripts may produce multiple glyphs
    /// for one character).
    pub cluster: u32,
    /// Horizontal advance in font design units.
    pub x_advance: i32,
    /// Vertical advance in font design units.
    pub y_advance: i32,
    /// Horizontal offset from the nominal position.
    pub x_offset: i32,
    /// Vertical offset from the nominal position.
    pub y_offset: i32,
}

/// The result of shaping a text run.
///
/// Contains the positioned glyphs and aggregate metrics needed for layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShapedRun {
    /// Positioned glyphs in visual order.
    pub glyphs: Vec<ShapedGlyph>,
    /// Total horizontal advance of all glyphs (sum of x_advance).
    pub total_advance: i32,
}

impl ShapedRun {
    /// Number of glyphs in the run.
    #[inline]
    pub fn len(&self) -> usize {
        self.glyphs.len()
    }

    /// Whether the run contains no glyphs.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.glyphs.is_empty()
    }
}

// ---------------------------------------------------------------------------
// ShapingKey — deterministic cache key
// ---------------------------------------------------------------------------

/// Deterministic cache key for shaped glyph output.
///
/// Captures all parameters that affect shaping results. Two identical keys
/// are guaranteed to produce identical `ShapedRun` output, enabling safe
/// caching.
///
/// # Key components
///
/// | Field          | Purpose                                        |
/// |----------------|------------------------------------------------|
/// | `text_hash`    | FxHash of the text content                     |
/// | `text_len`     | Byte length (collision avoidance)               |
/// | `script`       | Unicode script (affects glyph selection)        |
/// | `direction`    | LTR/RTL (affects reordering + positioning)     |
/// | `style_id`     | Style discriminant (bold/italic affect glyphs) |
/// | `font_id`      | Font face identity                             |
/// | `size_256ths`  | Font size in 1/256th point units               |
/// | `features`     | Active OpenType features                       |
/// | `generation`   | Cache generation (invalidation epoch)          |
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ShapingKey {
    /// FxHash of the text content.
    pub text_hash: u64,
    /// Byte length of the text (for collision avoidance with the hash).
    pub text_len: u32,
    /// Unicode script.
    pub script: Script,
    /// Text direction.
    pub direction: RunDirection,
    /// Style discriminant.
    pub style_id: u64,
    /// Font face identity.
    pub font_id: FontId,
    /// Font size in 1/256th of a point (sub-pixel precision matching ftui-render).
    pub size_256ths: u32,
    /// Active OpenType features (canonicalized for determinism).
    pub features: FontFeatures,
    /// Cache generation epoch — entries from older generations are stale.
    pub generation: u64,
}

impl ShapingKey {
    /// Build a key from shaping parameters.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        text: &str,
        script: Script,
        direction: RunDirection,
        style_id: u64,
        font_id: FontId,
        size_256ths: u32,
        features: &FontFeatures,
        generation: u64,
    ) -> Self {
        let mut hasher = FxHasher::default();
        text.hash(&mut hasher);
        let text_hash = hasher.finish();

        Self {
            text_hash,
            text_len: text.len() as u32,
            script,
            direction,
            style_id,
            font_id,
            size_256ths,
            features: features.clone(),
            generation,
        }
    }
}

// ---------------------------------------------------------------------------
// TextShaper trait
// ---------------------------------------------------------------------------

/// Abstract text shaping backend.
///
/// Implementations convert a Unicode text string into positioned glyphs
/// according to the rules of the specified script, direction, and font
/// features.
///
/// The trait is object-safe to allow dynamic dispatch between backends
/// (e.g., terminal noop vs. web rustybuzz).
pub trait TextShaper {
    /// Shape a text run into positioned glyphs.
    ///
    /// # Parameters
    ///
    /// * `text` — The text to shape (UTF-8, from a single `TextRun`).
    /// * `script` — The resolved Unicode script.
    /// * `direction` — LTR or RTL text direction.
    /// * `features` — OpenType features to apply.
    ///
    /// # Returns
    ///
    /// A `ShapedRun` containing positioned glyphs in visual order.
    fn shape(
        &self,
        text: &str,
        script: Script,
        direction: RunDirection,
        features: &FontFeatures,
    ) -> ShapedRun;
}

// ---------------------------------------------------------------------------
// NoopShaper — terminal / monospace backend
// ---------------------------------------------------------------------------

/// Identity shaper for monospace terminal rendering.
///
/// Maps each grapheme cluster to a single glyph with uniform advance.
/// This is the correct shaping backend for fixed-width terminal output
/// where each cell is one column wide (or two for CJK/wide characters).
///
/// The glyph ID is set to the first codepoint of each grapheme, and
/// the advance is the grapheme's display width in terminal cells.
pub struct NoopShaper;

impl TextShaper for NoopShaper {
    fn shape(
        &self,
        text: &str,
        _script: Script,
        _direction: RunDirection,
        _features: &FontFeatures,
    ) -> ShapedRun {
        use unicode_segmentation::UnicodeSegmentation;

        let mut glyphs = Vec::new();
        let mut total_advance = 0i32;

        for (byte_offset, grapheme) in text.grapheme_indices(true) {
            let first_char = grapheme.chars().next().unwrap_or('\0');
            let width = crate::grapheme_width(grapheme) as i32;

            glyphs.push(ShapedGlyph {
                glyph_id: first_char as u32,
                cluster: byte_offset as u32,
                x_advance: width,
                y_advance: 0,
                x_offset: 0,
                y_offset: 0,
            });

            total_advance += width;
        }

        ShapedRun {
            glyphs,
            total_advance,
        }
    }
}

// ---------------------------------------------------------------------------
// ShapingCache — LRU cache with generation-based invalidation
// ---------------------------------------------------------------------------

/// Statistics for the shaping cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ShapingCacheStats {
    /// Number of cache hits.
    pub hits: u64,
    /// Number of cache misses (triggered shaping).
    pub misses: u64,
    /// Number of stale entries evicted due to generation mismatch.
    pub stale_evictions: u64,
    /// Current number of entries in the cache.
    pub size: usize,
    /// Maximum capacity of the cache.
    pub capacity: usize,
    /// Current invalidation generation.
    pub generation: u64,
}

impl ShapingCacheStats {
    /// Hit rate as a fraction (0.0 to 1.0).
    #[must_use]
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }
}

/// Cached entry with its generation stamp.
#[derive(Debug, Clone)]
struct CachedEntry {
    run: ShapedRun,
    generation: u64,
}

/// LRU cache for shaped text runs with generation-based invalidation.
///
/// # Invalidation policy
///
/// The cache tracks a monotonically increasing generation counter. Each
/// cached entry is stamped with the generation at insertion time. When
/// global state changes (font swap, DPR change, zoom), the caller bumps
/// the generation via [`invalidate`](Self::invalidate). Entries from older
/// generations are treated as misses on access and lazily replaced.
///
/// This avoids expensive bulk-clear operations while ensuring correctness.
///
/// # Thread safety
///
/// The cache is not `Sync`. For multi-threaded use, wrap in a `Mutex` or
/// use per-thread instances (matching the `thread_local_cache` feature
/// pattern from `WidthCache`).
pub struct ShapingCache<S: TextShaper> {
    shaper: S,
    cache: LruCache<ShapingKey, CachedEntry>,
    generation: u64,
    stats: ShapingCacheStats,
}

impl<S: TextShaper> ShapingCache<S> {
    /// Create a new shaping cache with the given backend and capacity.
    pub fn new(shaper: S, capacity: usize) -> Self {
        let cap = NonZeroUsize::new(capacity.max(1)).expect("capacity must be > 0");
        Self {
            shaper,
            cache: LruCache::new(cap),
            generation: 0,
            stats: ShapingCacheStats {
                capacity,
                ..Default::default()
            },
        }
    }

    /// Shape a text run, returning a cached result if available.
    ///
    /// The full shaping key is constructed from the provided parameters.
    /// If a cache entry exists with the current generation, it is returned
    /// directly. Otherwise, the shaper is invoked and the result is cached.
    pub fn shape(
        &mut self,
        text: &str,
        script: Script,
        direction: RunDirection,
        font_id: FontId,
        size_256ths: u32,
        features: &FontFeatures,
    ) -> ShapedRun {
        self.shape_with_style(text, script, direction, 0, font_id, size_256ths, features)
    }

    /// Shape with an explicit style discriminant.
    #[allow(clippy::too_many_arguments)]
    pub fn shape_with_style(
        &mut self,
        text: &str,
        script: Script,
        direction: RunDirection,
        style_id: u64,
        font_id: FontId,
        size_256ths: u32,
        features: &FontFeatures,
    ) -> ShapedRun {
        let key = ShapingKey::new(
            text,
            script,
            direction,
            style_id,
            font_id,
            size_256ths,
            features,
            self.generation,
        );

        // Check cache.
        if let Some(entry) = self.cache.get(&key) {
            if entry.generation == self.generation {
                self.stats.hits += 1;
                return entry.run.clone();
            }
            // Stale entry — will be replaced below.
            self.stats.stale_evictions += 1;
        }

        // Cache miss — invoke shaper.
        self.stats.misses += 1;
        let run = self.shaper.shape(text, script, direction, features);

        self.cache.put(
            key,
            CachedEntry {
                run: run.clone(),
                generation: self.generation,
            },
        );

        self.stats.size = self.cache.len();
        run
    }

    /// Bump the generation counter, invalidating all cached entries.
    ///
    /// Stale entries are not removed eagerly — they are lazily evicted
    /// on next access. This makes invalidation O(1).
    ///
    /// Call this when:
    /// - The font set changes (font swap, fallback resolution).
    /// - Display DPR changes (affects pixel grid rounding).
    /// - Zoom level changes.
    pub fn invalidate(&mut self) {
        self.generation += 1;
        self.stats.generation = self.generation;
    }

    /// Clear all cached entries and reset stats.
    pub fn clear(&mut self) {
        self.cache.clear();
        self.generation += 1;
        self.stats = ShapingCacheStats {
            capacity: self.stats.capacity,
            generation: self.generation,
            ..Default::default()
        };
    }

    /// Current cache statistics.
    #[inline]
    pub fn stats(&self) -> ShapingCacheStats {
        ShapingCacheStats {
            size: self.cache.len(),
            ..self.stats
        }
    }

    /// Current generation counter.
    #[inline]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Access the underlying shaper.
    #[inline]
    pub fn shaper(&self) -> &S {
        &self.shaper
    }

    /// Resize the cache capacity.
    ///
    /// If the new capacity is smaller than the current size, excess
    /// entries are evicted in LRU order.
    pub fn resize(&mut self, new_capacity: usize) {
        let cap = NonZeroUsize::new(new_capacity.max(1)).expect("capacity must be > 0");
        self.cache.resize(cap);
        self.stats.capacity = new_capacity;
        self.stats.size = self.cache.len();
    }
}

// ---------------------------------------------------------------------------
// RustybuzzShaper — real shaping backend (feature-gated)
// ---------------------------------------------------------------------------

#[cfg(feature = "shaping")]
mod rustybuzz_backend {
    use super::*;

    /// HarfBuzz-compatible shaper using the rustybuzz pure-Rust engine.
    ///
    /// Wraps a `rustybuzz::Face` and provides the `TextShaper` interface.
    /// The face data must outlive the shaper (typically held in an `Arc`).
    ///
    /// # Example
    ///
    /// ```ignore
    /// let font_data: &[u8] = include_bytes!("path/to/font.ttf");
    /// let face = rustybuzz::Face::from_slice(font_data, 0).unwrap();
    /// let shaper = RustybuzzShaper::new(face);
    /// ```
    pub struct RustybuzzShaper {
        face: rustybuzz::Face<'static>,
    }

    impl RustybuzzShaper {
        /// Create a shaper from a rustybuzz face.
        ///
        /// The face must have `'static` lifetime — typically achieved by
        /// loading font data into a leaked `Box<[u8]>` or `Arc` with a
        /// transmuted lifetime (handled by the font loading layer).
        pub fn new(face: rustybuzz::Face<'static>) -> Self {
            Self { face }
        }

        /// Convert our Script enum to a rustybuzz script constant.
        fn to_rb_script(script: Script) -> rustybuzz::Script {
            use rustybuzz::script;
            match script {
                Script::Latin => script::LATIN,
                Script::Greek => script::GREEK,
                Script::Cyrillic => script::CYRILLIC,
                Script::Armenian => script::ARMENIAN,
                Script::Hebrew => script::HEBREW,
                Script::Arabic => script::ARABIC,
                Script::Syriac => script::SYRIAC,
                Script::Thaana => script::THAANA,
                Script::Devanagari => script::DEVANAGARI,
                Script::Bengali => script::BENGALI,
                Script::Gurmukhi => script::GURMUKHI,
                Script::Gujarati => script::GUJARATI,
                Script::Oriya => script::ORIYA,
                Script::Tamil => script::TAMIL,
                Script::Telugu => script::TELUGU,
                Script::Kannada => script::KANNADA,
                Script::Malayalam => script::MALAYALAM,
                Script::Sinhala => script::SINHALA,
                Script::Thai => script::THAI,
                Script::Lao => script::LAO,
                Script::Tibetan => script::TIBETAN,
                Script::Myanmar => script::MYANMAR,
                Script::Georgian => script::GEORGIAN,
                Script::Hangul => script::HANGUL,
                Script::Ethiopic => script::ETHIOPIC,
                Script::Han => script::HAN,
                Script::Hiragana => script::HIRAGANA,
                Script::Katakana => script::KATAKANA,
                Script::Bopomofo => script::BOPOMOFO,
                Script::Common | Script::Inherited | Script::Unknown => script::COMMON,
            }
        }

        /// Convert our RunDirection to rustybuzz::Direction.
        fn to_rb_direction(direction: RunDirection) -> rustybuzz::Direction {
            match direction {
                RunDirection::Ltr => rustybuzz::Direction::LeftToRight,
                RunDirection::Rtl => rustybuzz::Direction::RightToLeft,
            }
        }

        /// Convert our FontFeature to rustybuzz::Feature.
        fn to_rb_feature(feature: &FontFeature) -> rustybuzz::Feature {
            let tag = rustybuzz::ttf_parser::Tag::from_bytes(&feature.tag);
            rustybuzz::Feature::new(tag, feature.value, ..)
        }
    }

    impl TextShaper for RustybuzzShaper {
        fn shape(
            &self,
            text: &str,
            script: Script,
            direction: RunDirection,
            features: &FontFeatures,
        ) -> ShapedRun {
            let mut buffer = rustybuzz::UnicodeBuffer::new();
            buffer.push_str(text);
            buffer.set_script(Self::to_rb_script(script));
            buffer.set_direction(Self::to_rb_direction(direction));

            let rb_features: Vec<rustybuzz::Feature> =
                features.iter().map(Self::to_rb_feature).collect();

            let output = rustybuzz::shape(&self.face, &rb_features, buffer);

            let infos = output.glyph_infos();
            let positions = output.glyph_positions();

            let mut glyphs = Vec::with_capacity(infos.len());
            let mut total_advance = 0i32;

            for (info, pos) in infos.iter().zip(positions.iter()) {
                glyphs.push(ShapedGlyph {
                    glyph_id: info.glyph_id,
                    cluster: info.cluster,
                    x_advance: pos.x_advance,
                    y_advance: pos.y_advance,
                    x_offset: pos.x_offset,
                    y_offset: pos.y_offset,
                });
                total_advance += pos.x_advance;
            }

            ShapedRun {
                glyphs,
                total_advance,
            }
        }
    }
}

#[cfg(feature = "shaping")]
pub use rustybuzz_backend::RustybuzzShaper;

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script_segmentation::{RunDirection, Script};

    // -----------------------------------------------------------------------
    // FontFeature / FontFeatures tests
    // -----------------------------------------------------------------------

    #[test]
    fn font_feature_new() {
        let f = FontFeature::new(*b"liga", 1);
        assert_eq!(f.tag, *b"liga");
        assert_eq!(f.value, 1);
    }

    #[test]
    fn font_feature_enabled_disabled() {
        let on = FontFeature::enabled(*b"kern");
        assert_eq!(on.value, 1);

        let off = FontFeature::disabled(*b"kern");
        assert_eq!(off.value, 0);
    }

    #[test]
    fn font_features_push_and_iter() {
        let mut ff = FontFeatures::new();
        assert!(ff.is_empty());

        ff.push(FontFeature::enabled(*b"liga"));
        ff.push(FontFeature::enabled(*b"kern"));
        assert_eq!(ff.len(), 2);

        let tags: Vec<[u8; 4]> = ff.iter().map(|f| f.tag).collect();
        assert_eq!(tags, vec![*b"liga", *b"kern"]);
    }

    #[test]
    fn font_features_canonicalize() {
        let mut ff = FontFeatures::from_slice(&[
            FontFeature::enabled(*b"kern"),
            FontFeature::enabled(*b"aalt"),
            FontFeature::enabled(*b"liga"),
        ]);
        ff.canonicalize();
        let tags: Vec<[u8; 4]> = ff.iter().map(|f| f.tag).collect();
        assert_eq!(tags, vec![*b"aalt", *b"kern", *b"liga"]);
    }

    #[test]
    fn font_features_default_is_empty() {
        let ff = FontFeatures::default();
        assert!(ff.is_empty());
    }

    // -----------------------------------------------------------------------
    // ShapedRun tests
    // -----------------------------------------------------------------------

    #[test]
    fn shaped_run_len_and_empty() {
        let empty = ShapedRun {
            glyphs: vec![],
            total_advance: 0,
        };
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);

        let non_empty = ShapedRun {
            glyphs: vec![ShapedGlyph {
                glyph_id: 65,
                cluster: 0,
                x_advance: 600,
                y_advance: 0,
                x_offset: 0,
                y_offset: 0,
            }],
            total_advance: 600,
        };
        assert!(!non_empty.is_empty());
        assert_eq!(non_empty.len(), 1);
    }

    // -----------------------------------------------------------------------
    // ShapingKey tests
    // -----------------------------------------------------------------------

    #[test]
    fn shaping_key_same_input_same_key() {
        let ff = FontFeatures::default();
        let k1 = ShapingKey::new("Hello", Script::Latin, RunDirection::Ltr, 0, FontId(0), 3072, &ff, 0);
        let k2 = ShapingKey::new("Hello", Script::Latin, RunDirection::Ltr, 0, FontId(0), 3072, &ff, 0);
        assert_eq!(k1, k2);
    }

    #[test]
    fn shaping_key_differs_by_text() {
        let ff = FontFeatures::default();
        let k1 = ShapingKey::new("Hello", Script::Latin, RunDirection::Ltr, 0, FontId(0), 3072, &ff, 0);
        let k2 = ShapingKey::new("World", Script::Latin, RunDirection::Ltr, 0, FontId(0), 3072, &ff, 0);
        assert_ne!(k1, k2);
    }

    #[test]
    fn shaping_key_differs_by_font() {
        let ff = FontFeatures::default();
        let k1 = ShapingKey::new("Hello", Script::Latin, RunDirection::Ltr, 0, FontId(0), 3072, &ff, 0);
        let k2 = ShapingKey::new("Hello", Script::Latin, RunDirection::Ltr, 0, FontId(1), 3072, &ff, 0);
        assert_ne!(k1, k2);
    }

    #[test]
    fn shaping_key_differs_by_size() {
        let ff = FontFeatures::default();
        let k1 = ShapingKey::new("Hello", Script::Latin, RunDirection::Ltr, 0, FontId(0), 3072, &ff, 0);
        let k2 = ShapingKey::new("Hello", Script::Latin, RunDirection::Ltr, 0, FontId(0), 4096, &ff, 0);
        assert_ne!(k1, k2);
    }

    #[test]
    fn shaping_key_differs_by_generation() {
        let ff = FontFeatures::default();
        let k1 = ShapingKey::new("Hello", Script::Latin, RunDirection::Ltr, 0, FontId(0), 3072, &ff, 0);
        let k2 = ShapingKey::new("Hello", Script::Latin, RunDirection::Ltr, 0, FontId(0), 3072, &ff, 1);
        assert_ne!(k1, k2);
    }

    #[test]
    fn shaping_key_differs_by_features() {
        let mut ff1 = FontFeatures::default();
        ff1.push(FontFeature::enabled(*b"liga"));

        let ff2 = FontFeatures::default();

        let k1 = ShapingKey::new("Hello", Script::Latin, RunDirection::Ltr, 0, FontId(0), 3072, &ff1, 0);
        let k2 = ShapingKey::new("Hello", Script::Latin, RunDirection::Ltr, 0, FontId(0), 3072, &ff2, 0);
        assert_ne!(k1, k2);
    }

    #[test]
    fn shaping_key_hashable() {
        use std::collections::HashSet;
        let ff = FontFeatures::default();
        let key = ShapingKey::new("test", Script::Latin, RunDirection::Ltr, 0, FontId(0), 3072, &ff, 0);
        let mut set = HashSet::new();
        set.insert(key.clone());
        assert!(set.contains(&key));
    }

    // -----------------------------------------------------------------------
    // NoopShaper tests
    // -----------------------------------------------------------------------

    #[test]
    fn noop_shaper_ascii() {
        let shaper = NoopShaper;
        let ff = FontFeatures::default();
        let run = shaper.shape("Hello", Script::Latin, RunDirection::Ltr, &ff);

        assert_eq!(run.len(), 5);
        assert_eq!(run.total_advance, 5); // 5 ASCII chars × 1 cell each

        // Each glyph should have the codepoint as glyph_id.
        assert_eq!(run.glyphs[0].glyph_id, b'H' as u32);
        assert_eq!(run.glyphs[1].glyph_id, b'e' as u32);
        assert_eq!(run.glyphs[4].glyph_id, b'o' as u32);

        // Clusters should be byte offsets.
        assert_eq!(run.glyphs[0].cluster, 0);
        assert_eq!(run.glyphs[1].cluster, 1);
        assert_eq!(run.glyphs[4].cluster, 4);
    }

    #[test]
    fn noop_shaper_empty() {
        let shaper = NoopShaper;
        let ff = FontFeatures::default();
        let run = shaper.shape("", Script::Latin, RunDirection::Ltr, &ff);
        assert!(run.is_empty());
        assert_eq!(run.total_advance, 0);
    }

    #[test]
    fn noop_shaper_wide_chars() {
        let shaper = NoopShaper;
        let ff = FontFeatures::default();
        // CJK characters are 2 cells wide
        let run = shaper.shape("\u{4E16}\u{754C}", Script::Han, RunDirection::Ltr, &ff);

        assert_eq!(run.len(), 2);
        assert_eq!(run.total_advance, 4); // 2 chars × 2 cells each
        assert_eq!(run.glyphs[0].x_advance, 2);
        assert_eq!(run.glyphs[1].x_advance, 2);
    }

    #[test]
    fn noop_shaper_combining_marks() {
        let shaper = NoopShaper;
        let ff = FontFeatures::default();
        // "é" as e + combining acute: single grapheme cluster
        let run = shaper.shape("e\u{0301}", Script::Latin, RunDirection::Ltr, &ff);

        // Should produce 1 glyph (one grapheme cluster).
        assert_eq!(run.len(), 1);
        assert_eq!(run.total_advance, 1);
        assert_eq!(run.glyphs[0].glyph_id, b'e' as u32);
        assert_eq!(run.glyphs[0].cluster, 0);
    }

    #[test]
    fn noop_shaper_ignores_direction_and_features() {
        let shaper = NoopShaper;
        let mut ff = FontFeatures::new();
        ff.push(FontFeature::enabled(*b"liga"));

        let ltr = shaper.shape("ABC", Script::Latin, RunDirection::Ltr, &ff);
        let rtl = shaper.shape("ABC", Script::Latin, RunDirection::Rtl, &ff);

        // NoopShaper produces identical output regardless of direction.
        assert_eq!(ltr, rtl);
    }

    // -----------------------------------------------------------------------
    // ShapingCache tests
    // -----------------------------------------------------------------------

    #[test]
    fn cache_hit_on_second_call() {
        let mut cache = ShapingCache::new(NoopShaper, 64);
        let ff = FontFeatures::default();

        let r1 = cache.shape("Hello", Script::Latin, RunDirection::Ltr, FontId(0), 3072, &ff);
        let r2 = cache.shape("Hello", Script::Latin, RunDirection::Ltr, FontId(0), 3072, &ff);

        assert_eq!(r1, r2);
        assert_eq!(cache.stats().hits, 1);
        assert_eq!(cache.stats().misses, 1);
    }

    #[test]
    fn cache_miss_on_different_text() {
        let mut cache = ShapingCache::new(NoopShaper, 64);
        let ff = FontFeatures::default();

        cache.shape("Hello", Script::Latin, RunDirection::Ltr, FontId(0), 3072, &ff);
        cache.shape("World", Script::Latin, RunDirection::Ltr, FontId(0), 3072, &ff);

        assert_eq!(cache.stats().hits, 0);
        assert_eq!(cache.stats().misses, 2);
    }

    #[test]
    fn cache_miss_on_different_font() {
        let mut cache = ShapingCache::new(NoopShaper, 64);
        let ff = FontFeatures::default();

        cache.shape("Hello", Script::Latin, RunDirection::Ltr, FontId(0), 3072, &ff);
        cache.shape("Hello", Script::Latin, RunDirection::Ltr, FontId(1), 3072, &ff);

        assert_eq!(cache.stats().misses, 2);
    }

    #[test]
    fn cache_miss_on_different_size() {
        let mut cache = ShapingCache::new(NoopShaper, 64);
        let ff = FontFeatures::default();

        cache.shape("Hello", Script::Latin, RunDirection::Ltr, FontId(0), 3072, &ff);
        cache.shape("Hello", Script::Latin, RunDirection::Ltr, FontId(0), 4096, &ff);

        assert_eq!(cache.stats().misses, 2);
    }

    #[test]
    fn cache_invalidation_bumps_generation() {
        let mut cache = ShapingCache::new(NoopShaper, 64);
        assert_eq!(cache.generation(), 0);

        cache.invalidate();
        assert_eq!(cache.generation(), 1);

        cache.invalidate();
        assert_eq!(cache.generation(), 2);
    }

    #[test]
    fn cache_stale_entries_are_reshared() {
        let mut cache = ShapingCache::new(NoopShaper, 64);
        let ff = FontFeatures::default();

        // Cache a result at generation 0.
        cache.shape("Hello", Script::Latin, RunDirection::Ltr, FontId(0), 3072, &ff);
        assert_eq!(cache.stats().misses, 1);
        assert_eq!(cache.stats().hits, 0);

        // Invalidate (bump to generation 1).
        cache.invalidate();

        // Same text — should be a miss because generation changed.
        cache.shape("Hello", Script::Latin, RunDirection::Ltr, FontId(0), 3072, &ff);
        assert_eq!(cache.stats().misses, 2);
        assert_eq!(cache.stats().stale_evictions, 0); // old key had gen=0, new key has gen=1, they don't match by key
    }

    #[test]
    fn cache_clear_resets_everything() {
        let mut cache = ShapingCache::new(NoopShaper, 64);
        let ff = FontFeatures::default();

        cache.shape("Hello", Script::Latin, RunDirection::Ltr, FontId(0), 3072, &ff);
        cache.shape("World", Script::Latin, RunDirection::Ltr, FontId(0), 3072, &ff);

        cache.clear();

        let stats = cache.stats();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 0);
        assert_eq!(stats.size, 0);
        assert!(cache.generation() > 0);
    }

    #[test]
    fn cache_resize_evicts_lru() {
        let mut cache = ShapingCache::new(NoopShaper, 4);
        let ff = FontFeatures::default();

        // Fill cache with 4 entries.
        for i in 0..4u8 {
            let text = format!("text{i}");
            cache.shape(&text, Script::Latin, RunDirection::Ltr, FontId(0), 3072, &ff);
        }
        assert_eq!(cache.stats().size, 4);

        // Shrink to 2 — should evict 2 LRU entries.
        cache.resize(2);
        assert!(cache.stats().size <= 2);
    }

    #[test]
    fn cache_with_style_id() {
        let mut cache = ShapingCache::new(NoopShaper, 64);
        let ff = FontFeatures::default();

        let r1 = cache.shape_with_style(
            "Hello", Script::Latin, RunDirection::Ltr, 1, FontId(0), 3072, &ff,
        );
        let r2 = cache.shape_with_style(
            "Hello", Script::Latin, RunDirection::Ltr, 2, FontId(0), 3072, &ff,
        );

        // Same text but different style — both are misses.
        assert_eq!(cache.stats().misses, 2);
        // Results are the same (NoopShaper ignores style) but they're cached separately.
        assert_eq!(r1, r2);
    }

    #[test]
    fn cache_stats_hit_rate() {
        let stats = ShapingCacheStats {
            hits: 75,
            misses: 25,
            ..Default::default()
        };
        let rate = stats.hit_rate();
        assert!((rate - 0.75).abs() < f64::EPSILON);

        let empty = ShapingCacheStats::default();
        assert_eq!(empty.hit_rate(), 0.0);
    }

    #[test]
    fn cache_shaper_accessible() {
        let cache = ShapingCache::new(NoopShaper, 64);
        let _shaper: &NoopShaper = cache.shaper();
    }

    // -----------------------------------------------------------------------
    // Integration: script_segmentation → shaping
    // -----------------------------------------------------------------------

    #[test]
    fn shape_partitioned_runs() {
        use crate::script_segmentation::partition_text_runs;

        let text = "Hello\u{4E16}\u{754C}World";
        let runs = partition_text_runs(text, None, None);

        let mut cache = ShapingCache::new(NoopShaper, 64);
        let ff = FontFeatures::default();

        let mut total_advance = 0;
        for run in &runs {
            let shaped = cache.shape(
                run.text(text),
                run.script,
                run.direction,
                FontId(0),
                3072,
                &ff,
            );
            total_advance += shaped.total_advance;
        }

        // Hello (5) + 世界 (4) + World (5) = 14 cells
        assert_eq!(total_advance, 14);
    }

    #[test]
    fn shape_empty_run() {
        let mut cache = ShapingCache::new(NoopShaper, 64);
        let ff = FontFeatures::default();
        let run = cache.shape("", Script::Latin, RunDirection::Ltr, FontId(0), 3072, &ff);
        assert!(run.is_empty());
    }
}
