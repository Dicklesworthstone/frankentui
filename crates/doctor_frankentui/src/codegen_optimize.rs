// SPDX-License-Identifier: Apache-2.0
//! Post-generation optimization and readability passes for emitted Rust code.
//!
//! Each pass is:
//! - **Semantics-preserving**: the optimized code produces identical runtime behavior
//! - **Deterministic**: same input → same output (no randomness)
//! - **Traceable**: emits before/after decision records for audit
//!
//! Passes:
//! 1. **Dead branch elimination**: remove unreachable match arms and always-true/false guards
//! 2. **Style constant folding**: merge duplicate color/typography/border definitions
//! 3. **Helper extraction**: extract repeated widget patterns into named functions
//! 4. **Import deduplication**: consolidate repeated `use` statements
//! 5. **Whitespace normalization**: consistent blank-line spacing between logical sections

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::code_emission::{EmissionPlan, FileKind};

// ── Constants ──────────────────────────────────────────────────────────

/// Module version tag.
pub const CODEGEN_OPTIMIZE_VERSION: &str = "codegen-optimize-v1";

// ── Core Types ─────────────────────────────────────────────────────────

/// Result of running optimization passes on an emission plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizationResult {
    /// Schema version.
    pub version: String,
    /// The optimized emission plan.
    pub plan: EmissionPlan,
    /// Per-pass transformation records.
    pub records: Vec<TransformationRecord>,
    /// Summary statistics.
    pub stats: OptimizationStats,
}

/// A record of a single transformation applied by a pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransformationRecord {
    /// Which pass produced this transformation.
    pub pass: PassKind,
    /// File path affected.
    pub file_path: String,
    /// Human-readable description of the transformation.
    pub description: String,
    /// Lines removed (count).
    pub lines_removed: usize,
    /// Lines added (count).
    pub lines_added: usize,
    /// Snippet of the before state (first ~3 lines).
    pub before_snippet: String,
    /// Snippet of the after state (first ~3 lines).
    pub after_snippet: String,
}

/// The kind of optimization pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PassKind {
    /// Remove unreachable match arms and tautological guards.
    DeadBranchElimination,
    /// Merge duplicate style constant definitions.
    StyleConstantFolding,
    /// Extract repeated widget subtrees into helper functions.
    HelperExtraction,
    /// Consolidate duplicate `use` imports.
    ImportDeduplication,
    /// Normalize blank-line spacing.
    WhitespaceNormalization,
}

/// Configuration for which passes to run and their settings.
#[derive(Debug, Clone)]
pub struct OptimizeConfig {
    /// Which passes to run (in order).
    pub passes: Vec<PassKind>,
    /// Minimum number of duplicates before style folding kicks in.
    pub style_fold_threshold: usize,
    /// Minimum repetitions before helper extraction.
    pub helper_extract_threshold: usize,
}

impl Default for OptimizeConfig {
    fn default() -> Self {
        Self {
            passes: vec![
                PassKind::DeadBranchElimination,
                PassKind::StyleConstantFolding,
                PassKind::HelperExtraction,
                PassKind::ImportDeduplication,
                PassKind::WhitespaceNormalization,
            ],
            style_fold_threshold: 2,
            helper_extract_threshold: 3,
        }
    }
}

/// Summary statistics from optimization.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OptimizationStats {
    /// Total passes executed.
    pub passes_executed: usize,
    /// Total transformations applied.
    pub transformations: usize,
    /// Total lines removed.
    pub total_lines_removed: usize,
    /// Total lines added.
    pub total_lines_added: usize,
    /// Net line change (negative = reduction).
    pub net_line_change: i64,
    /// Per-pass counts.
    pub by_pass: BTreeMap<String, usize>,
}

// ── Public API ─────────────────────────────────────────────────────────

/// Run all default optimization passes on an emission plan.
pub fn optimize(plan: EmissionPlan) -> OptimizationResult {
    optimize_with_config(plan, &OptimizeConfig::default())
}

/// Run optimization passes with custom configuration.
pub fn optimize_with_config(mut plan: EmissionPlan, config: &OptimizeConfig) -> OptimizationResult {
    let mut records = Vec::new();
    let mut stats = OptimizationStats::default();

    for pass in &config.passes {
        stats.passes_executed += 1;
        let pass_records = run_pass(*pass, &mut plan, config);
        let count = pass_records.len();
        if count > 0 {
            let pass_name = format!("{pass:?}");
            *stats.by_pass.entry(pass_name).or_insert(0) += count;
            stats.transformations += count;
            for rec in &pass_records {
                stats.total_lines_removed += rec.lines_removed;
                stats.total_lines_added += rec.lines_added;
            }
            records.extend(pass_records);
        }
    }

    stats.net_line_change = stats.total_lines_added as i64 - stats.total_lines_removed as i64;

    OptimizationResult {
        version: CODEGEN_OPTIMIZE_VERSION.to_string(),
        plan,
        records,
        stats,
    }
}

// ── Pass Dispatch ──────────────────────────────────────────────────────

fn run_pass(
    pass: PassKind,
    plan: &mut EmissionPlan,
    config: &OptimizeConfig,
) -> Vec<TransformationRecord> {
    match pass {
        PassKind::DeadBranchElimination => pass_dead_branch_elimination(plan),
        PassKind::StyleConstantFolding => {
            pass_style_constant_folding(plan, config.style_fold_threshold)
        }
        PassKind::HelperExtraction => pass_helper_extraction(plan, config.helper_extract_threshold),
        PassKind::ImportDeduplication => pass_import_deduplication(plan),
        PassKind::WhitespaceNormalization => pass_whitespace_normalization(plan),
    }
}

// ── Pass 1: Dead Branch Elimination ────────────────────────────────────

fn pass_dead_branch_elimination(plan: &mut EmissionPlan) -> Vec<TransformationRecord> {
    let mut records = Vec::new();

    let paths: Vec<String> = plan
        .files
        .iter()
        .filter(|(_, f)| f.kind == FileKind::RustSource)
        .map(|(p, _)| p.clone())
        .collect();

    for path in paths {
        if let Some(file) = plan.files.get_mut(&path) {
            let before = file.content.clone();
            let before_lines = before.lines().count();
            let mut lines: Vec<String> = before.lines().map(String::from).collect();
            let mut changed = false;

            // `if true { ... }` becomes a plain block and `if false { ... }` is
            // removed. The body of `if true` used to be unwrapped, which moved
            // its `let`s into the enclosing scope, where they could shadow
            // later uses. A guard with an `else` is left alone.
            let mut i = 0;
            while i < lines.len() {
                let guard = lines[i].trim();
                if (guard == "if true {" || guard == "if false {")
                    && let Some(close) = find_matching_brace(&lines, i)
                    && closes_without_else(&lines, close)
                {
                    if guard == "if true {" {
                        let indent = &lines[i][..lines[i].len() - lines[i].trim_start().len()];
                        lines[i] = format!("{indent}{{");
                        i += 1;
                    } else {
                        lines.drain(i..=close);
                    }
                    changed = true;
                    continue;
                }
                i += 1;
            }

            // Remove `Cmd::None` arms that are no-ops after all mutations
            // (a match arm with zero mutations and Cmd::None is dead weight)
            i = 0;
            while i < lines.len() {
                let trimmed = lines[i].trim();
                if trimmed.contains("=> {")
                    && i + 2 < lines.len()
                    && lines[i + 1].trim() == "ftui_runtime::Cmd::None"
                    && lines[i + 2].trim() == "}"
                {
                    // This arm does nothing — mark with a comment
                    let indent = &lines[i][..lines[i].len() - lines[i].trim_start().len()];
                    lines[i + 1] = format!("{indent}    ftui_runtime::Cmd::None // no-op");
                    changed = true;
                }
                i += 1;
            }

            if changed {
                let new_content = lines.join("\n");
                let after_lines = new_content.lines().count();
                records.push(TransformationRecord {
                    pass: PassKind::DeadBranchElimination,
                    file_path: path.clone(),
                    description: "Removed tautological guards and dead branches".into(),
                    lines_removed: before_lines.saturating_sub(after_lines),
                    lines_added: after_lines.saturating_sub(before_lines),
                    before_snippet: snippet(&before, 3),
                    after_snippet: snippet(&new_content, 3),
                });
                file.content = new_content;
            }
        }
    }

    records
}

/// The index of the line holding the `}` that closes the first `{` on
/// `lines[start]`.
///
/// Braces in string and char literals and in comments do not count. The old
/// count included them. It also started below the guard line after that line
/// was removed, which skipped the first body line, so a body opening with
/// `let s = S {` was closed at that struct's `}`.
fn find_matching_brace(lines: &[String], start: usize) -> Option<usize> {
    let mut depth = 0_usize;
    let mut in_string = false;
    let mut block_comments = 0_usize;
    for (index, line) in lines.iter().enumerate().skip(start) {
        let mut chars = line.chars().peekable();
        while let Some(ch) = chars.next() {
            if in_string {
                match ch {
                    '\\' => {
                        chars.next();
                    }
                    '"' => in_string = false,
                    _ => {}
                }
            } else if block_comments > 0 {
                match (ch, chars.peek()) {
                    ('*', Some('/')) => {
                        chars.next();
                        block_comments -= 1;
                    }
                    ('/', Some('*')) => {
                        chars.next();
                        block_comments += 1;
                    }
                    _ => {}
                }
            } else {
                match ch {
                    '/' if chars.peek() == Some(&'/') => break,
                    '/' if chars.peek() == Some(&'*') => {
                        chars.next();
                        block_comments = 1;
                    }
                    '"' => in_string = true,
                    '\'' => skip_char_literal(&mut chars),
                    '{' => depth += 1,
                    '}' => {
                        depth = depth.checked_sub(1)?;
                        if depth == 0 {
                            return Some(index);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    None
}

/// Consumes a char literal after its opening `'`, such as `{'` or `\u{7b}'`.
/// A lifetime such as `'a` is left alone.
fn skip_char_literal(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    let mut ahead = chars.clone();
    match (ahead.next(), ahead.next()) {
        (Some('\\'), _) => {
            chars.next();
            chars.next();
            for c in chars.by_ref() {
                if c == '\'' {
                    break;
                }
            }
        }
        (Some(_), Some('\'')) => {
            chars.next();
            chars.next();
        }
        _ => {}
    }
}

/// Whether the `}` on `lines[close]` ends its `if` with no `else` after it.
fn closes_without_else(lines: &[String], close: usize) -> bool {
    lines[close].trim() == "}"
        && !lines[close + 1..]
            .iter()
            .map(|line| line.trim())
            .find(|line| !line.is_empty())
            .is_some_and(|line| line.starts_with("else"))
}

// ── Pass 2: Style Constant Folding ─────────────────────────────────────

fn pass_style_constant_folding(
    plan: &mut EmissionPlan,
    threshold: usize,
) -> Vec<TransformationRecord> {
    let mut records = Vec::new();

    if let Some(style_file) = plan.files.get_mut("src/style.rs") {
        let before = style_file.content.clone();
        let before_lines = before.lines().count();
        let lines: Vec<&str> = before.lines().collect();

        // Find duplicate constants: same type and value. Only unindented ones
        // count. The associated consts of a theme's `impl` cannot be named bare
        // from elsewhere, so an alias to one did not compile. Neither did an
        // alias between two types that happened to share a value.
        let mut value_to_names: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for line in &lines {
            if let Some((name, key)) = style_const_key(line) {
                value_to_names
                    .entry(key.to_string())
                    .or_default()
                    .push(name.to_string());
            }
        }

        // Find values with duplicates above threshold
        let duplicates: Vec<(String, Vec<String>)> = value_to_names
            .into_iter()
            .filter(|(_, names)| names.len() >= threshold)
            .collect();

        if !duplicates.is_empty() {
            let mut new_lines: Vec<String> = Vec::new();
            let mut folded_names: BTreeSet<String> = BTreeSet::new();
            let mut fold_comment_added = false;

            // Track which names are aliases
            for (_, names) in &duplicates {
                for name in &names[1..] {
                    folded_names.insert(name.clone());
                }
            }

            for line in &lines {
                let folded = style_const_key(line).filter(|(name, _)| folded_names.contains(*name));
                let Some((name, key)) = folded else {
                    new_lines.push(line.to_string());
                    continue;
                };
                if !fold_comment_added {
                    new_lines
                        .push("// Folded duplicate style constants (see aliases below)".into());
                    fold_comment_added = true;
                }
                // Replace with an alias to the first constant with this value.
                if let Some((_, names)) = duplicates.iter().find(|(k, _)| k == key) {
                    let (ty, _) = key.split_once(" = ").unwrap_or((key, ""));
                    new_lines.push(format!("pub const {name}: {ty} = {};", names[0]));
                }
            }

            let new_content = new_lines.join("\n");
            let after_lines = new_content.lines().count();
            records.push(TransformationRecord {
                pass: PassKind::StyleConstantFolding,
                file_path: "src/style.rs".into(),
                description: format!(
                    "Folded {} duplicate style constant groups",
                    duplicates.len()
                ),
                lines_removed: before_lines.saturating_sub(after_lines),
                lines_added: after_lines.saturating_sub(before_lines),
                before_snippet: snippet(&before, 3),
                after_snippet: snippet(&new_content, 3),
            });
            style_file.content = new_content;
        }
    }

    records
}

/// The name of an unindented `pub const NAME: TYPE = VALUE;` line and its
/// `TYPE = VALUE` key.
fn style_const_key(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("pub const ")?.strip_suffix(';')?;
    let (name, key) = rest.split_once(": ")?;
    key.contains(" = ").then_some((name, key))
}

// ── Pass 3: Helper Extraction ──────────────────────────────────────────

fn pass_helper_extraction(plan: &mut EmissionPlan, threshold: usize) -> Vec<TransformationRecord> {
    let mut records = Vec::new();

    if let Some(view_file) = plan.files.get_mut("src/view.rs") {
        let before = view_file.content.clone();
        let before_lines = before.lines().count();
        let lines: Vec<&str> = before.lines().collect();

        // Find repeated `frame.render_widget(...)` calls with identical widget constructors
        let mut widget_calls: BTreeMap<String, usize> = BTreeMap::new();
        for line in &lines {
            let trimmed = line.trim();
            if trimmed.starts_with("frame.render_widget(") && trimmed.ends_with(", area);") {
                let widget_expr =
                    &trimmed["frame.render_widget(".len()..trimmed.len() - ", area);".len()];
                *widget_calls.entry(widget_expr.to_string()).or_insert(0) += 1;
            }
        }

        // Extract helpers for expressions repeated >= threshold times
        let to_extract: Vec<(String, usize)> = widget_calls
            .into_iter()
            .filter(|(_, count)| *count >= threshold)
            .collect();

        if !to_extract.is_empty() {
            let mut new_lines: Vec<String> = Vec::new();
            let mut helpers: Vec<String> = Vec::new();
            let mut helper_map: BTreeMap<String, String> = BTreeMap::new();

            for (i, (expr, _)) in to_extract.iter().enumerate() {
                let fn_name = format!("render_widget_{i}");
                helper_map.insert(expr.clone(), fn_name.clone());
                helpers.push(format!(
                    "fn {fn_name}(frame: &mut ftui_render::Frame, area: ftui_layout::Rect) {{"
                ));
                helpers.push(format!("    frame.render_widget({expr}, area);"));
                helpers.push("}".into());
                helpers.push(String::new());
            }

            // Replace inline calls with helper calls
            for line in &lines {
                let trimmed = line.trim();
                if trimmed.starts_with("frame.render_widget(") && trimmed.ends_with(", area);") {
                    let widget_expr =
                        &trimmed["frame.render_widget(".len()..trimmed.len() - ", area);".len()];
                    if let Some(fn_name) = helper_map.get(widget_expr) {
                        let indent = &line[..line.len() - line.trim_start().len()];
                        new_lines.push(format!("{indent}{fn_name}(frame, area);"));
                        continue;
                    }
                }
                new_lines.push(line.to_string());
            }

            // Append helpers at the end
            new_lines.push(String::new());
            new_lines.push("// ── Extracted Widget Helpers ──".into());
            new_lines.push(String::new());
            new_lines.extend(helpers);

            let new_content = new_lines.join("\n");
            let after_lines = new_content.lines().count();
            records.push(TransformationRecord {
                pass: PassKind::HelperExtraction,
                file_path: "src/view.rs".into(),
                description: format!("Extracted {} repeated widget helpers", to_extract.len()),
                lines_removed: before_lines.saturating_sub(after_lines),
                lines_added: after_lines.saturating_sub(before_lines),
                before_snippet: snippet(&before, 3),
                after_snippet: snippet(&new_content, 3),
            });
            view_file.content = new_content;
        }
    }

    records
}

// ── Pass 4: Import Deduplication ───────────────────────────────────────

fn pass_import_deduplication(plan: &mut EmissionPlan) -> Vec<TransformationRecord> {
    let mut records = Vec::new();

    let paths: Vec<String> = plan
        .files
        .iter()
        .filter(|(_, f)| f.kind == FileKind::RustSource)
        .map(|(p, _)| p.clone())
        .collect();

    for path in paths {
        if let Some(file) = plan.files.get_mut(&path) {
            let before = file.content.clone();
            let before_lines = before.lines().count();
            let lines: Vec<&str> = before.lines().collect();

            let mut seen_imports: BTreeSet<String> = BTreeSet::new();
            let mut new_lines: Vec<String> = Vec::new();
            let mut removed = 0_usize;

            for line in &lines {
                let trimmed = line.trim();
                if trimmed.starts_with("use ") && trimmed.ends_with(';') {
                    if seen_imports.contains(trimmed) {
                        removed += 1;
                        continue;
                    }
                    seen_imports.insert(trimmed.to_string());
                }
                new_lines.push(line.to_string());
            }

            if removed > 0 {
                let new_content = new_lines.join("\n");
                let after_lines = new_content.lines().count();
                records.push(TransformationRecord {
                    pass: PassKind::ImportDeduplication,
                    file_path: path.clone(),
                    description: format!("Removed {removed} duplicate import(s)"),
                    lines_removed: before_lines.saturating_sub(after_lines),
                    lines_added: 0,
                    before_snippet: snippet(&before, 3),
                    after_snippet: snippet(&new_content, 3),
                });
                file.content = new_content;
            }
        }
    }

    records
}

// ── Pass 5: Whitespace Normalization ───────────────────────────────────

fn pass_whitespace_normalization(plan: &mut EmissionPlan) -> Vec<TransformationRecord> {
    let mut records = Vec::new();

    let paths: Vec<String> = plan
        .files
        .iter()
        .filter(|(_, f)| f.kind == FileKind::RustSource)
        .map(|(p, _)| p.clone())
        .collect();

    for path in paths {
        if let Some(file) = plan.files.get_mut(&path) {
            let before = file.content.clone();
            let before_lines = before.lines().count();
            let lines: Vec<&str> = before.lines().collect();

            let mut new_lines: Vec<String> = Vec::new();
            let mut prev_blank = false;

            for line in &lines {
                let is_blank = line.trim().is_empty();

                // Collapse runs of 2+ blank lines into exactly 1
                if is_blank && prev_blank {
                    continue;
                }

                new_lines.push(line.to_string());
                prev_blank = is_blank;
            }

            // Remove trailing blank line
            while new_lines.last().is_some_and(|l| l.trim().is_empty()) {
                new_lines.pop();
            }

            let new_content = new_lines.join("\n");
            let after_lines = new_content.lines().count();

            if before_lines != after_lines {
                records.push(TransformationRecord {
                    pass: PassKind::WhitespaceNormalization,
                    file_path: path.clone(),
                    description: "Normalized blank-line spacing".into(),
                    lines_removed: before_lines.saturating_sub(after_lines),
                    lines_added: 0,
                    before_snippet: snippet(&before, 3),
                    after_snippet: snippet(&new_content, 3),
                });
                file.content = new_content;
            }
        }
    }

    records
}

// ── Helpers ────────────────────────────────────────────────────────────

fn snippet(content: &str, max_lines: usize) -> String {
    content
        .lines()
        .take(max_lines)
        .collect::<Vec<_>>()
        .join("\n")
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_emission::{
        EmissionStats, EmittedFile, FileKind, MigrationManifest, ProjectScaffold,
    };

    fn make_plan_with_files(files: Vec<(&str, &str, FileKind)>) -> EmissionPlan {
        let mut file_map = BTreeMap::new();
        for (path, content, kind) in files {
            file_map.insert(
                path.to_string(),
                EmittedFile {
                    path: path.to_string(),
                    content: content.to_string(),
                    kind,
                    confidence: 1.0,
                    provenance_links: vec![],
                },
            );
        }
        EmissionPlan {
            version: "test".into(),
            run_id: "test".into(),
            scaffold: ProjectScaffold {
                crate_name: "test".into(),
                crate_version: "0.1.0".into(),
                edition: "2024".into(),
                dependencies: vec![],
                module_tree: vec![],
            },
            files: file_map,
            module_graph: vec![],
            manifest: MigrationManifest {
                version: "test".into(),
                source_project: "test".into(),
                plan_version: "test".into(),
                strategy_links: vec![],
                overall_confidence: 1.0,
                gap_count: 0,
                requires_human_review: false,
            },
            diagnostics: vec![],
            stats: EmissionStats::default(),
        }
    }

    // ── Dead Branch Elimination ────────────────────────────────────────

    #[test]
    fn dead_branch_removes_if_false() {
        let code = "fn test() {\n    if false {\n        do_stuff();\n    }\n    keep_this();\n}";
        let plan = make_plan_with_files(vec![("src/test.rs", code, FileKind::RustSource)]);
        let result = optimize(plan);
        let content = &result.plan.files["src/test.rs"].content;
        assert!(!content.contains("if false"));
        assert!(!content.contains("do_stuff()"));
        assert!(content.contains("keep_this()"));
    }

    #[test]
    fn dead_branch_unwraps_if_true() {
        let code = "fn test() {\n    if true {\n        do_stuff();\n    }\n}";
        let plan = make_plan_with_files(vec![("src/test.rs", code, FileKind::RustSource)]);
        let result = optimize(plan);
        let content = &result.plan.files["src/test.rs"].content;
        assert!(!content.contains("if true"));
        assert!(content.contains("do_stuff()"));
    }

    #[test]
    fn dead_branch_keeps_structure_and_scope() {
        let optimized = |code: &str| {
            let plan = make_plan_with_files(vec![("src/test.rs", code, FileKind::RustSource)]);
            optimize(plan).plan.files["src/test.rs"].content.clone()
        };

        // The body's first line opens a brace, and its `let` must stay scoped.
        assert_eq!(
            optimized(
                "fn t() {\n    let s = 0;\n    if true {\n        let s = S {\n            a: 1,\n        };\n        use_it(s);\n    }\n    after(s);\n}"
            ),
            "fn t() {\n    let s = 0;\n    {\n        let s = S {\n            a: 1,\n        };\n        use_it(s);\n    }\n    after(s);\n}"
        );
        // Braces in strings, chars and comments do not close the block.
        assert_eq!(
            optimized(
                "fn t() {\n    if false {\n        let s = \"}\"; // }\n        let c = '}'; /* } */\n        let e = '\\'';\n    }\n    keep();\n}"
            ),
            "fn t() {\n    keep();\n}"
        );
        // A guard with an `else` is left alone.
        let with_else =
            "fn t() {\n    if false {\n        a();\n    } else {\n        b();\n    }\n}";
        assert_eq!(optimized(with_else), with_else);
        let else_below =
            "fn t() {\n    if true {\n        a();\n    }\n    else {\n        b();\n    }\n}";
        assert_eq!(optimized(else_below), else_below);
    }

    #[test]
    fn dead_branch_marks_noop_arms() {
        let code = "    match msg {\n        Msg::Noop => {\n            ftui_runtime::Cmd::None\n        }\n    }";
        let plan = make_plan_with_files(vec![("src/update.rs", code, FileKind::RustSource)]);
        let result = optimize(plan);
        let content = &result.plan.files["src/update.rs"].content;
        assert!(content.contains("// no-op"));
    }

    #[test]
    fn dead_branch_skips_non_rust_files() {
        let code = "if false {\n    stuff\n}";
        let plan = make_plan_with_files(vec![("Cargo.toml", code, FileKind::CargoToml)]);
        let result = optimize(plan);
        // Should be unchanged
        assert_eq!(result.plan.files["Cargo.toml"].content, code);
    }

    // ── Style Constant Folding ────────────────────────────────────────

    #[test]
    fn style_folding_merges_duplicates() {
        let code = "pub const COLOR_PRIMARY: ftui_style::Color = Color::Red;\npub const COLOR_ACCENT: ftui_style::Color = Color::Red;\npub const COLOR_UNIQUE: ftui_style::Color = Color::Blue;";
        let plan = make_plan_with_files(vec![("src/style.rs", code, FileKind::RustSource)]);
        let result = optimize(plan);
        let content = &result.plan.files["src/style.rs"].content;
        // PRIMARY should keep its value, ACCENT should reference PRIMARY
        assert!(content.contains("COLOR_PRIMARY"));
        assert!(content.contains("COLOR_UNIQUE"));
        // At least one transformation should have happened
        assert!(result.stats.transformations > 0);
    }

    #[test]
    fn style_folding_respects_threshold() {
        let code = "pub const A: u8 = 1;\npub const B: u8 = 2;\npub const C: u8 = 3;";
        let plan = make_plan_with_files(vec![("src/style.rs", code, FileKind::RustSource)]);
        // With threshold 2, no values are duplicated enough
        let result = optimize(plan);
        assert_eq!(result.plan.files["src/style.rs"].content, code);
    }

    #[test]
    fn style_folding_aliases_only_nameable_constants_of_one_type() {
        let code = "pub const COLOR_A: ftui_style::Color = ftui_style::Color::rgb(1, 2, 3);\n\
                    pub const COLOR_B: ftui_style::Color = ftui_style::Color::rgb(1, 2, 3);\n\
                    pub const SMALL: u8 = 1;\n\
                    pub const WIDE: u16 = 1;\n\
                    pub struct DarkTheme;\n\
                    impl DarkTheme {\n    \
                    pub const BG: ftui_style::Color = ftui_style::Color::rgb(9, 9, 9);\n}\n\
                    pub struct LightTheme;\n\
                    impl LightTheme {\n    \
                    pub const BG: ftui_style::Color = ftui_style::Color::rgb(9, 9, 9);\n}";
        let plan = make_plan_with_files(vec![("src/style.rs", code, FileKind::RustSource)]);
        let result = optimize(plan);
        let content = &result.plan.files["src/style.rs"].content;

        assert!(
            content.contains("pub const COLOR_B: ftui_style::Color = COLOR_A;"),
            "{content}"
        );
        assert!(content.contains("pub const WIDE: u16 = 1;"), "{content}");
        assert_eq!(
            content
                .matches("pub const BG: ftui_style::Color = ftui_style::Color::rgb(9, 9, 9);")
                .count(),
            2,
            "{content}"
        );
    }

    #[test]
    fn style_folding_only_targets_style_rs() {
        let code = "pub const X: u8 = 1;\npub const Y: u8 = 1;";
        let plan = make_plan_with_files(vec![("src/model.rs", code, FileKind::RustSource)]);
        let result = optimize(plan);
        // model.rs should not be touched by style folding
        assert_eq!(result.plan.files["src/model.rs"].content, code);
    }

    // ── Helper Extraction ─────────────────────────────────────────────

    #[test]
    fn helper_extraction_extracts_repeated_widgets() {
        let code = "pub fn view() {\n    frame.render_widget(Block::default(), area);\n    frame.render_widget(Block::default(), area);\n    frame.render_widget(Block::default(), area);\n}";
        let plan = make_plan_with_files(vec![("src/view.rs", code, FileKind::RustSource)]);
        let result = optimize(plan);
        let content = &result.plan.files["src/view.rs"].content;
        assert!(content.contains("render_widget_0(frame, area)"));
        assert!(content.contains("fn render_widget_0("));
    }

    #[test]
    fn helper_extraction_below_threshold_noop() {
        let code = "pub fn view() {\n    frame.render_widget(Block::default(), area);\n    frame.render_widget(Block::default(), area);\n}";
        let plan = make_plan_with_files(vec![("src/view.rs", code, FileKind::RustSource)]);
        // Default threshold is 3, only 2 occurrences
        let result = optimize(plan);
        let content = &result.plan.files["src/view.rs"].content;
        assert!(!content.contains("render_widget_0"));
    }

    // ── Import Deduplication ──────────────────────────────────────────

    #[test]
    fn import_dedup_removes_duplicates() {
        let code = "use crate::model::Model;\nuse crate::msg::Msg;\nuse crate::model::Model;\n\nfn update() {}";
        let plan = make_plan_with_files(vec![("src/update.rs", code, FileKind::RustSource)]);
        let result = optimize(plan);
        let content = &result.plan.files["src/update.rs"].content;
        let import_count = content.matches("use crate::model::Model;").count();
        assert_eq!(import_count, 1);
        assert!(result.stats.transformations > 0);
    }

    #[test]
    fn import_dedup_preserves_different_imports() {
        let code = "use crate::model::Model;\nuse crate::msg::Msg;\n\nfn update() {}";
        let plan = make_plan_with_files(vec![("src/update.rs", code, FileKind::RustSource)]);
        let result = optimize(plan);
        assert_eq!(result.plan.files["src/update.rs"].content, code);
    }

    // ── Whitespace Normalization ──────────────────────────────────────

    #[test]
    fn whitespace_collapses_double_blanks() {
        let code = "fn a() {}\n\n\n\nfn b() {}";
        let plan = make_plan_with_files(vec![("src/test.rs", code, FileKind::RustSource)]);
        let result = optimize(plan);
        let content = &result.plan.files["src/test.rs"].content;
        assert_eq!(content, "fn a() {}\n\nfn b() {}");
    }

    #[test]
    fn whitespace_removes_trailing_blanks() {
        let code = "fn a() {}\n\n";
        let plan = make_plan_with_files(vec![("src/test.rs", code, FileKind::RustSource)]);
        let result = optimize(plan);
        let content = &result.plan.files["src/test.rs"].content;
        assert_eq!(content, "fn a() {}");
    }

    #[test]
    fn whitespace_preserves_single_blanks() {
        let code = "fn a() {}\n\nfn b() {}";
        let plan = make_plan_with_files(vec![("src/test.rs", code, FileKind::RustSource)]);
        let result = optimize(plan);
        assert_eq!(result.plan.files["src/test.rs"].content, code);
    }

    // ── Integration ───────────────────────────────────────────────────

    #[test]
    fn full_pipeline_runs_all_passes() {
        let plan = make_plan_with_files(vec![
            (
                "src/model.rs",
                "use crate::a;\nuse crate::a;\n\n\n\nfn model() {}",
                FileKind::RustSource,
            ),
            (
                "src/view.rs",
                "fn view() {\n    frame.render_widget(X, area);\n}",
                FileKind::RustSource,
            ),
            (
                "Cargo.toml",
                "[package]\nname = \"test\"",
                FileKind::CargoToml,
            ),
        ]);

        let result = optimize(plan);
        assert_eq!(result.stats.passes_executed, 5);
        assert!(result.stats.transformations > 0);
    }

    #[test]
    fn custom_config_runs_only_selected_passes() {
        let plan = make_plan_with_files(vec![(
            "src/test.rs",
            "fn a() {}\n\n\n\nfn b() {}",
            FileKind::RustSource,
        )]);

        let config = OptimizeConfig {
            passes: vec![PassKind::WhitespaceNormalization],
            style_fold_threshold: 2,
            helper_extract_threshold: 3,
        };

        let result = optimize_with_config(plan, &config);
        assert_eq!(result.stats.passes_executed, 1);
        assert!(
            result
                .records
                .iter()
                .all(|r| r.pass == PassKind::WhitespaceNormalization)
        );
    }

    #[test]
    fn optimization_is_deterministic() {
        let plan1 = make_plan_with_files(vec![
            (
                "src/test.rs",
                "fn a() {}\n\n\n\nfn b() {}",
                FileKind::RustSource,
            ),
            (
                "src/style.rs",
                "pub const X: u8 = 1;\npub const Y: u8 = 1;",
                FileKind::RustSource,
            ),
        ]);
        let plan2 = make_plan_with_files(vec![
            (
                "src/test.rs",
                "fn a() {}\n\n\n\nfn b() {}",
                FileKind::RustSource,
            ),
            (
                "src/style.rs",
                "pub const X: u8 = 1;\npub const Y: u8 = 1;",
                FileKind::RustSource,
            ),
        ]);

        let r1 = optimize(plan1);
        let r2 = optimize(plan2);

        let j1 = serde_json::to_string(&r1).unwrap();
        let j2 = serde_json::to_string(&r2).unwrap();
        assert_eq!(j1, j2);
    }

    #[test]
    fn transformation_records_have_snippets() {
        let code = "fn a() {}\n\n\n\nfn b() {}";
        let plan = make_plan_with_files(vec![("src/test.rs", code, FileKind::RustSource)]);
        let result = optimize(plan);
        for rec in &result.records {
            assert!(!rec.before_snippet.is_empty());
            assert!(!rec.after_snippet.is_empty());
        }
    }

    #[test]
    fn empty_plan_produces_zero_transformations() {
        let plan = make_plan_with_files(vec![]);
        let result = optimize(plan);
        assert_eq!(result.stats.transformations, 0);
        assert_eq!(result.stats.passes_executed, 5);
    }

    #[test]
    fn net_line_change_is_negative_for_reductions() {
        let code = "fn a() {}\n\n\n\n\n\nfn b() {}";
        let plan = make_plan_with_files(vec![("src/test.rs", code, FileKind::RustSource)]);
        let result = optimize(plan);
        assert!(result.stats.net_line_change < 0);
    }
}
