# Code Review Report for FrankenTUI

**Date:** February 3, 2026
**Reviewer:** Gemini CLI (Code Review Agent)

## 1. Executive Summary

A comprehensive code review of the FrankenTUI codebase was conducted, focusing on architectural integrity, correctness, performance, and security. The review covered the entire workspace: core components (`ftui-core`), rendering logic (`ftui-render`), runtime orchestration (`ftui-runtime`), layout engine (`ftui-layout`), text handling (`ftui-text`), styling (`ftui-style`), and the extensive widget library (`ftui-widgets`).

**Conclusion:** The codebase is of **exceptionally high quality**. It strictly adheres to the stated architecture (Layered: Core -> Render -> Runtime -> Widgets), employs robust defensive programming techniques (e.g., RAII, One-Writer Rule, DoS protection), and includes extensive testing (unit, property, and invariants). Several subtle rendering bugs were identified and fixed.

## 2. Key Findings

### 2.1 Architecture & Design
- **Layered Architecture:** The strict dependency hierarchy is well-maintained.
- **One-Writer Rule:** The `TerminalWriter` in `ftui-runtime` robustly enforces serialized access to the terminal, preventing race conditions and visual artifacts.
- **RAII Cleanup:** `TerminalSession` ensures terminal state (raw mode, mouse tracking, etc.) is restored even during panics, utilizing `Drop` semantics effectively.

### 2.2 Correctness & Robustness
- **Input Parsing:** `ftui-core/src/input_parser.rs` correctly implements a state machine for ANSI, UTF-8, and custom protocols (Kitty keyboard). It includes DoS protection by limiting sequence lengths.
- **Diff Algorithm:** `ftui-render/src/diff.rs` implements an efficient, cache-friendly diffing algorithm using 16-byte cells and block-based comparisons. It correctly handles row skipping and dirty flags.
- **Layout:** `ftui-layout` provides a solid constraint solver. The `round_layout_stable` algorithm uses the Largest Remainder Method with temporal tie-breaking to ensure stable, jitter-free layouts. The 2D `Grid` layout correctly handles cell spanning.
- **Text Handling:** `ftui-text` provides robust rope-backed storage (`rope.rs`) and grapheme-aware editing (`editor.rs`). Wrapping algorithms (`wrap.rs`) correctly handle Unicode width and word boundaries. Search (`search.rs`) supports exact, case-insensitive, and regex modes.

### 2.3 Performance
- **Cell Layout:** `Cell` struct is strictly packed to 16 bytes, optimizing cache usage (4 cells/cache line) and enabling potential SIMD optimizations.
- **Render Loop:** The `Presenter` uses a cost model to optimize cursor movements, choosing the cheapest sequence (CUP vs CHA vs CUF) for each update run.
- **Allocation Budget:** `ftui-runtime/src/allocation_budget.rs` implements advanced statistical monitoring (CUSUM + E-process) to detect allocation leaks or regressions.
- **Virtualization:** `ftui-widgets/src/virtualized.rs` provides a generic container for efficient rendering of large datasets, supporting variable item heights and O(log n) scroll mapping via a Fenwick tree.

### 2.4 Security
- **Sanitization:** `ftui-render/src/sanitize.rs` implements a strict whitelist-based sanitizer for untrusted text, stripping all control codes except safe whitespace (TAB, LF, CR) and removing potentially dangerous sequences (OSC, CSI, etc.).
- **Grapheme Handling:** `unicode-segmentation` is used correctly throughout to handle complex grapheme clusters, preventing display corruption from combining characters.

### 2.5 Widgets
- **LogViewer:** `ftui-widgets/src/log_viewer.rs` is highly optimized for streaming logs, featuring circular buffer eviction (`log_ring.rs`), incremental filtering, and search.
- **Toast System:** `ftui-widgets/src/toast.rs` and `notification_queue.rs` provide a sophisticated notification system with priority queuing, deduplication, and animations.
- **Editors:** `TextArea` and `TextInput` correctly handle cursor movement, selection, and undo/redo operations.
- **Data Display:** `Table`, `List`, `Tree`, `Sparkline`, `ProgressBar`, and `JsonView` are implemented robustly.
- **Debugging:** `Inspector` and `LayoutDebugger` provide valuable introspection tools.

## 3. Fixes Implemented

1.  **Double Background Application:** Fixed a visual artifact in `Table`, `List`, `TextInput`, `StatusLine`, and `ProgressBar` where semi-transparent backgrounds were applied twice (once by container/row clearing, once by text rendering), causing incorrect colors. Logic was added to strip the background from the style used for text rendering when appropriate.
2.  **Unicode/Grapheme Rendering:** Fixed multiple instances in `CommandPalette` and `Scrollbar` where code iterated over `char`s instead of graphemes, potentially breaking complex characters. Refactored to use `unicode-segmentation` and proper width calculation.
3.  **Search Highlighting:** Fixed a bug in `LogViewer` where search highlighting could split multi-byte graphemes (e.g. accented chars), causing rendering corruption. Added logic to snap match ranges to grapheme boundaries.
4.  **Virtualized Rendering Ghosting:** Fixed a bug in `VirtualizedList` where items in the overscan region (above the viewport) were incorrectly rendered at the top of the screen (`y=0`) due to coordinate clamping. Added a check to skip rendering items that start off-screen.

## 4. Recommendations

- **Input Parser Paste Handling:** The paste buffer truncation strategy discards the *beginning* of a large paste to prevent memory exhaustion. This is a safe default for DoS protection but technically data loss for the user. No action required unless this behavior is problematic for specific use cases.
- **Search:** The regex search feature is gated behind a feature flag, which is good practice.

## 5. Final Verdict

**Ready for use.** The codebase demonstrates a high degree of engineering rigor and attention to detail. It is safe, performant, and feature-complete for its intended purpose.
