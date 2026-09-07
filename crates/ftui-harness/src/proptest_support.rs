#![forbid(unsafe_code)]

//! Shared proptest strategies for cross-crate property tests.
//!
//! This module centralizes common generators so individual test files can
//! focus on invariants instead of re-implementing arbitrary data builders.

use ftui_core::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEvent, MouseEventKind,
    PasteEvent,
};
use ftui_render::cell::PackedRgba;
use ftui_style::{Style, StyleFlags};
use proptest::prelude::*;

/// Writer operation: kind (present/resize/strip/SGR/height), columns, rows,
/// UI height, adversarial fragment index, log bytes, and ASCII frame cells.
/// Each frame cell contains x, y, character byte, and bold flag.
pub type WriterOperation = (u8, u16, u16, u16, usize, Vec<u8>, Vec<(u16, u16, u8, bool)>);

/// The same operation distribution for headless invariants and real PTY sessions.
pub fn arb_writer_operations() -> impl Strategy<Value = Vec<WriterOperation>> {
    proptest::collection::vec(
        (
            0u8..5,
            20u16..=200,
            5u16..=60,
            1u16..=10,
            0usize..crate::ADVERSARIAL_PAYLOADS.len(),
            proptest::collection::vec(any::<u8>(), 0..32),
            proptest::collection::vec((0u16..200, 0u16..10, 0x20u8..=0x7e, any::<bool>()), 0..=32),
        ),
        1..=200,
    )
}

/// Minimal synthetic widget tree used by property tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WidgetTree {
    Paragraph {
        text: String,
        style: Style,
    },
    Split {
        axis: SplitAxis,
        children: Vec<WidgetTree>,
    },
}

/// Split direction for [`WidgetTree::Split`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitAxis {
    Horizontal,
    Vertical,
}

/// Arbitrary terminal dimensions (always at least 1x1).
pub fn arb_terminal_dimensions(max_width: u16, max_height: u16) -> BoxedStrategy<(u16, u16)> {
    let width = 1..=max_width.max(1);
    let height = 1..=max_height.max(1);
    (width, height).boxed()
}

/// Arbitrary byte stream for parser/runtime feeding.
pub fn arb_byte_stream(max_len: usize) -> BoxedStrategy<Vec<u8>> {
    proptest::collection::vec(any::<u8>(), 0..=max_len.max(1)).boxed()
}

/// Arbitrary printable ASCII payload.
pub fn arb_ascii_payload(max_len: usize) -> BoxedStrategy<Vec<u8>> {
    proptest::collection::vec(0x20u8..=0x7e, 0..=max_len.max(1)).boxed()
}

/// Arbitrary Unicode text (including combining/emoji/control chars).
pub fn arb_unicode_string(max_chars: usize) -> BoxedStrategy<String> {
    proptest::collection::vec(any::<char>(), 0..=max_chars.max(1))
        .prop_map(|chars| chars.into_iter().collect::<String>())
        .boxed()
}

/// Arbitrary style combinations (colors + attributes).
pub fn arb_style() -> BoxedStrategy<Style> {
    (
        arb_optional_rgba(),
        arb_optional_rgba(),
        arb_style_flags(),
        arb_optional_rgba(),
    )
        .prop_map(|(fg, bg, attrs, underline_color)| Style {
            fg,
            bg,
            attrs: (!attrs.is_empty()).then_some(attrs),
            underline_color,
        })
        .boxed()
}

/// Arbitrary canonical input event.
pub fn arb_event() -> BoxedStrategy<Event> {
    prop_oneof![
        arb_key_event().prop_map(Event::Key),
        arb_mouse_event().prop_map(Event::Mouse),
        arb_terminal_dimensions(300, 120)
            .prop_map(|(width, height)| Event::Resize { width, height }),
        arb_unicode_string(64).prop_map(|text| Event::Paste(PasteEvent::bracketed(text))),
        any::<bool>().prop_map(Event::Focus),
        Just(Event::Tick),
    ]
    .boxed()
}

/// Arbitrary sequence of canonical input events.
pub fn arb_event_sequence(max_len: usize) -> BoxedStrategy<Vec<Event>> {
    proptest::collection::vec(arb_event(), 0..=max_len.max(1)).boxed()
}

/// Arbitrary synthetic widget tree for layout/stateful rendering tests.
pub fn arb_widget_tree(
    max_depth: u32,
    max_children: usize,
    max_text_chars: usize,
) -> BoxedStrategy<WidgetTree> {
    let bounded_children = max_children.max(1);
    let bounded_depth = max_depth.max(1);

    let leaf = (arb_unicode_string(max_text_chars), arb_style())
        .prop_map(|(text, style)| WidgetTree::Paragraph { text, style });

    leaf.prop_recursive(bounded_depth, 256, bounded_children as u32, move |inner| {
        (
            prop_oneof![Just(SplitAxis::Horizontal), Just(SplitAxis::Vertical),],
            proptest::collection::vec(inner, 1..=bounded_children),
        )
            .prop_map(|(axis, children)| WidgetTree::Split { axis, children })
    })
    .boxed()
}

fn arb_optional_rgba() -> BoxedStrategy<Option<PackedRgba>> {
    prop_oneof![Just(None), arb_rgba().prop_map(Some),].boxed()
}

fn arb_rgba() -> BoxedStrategy<PackedRgba> {
    (any::<u8>(), any::<u8>(), any::<u8>(), any::<u8>())
        .prop_map(|(r, g, b, a)| PackedRgba::rgba(r, g, b, a))
        .boxed()
}

fn arb_style_flags() -> BoxedStrategy<StyleFlags> {
    (
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(
            |(
                bold,
                dim,
                italic,
                underline,
                blink,
                reverse,
                hidden,
                strikethrough,
                double_underline,
                curly_underline,
            )| {
                let mut flags = StyleFlags::NONE;
                if bold {
                    flags.insert(StyleFlags::BOLD);
                }
                if dim {
                    flags.insert(StyleFlags::DIM);
                }
                if italic {
                    flags.insert(StyleFlags::ITALIC);
                }
                if underline {
                    flags.insert(StyleFlags::UNDERLINE);
                }
                if blink {
                    flags.insert(StyleFlags::BLINK);
                }
                if reverse {
                    flags.insert(StyleFlags::REVERSE);
                }
                if hidden {
                    flags.insert(StyleFlags::HIDDEN);
                }
                if strikethrough {
                    flags.insert(StyleFlags::STRIKETHROUGH);
                }
                if double_underline {
                    flags.insert(StyleFlags::DOUBLE_UNDERLINE);
                }
                if curly_underline {
                    flags.insert(StyleFlags::CURLY_UNDERLINE);
                }
                flags
            },
        )
        .boxed()
}

fn arb_key_event() -> BoxedStrategy<KeyEvent> {
    (arb_key_code(), arb_modifiers(), arb_key_kind())
        .prop_map(|(code, modifiers, kind)| KeyEvent {
            code,
            modifiers,
            kind,
        })
        .boxed()
}

fn arb_mouse_event() -> BoxedStrategy<MouseEvent> {
    (arb_mouse_kind(), 0u16..=300, 0u16..=120, arb_modifiers())
        .prop_map(|(kind, x, y, modifiers)| MouseEvent {
            kind,
            x,
            y,
            modifiers,
        })
        .boxed()
}

fn arb_key_code() -> BoxedStrategy<KeyCode> {
    prop_oneof![
        any::<char>().prop_map(KeyCode::Char),
        Just(KeyCode::Enter),
        Just(KeyCode::Escape),
        Just(KeyCode::Backspace),
        Just(KeyCode::Tab),
        Just(KeyCode::BackTab),
        Just(KeyCode::Delete),
        Just(KeyCode::Insert),
        Just(KeyCode::Home),
        Just(KeyCode::End),
        Just(KeyCode::PageUp),
        Just(KeyCode::PageDown),
        Just(KeyCode::Up),
        Just(KeyCode::Down),
        Just(KeyCode::Left),
        Just(KeyCode::Right),
        (1u8..=24).prop_map(KeyCode::F),
        Just(KeyCode::Null),
    ]
    .boxed()
}

fn arb_mouse_kind() -> BoxedStrategy<MouseEventKind> {
    prop_oneof![
        arb_mouse_button().prop_map(MouseEventKind::Down),
        arb_mouse_button().prop_map(MouseEventKind::Up),
        arb_mouse_button().prop_map(MouseEventKind::Drag),
        Just(MouseEventKind::Moved),
        Just(MouseEventKind::ScrollUp),
        Just(MouseEventKind::ScrollDown),
        Just(MouseEventKind::ScrollLeft),
        Just(MouseEventKind::ScrollRight),
    ]
    .boxed()
}

fn arb_mouse_button() -> BoxedStrategy<MouseButton> {
    prop_oneof![
        Just(MouseButton::Left),
        Just(MouseButton::Right),
        Just(MouseButton::Middle),
    ]
    .boxed()
}

fn arb_key_kind() -> BoxedStrategy<KeyEventKind> {
    prop_oneof![
        Just(KeyEventKind::Press),
        Just(KeyEventKind::Repeat),
        Just(KeyEventKind::Release),
    ]
    .boxed()
}

fn arb_modifiers() -> BoxedStrategy<Modifiers> {
    (any::<bool>(), any::<bool>(), any::<bool>(), any::<bool>())
        .prop_map(|(shift, alt, ctrl, super_key)| {
            let mut modifiers = Modifiers::NONE;
            if shift {
                modifiers.insert(Modifiers::SHIFT);
            }
            if alt {
                modifiers.insert(Modifiers::ALT);
            }
            if ctrl {
                modifiers.insert(Modifiers::CTRL);
            }
            if super_key {
                modifiers.insert(Modifiers::SUPER);
            }
            modifiers
        })
        .boxed()
}
