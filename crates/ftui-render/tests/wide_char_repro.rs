use ftui_render::buffer::Buffer;
use ftui_render::cell::Cell;
use ftui_render::diff::BufferDiff;
use ftui_render::presenter::{Presenter, TerminalCapabilities};
use std::io::Write;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct SharedWriter(Arc<Mutex<Vec<u8>>>);

impl Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn repro_redundant_cup_after_wide_char() {
    let output = Arc::new(Mutex::new(Vec::new()));
    let writer = SharedWriter(output.clone());

    // Setup presenter
    let caps = TerminalCapabilities::basic();
    let mut presenter = Presenter::new(writer, caps);

    // Frame 1: Render a wide char "日" at (0,0)
    let mut buf1 = Buffer::new(10, 1);
    buf1.set_raw(0, 0, Cell::from_char('日')); // Width 2
    buf1.set_raw(1, 0, Cell::CONTINUATION);

    let empty = Buffer::new(10, 1);
    let diff1 = BufferDiff::compute(&empty, &buf1);

    presenter.present(&buf1, &diff1).unwrap();

    // Clear output buffer to check Frame 2 cleanly
    output.lock().unwrap().clear();

    // Frame 2: Render "A" at (2,0). "日" is unchanged.
    let mut buf2 = buf1.clone();
    buf2.set_raw(2, 0, Cell::from_char('A'));

    let diff2 = BufferDiff::compute(&buf1, &buf2);
    assert_eq!(diff2.len(), 1);

    presenter.present(&buf2, &diff2).unwrap();

    let bytes = output.lock().unwrap().clone();
    let output_str = String::from_utf8_lossy(&bytes);

    // Debug output
    println!("Frame 2 output: {:?}", output_str);

    // "A" is at x=2.
    // Previous "日" ended at x=2 (wide char advances cursor by width).
    // So cursor should already be at x=2, no CUP needed.
    // If Presenter thinks cursor is at x=1 or x=3, it will emit CUP(1, 3) (1-based).

    // Check for CUP sequence: ESC [ <row> ; <col> H
    // CUP format is: ESC [ Ps ; Ps H  where Ps are decimal numbers
    // SGR sequences (ESC [ ... m) are expected and acceptable.
    // Use regex-like check for CUP: contains sequence ending in 'H'
    let has_cup = output_str.contains("H") && {
        // Find if there's any sequence like ESC [ ... H
        let bytes = output_str.as_bytes();
        let mut i = 0;
        let mut found_cup = false;
        while i + 2 < bytes.len() {
            if bytes[i] == 0x1b && bytes[i + 1] == b'[' {
                // Found CSI, scan for terminator
                let mut j = i + 2;
                while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'H' {
                    found_cup = true;
                    break;
                }
            }
            i += 1;
        }
        found_cup
    };

    if has_cup {
        panic!("Found redundant CUP sequence: {:?}", output_str);
    }

    // Verify 'A' is present in output
    assert!(
        output_str.contains('A'),
        "Output should contain 'A': {:?}",
        output_str
    );
}
