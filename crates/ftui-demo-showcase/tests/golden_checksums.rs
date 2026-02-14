//! Golden output checksums for all demo screens (bd-3jlw5.9).
//!
//! Captures BLAKE3 checksums of the rendered buffer for every demo screen
//! at two canonical sizes (80x24 and 120x40). The checksums are stored in
//! `golden_checksums.txt` and serve as an isomorphism proof: any optimization
//! must produce identical rendered output.
//!
//! ## Usage
//!
//! Generate / update the golden file:
//!   BLESS_GOLDEN=1 cargo test -p ftui-demo-showcase --test golden_checksums
//!
//! Verify current renders match:
//!   cargo test -p ftui-demo-showcase --test golden_checksums
//!
//! The checksums file uses a simple `HASH  LABEL` format (one per line),
//! where LABEL is `{screen_slug}@{cols}x{rows}`.

use std::collections::BTreeMap;
use std::time::Duration;

use ftui_core::event::Event;
use ftui_demo_showcase::app::{AppModel, ScreenId};
use ftui_demo_showcase::screens;
use ftui_render::cell::{CellAttrs, CellContent};
use ftui_render::grapheme_pool::GraphemePool;
use ftui_web::step_program::StepProgram;

const TICK_MS: u64 = 100;

/// Pack CellAttrs into a u32 for hashing (mirrors render_trace::pack_attrs).
fn pack_attrs(attrs: CellAttrs) -> u32 {
    let flags = attrs.flags().bits() as u32;
    let link = attrs.link_id() & 0x00FF_FFFF;
    (flags << 24) | link
}

fn tick_event() -> Event {
    Event::Tick
}

/// Slugify a screen title for use as a checksum label.
fn screen_slug(screen: ScreenId) -> String {
    screen
        .title()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// Apply deterministic stabilization for screens that have timing-sensitive
/// output (mirrors `apply_web_sweep_deterministic_profile` in golden_trace_corpus).
fn stabilize_screen(program: &mut StepProgram<AppModel>, screen: ScreenId) {
    match screen {
        ScreenId::MermaidShowcase => {
            program
                .model_mut()
                .screens
                .mermaid_showcase
                .stabilize_metrics_for_snapshot();
        }
        ScreenId::MermaidMegaShowcase => {
            program
                .model_mut()
                .screens
                .mermaid_mega_showcase
                .stabilize_for_snapshot();
        }
        _ => {}
    }
}

/// Compute a BLAKE3 hash of the buffer's cell data.
///
/// Hashes each cell's content (resolving graphemes through the pool),
/// foreground, background, and attributes — matching the same cell
/// traversal order as `checksum_buffer` in ftui-runtime.
fn blake3_buffer(buf: &ftui_render::buffer::Buffer, pool: &GraphemePool) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new();
    let width = buf.width();
    let height = buf.height();

    // Hash dimensions first so different-sized identical content differs.
    hasher.update(&width.to_le_bytes());
    hasher.update(&height.to_le_bytes());

    for y in 0..height {
        for x in 0..width {
            let cell = buf.get_unchecked(x, y);
            match cell.content {
                CellContent::EMPTY => {
                    hasher.update(&[0u8]); // tag: empty
                }
                CellContent::CONTINUATION => {
                    hasher.update(&[3u8]); // tag: continuation
                }
                content => {
                    if let Some(ch) = content.as_char() {
                        hasher.update(&[1u8]); // tag: char
                        let mut buf = [0u8; 4];
                        let encoded = ch.encode_utf8(&mut buf);
                        hasher.update(encoded.as_bytes());
                    } else if let Some(gid) = content.grapheme_id() {
                        hasher.update(&[2u8]); // tag: grapheme
                        let text = pool.get(gid).unwrap_or("");
                        hasher.update(text.as_bytes());
                    } else {
                        hasher.update(&[0xFFu8]); // tag: unknown
                        hasher.update(&content.raw().to_le_bytes());
                    }
                }
            }
            hasher.update(&cell.fg.0.to_le_bytes());
            hasher.update(&cell.bg.0.to_le_bytes());
            let attrs = pack_attrs(cell.attrs);
            hasher.update(&attrs.to_le_bytes());
        }
    }

    hasher.finalize()
}

/// Sweep all screens at the given terminal size, returning (slug@WxH, blake3_hex) pairs.
fn sweep_checksums(cols: u16, rows: u16) -> BTreeMap<String, String> {
    let mut program = StepProgram::new(AppModel::new(), cols, rows);
    program.init().unwrap();

    let mut checksums = BTreeMap::new();

    for &screen in screens::screen_ids().iter() {
        program.model_mut().current_screen = screen;
        stabilize_screen(&mut program, screen);
        program.push_event(tick_event());
        program.advance_time(Duration::from_millis(TICK_MS));

        let step = program.step().unwrap();
        assert!(
            step.rendered,
            "screen sweep step should render for {}",
            screen.title()
        );

        let outputs = program.outputs();
        let buffer = outputs
            .last_buffer
            .as_ref()
            .expect("rendered step must capture last buffer");
        let hash = blake3_buffer(buffer, program.pool());
        let label = format!("{}@{}x{}", screen_slug(screen), cols, rows);
        checksums.insert(label, hash.to_hex().to_string());
    }

    checksums
}

/// Path to the golden checksums file (adjacent to the test source).
fn golden_path() -> std::path::PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    std::path::Path::new(manifest_dir).join("tests/golden_checksums.txt")
}

/// Format checksums as `BLAKE3_HEX  LABEL\n` lines (sorted by label).
fn format_checksums(checksums: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    out.push_str("# Golden output checksums for FrankenTUI demo screens (bd-3jlw5.9)\n");
    out.push_str("# Format: BLAKE3_HASH  SCREEN_SLUG@COLSxROWS\n");
    out.push_str(
        "# Regenerate: BLESS_GOLDEN=1 cargo test -p ftui-demo-showcase --test golden_checksums\n",
    );
    out.push('\n');
    for (label, hash) in checksums {
        out.push_str(hash);
        out.push_str("  ");
        out.push_str(label);
        out.push('\n');
    }
    out
}

/// Parse a golden checksums file back into a BTreeMap.
fn parse_checksums(content: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Format: HASH  LABEL (two spaces separator, matching sha256sum convention)
        if let Some((hash, label)) = line.split_once("  ") {
            map.insert(label.to_string(), hash.to_string());
        }
    }
    map
}

/// Generate checksums for all screens at both canonical sizes.
fn generate_all_checksums() -> BTreeMap<String, String> {
    let mut all = sweep_checksums(80, 24);
    all.extend(sweep_checksums(120, 40));
    all
}

/// Generate and write the golden checksums file (run with BLESS_GOLDEN=1).
#[test]
fn golden_checksums_bless() {
    if std::env::var("BLESS_GOLDEN").is_err() {
        // Skip blessing unless explicitly requested.
        return;
    }

    let checksums = generate_all_checksums();
    let content = format_checksums(&checksums);
    std::fs::write(golden_path(), &content).expect("failed to write golden_checksums.txt");

    eprintln!(
        "Blessed {} golden checksums to {}",
        checksums.len(),
        golden_path().display()
    );
}

/// Verify that current renders match the golden checksums file.
#[test]
fn golden_checksums_verify() {
    let path = golden_path();
    if !path.exists() {
        // First run: generate the golden file automatically.
        let checksums = generate_all_checksums();
        let content = format_checksums(&checksums);
        std::fs::write(&path, &content).expect("failed to write golden_checksums.txt");
        eprintln!(
            "Generated initial golden checksums ({} entries) at {}",
            checksums.len(),
            path.display()
        );
        return;
    }

    let golden_content =
        std::fs::read_to_string(&path).expect("failed to read golden_checksums.txt");
    let golden = parse_checksums(&golden_content);

    let current = generate_all_checksums();

    // Check for any mismatches.
    let mut mismatches = Vec::new();
    let mut missing_golden = Vec::new();
    let mut extra_golden = Vec::new();

    for (label, current_hash) in &current {
        match golden.get(label) {
            Some(golden_hash) if golden_hash != current_hash => {
                mismatches.push(format!(
                    "  {label}:\n    golden:  {golden_hash}\n    current: {current_hash}"
                ));
            }
            None => {
                missing_golden.push(format!("  {label}: {current_hash}"));
            }
            _ => {} // match
        }
    }

    for label in golden.keys() {
        if !current.contains_key(label) {
            extra_golden.push(format!("  {label}"));
        }
    }

    if !mismatches.is_empty() || !missing_golden.is_empty() || !extra_golden.is_empty() {
        let mut msg = String::from("Golden checksum verification failed!\n\n");

        if !mismatches.is_empty() {
            msg.push_str(&format!("MISMATCHES ({}):\n", mismatches.len()));
            for m in &mismatches {
                msg.push_str(m);
                msg.push('\n');
            }
            msg.push('\n');
        }

        if !missing_golden.is_empty() {
            msg.push_str(&format!(
                "NEW SCREENS (not in golden file, {}):\n",
                missing_golden.len()
            ));
            for m in &missing_golden {
                msg.push_str(m);
                msg.push('\n');
            }
            msg.push('\n');
        }

        if !extra_golden.is_empty() {
            msg.push_str(&format!(
                "REMOVED SCREENS (in golden file but not rendered, {}):\n",
                extra_golden.len()
            ));
            for m in &extra_golden {
                msg.push_str(m);
                msg.push('\n');
            }
            msg.push('\n');
        }

        msg.push_str(
            "To update: BLESS_GOLDEN=1 cargo test -p ftui-demo-showcase --test golden_checksums\n",
        );
        panic!("{msg}");
    }

    eprintln!(
        "Golden checksum verification passed: {} screens verified",
        current.len()
    );
}

/// Verify that checksums are deterministic (two sweeps produce identical hashes).
#[test]
fn golden_checksums_deterministic() {
    let sweep_a = generate_all_checksums();
    let sweep_b = generate_all_checksums();

    assert_eq!(
        sweep_a, sweep_b,
        "Golden checksums must be deterministic across runs"
    );
}
