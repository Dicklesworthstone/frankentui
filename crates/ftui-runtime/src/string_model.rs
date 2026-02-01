#![forbid(unsafe_code)]

//! Easy-mode adapter for string-based views.
//!
//! `StringModel` provides a simpler alternative to the full [`Model`] trait
//! for applications that render their view as a string rather than directly
//! manipulating a [`Frame`]. The string is parsed as styled text and rendered
//! into the frame automatically.
//!
//! This preserves the full kernel pipeline: String -> Text -> Frame -> Diff -> Presenter.
//!
//! # Example
//!
//! ```ignore
//! use ftui_runtime::string_model::StringModel;
//! use ftui_runtime::program::Cmd;
//! use ftui_core::event::Event;
//!
//! struct Counter { count: i32 }
//!
//! enum Msg { Increment, Quit }
//!
//! impl From<Event> for Msg {
//!     fn from(_: Event) -> Self { Msg::Increment }
//! }
//!
//! impl StringModel for Counter {
//!     type Message = Msg;
//!
//!     fn update(&mut self, msg: Msg) -> Cmd<Msg> {
//!         match msg {
//!             Msg::Increment => { self.count += 1; Cmd::none() }
//!             Msg::Quit => Cmd::quit(),
//!         }
//!     }
//!
//!     fn view_string(&self) -> String {
//!         format!("Count: {}", self.count)
//!     }
//! }
//! ```

use crate::program::{Cmd, Model};
use ftui_core::event::Event;
use ftui_render::cell::{Cell, CellContent};
use ftui_render::frame::Frame;
use ftui_text::Text;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// A simplified model trait that uses string-based views.
///
/// Instead of rendering directly to a [`Frame`], implementations return
/// a `String` from [`view_string`](Self::view_string). The string is
/// converted to [`Text`] and rendered automatically.
///
/// This is ideal for quick prototyping and simple applications where
/// full frame control isn't needed.
pub trait StringModel: Sized {
    /// The message type for this model.
    type Message: From<Event> + Send + 'static;

    /// Initialize the model with startup commands.
    fn init(&mut self) -> Cmd<Self::Message> {
        Cmd::none()
    }

    /// Update the model in response to a message.
    fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message>;

    /// Render the view as a string.
    ///
    /// The returned string is split by newlines and rendered into the frame.
    /// Each line is rendered left-aligned starting from the top of the frame area.
    fn view_string(&self) -> String;
}

/// Adapter that bridges a [`StringModel`] to the full [`Model`] trait.
///
/// This wrapper converts the string output of `view_string()` into
/// `Text` and renders it into the frame, preserving the full kernel
/// pipeline (Text -> Buffer -> Diff -> Presenter).
pub struct StringModelAdapter<S: StringModel> {
    inner: S,
}

impl<S: StringModel> StringModelAdapter<S> {
    /// Create a new adapter wrapping the given string model.
    pub fn new(inner: S) -> Self {
        Self { inner }
    }

    /// Get a reference to the inner model.
    pub fn inner(&self) -> &S {
        &self.inner
    }

    /// Get a mutable reference to the inner model.
    pub fn inner_mut(&mut self) -> &mut S {
        &mut self.inner
    }

    /// Consume the adapter and return the inner model.
    pub fn into_inner(self) -> S {
        self.inner
    }
}

impl<S: StringModel> Model for StringModelAdapter<S> {
    type Message = S::Message;

    fn init(&mut self) -> Cmd<Self::Message> {
        self.inner.init()
    }

    fn update(&mut self, msg: Self::Message) -> Cmd<Self::Message> {
        self.inner.update(msg)
    }

    fn view(&self, frame: &mut Frame) {
        let s = self.inner.view_string();
        let text = Text::raw(&s);
        render_text_to_frame(&text, frame);
    }
}

/// Render a `Text` into a `Buffer`, line by line with span styles.
///
/// Each line is rendered left-aligned from (0, y). Lines beyond the
/// buffer height are clipped. Characters beyond buffer width are clipped.
fn render_text_to_frame(text: &Text, frame: &mut Frame) {
    let width = frame.width();
    let height = frame.height();

    for (y, line) in text.lines().iter().enumerate() {
        if y as u16 >= height {
            break;
        }

        let mut x: u16 = 0;
        for span in line.spans() {
            if x >= width {
                break;
            }

            let style = span.style.unwrap_or_default();

            for grapheme in span.content.graphemes(true) {
                if x >= width {
                    break;
                }

                let w = UnicodeWidthStr::width(grapheme);
                if w == 0 {
                    continue;
                }

                // Skip if the wide character would exceed the buffer width
                if x + w as u16 > width {
                    break;
                }

                let content = if w > 1 || grapheme.chars().count() > 1 {
                    let id = frame.intern_with_width(grapheme, w as u8);
                    CellContent::from_grapheme(id)
                } else if let Some(c) = grapheme.chars().next() {
                    CellContent::from_char(c)
                } else {
                    continue;
                };

                let mut cell = Cell::new(content);
                apply_style(&mut cell, style);
                frame.buffer.set(x, y as u16, cell);

                x = x.saturating_add(w as u16);
            }
        }
    }
}

/// Apply a style to a cell.
fn apply_style(cell: &mut Cell, style: ftui_style::Style) {
    if let Some(fg) = style.fg {
        cell.fg = fg;
    }
    if let Some(bg) = style.bg {
        cell.bg = bg;
    }
    if let Some(attrs) = style.attrs {
        let cell_flags: ftui_render::cell::StyleFlags = attrs.into();
        cell.attrs = cell.attrs.with_flags(cell_flags);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::grapheme_pool::GraphemePool;

    // ---------- Shared test message type ----------

    #[derive(Debug)]
    enum TestMsg {
        Increment,
        Decrement,
        Quit,
        NoOp,
    }

    impl From<Event> for TestMsg {
        fn from(_: Event) -> Self {
            TestMsg::NoOp
        }
    }

    // ---------- Test StringModel ----------

    struct CounterModel {
        value: i32,
    }

    impl StringModel for CounterModel {
        type Message = TestMsg;

        fn update(&mut self, msg: TestMsg) -> Cmd<TestMsg> {
            match msg {
                TestMsg::Increment => {
                    self.value += 1;
                    Cmd::none()
                }
                TestMsg::Decrement => {
                    self.value -= 1;
                    Cmd::none()
                }
                TestMsg::Quit => Cmd::quit(),
                TestMsg::NoOp => Cmd::none(),
            }
        }

        fn view_string(&self) -> String {
            format!("Count: {}", self.value)
        }
    }

    // ---------- Tests ----------

    #[test]
    fn adapter_delegates_update() {
        let mut adapter = StringModelAdapter::new(CounterModel { value: 0 });
        adapter.update(TestMsg::Increment);
        assert_eq!(adapter.inner().value, 1);
        adapter.update(TestMsg::Decrement);
        assert_eq!(adapter.inner().value, 0);
    }

    #[test]
    fn adapter_delegates_quit() {
        let mut adapter = StringModelAdapter::new(CounterModel { value: 0 });
        let cmd = adapter.update(TestMsg::Quit);
        assert!(matches!(cmd, Cmd::Quit));
    }

    #[test]
    fn adapter_view_renders_text() {
        let adapter = StringModelAdapter::new(CounterModel { value: 42 });
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);

        adapter.view(&mut frame);

        // "Count: 42" should be rendered
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('C'));
        assert_eq!(frame.buffer.get(1, 0).unwrap().content.as_char(), Some('o'));
        assert_eq!(frame.buffer.get(7, 0).unwrap().content.as_char(), Some('4'));
        assert_eq!(frame.buffer.get(8, 0).unwrap().content.as_char(), Some('2'));
    }

    #[test]
    fn adapter_view_multiline() {
        struct MultiLineModel;

        impl StringModel for MultiLineModel {
            type Message = TestMsg;

            fn update(&mut self, _msg: TestMsg) -> Cmd<TestMsg> {
                Cmd::none()
            }

            fn view_string(&self) -> String {
                "Line 1\nLine 2\nLine 3".to_string()
            }
        }

        let adapter = StringModelAdapter::new(MultiLineModel);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 5, &mut pool);

        adapter.view(&mut frame);

        // Line 1 at y=0
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('L'));
        assert_eq!(frame.buffer.get(5, 0).unwrap().content.as_char(), Some('1'));

        // Line 2 at y=1
        assert_eq!(frame.buffer.get(0, 1).unwrap().content.as_char(), Some('L'));
        assert_eq!(frame.buffer.get(5, 1).unwrap().content.as_char(), Some('2'));

        // Line 3 at y=2
        assert_eq!(frame.buffer.get(0, 2).unwrap().content.as_char(), Some('L'));
        assert_eq!(frame.buffer.get(5, 2).unwrap().content.as_char(), Some('3'));
    }

    #[test]
    fn adapter_clips_to_buffer_height() {
        struct TallModel;

        impl StringModel for TallModel {
            type Message = TestMsg;
            fn update(&mut self, _: TestMsg) -> Cmd<TestMsg> {
                Cmd::none()
            }
            fn view_string(&self) -> String {
                (0..100)
                    .map(|i| format!("Line {}", i))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }

        let adapter = StringModelAdapter::new(TallModel);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(20, 3, &mut pool);

        // Should not panic even with 100 lines in a 3-row buffer
        adapter.view(&mut frame);

        // Only first 3 lines rendered
        assert_eq!(frame.buffer.get(5, 0).unwrap().content.as_char(), Some('0'));
        assert_eq!(frame.buffer.get(5, 1).unwrap().content.as_char(), Some('1'));
        assert_eq!(frame.buffer.get(5, 2).unwrap().content.as_char(), Some('2'));
    }

    #[test]
    fn adapter_clips_to_buffer_width() {
        struct WideModel;

        impl StringModel for WideModel {
            type Message = TestMsg;
            fn update(&mut self, _: TestMsg) -> Cmd<TestMsg> {
                Cmd::none()
            }
            fn view_string(&self) -> String {
                "ABCDEFGHIJKLMNOPQRSTUVWXYZ".to_string()
            }
        }

        let adapter = StringModelAdapter::new(WideModel);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(5, 1, &mut pool);

        // Should not panic
        adapter.view(&mut frame);

        // Only first 5 chars rendered
        assert_eq!(frame.buffer.get(0, 0).unwrap().content.as_char(), Some('A'));
        assert_eq!(frame.buffer.get(4, 0).unwrap().content.as_char(), Some('E'));
    }

    #[test]
    fn adapter_inner_access() {
        let adapter = StringModelAdapter::new(CounterModel { value: 99 });
        assert_eq!(adapter.inner().value, 99);
    }

    #[test]
    fn adapter_inner_mut_access() {
        let mut adapter = StringModelAdapter::new(CounterModel { value: 0 });
        adapter.inner_mut().value = 50;
        assert_eq!(adapter.inner().value, 50);
    }

    #[test]
    fn adapter_into_inner() {
        let adapter = StringModelAdapter::new(CounterModel { value: 42 });
        let model = adapter.into_inner();
        assert_eq!(model.value, 42);
    }

    #[test]
    fn empty_view_string() {
        struct EmptyModel;

        impl StringModel for EmptyModel {
            type Message = TestMsg;
            fn update(&mut self, _: TestMsg) -> Cmd<TestMsg> {
                Cmd::none()
            }
            fn view_string(&self) -> String {
                String::new()
            }
        }

        let adapter = StringModelAdapter::new(EmptyModel);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(10, 5, &mut pool);

        // Should not panic
        adapter.view(&mut frame);
    }
}
