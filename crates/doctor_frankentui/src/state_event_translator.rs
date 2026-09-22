// SPDX-License-Identifier: Apache-2.0
//! Translate state and event semantics into ftui-runtime Model/update/subscriptions.
//!
//! Consumes a [`MigrationIr`] (state graph, event catalog, effect registry)
//! plus a [`CanonicalEffectModel`] and produces a [`TranslatedRuntime`] —
//! a structured description of the generated ftui-runtime code:
//!
//! - **Model struct**: fields from IR state variables, derived computations
//! - **Message enum**: variants from IR events + internal lifecycle signals
//! - **update() logic**: match arms per message variant, preserving transition
//!   invariants and guard conditions
//! - **Subscriptions**: from canonical effects classified as `Subscription`
//! - **Commands**: from canonical effects classified as `Command`
//!
//! Design invariants:
//! - **One-writer rule**: each state field has at most one writer path through
//!   update(); concurrent writes are flagged as diagnostics.
//! - **Deterministic ordering**: fields, variants, and match arms are sorted
//!   by IR node id for reproducible output.
//! - **Transition preservation**: every `EventTransition` in the IR maps to
//!   exactly one match arm with guard conditions preserved.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::effect_canonical::{CanonicalEffect, CanonicalEffectModel, ExecutionModel};
use crate::migration_ir::{
    DerivedState, EventKind, EventTransition, IrNodeId, MigrationIr, Provenance, StateScope,
    StateVariable,
};
use crate::util::rust_comment_text;

// ── Constants ──────────────────────────────────────────────────────────

/// Module version tag.
pub const TRANSLATOR_VERSION: &str = "state-event-translator-v1";

// ── Core Output Types ──────────────────────────────────────────────────

/// The complete translated runtime description.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslatedRuntime {
    /// Schema version.
    pub version: String,
    /// Source IR run id.
    pub run_id: String,
    /// The generated Model struct.
    pub model: ModelStruct,
    /// The generated Message enum.
    pub message_enum: MessageEnum,
    /// The generated update() match arms.
    pub update_arms: Vec<UpdateArm>,
    /// The generated init() commands.
    pub init_commands: Vec<InitCommand>,
    /// The generated subscriptions.
    pub subscriptions: Vec<SubscriptionDecl>,
    /// Diagnostics emitted during translation.
    pub diagnostics: Vec<TranslationDiagnostic>,
    /// Statistics.
    pub stats: TranslationStats,
}

/// A generated Model struct field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelField {
    /// Field name (snake_case from IR variable name).
    pub name: String,
    /// Rust type annotation.
    pub rust_type: String,
    /// Default/initial value expression.
    pub initial_value: String,
    /// Source scope from the IR.
    pub scope: FieldScope,
    /// Source IR node id.
    pub source_id: IrNodeId,
    /// Whether this is a derived (computed) field.
    pub derived: bool,
    /// IDs of state variables this field depends on (for derived fields).
    pub dependencies: BTreeSet<IrNodeId>,
    /// Provenance from the IR.
    pub provenance: Provenance,
}

/// The generated Model struct.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelStruct {
    /// Struct name (PascalCase from source project).
    pub name: String,
    /// Fields in deterministic order.
    pub fields: Vec<ModelField>,
    /// Shared/global fields that live outside this model.
    pub shared_fields: Vec<SharedFieldRef>,
}

/// A reference to a field that lives in a parent/global scope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedFieldRef {
    /// Field name.
    pub name: String,
    /// How to access it (e.g. "shared.field_name").
    pub access_path: String,
    /// Source scope.
    pub scope: FieldScope,
    /// Source IR node id.
    pub source_id: IrNodeId,
}

/// Scope of a translated field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldScope {
    /// Model-local field.
    Local,
    /// Shared across components via a registry.
    Shared,
    /// Global application state.
    Global,
    /// Screen/route state.
    Route,
    /// Server-fetched (backed by Cmd::Task).
    Server,
}

impl From<StateScope> for FieldScope {
    fn from(scope: StateScope) -> Self {
        match scope {
            StateScope::Local => FieldScope::Local,
            StateScope::Context => FieldScope::Shared,
            StateScope::Global => FieldScope::Global,
            StateScope::Route => FieldScope::Route,
            StateScope::Server => FieldScope::Server,
        }
    }
}

/// The generated Message enum.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageEnum {
    /// Enum name.
    pub name: String,
    /// Variants in deterministic order.
    pub variants: Vec<MessageVariant>,
}

/// A variant in the Message enum.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageVariant {
    /// Variant name (PascalCase from IR event name).
    pub name: String,
    /// Optional payload type.
    pub payload: Option<String>,
    /// Source event kind.
    pub source_kind: TranslatedEventKind,
    /// Source IR node id.
    pub source_id: IrNodeId,
}

/// Translated event kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TranslatedEventKind {
    /// Direct user input (key/mouse).
    UserInput,
    /// Lifecycle (init, mount, unmount).
    Lifecycle,
    /// Timer tick.
    Timer,
    /// Network/async response.
    Network,
    /// Custom application event.
    Custom,
    /// Internal: terminal event passthrough.
    TerminalEvent,
    /// Internal: effect response.
    EffectResponse,
}

impl From<EventKind> for TranslatedEventKind {
    fn from(kind: EventKind) -> Self {
        match kind {
            EventKind::UserInput => TranslatedEventKind::UserInput,
            EventKind::Lifecycle => TranslatedEventKind::Lifecycle,
            EventKind::Timer => TranslatedEventKind::Timer,
            EventKind::Network => TranslatedEventKind::Network,
            EventKind::Custom => TranslatedEventKind::Custom,
        }
    }
}

/// A match arm in the update() function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateArm {
    /// The message variant this arm matches.
    pub message_variant: String,
    /// Guard conditions (from IR transition guards).
    pub guards: Vec<String>,
    /// State mutations performed.
    pub mutations: Vec<StateMutation>,
    /// Commands emitted.
    pub commands: Vec<CommandEmission>,
    /// Source transition from the IR.
    pub source_transition: Option<TransitionRef>,
}

/// A state mutation within an update arm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateMutation {
    /// Target field name.
    pub field: String,
    /// Assignment expression.
    pub expression: String,
    /// Source state variable id.
    pub target_id: IrNodeId,
}

/// A command emitted from an update arm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandEmission {
    /// Kind of command.
    pub kind: CommandKind,
    /// Description of what this command does.
    pub description: String,
    /// Source effect id (if from canonical effect model).
    pub source_effect_id: Option<IrNodeId>,
}

/// Kind of emitted command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandKind {
    None,
    Quit,
    Msg,
    Task,
    Tick,
    Log,
    Batch,
    Sequence,
    SaveState,
    RestoreState,
    SetMouseCapture,
}

/// Reference to a source IR transition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionRef {
    pub event_id: IrNodeId,
    pub target_state: IrNodeId,
    pub action_snippet: String,
}

/// An init() command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitCommand {
    /// Kind of command.
    pub kind: CommandKind,
    /// Description.
    pub description: String,
    /// Source effect id.
    pub source_effect_id: Option<IrNodeId>,
}

/// A subscription declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionDecl {
    /// Subscription name (snake_case).
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// The message variant this subscription sends.
    pub message_variant: String,
    /// Whether this is a timer (Every) subscription.
    pub is_timer: bool,
    /// Source canonical effect id.
    pub source_effect_id: IrNodeId,
}

/// A diagnostic emitted during translation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslationDiagnostic {
    /// Severity level.
    pub level: DiagnosticLevel,
    /// Diagnostic message.
    pub message: String,
    /// Related IR node ids.
    pub related_ids: Vec<IrNodeId>,
}

/// Diagnostic severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiagnosticLevel {
    Info,
    Warning,
    Error,
}

/// Statistics about the translation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslationStats {
    pub model_fields: usize,
    pub derived_fields: usize,
    pub shared_refs: usize,
    pub message_variants: usize,
    pub update_arms: usize,
    pub subscriptions: usize,
    pub init_commands: usize,
    pub diagnostics_by_level: BTreeMap<String, usize>,
}

// ── Public API ─────────────────────────────────────────────────────────

/// Translate state and event semantics from the IR into ftui-runtime constructs.
pub fn translate_state_events(
    ir: &MigrationIr,
    effects: Option<&CanonicalEffectModel>,
) -> TranslatedRuntime {
    let mut diagnostics = Vec::new();

    // Step 1: Build model fields from state variables.
    let (fields, shared_fields) = build_model_fields(ir, &mut diagnostics);

    // Step 2: Build message variants from events.
    let event_variants = event_variant_names(ir);
    let variants = build_message_variants(ir, effects, &event_variants);

    // Step 3: Build update arms from event transitions.
    let update_arms = build_update_arms(ir, effects, &event_variants, &mut diagnostics);

    // Step 4: Build init commands from lifecycle effects.
    let init_commands = build_init_commands(effects);

    // Step 5: Build subscriptions from canonical effects.
    let subscriptions = build_subscriptions(effects);

    // Step 6: Check one-writer rule.
    check_one_writer_rule(ir, &mut diagnostics);

    let model_name = derive_model_name(&ir.source_project);

    let stats = TranslationStats {
        model_fields: fields.len(),
        derived_fields: fields.iter().filter(|f| f.derived).count(),
        shared_refs: shared_fields.len(),
        message_variants: variants.len(),
        update_arms: update_arms.len(),
        subscriptions: subscriptions.len(),
        init_commands: init_commands.len(),
        diagnostics_by_level: count_diagnostics(&diagnostics),
    };

    TranslatedRuntime {
        version: TRANSLATOR_VERSION.to_string(),
        run_id: ir.run_id.clone(),
        model: ModelStruct {
            name: model_name,
            fields,
            shared_fields,
        },
        message_enum: MessageEnum {
            name: "Msg".to_string(),
            variants,
        },
        update_arms,
        init_commands,
        subscriptions,
        diagnostics,
        stats,
    }
}

// ── Model Field Construction ───────────────────────────────────────────

fn build_model_fields(
    ir: &MigrationIr,
    diagnostics: &mut Vec<TranslationDiagnostic>,
) -> (Vec<ModelField>, Vec<SharedFieldRef>) {
    let mut fields = Vec::new();
    let mut shared = Vec::new();

    // Concrete state variables → fields.
    let mut sorted_vars: Vec<_> = ir.state_graph.variables.iter().collect();
    sorted_vars.sort_by_key(|(id, _)| (*id).clone());

    for (id, var) in sorted_vars {
        let scope = FieldScope::from(var.scope.clone());
        match scope {
            FieldScope::Local | FieldScope::Route => {
                fields.push(model_field_from_var(id, var, false));
            }
            FieldScope::Shared | FieldScope::Global => {
                shared.push(SharedFieldRef {
                    name: to_snake_case(&var.name),
                    access_path: format!("shared.{}", to_snake_case(&var.name)),
                    scope,
                    source_id: id.clone(),
                });
            }
            FieldScope::Server => {
                // Server state → model field + Cmd::Task fetch.
                fields.push(model_field_from_var(id, var, false));
                diagnostics.push(TranslationDiagnostic {
                    level: DiagnosticLevel::Info,
                    message: format!(
                        "Server state '{}' will require Cmd::Task for data fetching",
                        var.name
                    ),
                    related_ids: vec![id.clone()],
                });
            }
        }
    }

    // Derived state → computed fields.
    let mut sorted_derived: Vec<_> = ir.state_graph.derived.iter().collect();
    sorted_derived.sort_by_key(|(id, _)| (*id).clone());

    for (id, derived) in sorted_derived {
        fields.push(model_field_from_derived(id, derived));
    }

    (fields, shared)
}

fn model_field_from_var(id: &IrNodeId, var: &StateVariable, derived: bool) -> ModelField {
    let rust_type = ir_type_to_rust(var.type_annotation.as_deref());
    let initial_value = var
        .initial_value
        .as_deref()
        .map(ir_value_to_rust)
        .unwrap_or_else(|| default_for_type(&rust_type));

    ModelField {
        name: to_snake_case(&var.name),
        rust_type,
        initial_value,
        scope: FieldScope::from(var.scope.clone()),
        source_id: id.clone(),
        derived,
        dependencies: BTreeSet::new(),
        provenance: var.provenance.clone(),
    }
}

fn model_field_from_derived(id: &IrNodeId, derived: &DerivedState) -> ModelField {
    ModelField {
        name: to_snake_case(&derived.name),
        rust_type: "String".to_string(), // Conservative default for derived.
        initial_value: "String::new()".to_string(),
        scope: FieldScope::Local,
        source_id: id.clone(),
        derived: true,
        dependencies: derived.dependencies.clone(),
        provenance: derived.provenance.clone(),
    }
}

// ── Message Variant Construction ───────────────────────────────────────

/// The message variant of each event: its name in PascalCase, numbered when
/// events share one.
///
/// Two elements with an `onChange` handler are two events named `onChange`.
/// Each used to get an `OnChange` variant and arm, and the duplicate variant
/// did not compile.
fn event_variant_names(ir: &MigrationIr) -> BTreeMap<IrNodeId, String> {
    let mut taken = BTreeSet::from(["TerminalEvent".to_string()]);
    let mut names = BTreeMap::new();
    for (id, event) in &ir.event_catalog.events {
        let base = to_pascal_case(&event.name);
        let mut name = base.clone();
        let mut suffix = 2;
        while !taken.insert(name.clone()) {
            name = format!("{base}{suffix}");
            suffix += 1;
        }
        names.insert(id.clone(), name);
    }
    names
}

fn build_message_variants(
    ir: &MigrationIr,
    effects: Option<&CanonicalEffectModel>,
    event_variants: &BTreeMap<IrNodeId, String>,
) -> Vec<MessageVariant> {
    let mut variants = Vec::new();

    // Event-sourced variants.
    let mut sorted_events: Vec<_> = ir.event_catalog.events.iter().collect();
    sorted_events.sort_by_key(|(id, _)| (*id).clone());

    for (id, event) in sorted_events {
        variants.push(MessageVariant {
            name: event_variants[id].clone(),
            payload: event
                .payload_type
                .as_deref()
                .map(|t| ir_type_to_rust(Some(t))),
            source_kind: TranslatedEventKind::from(event.kind.clone()),
            source_id: id.clone(),
        });
    }

    // Effect response variants from canonical effects classified as Command.
    if let Some(effect_model) = effects {
        let mut sorted_cmds: Vec<_> = effect_model.commands.iter().collect();
        sorted_cmds.sort();

        for cmd_id in sorted_cmds {
            if let Some(effect) = effect_model.effects.get(cmd_id) {
                // Only add if not already covered by an event.
                let variant_name = format!("{}Response", to_pascal_case(&effect.name));
                if !variants.iter().any(|v| v.name == variant_name) {
                    variants.push(MessageVariant {
                        name: variant_name,
                        payload: Some("String".to_string()),
                        source_kind: TranslatedEventKind::EffectResponse,
                        source_id: cmd_id.clone(),
                    });
                }
            }
        }
    }

    // Always include a TerminalEvent passthrough variant.
    variants.push(MessageVariant {
        name: "TerminalEvent".to_string(),
        payload: Some("ftui_core::Event".to_string()),
        source_kind: TranslatedEventKind::TerminalEvent,
        source_id: IrNodeId("ir-builtin-terminal-event".to_string()),
    });

    variants
}

// ── Update Arm Construction ────────────────────────────────────────────

fn build_update_arms(
    ir: &MigrationIr,
    effects: Option<&CanonicalEffectModel>,
    event_variants: &BTreeMap<IrNodeId, String>,
    diagnostics: &mut Vec<TranslationDiagnostic>,
) -> Vec<UpdateArm> {
    let mut arms = Vec::new();

    // Build arms from IR transitions.
    let mut sorted_transitions = ir.event_catalog.transitions.clone();
    sorted_transitions.sort_by(|a, b| a.event_id.cmp(&b.event_id));

    for transition in &sorted_transitions {
        let variant_name = event_variants
            .get(&transition.event_id)
            .cloned()
            .unwrap_or_else(|| format!("Unknown_{}", transition.event_id.0));

        let mutations = build_mutations_from_transition(ir, transition);
        let commands = build_commands_from_transition(transition, effects, diagnostics);

        arms.push(UpdateArm {
            message_variant: variant_name,
            guards: transition.guards.clone(),
            mutations,
            commands,
            source_transition: Some(TransitionRef {
                event_id: transition.event_id.clone(),
                target_state: transition.target_state.clone(),
                action_snippet: transition.action_snippet.clone(),
            }),
        });
    }

    // Add a default arm for TerminalEvent passthrough.
    arms.push(UpdateArm {
        message_variant: "TerminalEvent".to_string(),
        guards: Vec::new(),
        mutations: Vec::new(),
        commands: vec![CommandEmission {
            kind: CommandKind::None,
            description: "Terminal event passthrough (no-op by default)".to_string(),
            source_effect_id: None,
        }],
        source_transition: None,
    });

    arms
}

fn build_mutations_from_transition(
    ir: &MigrationIr,
    transition: &EventTransition,
) -> Vec<StateMutation> {
    let mut mutations = Vec::new();

    // If the transition targets a state variable, emit a mutation.
    if let Some(var) = ir.state_graph.variables.get(&transition.target_state) {
        let field = to_snake_case(&var.name);
        mutations.push(StateMutation {
            expression: translate_action_snippet(&transition.action_snippet, &field),
            field,
            target_id: transition.target_state.clone(),
        });
    }

    mutations
}

fn build_commands_from_transition(
    transition: &EventTransition,
    effects: Option<&CanonicalEffectModel>,
    diagnostics: &mut Vec<TranslationDiagnostic>,
) -> Vec<CommandEmission> {
    let mut commands = Vec::new();

    // Check if any canonical effects are triggered by this event.
    if let Some(effect_model) = effects {
        for (eff_id, effect) in &effect_model.effects {
            if effect.dependencies.contains(&transition.event_id) {
                commands.push(command_from_effect(effect, eff_id));
            }
        }
    }

    // If no commands were generated, emit Cmd::none().
    if commands.is_empty() {
        commands.push(CommandEmission {
            kind: CommandKind::None,
            description: "No side effects for this transition".to_string(),
            source_effect_id: None,
        });
    }

    // Warn if multiple commands are generated (needs Cmd::Batch).
    if commands.len() > 1 {
        diagnostics.push(TranslationDiagnostic {
            level: DiagnosticLevel::Info,
            message: format!(
                "Transition for event {} produces {} commands; wrapping in Cmd::Batch",
                transition.event_id,
                commands.len()
            ),
            related_ids: vec![transition.event_id.clone()],
        });
    }

    commands
}

fn command_from_effect(effect: &CanonicalEffect, eff_id: &IrNodeId) -> CommandEmission {
    match effect.execution_model {
        ExecutionModel::Command => CommandEmission {
            kind: CommandKind::Task,
            description: format!(
                "Cmd::Task for '{}' ({})",
                effect.name,
                effect.original_kind.label()
            ),
            source_effect_id: Some(eff_id.clone()),
        },
        ExecutionModel::FireAndForget => CommandEmission {
            kind: CommandKind::Msg,
            description: format!("Fire-and-forget message for '{}'", effect.name),
            source_effect_id: Some(eff_id.clone()),
        },
        ExecutionModel::Subscription => CommandEmission {
            kind: CommandKind::None,
            description: format!(
                "Subscription '{}' handled via subscriptions() method",
                effect.name
            ),
            source_effect_id: Some(eff_id.clone()),
        },
    }
}

// ── Init Commands ──────────────────────────────────────────────────────

fn build_init_commands(effects: Option<&CanonicalEffectModel>) -> Vec<InitCommand> {
    let mut commands = Vec::new();

    if let Some(effect_model) = effects {
        let mut sorted_effects: Vec<_> = effect_model.effects.iter().collect();
        sorted_effects.sort_by_key(|(id, _)| (*id).clone());

        for (eff_id, effect) in sorted_effects {
            if effect.execution_model == ExecutionModel::Command && is_lifecycle_trigger(effect) {
                commands.push(InitCommand {
                    kind: CommandKind::Task,
                    description: format!("Init: fetch '{}' via Cmd::Task", effect.name),
                    source_effect_id: Some(eff_id.clone()),
                });
            }
        }
    }

    commands
}

fn is_lifecycle_trigger(effect: &CanonicalEffect) -> bool {
    matches!(
        effect.trigger,
        crate::effect_canonical::TriggerCondition::OnMount
    )
}

// ── Subscriptions ──────────────────────────────────────────────────────

fn build_subscriptions(effects: Option<&CanonicalEffectModel>) -> Vec<SubscriptionDecl> {
    let mut subs = Vec::new();

    if let Some(effect_model) = effects {
        let mut sorted_subs: Vec<_> = effect_model.subscriptions.iter().collect();
        sorted_subs.sort();

        for sub_id in sorted_subs {
            if let Some(effect) = effect_model.effects.get(sub_id) {
                let is_timer =
                    matches!(effect.original_kind, crate::migration_ir::EffectKind::Timer);
                subs.push(SubscriptionDecl {
                    name: to_snake_case(&effect.name),
                    description: format!(
                        "Subscription for '{}' ({})",
                        effect.name,
                        if is_timer { "Every<Msg>" } else { "custom" }
                    ),
                    message_variant: format!("{}Response", to_pascal_case(&effect.name)),
                    is_timer,
                    source_effect_id: sub_id.clone(),
                });
            }
        }
    }

    subs
}

// ── One-Writer Rule Check ──────────────────────────────────────────────

fn check_one_writer_rule(ir: &MigrationIr, diagnostics: &mut Vec<TranslationDiagnostic>) {
    for (id, var) in &ir.state_graph.variables {
        if var.writers.len() > 1 {
            diagnostics.push(TranslationDiagnostic {
                level: DiagnosticLevel::Warning,
                message: format!(
                    "State variable '{}' has {} writers: {:?}. \
                     FrankenTUI's one-writer rule may require refactoring.",
                    var.name,
                    var.writers.len(),
                    var.writers
                ),
                related_ids: std::iter::once(id.clone())
                    .chain(var.writers.iter().cloned())
                    .collect(),
            });
        }
    }
}

// ── Utility Functions ──────────────────────────────────────────────────

fn derive_model_name(source_project: &str) -> String {
    let cleaned: String = source_project
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    if cleaned.chars().any(is_ident_char) {
        format!("{}Model", to_pascal_case(&cleaned))
    } else {
        "AppModel".to_string()
    }
}

/// Whether `ch` may appear in a generated identifier.
fn is_ident_char(ch: char) -> bool {
    ch.is_alphabetic() || ch.is_ascii_digit()
}

/// `s` as a snake_case Rust identifier: `setCount` → `set_count`, `type` →
/// `r#type`.
///
/// Names come from the analysed source, so they may hold characters Rust
/// identifiers cannot, such as `$` or the `/`, `.`, `::` and `#` of a
/// qualified effect name, or be keywords. Those used to reach the generated
/// code verbatim, which then did not compile.
fn to_snake_case(s: &str) -> String {
    let mut result = String::with_capacity(s.len() + 4);
    for ch in s.chars() {
        if ch == '_' {
            result.push('_');
        } else if !is_ident_char(ch) {
            if !result.is_empty() && !result.ends_with('_') {
                result.push('_');
            }
        } else {
            if ch.is_uppercase() && !result.is_empty() && !result.ends_with('_') {
                result.push('_');
            }
            result.extend(ch.to_lowercase());
        }
    }
    valid_ident(result)
}

/// `s` as a PascalCase Rust identifier: `on_click` → `OnClick`. See
/// [`to_snake_case`] for why every character is checked.
fn to_pascal_case(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut capitalize_next = true;
    for ch in s.chars() {
        if !is_ident_char(ch) {
            capitalize_next = true;
        } else if capitalize_next {
            result.extend(ch.to_uppercase());
            capitalize_next = false;
        } else {
            result.push(ch);
        }
    }
    valid_ident(result)
}

/// `ident` made usable as a Rust identifier: not empty, not starting with a
/// digit, and a keyword written raw.
fn valid_ident(mut ident: String) -> String {
    if ident.chars().all(|c| c == '_') {
        ident.push_str("unnamed");
    }
    if ident.starts_with(|c: char| c.is_ascii_digit()) {
        ident.insert(0, '_');
    }
    match ident.as_str() {
        // These cannot be raw identifiers.
        "crate" | "self" | "super" | "Self" => ident.push('_'),
        "as" | "async" | "await" | "break" | "const" | "continue" | "dyn" | "else" | "enum"
        | "extern" | "false" | "fn" | "for" | "gen" | "if" | "impl" | "in" | "let" | "loop"
        | "match" | "mod" | "move" | "mut" | "pub" | "ref" | "return" | "static" | "struct"
        | "trait" | "true" | "try" | "type" | "unsafe" | "use" | "where" | "while" | "abstract"
        | "become" | "box" | "do" | "final" | "macro" | "override" | "priv" | "typeof"
        | "unsized" | "virtual" | "yield" => ident.insert_str(0, "r#"),
        _ => {}
    }
    ident
}

fn ir_type_to_rust(type_annotation: Option<&str>) -> String {
    match type_annotation {
        Some("number") | Some("Number") => "i64".to_string(),
        Some("float") | Some("Float") | Some("f64") => "f64".to_string(),
        Some("string") | Some("String") => "String".to_string(),
        Some("boolean") | Some("bool") => "bool".to_string(),
        Some("array") | Some("Array") => "Vec<String>".to_string(),
        Some("object") | Some("Object") | Some("Record") => "BTreeMap<String, String>".to_string(),
        Some(other) => other.to_string(),
        None => "String".to_string(),
    }
}

fn ir_value_to_rust(value: &str) -> String {
    match value {
        "0" | "0.0" => value.to_string(),
        "true" | "false" => value.to_string(),
        "null" | "undefined" | "None" => "Default::default()".to_string(),
        "\"\"" | "''" => "String::new()".to_string(),
        "[]" => "Vec::new()".to_string(),
        "{}" => "BTreeMap::new()".to_string(),
        // `{:?}` writes a Rust literal whatever the text holds.
        _ => match js_string_literal(value) {
            Some(text) => format!("{text:?}.to_string()"),
            None => value.to_string(),
        },
    }
}

/// The text of `value` if it is a single JavaScript string literal such as
/// `'it\'s'`, with its escapes decoded.
///
/// Values are cut from source, so they may be truncated: `useState(')')`
/// yields a lone `'`, which used to panic when sliced. The body used to be
/// copied between Rust quotes, so a single-quoted `'say "hi"'` emitted code
/// that did not compile.
fn js_string_literal(value: &str) -> Option<String> {
    let quote = value
        .chars()
        .next()
        .filter(|c| matches!(c, '\'' | '"' | '`'))?;
    if value.len() < 2 || !value.ends_with(quote) {
        return None;
    }
    let body = &value[1..value.len() - 1];
    if quote == '`' && body.contains("${") {
        return None;
    }
    let mut text = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c == quote {
            // `'a' + 'b'` is an expression, not one literal.
            return None;
        }
        if c != '\\' {
            text.push(c);
            continue;
        }
        // A trailing backslash escaped the closing quote.
        match chars.next()? {
            'n' => text.push('\n'),
            'r' => text.push('\r'),
            't' => text.push('\t'),
            'b' => text.push('\u{8}'),
            'f' => text.push('\u{c}'),
            'v' => text.push('\u{b}'),
            '0' => text.push('\0'),
            'x' => text.push(char::from_u32(js_hex_escape(&mut chars, 2)?)?),
            'u' => {
                let mut code = js_unicode_escape(&mut chars)?;
                if (0xD800..0xDC00).contains(&code) {
                    // A surrogate pair is spelled as two escapes.
                    let mut ahead = chars.clone();
                    if ahead.next() == Some('\\')
                        && ahead.next() == Some('u')
                        && let Some(low) = js_unicode_escape(&mut ahead)
                        && (0xDC00..0xE000).contains(&low)
                    {
                        code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                        chars = ahead;
                    }
                }
                text.push(char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER));
            }
            // A line continuation.
            '\n' => {}
            '\r' => {
                if chars.clone().next() == Some('\n') {
                    chars.next();
                }
            }
            other => text.push(other),
        }
    }
    Some(text)
}

/// The value of the `\uXXXX` or `\u{X...}` escape after its `\u`.
fn js_unicode_escape(chars: &mut std::str::Chars<'_>) -> Option<u32> {
    if chars.clone().next() != Some('{') {
        return js_hex_escape(chars, 4);
    }
    chars.next();
    let digits: String = chars.by_ref().take_while(|&c| c != '}').collect();
    if digits.is_empty() || digits.len() > 6 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(&digits, 16).ok()
}

/// The value of the next `len` hex digits.
fn js_hex_escape(chars: &mut std::str::Chars<'_>, len: usize) -> Option<u32> {
    let digits: String = chars.by_ref().take(len).collect();
    if digits.len() != len || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(&digits, 16).ok()
}

fn default_for_type(rust_type: &str) -> String {
    match rust_type {
        "i64" | "i32" | "u64" | "u32" | "usize" => "0".to_string(),
        "f64" | "f32" => "0.0".to_string(),
        "bool" => "false".to_string(),
        "String" => "String::new()".to_string(),
        t if t.starts_with("Vec<") => "Vec::new()".to_string(),
        t if t.starts_with("BTreeMap<") => "BTreeMap::new()".to_string(),
        t if t.starts_with("Option<") => "None".to_string(),
        _ => "Default::default()".to_string(),
    }
}

/// The value assigned to `model.<field>` for a transition's action snippet.
///
/// A setter call or dispatch has no value to assign, so its placeholder keeps
/// the field as it is. The placeholder used to be the TODO comment alone, which
/// left `model.x = /* TODO */;`. Setters such as `setCount(...)`, the form
/// lowering records, were not recognised and were emitted as calls to an
/// undefined function. Either kept the generated project from compiling.
fn translate_action_snippet(snippet: &str, field: &str) -> String {
    let trimmed = snippet.trim();
    let todo = |what: &str| {
        format!(
            "model.{field}.clone() /* TODO: translate {} */",
            rust_comment_text(what)
        )
    };
    if trimmed.is_empty() {
        todo(&format!("action for {field}"))
    } else if is_setter_call(trimmed) {
        todo(&format!("setState call: {trimmed}"))
    } else if trimmed.contains("dispatch") {
        todo(&format!("dispatch: {trimmed}"))
    } else {
        trimmed.to_string()
    }
}

/// Whether `snippet` calls a state setter: `setState`, `setCount` or `set_count`,
/// but not `offsetX` or `reset_all`.
fn is_setter_call(snippet: &str) -> bool {
    snippet.match_indices("set").any(|(at, _)| {
        let starts_word = !snippet[..at].ends_with(|c: char| c.is_alphanumeric() || c == '_');
        let next = snippet[at + 3..].chars().next();
        starts_word && next.is_some_and(|c| c.is_ascii_uppercase() || c == '_')
    })
}

fn count_diagnostics(diagnostics: &[TranslationDiagnostic]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for d in diagnostics {
        *counts.entry(format!("{:?}", d.level)).or_insert(0) += 1;
    }
    counts
}

/// Label for EffectKind (needed by command_from_effect).
trait EffectKindLabel {
    fn label(&self) -> &'static str;
}

impl EffectKindLabel for crate::migration_ir::EffectKind {
    fn label(&self) -> &'static str {
        match self {
            crate::migration_ir::EffectKind::Dom => "DOM",
            crate::migration_ir::EffectKind::Network => "network",
            crate::migration_ir::EffectKind::Timer => "timer",
            crate::migration_ir::EffectKind::Storage => "storage",
            crate::migration_ir::EffectKind::Subscription => "subscription",
            crate::migration_ir::EffectKind::Process => "process",
            crate::migration_ir::EffectKind::Telemetry => "telemetry",
            crate::migration_ir::EffectKind::Other => "other",
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migration_ir::{
        EventDecl, EventTransition, IrBuilder, StateVariable, ViewNode, ViewNodeKind,
    };

    fn test_provenance() -> Provenance {
        Provenance {
            file: "test.tsx".to_string(),
            line: 1,
            column: None,
            source_name: None,
            policy_category: None,
        }
    }

    fn minimal_ir() -> MigrationIr {
        let mut builder = IrBuilder::new("test-translate".to_string(), "my-app".to_string());
        let node_id = IrNodeId("ir-node-app".to_string());
        builder.add_root(node_id.clone());
        builder.add_view_node(ViewNode {
            id: node_id,
            kind: ViewNodeKind::Component,
            name: "App".to_string(),
            children: Vec::new(),
            props: Vec::new(),
            slots: Vec::new(),
            conditions: Vec::new(),
            provenance: test_provenance(),
        });
        builder.add_state_variable(StateVariable {
            id: IrNodeId("ir-state-count".to_string()),
            name: "count".to_string(),
            scope: StateScope::Local,
            type_annotation: Some("number".to_string()),
            initial_value: Some("0".to_string()),
            readers: BTreeSet::new(),
            writers: BTreeSet::new(),
            provenance: test_provenance(),
        });
        builder.add_event(EventDecl {
            id: IrNodeId("ir-evt-inc".to_string()),
            name: "increment".to_string(),
            kind: EventKind::UserInput,
            source_node: None,
            payload_type: None,
            provenance: test_provenance(),
        });
        builder.add_transition(EventTransition {
            event_id: IrNodeId("ir-evt-inc".to_string()),
            target_state: IrNodeId("ir-state-count".to_string()),
            action_snippet: "count + 1".to_string(),
            guards: Vec::new(),
        });
        builder.build()
    }

    fn ir_with_multi_writer() -> MigrationIr {
        let mut builder = IrBuilder::new("test-multi-writer".to_string(), "multi-app".to_string());
        let node_id = IrNodeId("ir-node-app".to_string());
        builder.add_root(node_id.clone());
        builder.add_view_node(ViewNode {
            id: node_id,
            kind: ViewNodeKind::Component,
            name: "App".to_string(),
            children: Vec::new(),
            props: Vec::new(),
            slots: Vec::new(),
            conditions: Vec::new(),
            provenance: test_provenance(),
        });
        let mut writers = BTreeSet::new();
        writers.insert(IrNodeId("ir-evt-a".to_string()));
        writers.insert(IrNodeId("ir-evt-b".to_string()));
        builder.add_state_variable(StateVariable {
            id: IrNodeId("ir-state-shared".to_string()),
            name: "shared_value".to_string(),
            scope: StateScope::Local,
            type_annotation: Some("string".to_string()),
            initial_value: None,
            readers: BTreeSet::new(),
            writers,
            provenance: test_provenance(),
        });
        builder.build()
    }

    fn ir_with_context_state() -> MigrationIr {
        let mut builder = IrBuilder::new("test-context".to_string(), "context-app".to_string());
        let node_id = IrNodeId("ir-node-app".to_string());
        builder.add_root(node_id.clone());
        builder.add_view_node(ViewNode {
            id: node_id,
            kind: ViewNodeKind::Component,
            name: "App".to_string(),
            children: Vec::new(),
            props: Vec::new(),
            slots: Vec::new(),
            conditions: Vec::new(),
            provenance: test_provenance(),
        });
        builder.add_state_variable(StateVariable {
            id: IrNodeId("ir-state-theme".to_string()),
            name: "theme".to_string(),
            scope: StateScope::Context,
            type_annotation: Some("string".to_string()),
            initial_value: Some("\"dark\"".to_string()),
            readers: BTreeSet::new(),
            writers: BTreeSet::new(),
            provenance: test_provenance(),
        });
        builder.add_state_variable(StateVariable {
            id: IrNodeId("ir-state-local".to_string()),
            name: "counter".to_string(),
            scope: StateScope::Local,
            type_annotation: Some("number".to_string()),
            initial_value: Some("0".to_string()),
            readers: BTreeSet::new(),
            writers: BTreeSet::new(),
            provenance: test_provenance(),
        });
        builder.build()
    }

    #[test]
    fn translate_produces_valid_output() {
        let ir = minimal_ir();
        let result = translate_state_events(&ir, None);
        assert_eq!(result.version, TRANSLATOR_VERSION);
        assert_eq!(result.run_id, ir.run_id);
    }

    #[test]
    fn model_name_derived_from_project() {
        let ir = minimal_ir();
        let result = translate_state_events(&ir, None);
        assert_eq!(result.model.name, "MyAppModel");
    }

    #[test]
    fn model_has_correct_fields() {
        let ir = minimal_ir();
        let result = translate_state_events(&ir, None);
        assert_eq!(result.model.fields.len(), 1);
        assert_eq!(result.model.fields[0].name, "count");
        assert_eq!(result.model.fields[0].rust_type, "i64");
        assert_eq!(result.model.fields[0].initial_value, "0");
        assert!(!result.model.fields[0].derived);
    }

    #[test]
    fn message_enum_has_event_variants() {
        let ir = minimal_ir();
        let result = translate_state_events(&ir, None);
        // Should have: Increment + TerminalEvent
        assert!(result.message_enum.variants.len() >= 2);
        let names: Vec<_> = result
            .message_enum
            .variants
            .iter()
            .map(|v| &v.name)
            .collect();
        assert!(names.contains(&&"Increment".to_string()));
        assert!(names.contains(&&"TerminalEvent".to_string()));
    }

    #[test]
    fn update_arms_from_transitions() {
        let ir = minimal_ir();
        let result = translate_state_events(&ir, None);
        // Should have arm for Increment + TerminalEvent
        assert!(result.update_arms.len() >= 2);
        let inc_arm = result
            .update_arms
            .iter()
            .find(|a| a.message_variant == "Increment");
        assert!(inc_arm.is_some());
        let arm = inc_arm.unwrap();
        assert_eq!(arm.mutations.len(), 1);
        assert_eq!(arm.mutations[0].field, "count");
    }

    #[test]
    fn terminal_event_passthrough_arm_exists() {
        let ir = minimal_ir();
        let result = translate_state_events(&ir, None);
        let te_arm = result
            .update_arms
            .iter()
            .find(|a| a.message_variant == "TerminalEvent");
        assert!(te_arm.is_some());
    }

    #[test]
    fn multi_writer_emits_warning() {
        let ir = ir_with_multi_writer();
        let result = translate_state_events(&ir, None);
        let warnings: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|d| d.level == DiagnosticLevel::Warning)
            .collect();
        assert!(
            !warnings.is_empty(),
            "Multi-writer should produce a warning"
        );
        assert!(warnings[0].message.contains("shared_value"));
    }

    #[test]
    fn context_state_becomes_shared_ref() {
        let ir = ir_with_context_state();
        let result = translate_state_events(&ir, None);
        // Context-scoped → shared_fields
        assert_eq!(result.model.shared_fields.len(), 1);
        assert_eq!(result.model.shared_fields[0].name, "theme");
        assert_eq!(result.model.shared_fields[0].scope, FieldScope::Shared);
        // Local-scoped → fields
        assert!(result.model.fields.iter().any(|f| f.name == "counter"));
    }

    #[test]
    fn empty_ir_produces_minimal_output() {
        let ir = IrBuilder::new("test-empty".to_string(), "empty".to_string()).build();
        let result = translate_state_events(&ir, None);
        assert_eq!(result.model.fields.len(), 0);
        // Should still have TerminalEvent variant.
        assert_eq!(result.message_enum.variants.len(), 1);
        assert_eq!(result.message_enum.variants[0].name, "TerminalEvent");
    }

    #[test]
    fn stats_are_consistent() {
        let ir = minimal_ir();
        let result = translate_state_events(&ir, None);
        assert_eq!(result.stats.model_fields, result.model.fields.len());
        assert_eq!(
            result.stats.message_variants,
            result.message_enum.variants.len()
        );
        assert_eq!(result.stats.update_arms, result.update_arms.len());
        assert_eq!(result.stats.subscriptions, result.subscriptions.len());
        assert_eq!(result.stats.init_commands, result.init_commands.len());
    }

    #[test]
    fn deterministic_output() {
        let ir = minimal_ir();
        let r1 = translate_state_events(&ir, None);
        let r2 = translate_state_events(&ir, None);
        assert_eq!(r1.model.fields.len(), r2.model.fields.len());
        for (f1, f2) in r1.model.fields.iter().zip(&r2.model.fields) {
            assert_eq!(f1.name, f2.name);
            assert_eq!(f1.rust_type, f2.rust_type);
        }
        for (v1, v2) in r1
            .message_enum
            .variants
            .iter()
            .zip(&r2.message_enum.variants)
        {
            assert_eq!(v1.name, v2.name);
        }
    }

    #[test]
    fn snake_case_conversion() {
        assert_eq!(to_snake_case("onClick"), "on_click");
        assert_eq!(to_snake_case("myVariable"), "my_variable");
        assert_eq!(to_snake_case("HTTPRequest"), "h_t_t_p_request");
        assert_eq!(to_snake_case("simple"), "simple");
        assert_eq!(to_snake_case("kebab-case"), "kebab_case");
    }

    #[test]
    fn pascal_case_conversion() {
        assert_eq!(to_pascal_case("on_click"), "OnClick");
        assert_eq!(to_pascal_case("my-variable"), "MyVariable");
        assert_eq!(to_pascal_case("increment"), "Increment");
        assert_eq!(to_pascal_case("already_pascal"), "AlreadyPascal");
    }

    #[test]
    fn source_names_become_rust_identifiers() {
        // Lowering names effects by file, component and index.
        assert_eq!(
            to_pascal_case("src/App.tsx::App::effect#0"),
            "SrcAppTsxAppEffect0"
        );
        assert_eq!(
            to_snake_case("src/App.tsx::App::effect#0"),
            "src_app_tsx_app_effect_0"
        );
        assert_eq!(to_snake_case("$value"), "value");
        assert_eq!(to_snake_case("café"), "café");
        assert_eq!(to_snake_case("ÉtatX"), "état_x");
        assert_eq!(to_snake_case("type"), "r#type");
        assert_eq!(to_snake_case("match"), "r#match");
        assert_eq!(to_snake_case("self"), "self_");
        assert_eq!(to_snake_case("2fa"), "_2fa");
        assert_eq!(to_snake_case("$"), "unnamed");
        assert_eq!(to_pascal_case("self"), "Self_");
        assert_eq!(to_pascal_case("404-page"), "_404Page");
        assert_eq!(to_pascal_case("on:click"), "OnClick");
        assert_eq!(derive_model_name("!!!"), "AppModel");
        assert_eq!(derive_model_name("my-app"), "MyAppModel");
    }

    #[test]
    fn ir_type_to_rust_mappings() {
        assert_eq!(ir_type_to_rust(Some("number")), "i64");
        assert_eq!(ir_type_to_rust(Some("string")), "String");
        assert_eq!(ir_type_to_rust(Some("boolean")), "bool");
        assert_eq!(ir_type_to_rust(Some("array")), "Vec<String>");
        assert_eq!(ir_type_to_rust(None), "String");
    }

    #[test]
    fn ir_value_to_rust_mappings() {
        assert_eq!(ir_value_to_rust("0"), "0");
        assert_eq!(ir_value_to_rust("true"), "true");
        assert_eq!(ir_value_to_rust("null"), "Default::default()");
        assert_eq!(ir_value_to_rust("\"\""), "String::new()");
        assert_eq!(ir_value_to_rust("[]"), "Vec::new()");
        assert_eq!(ir_value_to_rust("\"hello\""), "\"hello\".to_string()");
    }

    #[test]
    fn ir_value_to_rust_emits_valid_string_literals() {
        // `useState(')')` is cut at the first `)`, leaving a lone quote.
        assert_eq!(ir_value_to_rust("'"), "'");
        assert_eq!(ir_value_to_rust("\""), "\"");
        assert_eq!(
            ir_value_to_rust(r#"'say "hi"'"#),
            r#""say \"hi\"".to_string()"#
        );
        assert_eq!(ir_value_to_rust(r"'it\'s'"), r#""it's".to_string()"#);
        assert_eq!(ir_value_to_rust(r#""a\nb\\c""#), r#""a\nb\\c".to_string()"#);
        assert_eq!(ir_value_to_rust("`plain`"), r#""plain".to_string()"#);
        assert_eq!(ir_value_to_rust(r"'\x41B\u{43}'"), r#""ABC".to_string()"#);
        assert_eq!(ir_value_to_rust(r"'😀'"), "\"\u{1F600}\".to_string()");
        assert_eq!(ir_value_to_rust(r"'\uD83D'"), "\"\u{FFFD}\".to_string()");
        // Not one literal: left as the expression it is.
        assert_eq!(ir_value_to_rust("'a' + 'b'"), "'a' + 'b'");
        assert_eq!(ir_value_to_rust(r"'a\'"), r"'a\'");
        assert_eq!(ir_value_to_rust("`${a}`"), "`${a}`");
        assert_eq!(ir_value_to_rust(r"'\xZZ'"), r"'\xZZ'");
    }

    #[test]
    fn setter_actions_become_placeholders_that_compile() {
        assert_eq!(
            translate_action_snippet("setSelected(setSelected)", "selected"),
            "model.selected.clone() /* TODO: translate setState call: setSelected(setSelected) */"
        );
        assert_eq!(
            translate_action_snippet("this.setState({ a: 1 })", "a"),
            "model.a.clone() /* TODO: translate setState call: this.setState({ a: 1 }) */"
        );
        assert_eq!(
            translate_action_snippet("", "count"),
            "model.count.clone() /* TODO: translate action for count */"
        );
        assert_eq!(
            translate_action_snippet("dispatch({ type: 'inc' })", "count"),
            "model.count.clone() /* TODO: translate dispatch: dispatch({ type: 'inc' }) */"
        );
        assert_eq!(
            translate_action_snippet("set_x(1) /* x */ \n y", "x"),
            "model.x.clone() /* TODO: translate setState call: set_x(1) / * x * /   y */"
        );
        // Not setters.
        assert_eq!(translate_action_snippet("offsetX + 1", "x"), "offsetX + 1");
        assert_eq!(translate_action_snippet("reset_all()", "x"), "reset_all()");
        assert_eq!(
            translate_action_snippet(" count + 1 ", "count"),
            "count + 1"
        );
    }

    #[test]
    fn default_for_type_coverage() {
        assert_eq!(default_for_type("i64"), "0");
        assert_eq!(default_for_type("bool"), "false");
        assert_eq!(default_for_type("String"), "String::new()");
        assert_eq!(default_for_type("Vec<i32>"), "Vec::new()");
        assert_eq!(default_for_type("Option<String>"), "None");
        assert_eq!(default_for_type("CustomType"), "Default::default()");
    }

    #[test]
    fn transition_with_guards() {
        let mut builder = IrBuilder::new("test-guards".to_string(), "guard-app".to_string());
        let node_id = IrNodeId("ir-node-app".to_string());
        builder.add_root(node_id.clone());
        builder.add_view_node(ViewNode {
            id: node_id,
            kind: ViewNodeKind::Component,
            name: "App".to_string(),
            children: Vec::new(),
            props: Vec::new(),
            slots: Vec::new(),
            conditions: Vec::new(),
            provenance: test_provenance(),
        });
        builder.add_state_variable(StateVariable {
            id: IrNodeId("ir-state-val".to_string()),
            name: "value".to_string(),
            scope: StateScope::Local,
            type_annotation: Some("number".to_string()),
            initial_value: Some("0".to_string()),
            readers: BTreeSet::new(),
            writers: BTreeSet::new(),
            provenance: test_provenance(),
        });
        builder.add_event(EventDecl {
            id: IrNodeId("ir-evt-guarded".to_string()),
            name: "guardedAction".to_string(),
            kind: EventKind::Custom,
            source_node: None,
            payload_type: None,
            provenance: test_provenance(),
        });
        builder.add_transition(EventTransition {
            event_id: IrNodeId("ir-evt-guarded".to_string()),
            target_state: IrNodeId("ir-state-val".to_string()),
            action_snippet: "value + 1".to_string(),
            guards: vec!["value < 100".to_string()],
        });
        let ir = builder.build();
        let result = translate_state_events(&ir, None);

        let arm = result
            .update_arms
            .iter()
            .find(|a| a.message_variant == "GuardedAction");
        assert!(arm.is_some());
        let arm = arm.unwrap();
        assert_eq!(arm.guards.len(), 1);
        assert_eq!(arm.guards[0], "value < 100");
    }

    #[test]
    fn server_state_emits_info_diagnostic() {
        let mut builder = IrBuilder::new("test-server".to_string(), "server-app".to_string());
        builder.add_state_variable(StateVariable {
            id: IrNodeId("ir-state-data".to_string()),
            name: "serverData".to_string(),
            scope: StateScope::Server,
            type_annotation: Some("string".to_string()),
            initial_value: None,
            readers: BTreeSet::new(),
            writers: BTreeSet::new(),
            provenance: test_provenance(),
        });
        let ir = builder.build();
        let result = translate_state_events(&ir, None);

        let infos: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|d| d.level == DiagnosticLevel::Info)
            .collect();
        assert!(!infos.is_empty());
        assert!(infos[0].message.contains("Cmd::Task"));
    }

    #[test]
    fn field_scope_from_state_scope() {
        assert_eq!(FieldScope::from(StateScope::Local), FieldScope::Local);
        assert_eq!(FieldScope::from(StateScope::Context), FieldScope::Shared);
        assert_eq!(FieldScope::from(StateScope::Global), FieldScope::Global);
        assert_eq!(FieldScope::from(StateScope::Route), FieldScope::Route);
        assert_eq!(FieldScope::from(StateScope::Server), FieldScope::Server);
    }
}
