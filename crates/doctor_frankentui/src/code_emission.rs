// SPDX-License-Identifier: Apache-2.0
//! Code emission backend, module partitioner, and project scaffolder.
//!
//! Consumes the translated outputs from all pipeline stages and produces an
//! [`EmissionPlan`] — a deterministic, structured description of every file
//! in the generated FrankenTUI project:
//!
//! - **Model module**: struct fields, defaults, derived computations
//! - **Message module**: enum variants with payloads
//! - **Update module**: match arms dispatching message → state mutations + commands
//! - **View module**: widget tree with layout, props, conditional rendering
//! - **Style module**: color/typography/border mappings, theme structs
//! - **Effects module**: Cmd/subscription wiring with timeout, retry, cancellation
//! - **Main module**: program entry point wiring Model + init + update + view
//! - **Cargo.toml**: workspace-aware dependency declarations
//! - **Migration manifest**: links back to strategy decisions and evidence records

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::effect_translator::EffectOrchestrationPlan;
use crate::migration_ir::{MigrationIr, Provenance};
use crate::state_event_translator::TranslatedRuntime;
use crate::style_translator::TranslatedStyle;
use crate::translation_planner::TranslationPlan;
use crate::view_layout_translator::TranslatedView;

// ── Constants ──────────────────────────────────────────────────────────

/// Module version tag.
pub const CODE_EMISSION_VERSION: &str = "code-emission-v1";

/// Default crate name for the generated project.
const DEFAULT_CRATE_NAME: &str = "migrated-app";

/// FrankenTUI dependency version to pin in generated Cargo.toml.
const FTUI_VERSION: &str = "0.1.0";

// ── Core Output Types ──────────────────────────────────────────────────

/// The complete emission plan — describes every file to generate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmissionPlan {
    /// Schema version.
    pub version: String,
    /// Source run id for traceability.
    pub run_id: String,
    /// Project-level scaffold (Cargo.toml, directory layout).
    pub scaffold: ProjectScaffold,
    /// All generated files keyed by relative path.
    pub files: BTreeMap<String, EmittedFile>,
    /// Module dependency graph (which modules import which).
    pub module_graph: Vec<ModuleDependency>,
    /// Migration manifest linking to evidence/strategy records.
    pub manifest: MigrationManifest,
    /// Diagnostics from emission.
    pub diagnostics: Vec<EmissionDiagnostic>,
    /// Statistics.
    pub stats: EmissionStats,
}

/// A single file to be written to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmittedFile {
    /// Relative path within the generated project (e.g. "src/model.rs").
    pub path: String,
    /// File content (Rust source, TOML, JSON, etc.).
    pub content: String,
    /// What kind of file this is.
    pub kind: FileKind,
    /// Confidence in the generated content (min of contributing translations).
    pub confidence: f64,
    /// Source provenance entries that contributed to this file.
    pub provenance_links: Vec<ProvenanceLink>,
}

/// What kind of generated file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileKind {
    /// Rust source file.
    RustSource,
    /// Cargo.toml manifest.
    CargoToml,
    /// Migration metadata (JSON).
    MigrationMetadata,
    /// README or documentation.
    Documentation,
}

/// A link from generated code back to source provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenanceLink {
    /// Generated line range (start, end inclusive).
    pub generated_lines: (usize, usize),
    /// Original source location.
    pub source: Provenance,
    /// Strategy decision id that authorized this emission.
    pub decision_id: Option<String>,
}

/// Project scaffold — Cargo.toml and directory structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectScaffold {
    /// Crate name for the generated project.
    pub crate_name: String,
    /// Crate version.
    pub crate_version: String,
    /// Rust edition.
    pub edition: String,
    /// Dependencies for Cargo.toml.
    pub dependencies: Vec<CrateDependency>,
    /// Module tree (ordered list of modules to declare in lib.rs/main.rs).
    pub module_tree: Vec<ModuleDecl>,
}

/// A dependency entry for Cargo.toml.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrateDependency {
    /// Crate name.
    pub name: String,
    /// Version requirement.
    pub version: String,
    /// Optional features to enable.
    pub features: Vec<String>,
    /// Whether this is a path dependency (workspace member).
    pub path: Option<String>,
}

/// A module declaration in the generated project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleDecl {
    /// Module name (e.g. "model", "msg", "update").
    pub name: String,
    /// Relative file path.
    pub file_path: String,
    /// What this module contains.
    pub purpose: String,
}

/// A dependency edge between modules.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleDependency {
    /// The importing module.
    pub from: String,
    /// The imported module.
    pub to: String,
    /// What symbols are imported.
    pub symbols: Vec<String>,
}

/// Migration manifest for audit trail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationManifest {
    /// Schema version.
    pub version: String,
    /// Source project identifier.
    pub source_project: String,
    /// Translation plan version used.
    pub plan_version: String,
    /// Per-module strategy links.
    pub strategy_links: Vec<StrategyLink>,
    /// Overall migration confidence (min across all files).
    pub overall_confidence: f64,
    /// Total gap count from translation plan.
    pub gap_count: usize,
    /// Human review required?
    pub requires_human_review: bool,
}

/// A link from a generated module to the strategy decision that produced it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyLink {
    /// Generated module name.
    pub module: String,
    /// Strategy decision ids that contributed.
    pub decision_ids: Vec<String>,
    /// Minimum confidence across contributing decisions.
    pub confidence: f64,
}

/// An emission diagnostic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmissionDiagnostic {
    /// Diagnostic code.
    pub code: String,
    /// Severity: info, warning, error.
    pub severity: String,
    /// Message.
    pub message: String,
    /// Related file path.
    pub file_path: Option<String>,
}

/// Emission statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmissionStats {
    /// Total files emitted.
    pub total_files: usize,
    /// Rust source files emitted.
    pub rust_files: usize,
    /// Total lines of generated Rust code.
    pub total_rust_lines: usize,
    /// Total model fields.
    pub model_fields: usize,
    /// Total message variants.
    pub message_variants: usize,
    /// Total update arms.
    pub update_arms: usize,
    /// Total widgets in view tree.
    pub widget_count: usize,
    /// Total effects wired.
    pub effects_wired: usize,
    /// Minimum confidence across all files.
    pub min_confidence: f64,
}

// ── Public API ─────────────────────────────────────────────────────────

/// All translated outputs bundled for emission.
pub struct EmissionInputs<'a> {
    pub ir: &'a MigrationIr,
    pub runtime: &'a TranslatedRuntime,
    pub view: &'a TranslatedView,
    pub style: &'a TranslatedStyle,
    pub effects: &'a EffectOrchestrationPlan,
    pub plan: &'a TranslationPlan,
}

/// Emit a complete FrankenTUI project from translated pipeline outputs.
pub fn emit_project(inputs: &EmissionInputs<'_>) -> EmissionPlan {
    let mut files = BTreeMap::new();
    let mut diagnostics = Vec::new();
    let mut stats = EmissionStats::default();
    let mut module_graph = Vec::new();

    // 1. Scaffold
    let scaffold = build_scaffold(inputs);

    // 2. Emit each module
    let model_file = emit_model_module(inputs, &mut diagnostics);
    let msg_file = emit_message_module(inputs, &mut diagnostics);
    let update_file = emit_update_module(inputs, &mut diagnostics);
    let view_file = emit_view_module(inputs, &mut diagnostics);
    let style_file = emit_style_module(inputs, &mut diagnostics);
    let effects_file = emit_effects_module(inputs, &mut diagnostics);
    let main_file = emit_main_module(inputs, &mut diagnostics);

    // 3. Emit Cargo.toml
    let cargo_file = emit_cargo_toml(&scaffold);

    // 4. Emit migration manifest
    let manifest = build_manifest(inputs, &files);
    let manifest_file = emit_manifest_file(&manifest);

    // 5. Build module graph
    module_graph.push(ModuleDependency {
        from: "main".into(),
        to: "model".into(),
        symbols: vec!["Model".into()],
    });
    module_graph.push(ModuleDependency {
        from: "main".into(),
        to: "msg".into(),
        symbols: vec!["Msg".into()],
    });
    module_graph.push(ModuleDependency {
        from: "main".into(),
        to: "update".into(),
        symbols: vec!["update".into()],
    });
    module_graph.push(ModuleDependency {
        from: "main".into(),
        to: "view".into(),
        symbols: vec!["view".into()],
    });
    module_graph.push(ModuleDependency {
        from: "update".into(),
        to: "model".into(),
        symbols: vec!["Model".into()],
    });
    module_graph.push(ModuleDependency {
        from: "update".into(),
        to: "msg".into(),
        symbols: vec!["Msg".into()],
    });
    module_graph.push(ModuleDependency {
        from: "update".into(),
        to: "effects".into(),
        symbols: vec!["make_cmd".into()],
    });
    module_graph.push(ModuleDependency {
        from: "view".into(),
        to: "model".into(),
        symbols: vec!["Model".into()],
    });
    module_graph.push(ModuleDependency {
        from: "view".into(),
        to: "style".into(),
        symbols: vec!["theme".into()],
    });

    // 6. Collect files and compute stats
    for file in [
        model_file,
        msg_file,
        update_file,
        view_file,
        style_file,
        effects_file,
        main_file,
        cargo_file,
        manifest_file,
    ] {
        update_stats(&file, &mut stats);
        files.insert(file.path.clone(), file);
    }

    stats.min_confidence = files
        .values()
        .map(|f| f.confidence)
        .fold(f64::INFINITY, f64::min);
    if stats.min_confidence == f64::INFINITY {
        stats.min_confidence = 0.0;
    }

    // Rebuild manifest with complete file map
    let manifest = build_manifest(inputs, &files);

    EmissionPlan {
        version: CODE_EMISSION_VERSION.to_string(),
        run_id: inputs.ir.run_id.clone(),
        scaffold,
        files,
        module_graph,
        manifest,
        diagnostics,
        stats,
    }
}

// ── Scaffold ───────────────────────────────────────────────────────────

fn build_scaffold(inputs: &EmissionInputs<'_>) -> ProjectScaffold {
    let crate_name = sanitize_crate_name(&inputs.ir.source_project);

    let mut dependencies = vec![
        CrateDependency {
            name: "ftui".into(),
            version: FTUI_VERSION.into(),
            features: vec![],
            path: None,
        },
        CrateDependency {
            name: "ftui-runtime".into(),
            version: FTUI_VERSION.into(),
            features: vec![],
            path: None,
        },
        CrateDependency {
            name: "ftui-widgets".into(),
            version: FTUI_VERSION.into(),
            features: vec![],
            path: None,
        },
        CrateDependency {
            name: "ftui-layout".into(),
            version: FTUI_VERSION.into(),
            features: vec![],
            path: None,
        },
        CrateDependency {
            name: "ftui-core".into(),
            version: FTUI_VERSION.into(),
            features: vec![],
            path: None,
        },
        CrateDependency {
            name: "ftui-render".into(),
            version: FTUI_VERSION.into(),
            features: vec![],
            path: None,
        },
        CrateDependency {
            name: "ftui-style".into(),
            version: FTUI_VERSION.into(),
            features: vec![],
            path: None,
        },
    ];

    // Add crossterm for event handling.
    dependencies.push(CrateDependency {
        name: "crossterm".into(),
        version: "0.28".into(),
        features: vec![],
        path: None,
    });

    let module_tree = vec![
        ModuleDecl {
            name: "model".into(),
            file_path: "src/model.rs".into(),
            purpose: "Application state (Model struct + defaults)".into(),
        },
        ModuleDecl {
            name: "msg".into(),
            file_path: "src/msg.rs".into(),
            purpose: "Message enum (all event variants)".into(),
        },
        ModuleDecl {
            name: "update".into(),
            file_path: "src/update.rs".into(),
            purpose: "Update function (message dispatch + state mutations)".into(),
        },
        ModuleDecl {
            name: "view".into(),
            file_path: "src/view.rs".into(),
            purpose: "View function (widget tree rendering)".into(),
        },
        ModuleDecl {
            name: "style".into(),
            file_path: "src/style.rs".into(),
            purpose: "Style definitions (colors, typography, themes)".into(),
        },
        ModuleDecl {
            name: "effects".into(),
            file_path: "src/effects.rs".into(),
            purpose: "Effect orchestration (Cmd/subscription wiring)".into(),
        },
    ];

    ProjectScaffold {
        crate_name,
        crate_version: "0.1.0".into(),
        edition: "2024".into(),
        dependencies,
        module_tree,
    }
}

fn sanitize_crate_name(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = sanitized.trim_matches('-').to_lowercase();
    if trimmed.is_empty() {
        DEFAULT_CRATE_NAME.to_string()
    } else {
        trimmed
    }
}

// ── Model Module ───────────────────────────────────────────────────────

fn emit_model_module(
    inputs: &EmissionInputs<'_>,
    diagnostics: &mut Vec<EmissionDiagnostic>,
) -> EmittedFile {
    let mut lines = Vec::new();
    let mut provenance_links = Vec::new();
    let mut min_confidence = 1.0_f64;

    lines.push("//! Application state model.".into());
    lines.push(String::new());

    // Struct definition
    let model_name = &inputs.runtime.model.name;
    lines.push("#[derive(Debug, Clone)]".into());
    lines.push(format!("pub struct {model_name} {{"));

    for field in &inputs.runtime.model.fields {
        let start_line = lines.len() + 1;
        lines.push(format!("    pub {}: {},", field.name, field.rust_type));
        provenance_links.push(ProvenanceLink {
            generated_lines: (start_line, start_line),
            source: field.provenance.clone(),
            decision_id: None,
        });
        min_confidence = min_confidence.min(1.0); // fields have implicit full confidence
    }

    // Shared fields
    for shared in &inputs.runtime.model.shared_fields {
        lines.push(format!("    pub {}: {},", shared.name, shared.access_path));
    }

    lines.push("}".into());
    lines.push(String::new());

    // Default impl
    lines.push(format!("impl Default for {model_name} {{"));
    lines.push("    fn default() -> Self {".into());
    lines.push(format!("        {model_name} {{"));

    for field in &inputs.runtime.model.fields {
        lines.push(format!(
            "            {}: {},",
            field.name,
            model_initial_value(field)
        ));
    }

    for shared in &inputs.runtime.model.shared_fields {
        lines.push(format!("            {}: Default::default(),", shared.name));
    }

    lines.push("        }".into());
    lines.push("    }".into());
    lines.push("}".into());

    if inputs.runtime.model.fields.is_empty() {
        diagnostics.push(EmissionDiagnostic {
            code: "CE001".into(),
            severity: "warning".into(),
            message: "Model has no fields; generated struct will be empty".into(),
            file_path: Some("src/model.rs".into()),
        });
    }

    let content = lines.join("\n");

    EmittedFile {
        path: "src/model.rs".into(),
        content,
        kind: FileKind::RustSource,
        confidence: min_confidence,
        provenance_links,
    }
}

fn model_initial_value(field: &crate::state_event_translator::ModelField) -> String {
    if field.rust_type == "String"
        && !field.initial_value.ends_with(".to_string()")
        && field.initial_value != "String::new()"
        && !(field.initial_value.starts_with('"') && field.initial_value.ends_with('"'))
    {
        return format!("{:?}.to_string()", field.initial_value);
    }
    field.initial_value.clone()
}

// ── Message Module ─────────────────────────────────────────────────────

fn emit_message_module(
    inputs: &EmissionInputs<'_>,
    _diagnostics: &mut Vec<EmissionDiagnostic>,
) -> EmittedFile {
    let mut lines = Vec::new();
    let mut provenance_links = Vec::new();

    lines.push("//! Message enum — all events the application handles.".into());
    lines.push(String::new());

    let enum_name = &inputs.runtime.message_enum.name;
    lines.push("#[derive(Debug, Clone)]".into());
    lines.push(format!("pub enum {enum_name} {{"));

    for variant in &inputs.runtime.message_enum.variants {
        let start_line = lines.len() + 1;
        match &variant.payload {
            Some(payload) => {
                lines.push(format!(
                    "    {}({}),",
                    variant.name,
                    render_payload_type(payload)
                ));
            }
            None => {
                lines.push(format!("    {},", variant.name));
            }
        }
        provenance_links.push(ProvenanceLink {
            generated_lines: (start_line, start_line),
            source: Provenance {
                file: String::new(),
                line: 0,
                column: None,
                source_name: Some(variant.name.clone()),
                policy_category: None,
            },
            decision_id: None,
        });
    }

    lines.push("}".into());

    let content = lines.join("\n");

    EmittedFile {
        path: "src/msg.rs".into(),
        content,
        kind: FileKind::RustSource,
        confidence: 1.0,
        provenance_links,
    }
}

fn render_payload_type(payload: &str) -> String {
    match payload {
        "ftui_core::Event" => "ftui_core::event::Event".to_string(),
        other => other.to_string(),
    }
}

// ── Update Module ──────────────────────────────────────────────────────

fn emit_update_module(
    inputs: &EmissionInputs<'_>,
    diagnostics: &mut Vec<EmissionDiagnostic>,
) -> EmittedFile {
    let mut lines = Vec::new();
    let provenance_links = Vec::new();
    let min_confidence = 1.0_f64;
    let model_name = &inputs.runtime.model.name;
    let msg_name = &inputs.runtime.message_enum.name;

    lines.push("//! Update function — message dispatch and state transitions.".into());
    lines.push(String::new());
    lines.push(format!("use crate::model::{model_name};"));
    lines.push(format!("use crate::msg::{msg_name};"));
    lines.push(String::new());

    lines.push(format!(
        "pub fn update(model: &mut {model_name}, msg: {msg_name}) -> ftui_runtime::Cmd<{msg_name}> {{"
    ));
    lines.push("    match msg {".into());

    for arm in &inputs.runtime.update_arms {
        let guards_str = if arm.guards.is_empty() {
            String::new()
        } else {
            format!(" if {}", arm.guards.join(" && "))
        };
        let pattern = message_variant_pattern(msg_name, &arm.message_variant, inputs);

        lines.push(format!("        {pattern}{guards_str} => {{"));

        // Emit mutations
        for mutation in &arm.mutations {
            lines.push(format!(
                "            model.{} = {};",
                mutation.field, mutation.expression
            ));
        }

        // Emit commands
        if arm.commands.is_empty() {
            lines.push("            ftui_runtime::Cmd::None".into());
        } else if arm.commands.len() == 1 {
            let cmd = &arm.commands[0];
            lines.push(format!(
                "            {} // {}",
                emit_cmd_expression(cmd),
                cmd.description
            ));
        } else {
            lines.push("            ftui_runtime::Cmd::Batch(vec![".into());
            for cmd in &arm.commands {
                lines.push(format!(
                    "                {}, // {}",
                    emit_cmd_expression(cmd),
                    cmd.description
                ));
            }
            lines.push("            ])".into());
        }

        lines.push("        },".into());
    }

    lines.push("        _ => ftui_runtime::Cmd::None,".into());
    lines.push("    }".into());
    lines.push("}".into());

    if inputs.runtime.update_arms.is_empty() {
        diagnostics.push(EmissionDiagnostic {
            code: "CE002".into(),
            severity: "warning".into(),
            message: "No update arms generated; update function returns Cmd::None for all messages"
                .into(),
            file_path: Some("src/update.rs".into()),
        });
    }

    let content = lines.join("\n");

    EmittedFile {
        path: "src/update.rs".into(),
        content,
        kind: FileKind::RustSource,
        confidence: min_confidence,
        provenance_links,
    }
}

fn message_variant_pattern(
    msg_name: &str,
    variant_name: &str,
    inputs: &EmissionInputs<'_>,
) -> String {
    let has_payload = inputs
        .runtime
        .message_enum
        .variants
        .iter()
        .any(|variant| variant.name == variant_name && variant.payload.is_some());
    if has_payload {
        format!("{msg_name}::{variant_name}(_)")
    } else {
        format!("{msg_name}::{variant_name}")
    }
}

fn emit_cmd_expression(cmd: &crate::state_event_translator::CommandEmission) -> String {
    use crate::state_event_translator::CommandKind;
    match cmd.kind {
        CommandKind::Task => format!("ftui_runtime::Cmd::task(\"{}\")", cmd.description),
        CommandKind::Log => format!("ftui_runtime::Cmd::Log({:?}.into())", cmd.description),
        CommandKind::Quit => "ftui_runtime::Cmd::Quit".into(),
        CommandKind::Tick => {
            "ftui_runtime::Cmd::Tick(std::time::Duration::from_millis(100))".into()
        }
        CommandKind::Msg => format!(
            "ftui_runtime::Cmd::Msg(Msg::{}) /* TODO */",
            cmd.description
        ),
        CommandKind::Batch => "ftui_runtime::Cmd::Batch(vec![]) /* TODO */".into(),
        CommandKind::Sequence => "ftui_runtime::Cmd::Sequence(vec![]) /* TODO */".into(),
        CommandKind::SaveState => "ftui_runtime::Cmd::SaveState".into(),
        CommandKind::RestoreState => "ftui_runtime::Cmd::RestoreState".into(),
        CommandKind::SetMouseCapture => "ftui_runtime::Cmd::SetMouseCapture(true)".into(),
        CommandKind::None => "ftui_runtime::Cmd::None".into(),
    }
}

// ── View Module ────────────────────────────────────────────────────────

fn emit_view_module(
    inputs: &EmissionInputs<'_>,
    diagnostics: &mut Vec<EmissionDiagnostic>,
) -> EmittedFile {
    let mut lines = Vec::new();
    let mut provenance_links = Vec::new();
    let min_confidence = 1.0_f64;
    let model_name = &inputs.runtime.model.name;

    lines.push("//! View function — widget tree rendering.".into());
    lines.push(String::new());
    lines.push("use ftui_core::geometry::Rect;".into());
    lines.push("use ftui_widgets::Widget;".into());
    lines.push(String::new());
    lines.push(format!("use crate::model::{model_name};"));
    lines.push(String::new());

    lines.push(format!(
        "pub fn view(model: &{model_name}, frame: &mut ftui_render::frame::Frame) {{"
    ));
    lines.push("    let _ = model;".into());
    lines.push("    let area = Rect::new(0, 0, frame.width(), frame.height());".into());

    // Emit root widget rendering calls
    for (i, root_id) in inputs.view.roots.iter().enumerate() {
        if let Some(widget) = inputs.view.widgets.get(root_id) {
            let start_line = lines.len() + 1;
            emit_widget_tree(widget, inputs, &mut lines, 1);
            let end_line = lines.len();
            provenance_links.push(ProvenanceLink {
                generated_lines: (start_line, end_line),
                source: widget.provenance.clone(),
                decision_id: None,
            });
        } else {
            diagnostics.push(EmissionDiagnostic {
                code: "CE003".into(),
                severity: "warning".into(),
                message: format!(
                    "Root widget '{}' (index {}) not found in widget map",
                    root_id, i
                ),
                file_path: Some("src/view.rs".into()),
            });
        }
    }

    lines.push("}".into());

    let content = lines.join("\n");

    EmittedFile {
        path: "src/view.rs".into(),
        content,
        kind: FileKind::RustSource,
        confidence: min_confidence,
        provenance_links,
    }
}

fn emit_widget_tree(
    widget: &crate::view_layout_translator::WidgetNode,
    inputs: &EmissionInputs<'_>,
    lines: &mut Vec<String>,
    depth: usize,
) {
    let indent = "    ".repeat(depth);
    let widget_expr = widget_type_to_constructor(&widget.widget_type, &widget.props);

    // Conditional rendering
    if let Some(cond) = &widget.condition {
        lines.push(format!("{indent}if {} {{", cond.expression));
        lines.push(format!("{indent}    let widget = {widget_expr};"));
        lines.push(format!("{indent}    Widget::render(&widget, area, frame);"));
        lines.push(format!("{indent}}}"));
    } else {
        lines.push(format!("{indent}let widget = {widget_expr};"));
        lines.push(format!("{indent}Widget::render(&widget, area, frame);"));
    }

    // Recurse into children
    for child_id in &widget.children {
        if let Some(child) = inputs.view.widgets.get(child_id) {
            emit_widget_tree(child, inputs, lines, depth);
        }
    }
}

fn widget_type_to_constructor(
    wt: &crate::view_layout_translator::WidgetType,
    props: &[crate::view_layout_translator::WidgetProp],
) -> String {
    use crate::view_layout_translator::WidgetType;

    let base = match wt {
        WidgetType::Block => "ftui_widgets::block::Block::default()",
        WidgetType::Paragraph => "ftui_widgets::paragraph::Paragraph::new(\"\")",
        WidgetType::List => "ftui_widgets::block::Block::default()",
        WidgetType::Table => "ftui_widgets::block::Block::default()",
        WidgetType::Tabs => "ftui_widgets::block::Block::default()",
        WidgetType::TextInput => "ftui_widgets::input::TextInput::default()",
        WidgetType::ProgressBar => "ftui_widgets::progress::ProgressBar::new()",
        WidgetType::Scrollbar => {
            "ftui_widgets::scrollbar::Scrollbar::new(ftui_widgets::scrollbar::ScrollbarOrientation::VerticalRight)"
        }
        WidgetType::Spinner => "ftui_widgets::spinner::Spinner::new()",
        WidgetType::Rule => "ftui_widgets::rule::Rule::new()",
        WidgetType::Badge => "ftui_widgets::badge::Badge::new(\"\")",
        WidgetType::LayoutContainer | WidgetType::Fragment | WidgetType::Custom => {
            "ftui_widgets::block::Block::default()"
        }
    };

    // Apply title prop if present
    let title_prop = props.iter().find(|p| p.name == "title");
    if let Some(tp) = title_prop {
        format!("{base}.title({:?})", tp.value)
    } else {
        base.to_string()
    }
}

// ── Style Module ───────────────────────────────────────────────────────

fn emit_style_module(
    inputs: &EmissionInputs<'_>,
    _diagnostics: &mut Vec<EmissionDiagnostic>,
) -> EmittedFile {
    let mut lines = Vec::new();
    let mut min_confidence = 1.0_f64;

    lines.push("//! Style definitions — colors, typography, borders, themes.".into());
    lines.push(String::new());

    // Color constants
    if !inputs.style.color_mappings.is_empty() {
        lines.push("// ── Colors ──".into());
        lines.push(String::new());

        for (token, mapping) in &inputs.style.color_mappings {
            min_confidence = min_confidence.min(mapping.confidence);
            let comment = if mapping.a11y_adjusted {
                " // a11y-adjusted"
            } else {
                ""
            };
            lines.push(format!(
                "pub const COLOR_{}: ftui_style::Color = {};{comment}",
                token.to_uppercase().replace('-', "_"),
                mapping.ftui_repr
            ));
        }
        lines.push(String::new());
    }

    // Typography helpers
    if !inputs.style.typography_rules.is_empty() {
        lines.push("// ── Typography ──".into());
        lines.push(String::new());

        for (token, rule) in &inputs.style.typography_rules {
            min_confidence = min_confidence.min(rule.confidence);
            let flags = rule.flags.join(" | ");
            let modifier = if flags.is_empty() {
                "ftui_style::Modifier::empty()".into()
            } else {
                format!("ftui_style::Modifier::{flags}")
            };
            lines.push(format!(
                "pub const TYPO_{}: ftui_style::Modifier = {modifier};",
                token.to_uppercase().replace('-', "_")
            ));
        }
        lines.push(String::new());
    }

    // Border rules
    if !inputs.style.border_rules.is_empty() {
        lines.push("// ── Borders ──".into());
        lines.push(String::new());

        for (token, rule) in &inputs.style.border_rules {
            min_confidence = min_confidence.min(rule.confidence);
            lines.push(format!(
                "pub const BORDER_{}: ftui_widgets::BorderType = ftui_widgets::BorderType::{};",
                token.to_uppercase().replace('-', "_"),
                rule.border_type
            ));
        }
        lines.push(String::new());
    }

    // Theme struct
    if !inputs.style.themes.is_empty() {
        lines.push("// ── Themes ──".into());
        lines.push(String::new());

        for theme in &inputs.style.themes {
            let struct_name = to_pascal_case(&theme.name);
            lines.push(format!("pub struct {struct_name}Theme;"));
            lines.push(String::new());
            lines.push(format!("impl {struct_name}Theme {{"));
            for (key, color) in &theme.color_overrides {
                lines.push(format!(
                    "    pub const {}: ftui_style::Color = {};",
                    key.to_uppercase().replace('-', "_"),
                    color.ftui_repr
                ));
            }
            lines.push("}".into());
            lines.push(String::new());
        }
    }

    // Accessibility upgrades as comments
    if !inputs.style.accessibility_upgrades.is_empty() {
        lines.push("// ── Accessibility Upgrades Applied ──".into());
        for upgrade in &inputs.style.accessibility_upgrades {
            lines.push(format!(
                "// {}: {} → {} ({})",
                upgrade.token_name, upgrade.original, upgrade.upgraded, upgrade.rationale
            ));
        }
        lines.push(String::new());
    }

    let content = lines.join("\n");

    EmittedFile {
        path: "src/style.rs".into(),
        content,
        kind: FileKind::RustSource,
        confidence: min_confidence,
        provenance_links: Vec::new(),
    }
}

// ── Effects Module ─────────────────────────────────────────────────────

fn emit_effects_module(
    inputs: &EmissionInputs<'_>,
    diagnostics: &mut Vec<EmissionDiagnostic>,
) -> EmittedFile {
    let mut lines = Vec::new();
    let mut min_confidence = 1.0_f64;

    lines.push("//! Effect orchestration — Cmd and subscription wiring.".into());
    lines.push(String::new());
    lines.push("use crate::msg::Msg;".into());
    lines.push(String::new());

    let msg_name = &inputs.runtime.message_enum.name;

    // Emit subscription declarations
    if !inputs.runtime.subscriptions.is_empty() {
        lines.push("// ── Subscriptions ──".into());
        lines.push(String::new());

        lines.push(format!(
            "pub fn subscriptions() -> Vec<Box<dyn ftui_runtime::Subscription<{msg_name}>>> {{"
        ));
        lines.push("    vec![".into());

        for sub in &inputs.runtime.subscriptions {
            lines.push(format!("        // {}: {}", sub.name, sub.description));
            if sub.is_timer {
                lines.push(format!(
                    "        // Timer subscription → {msg_name}::{}",
                    sub.message_variant
                ));
            } else {
                lines.push(format!(
                    "        // Event subscription → {msg_name}::{}",
                    sub.message_variant
                ));
            }
        }

        lines.push("    ]".into());
        lines.push("}".into());
        lines.push(String::new());
    }

    // Emit effect orchestration comments from the plan
    if !inputs.effects.orchestrations.is_empty() {
        lines.push("// ── Effect Orchestration Plan ──".into());
        lines.push(String::new());

        for (id, orch) in &inputs.effects.orchestrations {
            min_confidence = min_confidence.min(orch.confidence);

            let construct = match orch.runtime_construct {
                crate::effect_translator::RuntimeConstruct::CmdTask => "Cmd::Task",
                crate::effect_translator::RuntimeConstruct::Subscription => "Subscription",
                crate::effect_translator::RuntimeConstruct::CmdFireAndForget => {
                    "Cmd::Task (fire-and-forget)"
                }
            };

            lines.push(format!("// Effect '{}' ({id}):", orch.name));
            lines.push(format!("//   Runtime: {construct}"));
            lines.push(format!("//   Timeout: {}ms", orch.timeout_ms));
            lines.push(format!("//   Async: {}", orch.async_boundary));

            if let Some(retry) = &orch.retry_config {
                lines.push(format!("//   Retry: max {} attempts", retry.max_attempts));
            }

            if orch.cancellation.observable {
                lines.push(format!("//   Cancellation: {:?}", orch.cancellation.kind));
            }

            lines.push(String::new());
        }
    }

    // Init commands
    if !inputs.runtime.init_commands.is_empty() {
        lines.push("// ── Init Commands ──".into());
        lines.push(String::new());

        lines.push(format!("pub fn init() -> ftui_runtime::Cmd<{msg_name}> {{"));

        if inputs.runtime.init_commands.len() == 1 {
            let cmd = &inputs.runtime.init_commands[0];
            lines.push(format!(
                "    {} // {}",
                emit_init_cmd_expression(cmd),
                cmd.description
            ));
        } else {
            lines.push("    ftui_runtime::Cmd::Batch(vec![".into());
            for cmd in &inputs.runtime.init_commands {
                lines.push(format!(
                    "        {}, // {}",
                    emit_init_cmd_expression(cmd),
                    cmd.description
                ));
            }
            lines.push("    ])".into());
        }

        lines.push("}".into());
    } else {
        lines.push(format!("pub fn init() -> ftui_runtime::Cmd<{msg_name}> {{"));
        lines.push("    ftui_runtime::Cmd::None".into());
        lines.push("}".into());
    }

    if inputs.effects.orchestrations.is_empty() && inputs.runtime.subscriptions.is_empty() {
        diagnostics.push(EmissionDiagnostic {
            code: "CE004".into(),
            severity: "info".into(),
            message: "No effects or subscriptions — application is purely synchronous".into(),
            file_path: Some("src/effects.rs".into()),
        });
    }

    let content = lines.join("\n");

    EmittedFile {
        path: "src/effects.rs".into(),
        content,
        kind: FileKind::RustSource,
        confidence: min_confidence,
        provenance_links: Vec::new(),
    }
}

fn emit_init_cmd_expression(cmd: &crate::state_event_translator::InitCommand) -> String {
    use crate::state_event_translator::CommandKind;
    match cmd.kind {
        CommandKind::Task => format!("ftui_runtime::Cmd::task(\"{}\")", cmd.description),
        CommandKind::Tick => {
            "ftui_runtime::Cmd::Tick(std::time::Duration::from_millis(100))".into()
        }
        CommandKind::Log => format!("ftui_runtime::Cmd::Log({:?}.into())", cmd.description),
        _ => format!("ftui_runtime::Cmd::None /* TODO: {} */", cmd.description),
    }
}

// ── Main Module ────────────────────────────────────────────────────────

fn emit_main_module(
    inputs: &EmissionInputs<'_>,
    _diagnostics: &mut Vec<EmissionDiagnostic>,
) -> EmittedFile {
    let mut lines = Vec::new();

    let model_name = &inputs.runtime.model.name;
    let msg_name = &inputs.runtime.message_enum.name;

    lines.push("//! Application entry point.".into());
    lines.push(String::new());
    lines.push("mod model;".into());
    lines.push("mod msg;".into());
    lines.push("mod update;".into());
    lines.push("mod view;".into());
    lines.push("mod style;".into());
    lines.push("mod effects;".into());
    lines.push(String::new());
    lines.push(format!("use model::{model_name};"));
    lines.push(format!("use msg::{msg_name};"));
    lines.push(String::new());

    lines.push("fn main() -> Result<(), Box<dyn std::error::Error>> {".into());
    lines.push(format!("    let _model = {model_name}::default();"));
    lines.push(format!(
        "    let _init_cmd: ftui_runtime::Cmd<{msg_name}> = effects::init();"
    ));
    lines.push(format!(
        "    let _update_fn: fn(&mut {model_name}, {msg_name}) -> ftui_runtime::Cmd<{msg_name}> = update::update;"
    ));
    lines.push(String::new());
    lines.push("    // Generated projects compile before runtime wiring is activated.".into());
    lines.push(String::new());
    lines.push("    Ok(())".into());
    lines.push("}".into());

    let content = lines.join("\n");

    EmittedFile {
        path: "src/main.rs".into(),
        content,
        kind: FileKind::RustSource,
        confidence: 1.0,
        provenance_links: Vec::new(),
    }
}

// ── Cargo.toml ─────────────────────────────────────────────────────────

fn emit_cargo_toml(scaffold: &ProjectScaffold) -> EmittedFile {
    let mut lines = Vec::new();

    lines.push("[package]".into());
    lines.push(format!("name = \"{}\"", scaffold.crate_name));
    lines.push(format!("version = \"{}\"", scaffold.crate_version));
    lines.push(format!("edition = \"{}\"", scaffold.edition));
    lines.push(String::new());
    lines.push("[dependencies]".into());

    for dep in &scaffold.dependencies {
        if dep.features.is_empty() {
            if let Some(path) = &dep.path {
                lines.push(format!(
                    "{} = {{ version = \"{}\", path = \"{}\" }}",
                    dep.name, dep.version, path
                ));
            } else {
                lines.push(format!("{} = \"{}\"", dep.name, dep.version));
            }
        } else {
            let features = dep
                .features
                .iter()
                .map(|f| format!("\"{f}\""))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!(
                "{} = {{ version = \"{}\", features = [{}] }}",
                dep.name, dep.version, features
            ));
        }
    }

    let content = lines.join("\n");

    EmittedFile {
        path: "Cargo.toml".into(),
        content,
        kind: FileKind::CargoToml,
        confidence: 1.0,
        provenance_links: Vec::new(),
    }
}

// ── Migration Manifest ─────────────────────────────────────────────────

fn build_manifest(
    inputs: &EmissionInputs<'_>,
    files: &BTreeMap<String, EmittedFile>,
) -> MigrationManifest {
    let mut strategy_links = Vec::new();

    // Group decisions by target_crate → module mapping
    let mut module_decisions: BTreeMap<String, Vec<(String, f64)>> = BTreeMap::new();
    for decision in &inputs.plan.decisions {
        let module = match decision.segment.category {
            crate::translation_planner::SegmentCategory::View => "view",
            crate::translation_planner::SegmentCategory::State => "model",
            crate::translation_planner::SegmentCategory::Event => "msg",
            crate::translation_planner::SegmentCategory::Effect => "effects",
            crate::translation_planner::SegmentCategory::Layout => "view",
            crate::translation_planner::SegmentCategory::Style => "style",
            crate::translation_planner::SegmentCategory::Accessibility => "style",
            crate::translation_planner::SegmentCategory::Capability => "effects",
        };
        module_decisions
            .entry(module.to_string())
            .or_default()
            .push((decision.segment.id.0.clone(), decision.confidence));
    }

    for (module, entries) in &module_decisions {
        let min_conf = entries
            .iter()
            .map(|(_, c)| *c)
            .fold(f64::INFINITY, f64::min);
        let decision_ids: Vec<String> = entries.iter().map(|(id, _)| id.clone()).collect();
        strategy_links.push(StrategyLink {
            module: module.clone(),
            decision_ids,
            confidence: if min_conf == f64::INFINITY {
                1.0
            } else {
                min_conf
            },
        });
    }

    let overall_confidence = files
        .values()
        .map(|f| f.confidence)
        .fold(f64::INFINITY, f64::min);

    let requires_human_review = inputs.plan.stats.human_review > 0
        || inputs.plan.stats.rejected > 0
        || overall_confidence < 0.7;

    MigrationManifest {
        version: CODE_EMISSION_VERSION.to_string(),
        source_project: inputs.ir.source_project.clone(),
        plan_version: inputs.plan.version.clone(),
        strategy_links,
        overall_confidence: if overall_confidence == f64::INFINITY {
            1.0
        } else {
            overall_confidence
        },
        gap_count: inputs.plan.gap_tickets.len(),
        requires_human_review,
    }
}

fn emit_manifest_file(manifest: &MigrationManifest) -> EmittedFile {
    let content = serde_json::to_string_pretty(manifest).unwrap_or_else(|_| "{}".into());

    EmittedFile {
        path: "migration_manifest.json".into(),
        content,
        kind: FileKind::MigrationMetadata,
        confidence: 1.0,
        provenance_links: Vec::new(),
    }
}

// ── Stats Helpers ──────────────────────────────────────────────────────

fn update_stats(file: &EmittedFile, stats: &mut EmissionStats) {
    stats.total_files += 1;
    if file.kind == FileKind::RustSource {
        stats.rust_files += 1;
        stats.total_rust_lines += file.content.lines().count();
    }
}

fn to_pascal_case(s: &str) -> String {
    s.split(['_', '-', ' '])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(c) => {
                    let upper: String = c.to_uppercase().collect();
                    upper + &chars.as_str().to_lowercase()
                }
                None => String::new(),
            }
        })
        .collect()
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::effect_canonical::OrderingConstraint;
    use crate::effect_translator::EffectTranslationStats;
    use crate::migration_ir::*;
    use crate::state_event_translator::*;
    use crate::style_translator::*;
    use crate::translation_planner::*;
    use crate::view_layout_translator::*;
    use std::collections::{BTreeMap, BTreeSet};

    // ── Helpers ────────────────────────────────────────────────────────

    fn make_ir() -> MigrationIr {
        MigrationIr {
            schema_version: "migration-ir-v1".into(),
            run_id: "test-run-001".into(),
            source_project: "my-react-app".into(),
            view_tree: ViewTree {
                roots: vec![],
                nodes: BTreeMap::new(),
            },
            state_graph: StateGraph {
                variables: BTreeMap::new(),
                derived: BTreeMap::new(),
                data_flow: BTreeMap::new(),
            },
            event_catalog: EventCatalog {
                events: BTreeMap::new(),
                transitions: vec![],
            },
            effect_registry: EffectRegistry {
                effects: BTreeMap::new(),
            },
            style_intent: StyleIntent {
                tokens: BTreeMap::new(),
                layouts: BTreeMap::new(),
                themes: vec![],
            },
            capabilities: CapabilityProfile {
                required: BTreeSet::new(),
                optional: BTreeSet::new(),
                platform_assumptions: vec![],
            },
            accessibility: AccessibilityMap {
                entries: BTreeMap::new(),
            },
            metadata: IrMetadata {
                created_at: "2026-01-01".into(),
                source_file_count: 0,
                total_nodes: 0,
                total_state_vars: 0,
                total_events: 0,
                total_effects: 0,
                warnings: vec![],
                integrity_hash: None,
            },
        }
    }

    fn make_runtime() -> TranslatedRuntime {
        TranslatedRuntime {
            version: "state-event-translator-v1".into(),
            run_id: "test-run-001".into(),
            model: ModelStruct {
                name: "AppModel".into(),
                fields: vec![ModelField {
                    name: "count".into(),
                    rust_type: "u32".into(),
                    initial_value: "0".into(),
                    scope: FieldScope::Local,
                    source_id: IrNodeId("s-count".into()),
                    derived: false,
                    dependencies: BTreeSet::new(),
                    provenance: Provenance {
                        file: "App.tsx".into(),
                        line: 5,
                        column: None,
                        source_name: Some("count".into()),
                        policy_category: None,
                    },
                }],
                shared_fields: vec![],
            },
            message_enum: MessageEnum {
                name: "Msg".into(),
                variants: vec![
                    MessageVariant {
                        name: "Increment".into(),
                        payload: None,
                        source_kind: TranslatedEventKind::UserInput,
                        source_id: IrNodeId("e-inc".into()),
                    },
                    MessageVariant {
                        name: "SetCount".into(),
                        payload: Some("u32".into()),
                        source_kind: TranslatedEventKind::Custom,
                        source_id: IrNodeId("e-set".into()),
                    },
                ],
            },
            update_arms: vec![UpdateArm {
                message_variant: "Increment".into(),
                guards: vec![],
                mutations: vec![StateMutation {
                    field: "count".into(),
                    expression: "model.count + 1".into(),
                    target_id: IrNodeId("t-inc".into()),
                }],
                commands: vec![],
                source_transition: None,
            }],
            init_commands: vec![],
            subscriptions: vec![],
            diagnostics: vec![],
            stats: TranslationStats {
                model_fields: 1,
                derived_fields: 0,
                shared_refs: 0,
                message_variants: 2,
                update_arms: 1,
                subscriptions: 0,
                init_commands: 0,
                diagnostics_by_level: BTreeMap::new(),
            },
        }
    }

    fn make_view() -> TranslatedView {
        let mut widgets = BTreeMap::new();
        widgets.insert(
            "w-root".into(),
            WidgetNode {
                id: "w-root".into(),
                name: "Root".into(),
                widget_type: WidgetType::Block,
                children: vec![],
                layout: LayoutDecl {
                    kind: LayoutDeclKind::Flex,
                    direction: Some("Vertical".into()),
                    alignment: None,
                    constraints: vec![],
                    gap: None,
                },
                props: vec![WidgetProp {
                    name: "title".into(),
                    value: "Counter App".into(),
                }],
                condition: None,
                focus: None,
                source_id: IrNodeId("v-root".into()),
                provenance: Provenance {
                    file: "App.tsx".into(),
                    line: 10,
                    column: None,
                    source_name: None,
                    policy_category: None,
                },
            },
        );

        TranslatedView {
            version: "view-layout-translator-v1".into(),
            run_id: "test-run-001".into(),
            roots: vec!["w-root".into()],
            widgets,
            focus_groups: vec![],
            layout_pattern: "single-panel".into(),
            diagnostics: vec![],
            stats: ViewTranslationStats {
                total_widgets: 0,
                by_type: BTreeMap::new(),
                focus_groups: 0,
                conditional_widgets: 0,
                layout_containers: 0,
                diagnostics_by_level: BTreeMap::new(),
            },
        }
    }

    fn make_style() -> TranslatedStyle {
        TranslatedStyle {
            version: "style-translator-v1".into(),
            run_id: "test-run-001".into(),
            color_mappings: BTreeMap::new(),
            typography_rules: BTreeMap::new(),
            spacing_rules: BTreeMap::new(),
            border_rules: BTreeMap::new(),
            layout_rules: BTreeMap::new(),
            themes: vec![],
            accessibility_upgrades: vec![],
            unsupported_tokens: vec![],
            diagnostics: vec![],
            stats: StyleTranslationStats {
                total_tokens: 0,
                colors_mapped: 0,
                typography_rules: 0,
                spacing_rules: 0,
                border_rules: 0,
                layout_rules: 0,
                themes_generated: 0,
                a11y_upgrades: 0,
                unsupported_count: 0,
            },
        }
    }

    fn make_effects() -> EffectOrchestrationPlan {
        EffectOrchestrationPlan {
            version: "effect-translator-v1".into(),
            orchestrations: BTreeMap::new(),
            ordering_constraints: vec![],
            diagnostics: vec![],
            stats: EffectTranslationStats::default(),
        }
    }

    fn make_plan() -> TranslationPlan {
        TranslationPlan {
            version: "translation-planner-v1".into(),
            run_id: "test-run-001".into(),
            seed: 42,
            decisions: vec![],
            gap_tickets: vec![],
            stats: PlanStats {
                total_segments: 0,
                auto_approve: 0,
                human_review: 0,
                rejected: 0,
                gap_tickets: 0,
                mean_confidence: 0.0,
                by_category: BTreeMap::new(),
                by_handling_class: BTreeMap::new(),
            },
        }
    }

    fn make_inputs<'a>(
        ir: &'a MigrationIr,
        runtime: &'a TranslatedRuntime,
        view: &'a TranslatedView,
        style: &'a TranslatedStyle,
        effects: &'a EffectOrchestrationPlan,
        plan: &'a TranslationPlan,
    ) -> EmissionInputs<'a> {
        EmissionInputs {
            ir,
            runtime,
            view,
            style,
            effects,
            plan,
        }
    }

    // ── Tests ──────────────────────────────────────────────────────────

    #[test]
    fn empty_inputs_produce_valid_plan() {
        let ir = make_ir();
        let runtime = TranslatedRuntime {
            version: "v1".into(),
            run_id: "r".into(),
            model: ModelStruct {
                name: "Model".into(),
                fields: vec![],
                shared_fields: vec![],
            },
            message_enum: MessageEnum {
                name: "Msg".into(),
                variants: vec![],
            },
            update_arms: vec![],
            init_commands: vec![],
            subscriptions: vec![],
            diagnostics: vec![],
            stats: TranslationStats {
                model_fields: 0,
                derived_fields: 0,
                shared_refs: 0,
                message_variants: 0,
                update_arms: 0,
                subscriptions: 0,
                init_commands: 0,
                diagnostics_by_level: BTreeMap::new(),
            },
        };
        let view = TranslatedView {
            version: "v1".into(),
            run_id: "r".into(),
            roots: vec![],
            widgets: BTreeMap::new(),
            focus_groups: vec![],
            layout_pattern: "empty".into(),
            diagnostics: vec![],
            stats: ViewTranslationStats {
                total_widgets: 0,
                by_type: BTreeMap::new(),
                focus_groups: 0,
                conditional_widgets: 0,
                layout_containers: 0,
                diagnostics_by_level: BTreeMap::new(),
            },
        };
        let style = make_style();
        let effects = make_effects();
        let plan = make_plan();
        let inputs = make_inputs(&ir, &runtime, &view, &style, &effects, &plan);

        let emission = emit_project(&inputs);

        assert_eq!(emission.version, CODE_EMISSION_VERSION);
        assert!(!emission.files.is_empty());
        assert!(emission.files.contains_key("src/model.rs"));
        assert!(emission.files.contains_key("src/main.rs"));
        assert!(emission.files.contains_key("Cargo.toml"));
    }

    #[test]
    fn model_module_has_fields() {
        let ir = make_ir();
        let runtime = make_runtime();
        let view = make_view();
        let style = make_style();
        let effects = make_effects();
        let plan = make_plan();
        let inputs = make_inputs(&ir, &runtime, &view, &style, &effects, &plan);

        let emission = emit_project(&inputs);
        let model = &emission.files["src/model.rs"];

        assert!(model.content.contains("pub struct AppModel"));
        assert!(model.content.contains("pub count: u32"));
        assert!(model.content.contains("impl Default for AppModel"));
        assert!(model.content.contains("count: 0"));
    }

    #[test]
    fn message_module_has_variants() {
        let ir = make_ir();
        let runtime = make_runtime();
        let view = make_view();
        let style = make_style();
        let effects = make_effects();
        let plan = make_plan();
        let inputs = make_inputs(&ir, &runtime, &view, &style, &effects, &plan);

        let emission = emit_project(&inputs);
        let msg = &emission.files["src/msg.rs"];

        assert!(msg.content.contains("pub enum Msg"));
        assert!(msg.content.contains("Increment,"));
        assert!(msg.content.contains("SetCount(u32),"));
    }

    #[test]
    fn update_module_has_arms() {
        let ir = make_ir();
        let runtime = make_runtime();
        let view = make_view();
        let style = make_style();
        let effects = make_effects();
        let plan = make_plan();
        let inputs = make_inputs(&ir, &runtime, &view, &style, &effects, &plan);

        let emission = emit_project(&inputs);
        let update = &emission.files["src/update.rs"];

        assert!(update.content.contains("pub fn update("));
        assert!(update.content.contains("Msg::Increment"));
        assert!(update.content.contains("model.count = model.count + 1"));
    }

    #[test]
    fn view_module_renders_widgets() {
        let ir = make_ir();
        let runtime = make_runtime();
        let view = make_view();
        let style = make_style();
        let effects = make_effects();
        let plan = make_plan();
        let inputs = make_inputs(&ir, &runtime, &view, &style, &effects, &plan);

        let emission = emit_project(&inputs);
        let view_file = &emission.files["src/view.rs"];

        assert!(view_file.content.contains("pub fn view("));
        assert!(view_file.content.contains("Block::default()"));
        assert!(view_file.content.contains("Counter App"));
    }

    #[test]
    fn cargo_toml_has_dependencies() {
        let ir = make_ir();
        let runtime = make_runtime();
        let view = make_view();
        let style = make_style();
        let effects = make_effects();
        let plan = make_plan();
        let inputs = make_inputs(&ir, &runtime, &view, &style, &effects, &plan);

        let emission = emit_project(&inputs);
        let cargo = &emission.files["Cargo.toml"];

        assert!(cargo.content.contains("[package]"));
        assert!(cargo.content.contains("name = \"my-react-app\""));
        assert!(cargo.content.contains("[dependencies]"));
        assert!(cargo.content.contains("ftui ="));
        assert!(cargo.content.contains("ftui-runtime ="));
    }

    #[test]
    fn migration_manifest_generated() {
        let ir = make_ir();
        let runtime = make_runtime();
        let view = make_view();
        let style = make_style();
        let effects = make_effects();
        let plan = make_plan();
        let inputs = make_inputs(&ir, &runtime, &view, &style, &effects, &plan);

        let emission = emit_project(&inputs);
        let manifest = &emission.files["migration_manifest.json"];

        assert_eq!(manifest.kind, FileKind::MigrationMetadata);
        assert!(manifest.content.contains("code-emission-v1"));
        assert!(manifest.content.contains("my-react-app"));
    }

    #[test]
    fn main_module_wires_everything() {
        let ir = make_ir();
        let runtime = make_runtime();
        let view = make_view();
        let style = make_style();
        let effects = make_effects();
        let plan = make_plan();
        let inputs = make_inputs(&ir, &runtime, &view, &style, &effects, &plan);

        let emission = emit_project(&inputs);
        let main = &emission.files["src/main.rs"];

        assert!(main.content.contains("mod model;"));
        assert!(main.content.contains("mod msg;"));
        assert!(main.content.contains("mod update;"));
        assert!(main.content.contains("mod view;"));
        assert!(main.content.contains("mod style;"));
        assert!(main.content.contains("mod effects;"));
        assert!(main.content.contains("fn main()"));
        assert!(main.content.contains("AppModel::default()"));
    }

    #[test]
    fn stats_computed_correctly() {
        let ir = make_ir();
        let runtime = make_runtime();
        let view = make_view();
        let style = make_style();
        let effects = make_effects();
        let plan = make_plan();
        let inputs = make_inputs(&ir, &runtime, &view, &style, &effects, &plan);

        let emission = emit_project(&inputs);

        // 7 Rust files + 1 Cargo.toml + 1 manifest = 9
        assert_eq!(emission.stats.total_files, 9);
        assert_eq!(emission.stats.rust_files, 7);
        assert!(emission.stats.total_rust_lines > 0);
    }

    #[test]
    fn module_graph_has_expected_edges() {
        let ir = make_ir();
        let runtime = make_runtime();
        let view = make_view();
        let style = make_style();
        let effects = make_effects();
        let plan = make_plan();
        let inputs = make_inputs(&ir, &runtime, &view, &style, &effects, &plan);

        let emission = emit_project(&inputs);

        let edge_pairs: Vec<(&str, &str)> = emission
            .module_graph
            .iter()
            .map(|e| (e.from.as_str(), e.to.as_str()))
            .collect();

        assert!(edge_pairs.contains(&("main", "model")));
        assert!(edge_pairs.contains(&("main", "msg")));
        assert!(edge_pairs.contains(&("main", "update")));
        assert!(edge_pairs.contains(&("main", "view")));
        assert!(edge_pairs.contains(&("update", "model")));
        assert!(edge_pairs.contains(&("update", "effects")));
        assert!(edge_pairs.contains(&("view", "style")));
    }

    #[test]
    fn sanitize_crate_name_handles_special_chars() {
        assert_eq!(sanitize_crate_name("My React App!"), "my-react-app");
        assert_eq!(sanitize_crate_name("simple"), "simple");
        assert_eq!(sanitize_crate_name(""), DEFAULT_CRATE_NAME);
        assert_eq!(sanitize_crate_name("---"), DEFAULT_CRATE_NAME);
        assert_eq!(sanitize_crate_name("my_app-v2"), "my_app-v2");
    }

    #[test]
    fn style_module_with_colors() {
        let ir = make_ir();
        let runtime = make_runtime();
        let view = make_view();
        let mut style = make_style();
        style.color_mappings.insert(
            "primary".into(),
            ColorMapping {
                token_name: "primary".into(),
                rgb: Some((0, 128, 255)),
                ftui_repr: "Color::Rgb(0, 128, 255)".into(),
                confidence: 0.95,
                a11y_adjusted: false,
                original_value: "#0080ff".into(),
                provenance: None,
            },
        );
        let effects = make_effects();
        let plan = make_plan();
        let inputs = make_inputs(&ir, &runtime, &view, &style, &effects, &plan);

        let emission = emit_project(&inputs);
        let style_file = &emission.files["src/style.rs"];

        assert!(style_file.content.contains("COLOR_PRIMARY"));
        assert!(style_file.content.contains("Color::Rgb(0, 128, 255)"));
    }

    #[test]
    fn effects_module_with_subscriptions() {
        let ir = make_ir();
        let mut runtime = make_runtime();
        runtime.subscriptions.push(SubscriptionDecl {
            name: "tick_timer".into(),
            description: "Periodic tick every 100ms".into(),
            message_variant: "Tick".into(),
            is_timer: true,
            source_effect_id: IrNodeId("eff-timer".into()),
        });
        let view = make_view();
        let style = make_style();
        let effects = make_effects();
        let plan = make_plan();
        let inputs = make_inputs(&ir, &runtime, &view, &style, &effects, &plan);

        let emission = emit_project(&inputs);
        let effects_file = &emission.files["src/effects.rs"];

        assert!(effects_file.content.contains("pub fn subscriptions()"));
        assert!(effects_file.content.contains("tick_timer"));
    }

    #[test]
    fn conditional_rendering_in_view() {
        let ir = make_ir();
        let runtime = make_runtime();
        let mut view = make_view();
        // Add a conditionally rendered widget
        view.widgets.insert(
            "w-cond".into(),
            WidgetNode {
                id: "w-cond".into(),
                name: "ConditionalBlock".into(),
                widget_type: WidgetType::Paragraph,
                children: vec![],
                layout: LayoutDecl {
                    kind: LayoutDeclKind::None,
                    direction: None,
                    alignment: None,
                    constraints: vec![],
                    gap: None,
                },
                props: vec![],
                condition: Some(RenderConditionDecl {
                    kind: "guard".into(),
                    expression: "model.count > 0".into(),
                    state_deps: vec![IrNodeId("s-count".into())],
                }),
                focus: None,
                source_id: IrNodeId("v-cond".into()),
                provenance: Provenance {
                    file: "App.tsx".into(),
                    line: 15,
                    column: None,
                    source_name: None,
                    policy_category: None,
                },
            },
        );
        view.roots.push("w-cond".into());
        let style = make_style();
        let effects = make_effects();
        let plan = make_plan();
        let inputs = make_inputs(&ir, &runtime, &view, &style, &effects, &plan);

        let emission = emit_project(&inputs);
        let view_file = &emission.files["src/view.rs"];

        assert!(view_file.content.contains("if model.count > 0"));
    }

    #[test]
    fn update_with_commands() {
        let ir = make_ir();
        let mut runtime = make_runtime();
        runtime.update_arms.push(UpdateArm {
            message_variant: "SetCount".into(),
            guards: vec![],
            mutations: vec![],
            commands: vec![CommandEmission {
                kind: CommandKind::Log,
                description: "Count updated".into(),
                source_effect_id: None,
            }],
            source_transition: None,
        });
        let view = make_view();
        let style = make_style();
        let effects = make_effects();
        let plan = make_plan();
        let inputs = make_inputs(&ir, &runtime, &view, &style, &effects, &plan);

        let emission = emit_project(&inputs);
        let update = &emission.files["src/update.rs"];

        assert!(update.content.contains("Msg::SetCount"));
        assert!(update.content.contains("Cmd::Log"));
    }

    #[test]
    fn manifest_flags_review_needed() {
        let ir = make_ir();
        let runtime = make_runtime();
        let view = make_view();
        let style = make_style();
        let effects = make_effects();
        let mut plan = make_plan();
        plan.stats.human_review = 3;
        let inputs = make_inputs(&ir, &runtime, &view, &style, &effects, &plan);

        let emission = emit_project(&inputs);

        assert!(emission.manifest.requires_human_review);
    }

    #[test]
    fn deterministic_output() {
        let ir = make_ir();
        let runtime = make_runtime();
        let view = make_view();
        let style = make_style();
        let effects = make_effects();
        let plan = make_plan();
        let inputs = make_inputs(&ir, &runtime, &view, &style, &effects, &plan);

        let p1 = emit_project(&inputs);
        let p2 = emit_project(&inputs);

        let j1 = serde_json::to_string(&p1).unwrap();
        let j2 = serde_json::to_string(&p2).unwrap();
        assert_eq!(j1, j2);
    }

    #[test]
    fn empty_model_produces_warning() {
        let ir = make_ir();
        let runtime = TranslatedRuntime {
            version: "v1".into(),
            run_id: "r".into(),
            model: ModelStruct {
                name: "EmptyModel".into(),
                fields: vec![],
                shared_fields: vec![],
            },
            message_enum: MessageEnum {
                name: "Msg".into(),
                variants: vec![],
            },
            update_arms: vec![],
            init_commands: vec![],
            subscriptions: vec![],
            diagnostics: vec![],
            stats: TranslationStats {
                model_fields: 0,
                derived_fields: 0,
                shared_refs: 0,
                message_variants: 0,
                update_arms: 0,
                subscriptions: 0,
                init_commands: 0,
                diagnostics_by_level: BTreeMap::new(),
            },
        };
        let view = TranslatedView {
            version: "v1".into(),
            run_id: "r".into(),
            roots: vec![],
            widgets: BTreeMap::new(),
            focus_groups: vec![],
            layout_pattern: "empty".into(),
            diagnostics: vec![],
            stats: ViewTranslationStats {
                total_widgets: 0,
                by_type: BTreeMap::new(),
                focus_groups: 0,
                conditional_widgets: 0,
                layout_containers: 0,
                diagnostics_by_level: BTreeMap::new(),
            },
        };
        let style = make_style();
        let effects = make_effects();
        let plan = make_plan();
        let inputs = make_inputs(&ir, &runtime, &view, &style, &effects, &plan);

        let emission = emit_project(&inputs);

        assert!(emission.diagnostics.iter().any(|d| d.code == "CE001"));
    }

    #[test]
    fn scaffold_has_correct_modules() {
        let ir = make_ir();
        let runtime = make_runtime();
        let view = make_view();
        let style = make_style();
        let effects = make_effects();
        let plan = make_plan();
        let inputs = make_inputs(&ir, &runtime, &view, &style, &effects, &plan);

        let emission = emit_project(&inputs);

        let module_names: Vec<&str> = emission
            .scaffold
            .module_tree
            .iter()
            .map(|m| m.name.as_str())
            .collect();

        assert!(module_names.contains(&"model"));
        assert!(module_names.contains(&"msg"));
        assert!(module_names.contains(&"update"));
        assert!(module_names.contains(&"view"));
        assert!(module_names.contains(&"style"));
        assert!(module_names.contains(&"effects"));
    }

    #[test]
    fn file_kinds_correct() {
        let ir = make_ir();
        let runtime = make_runtime();
        let view = make_view();
        let style = make_style();
        let effects = make_effects();
        let plan = make_plan();
        let inputs = make_inputs(&ir, &runtime, &view, &style, &effects, &plan);

        let emission = emit_project(&inputs);

        assert_eq!(emission.files["src/model.rs"].kind, FileKind::RustSource);
        assert_eq!(emission.files["Cargo.toml"].kind, FileKind::CargoToml);
        assert_eq!(
            emission.files["migration_manifest.json"].kind,
            FileKind::MigrationMetadata
        );
    }
}
