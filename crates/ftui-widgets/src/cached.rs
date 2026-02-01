#![forbid(unsafe_code)]

//! Cached widget wrapper with manual invalidation and optional cache keys.

use crate::{StatefulWidget, Widget};
use ftui_core::geometry::Rect;
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use ftui_render::frame::Frame;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::mem::size_of;

#[cfg(feature = "tracing")]
use tracing::{debug, trace};

/// Cache key strategy for a widget.
pub trait CacheKey<W> {
    /// Return a cache key for the widget, or `None` to disable key-based invalidation.
    fn cache_key(&self, widget: &W) -> Option<u64>;
}

/// No cache key: invalidation is manual only.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoCacheKey;

impl<W> CacheKey<W> for NoCacheKey {
    fn cache_key(&self, _widget: &W) -> Option<u64> {
        None
    }
}

/// Hash-based cache key using `std::hash::Hash`.
#[derive(Debug, Clone, Copy, Default)]
pub struct HashKey;

impl<W: Hash> CacheKey<W> for HashKey {
    fn cache_key(&self, widget: &W) -> Option<u64> {
        Some(hash_value(widget))
    }
}

/// Custom key function wrapper.
#[derive(Debug, Clone, Copy)]
pub struct FnKey<F>(pub F);

impl<W, F: Fn(&W) -> u64> CacheKey<W> for FnKey<F> {
    fn cache_key(&self, widget: &W) -> Option<u64> {
        Some((self.0)(widget))
    }
}

/// Cached widget wrapper.
///
/// Use with [`CachedWidgetState`] via the [`StatefulWidget`] trait.
pub struct CachedWidget<W, K = NoCacheKey> {
    inner: W,
    key: K,
}

/// Internal cached buffer.
#[derive(Debug, Clone)]
struct CachedBuffer {
    buffer: Buffer,
}

/// State for a cached widget.
#[derive(Debug, Clone, Default)]
pub struct CachedWidgetState {
    cache: Option<CachedBuffer>,
    last_area: Option<Rect>,
    dirty: bool,
    last_key: Option<u64>,
}

#[cfg(feature = "tracing")]
#[derive(Debug, Clone, Copy)]
enum CacheMissReason {
    Empty,
    Dirty,
    AreaChanged,
    KeyChanged,
}

impl<W> CachedWidget<W, NoCacheKey> {
    /// Create a cached widget with manual invalidation.
    pub fn new(widget: W) -> Self {
        Self {
            inner: widget,
            key: NoCacheKey,
        }
    }
}

impl<W: Hash> CachedWidget<W, HashKey> {
    /// Create a cached widget using `Hash` as the cache key.
    pub fn with_hash(widget: W) -> Self {
        Self {
            inner: widget,
            key: HashKey,
        }
    }
}

impl<W, F: Fn(&W) -> u64> CachedWidget<W, FnKey<F>> {
    /// Create a cached widget with a custom cache key function.
    pub fn with_key(widget: W, key_fn: F) -> Self {
        Self {
            inner: widget,
            key: FnKey(key_fn),
        }
    }
}

impl<W, K> CachedWidget<W, K> {
    /// Access the inner widget.
    pub fn inner(&self) -> &W {
        &self.inner
    }

    /// Mutable access to the inner widget.
    pub fn inner_mut(&mut self) -> &mut W {
        &mut self.inner
    }

    /// Consume the wrapper and return the inner widget.
    pub fn into_inner(self) -> W {
        self.inner
    }

    /// Mark the cache as dirty (forces a re-render on next draw).
    pub fn mark_dirty(&self, state: &mut CachedWidgetState) {
        state.mark_dirty();
        #[cfg(feature = "tracing")]
        debug!(
            widget = std::any::type_name::<W>(),
            "Cache invalidated via mark_dirty()"
        );
    }
}

impl CachedWidgetState {
    /// Create a new empty cache state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark the cache dirty without logging.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Drop cached buffer to free memory.
    pub fn clear_cache(&mut self) {
        self.cache = None;
    }

    /// Approximate cache size in bytes.
    pub fn cache_size_bytes(&self) -> usize {
        self.cache
            .as_ref()
            .map(|cache| cache.buffer.len() * size_of::<Cell>())
            .unwrap_or(0)
    }
}

impl<W: Widget, K: CacheKey<W>> StatefulWidget for CachedWidget<W, K> {
    type State = CachedWidgetState;

    fn render(&self, area: Rect, frame: &mut Frame, state: &mut CachedWidgetState) {
        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!(
            "widget_render",
            widget = "CachedWidget",
            x = area.x,
            y = area.y,
            w = area.width,
            h = area.height
        )
        .entered();

        if area.is_empty() {
            state.clear_cache();
            state.last_area = Some(area);
            return;
        }

        let key = self.key.cache_key(&self.inner);
        let area_changed = state.last_area != Some(area);
        let key_changed = key != state.last_key;

        let needs_render = state.cache.is_none() || state.dirty || area_changed || key_changed;

        #[cfg(feature = "tracing")]
        let reason = if state.cache.is_none() {
            CacheMissReason::Empty
        } else if state.dirty {
            CacheMissReason::Dirty
        } else if area_changed {
            CacheMissReason::AreaChanged
        } else {
            CacheMissReason::KeyChanged
        };

        if needs_render {
            let local_area = Rect::from_size(area.width, area.height);
            // Create a temporary frame for the inner widget
            let mut cache_frame = Frame::new(area.width, area.height, frame.pool);
            self.inner.render(local_area, &mut cache_frame);
            // Extract the buffer from the frame for caching
            state.cache = Some(CachedBuffer {
                buffer: cache_frame.buffer,
            });
            state.last_area = Some(area);
            state.dirty = false;
            state.last_key = key;

            #[cfg(feature = "tracing")]
            debug!(
                widget = std::any::type_name::<W>(),
                reason = ?reason,
                "Cache miss, re-rendering"
            );
        } else {
            #[cfg(feature = "tracing")]
            trace!(
                widget = std::any::type_name::<W>(),
                "Cache hit, using cached buffer"
            );
        }

        if let Some(cache) = &state.cache {
            let src_rect = Rect::from_size(area.width, area.height);
            frame.buffer.copy_from(&cache.buffer, src_rect, area.x, area.y);
        }
    }
}

fn hash_value<T: Hash>(value: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell as CounterCell;
    use std::rc::Rc;

    #[derive(Debug, Clone)]
    struct CountWidget {
        count: Rc<CounterCell<u32>>,
    }

    impl Widget for CountWidget {
        fn render(&self, area: Rect, buf: &mut Buffer) {
            self.count.set(self.count.get() + 1);
            if !area.is_empty() {
                buf.set(area.x, area.y, Cell::from_char('x'));
            }
        }
    }

    #[derive(Debug, Clone)]
    struct KeyWidget {
        count: Rc<CounterCell<u32>>,
        key: Rc<CounterCell<u64>>,
    }

    impl Widget for KeyWidget {
        fn render(&self, area: Rect, buf: &mut Buffer) {
            self.count.set(self.count.get() + 1);
            if !area.is_empty() {
                buf.set(area.x, area.y, Cell::from_char('k'));
            }
        }
    }

    #[test]
    fn cache_hit_skips_rerender() {
        let count = Rc::new(CounterCell::new(0));
        let widget = CountWidget {
            count: count.clone(),
        };
        let cached = CachedWidget::new(widget);
        let mut state = CachedWidgetState::default();
        let mut buf = Buffer::new(5, 5);
        let area = Rect::new(1, 1, 3, 3);

        cached.render(area, &mut buf, &mut state);
        cached.render(area, &mut buf, &mut state);

        assert_eq!(count.get(), 1);
    }

    #[test]
    fn area_change_forces_rerender() {
        let count = Rc::new(CounterCell::new(0));
        let widget = CountWidget {
            count: count.clone(),
        };
        let cached = CachedWidget::new(widget);
        let mut state = CachedWidgetState::default();
        let mut buf = Buffer::new(6, 6);

        cached.render(Rect::new(0, 0, 3, 3), &mut buf, &mut state);
        cached.render(Rect::new(1, 1, 3, 3), &mut buf, &mut state);

        assert_eq!(count.get(), 2);
    }

    #[test]
    fn mark_dirty_forces_rerender() {
        let count = Rc::new(CounterCell::new(0));
        let widget = CountWidget {
            count: count.clone(),
        };
        let cached = CachedWidget::new(widget);
        let mut state = CachedWidgetState::default();
        let mut buf = Buffer::new(5, 5);
        let area = Rect::new(0, 0, 3, 3);

        cached.render(area, &mut buf, &mut state);
        cached.mark_dirty(&mut state);
        cached.render(area, &mut buf, &mut state);

        assert_eq!(count.get(), 2);
    }

    #[test]
    fn key_change_forces_rerender() {
        let count = Rc::new(CounterCell::new(0));
        let key = Rc::new(CounterCell::new(1));
        let widget = KeyWidget {
            count: count.clone(),
            key: key.clone(),
        };
        let cached = CachedWidget::with_key(widget, |w| w.key.get());
        let mut state = CachedWidgetState::default();
        let mut buf = Buffer::new(5, 5);
        let area = Rect::new(0, 0, 3, 3);

        cached.render(area, &mut buf, &mut state);
        key.set(2);
        cached.render(area, &mut buf, &mut state);

        assert_eq!(count.get(), 2);
    }

    #[test]
    fn empty_area_clears_cache() {
        let count = Rc::new(CounterCell::new(0));
        let widget = CountWidget {
            count: count.clone(),
        };
        let cached = CachedWidget::new(widget);
        let mut state = CachedWidgetState::default();
        let mut buf = Buffer::new(5, 5);

        // First render populates cache
        cached.render(Rect::new(0, 0, 3, 3), &mut buf, &mut state);
        assert!(state.cache.is_some());

        // Empty area should clear cache
        cached.render(Rect::new(0, 0, 0, 0), &mut buf, &mut state);
        assert!(state.cache.is_none());
        assert_eq!(count.get(), 1);
    }

    #[test]
    fn cache_size_bytes_empty() {
        let state = CachedWidgetState::new();
        assert_eq!(state.cache_size_bytes(), 0);
    }

    #[test]
    fn cache_size_bytes_after_render() {
        let count = Rc::new(CounterCell::new(0));
        let widget = CountWidget {
            count: count.clone(),
        };
        let cached = CachedWidget::new(widget);
        let mut state = CachedWidgetState::new();
        let mut buf = Buffer::new(5, 5);

        cached.render(Rect::new(0, 0, 3, 3), &mut buf, &mut state);
        assert!(state.cache_size_bytes() > 0);
        assert_eq!(state.cache_size_bytes(), 9 * std::mem::size_of::<Cell>());
    }

    #[test]
    fn clear_cache_drops_buffer() {
        let count = Rc::new(CounterCell::new(0));
        let widget = CountWidget {
            count: count.clone(),
        };
        let cached = CachedWidget::new(widget);
        let mut state = CachedWidgetState::new();
        let mut buf = Buffer::new(5, 5);

        cached.render(Rect::new(0, 0, 3, 3), &mut buf, &mut state);
        assert!(state.cache_size_bytes() > 0);

        state.clear_cache();
        assert_eq!(state.cache_size_bytes(), 0);
    }

    #[test]
    fn mark_dirty_then_clear_on_render() {
        let count = Rc::new(CounterCell::new(0));
        let widget = CountWidget {
            count: count.clone(),
        };
        let cached = CachedWidget::new(widget);
        let mut state = CachedWidgetState::new();
        let mut buf = Buffer::new(5, 5);
        let area = Rect::new(0, 0, 3, 3);

        cached.render(area, &mut buf, &mut state);
        assert_eq!(count.get(), 1);

        state.mark_dirty();
        assert!(state.dirty);

        cached.render(area, &mut buf, &mut state);
        assert_eq!(count.get(), 2);
        assert!(!state.dirty);
    }

    #[test]
    fn no_cache_key_returns_none() {
        let key = NoCacheKey;
        assert_eq!(CacheKey::<u32>::cache_key(&key, &42), None);
    }

    #[test]
    fn hash_key_returns_some() {
        let key = HashKey;
        let result = CacheKey::<String>::cache_key(&key, &"hello".to_string());
        assert!(result.is_some());
    }

    #[test]
    fn hash_key_same_value_same_key() {
        let key = HashKey;
        let a = CacheKey::<u64>::cache_key(&key, &42);
        let b = CacheKey::<u64>::cache_key(&key, &42);
        assert_eq!(a, b);
    }

    #[test]
    fn hash_key_different_value_different_key() {
        let key = HashKey;
        let a = CacheKey::<u64>::cache_key(&key, &1);
        let b = CacheKey::<u64>::cache_key(&key, &2);
        assert_ne!(a, b);
    }

    #[test]
    fn fn_key_custom_function() {
        let key = FnKey(|x: &u32| (*x as u64) * 100);
        assert_eq!(CacheKey::<u32>::cache_key(&key, &5), Some(500));
        assert_eq!(CacheKey::<u32>::cache_key(&key, &0), Some(0));
    }

    #[test]
    fn inner_accessors() {
        let count = Rc::new(CounterCell::new(0));
        let widget = CountWidget {
            count: count.clone(),
        };
        let mut cached = CachedWidget::new(widget);

        assert_eq!(cached.inner().count.get(), 0);

        cached.inner_mut().count.set(5);
        assert_eq!(count.get(), 5);

        let inner = cached.into_inner();
        assert_eq!(inner.count.get(), 5);
    }

    #[test]
    fn cached_content_matches_uncached() {
        let count = Rc::new(CounterCell::new(0));
        let widget = CountWidget {
            count: count.clone(),
        };
        let cached = CachedWidget::new(widget.clone());
        let mut state = CachedWidgetState::new();
        let area = Rect::new(0, 0, 3, 3);

        let mut buf_cached = Buffer::new(3, 3);
        cached.render(area, &mut buf_cached, &mut state);

        let mut buf_direct = Buffer::new(3, 3);
        widget.render(area, &mut buf_direct);

        assert_eq!(
            buf_cached.get(0, 0).unwrap().content.as_char(),
            buf_direct.get(0, 0).unwrap().content.as_char()
        );
    }

    #[test]
    fn multiple_cache_hits_never_rerender() {
        let count = Rc::new(CounterCell::new(0));
        let widget = CountWidget {
            count: count.clone(),
        };
        let cached = CachedWidget::new(widget);
        let mut state = CachedWidgetState::new();
        let mut buf = Buffer::new(5, 5);
        let area = Rect::new(0, 0, 3, 3);

        for _ in 0..10 {
            cached.render(area, &mut buf, &mut state);
        }
        assert_eq!(count.get(), 1);
    }

    #[test]
    fn same_key_no_rerender() {
        let count = Rc::new(CounterCell::new(0));
        let key = Rc::new(CounterCell::new(42));
        let widget = KeyWidget {
            count: count.clone(),
            key: key.clone(),
        };
        let cached = CachedWidget::with_key(widget, |w| w.key.get());
        let mut state = CachedWidgetState::new();
        let mut buf = Buffer::new(5, 5);
        let area = Rect::new(0, 0, 3, 3);

        cached.render(area, &mut buf, &mut state);
        cached.render(area, &mut buf, &mut state);
        cached.render(area, &mut buf, &mut state);

        assert_eq!(count.get(), 1);
    }
}
