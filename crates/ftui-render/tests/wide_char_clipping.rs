use ftui_core::geometry::Rect;
use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use ftui_render::diff::BufferDiff;
use ftui_render::presenter::{Presenter, TerminalCapabilities};

#[test]
fn wide_char_clipped_tail_atomic_rejection() {
    // 1. Setup buffer with 'A' at x=1
    let mut buffer = Buffer::new(10, 1);
    buffer.set(1, 0, Cell::from_char('A'));

    // 2. Setup scissor to only allow x=0
    buffer.push_scissor(Rect::new(0, 0, 1, 1));

    // 3. Set wide char '中' at x=0
    // This requires x=0 and x=1. x=1 is outside scissor.
    // Should be ATOMICALLY rejected.
    buffer.set(0, 0, Cell::from_char('中'));

    // 4. Inspect buffer state
    let head = buffer.get(0, 0).unwrap();
    let tail = buffer.get(1, 0).unwrap();

    // The fix: Head is NOT set because tail was clipped. Atomic write.
    assert!(head.is_empty(), "Head should be empty (atomic rejection)");
    assert_eq!(tail.content.as_char(), Some('A'), "Tail should remain 'A'");

    // 5. Verify Presenter output
    let old = Buffer::new(10, 1);
    let diff = BufferDiff::compute(&old, &buffer);
    let mut output = Vec::new();
    let mut presenter = Presenter::new(&mut output, TerminalCapabilities::basic());

    presenter.present(&buffer, &diff).unwrap();
    drop(presenter);

    let output_str = String::from_utf8_lossy(&output);
    println!("Output: {:?}", output_str);

    // Presenter sees [Empty, A].
    // Emits 'A' at 1.
    // Output should contain 'A'. Should NOT contain '中'.
    assert!(!output_str.contains('中'));
    assert!(output_str.contains('A'));
}
