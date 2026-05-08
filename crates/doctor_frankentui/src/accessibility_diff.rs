//! Accessibility parity comparison for migration certification.
//!
//! The comparator works over extracted accessibility snapshots rather than
//! concrete widget types. It verifies focus traversal, action reachability,
//! contrast and assistive output parity, while recording evidence-backed
//! improvements that are allowed by the migration improvement envelope.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::semantic_contract::{
    ExpectedLossResult, TransformationRiskLevel, load_builtin_confidence_model,
    load_builtin_semantic_contract,
};

pub const ACCESSIBILITY_DIFF_VALIDATOR_ID: &str = "accessibility_diff_validator";

const A11Y_FOCUS_POLICY_ID: &str = "AD-001";
const A11Y_ACTION_POLICY_ID: &str = "AD-002";
const A11Y_ASSISTIVE_POLICY_ID: &str = "AD-003";
const A11Y_CONTRAST_POLICY_ID: &str = "AD-004";
const A11Y_IMPROVEMENT_POLICY_ID: &str = "AD-005";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AccessibilityDiffVerdict {
    Equivalent,
    Improved,
    Violation,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AccessibilityRole {
    Window,
    Dialog,
    Button,
    TextInput,
    Label,
    List,
    ListItem,
    Table,
    TableRow,
    TableCell,
    Checkbox,
    RadioButton,
    ProgressBar,
    Slider,
    Tab,
    TabPanel,
    Menu,
    MenuItem,
    Toolbar,
    ScrollBar,
    Separator,
    Group,
    Presentation,
}

impl AccessibilityRole {
    #[must_use]
    pub const fn is_interactive(self) -> bool {
        matches!(
            self,
            Self::Button
                | Self::TextInput
                | Self::Checkbox
                | Self::RadioButton
                | Self::Slider
                | Self::Tab
                | Self::MenuItem
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AccessibilityActionKind {
    Focus,
    Activate,
    Toggle,
    SetValue,
    Expand,
    Collapse,
    Select,
    Custom(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccessibilityAction {
    pub action_id: String,
    pub kind: AccessibilityActionKind,
    pub label: String,
    pub enabled: bool,
    pub result_node_id: Option<String>,
}

impl AccessibilityAction {
    #[must_use]
    pub fn new(
        action_id: impl Into<String>,
        kind: AccessibilityActionKind,
        label: impl Into<String>,
    ) -> Self {
        Self {
            action_id: action_id.into(),
            kind,
            label: label.into(),
            enabled: true,
            result_node_id: None,
        }
    }

    #[must_use]
    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    #[must_use]
    pub fn with_result_node(mut self, result_node_id: impl Into<String>) -> Self {
        self.result_node_id = Some(result_node_id.into());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccessibilityNode {
    pub node_id: String,
    pub role: AccessibilityRole,
    pub name: Option<String>,
    pub description: Option<String>,
    pub focusable: bool,
    pub enabled: bool,
    pub focus_order: Option<u32>,
    pub actions: Vec<AccessibilityAction>,
    pub contrast_ratio: Option<f32>,
    pub shortcut: Option<String>,
    pub source_ref: Option<String>,
}

impl AccessibilityNode {
    #[must_use]
    pub fn new(node_id: impl Into<String>, role: AccessibilityRole) -> Self {
        let focusable = role.is_interactive();
        Self {
            node_id: node_id.into(),
            role,
            name: None,
            description: None,
            focusable,
            enabled: true,
            focus_order: None,
            actions: Vec::new(),
            contrast_ratio: None,
            shortcut: None,
            source_ref: None,
        }
    }

    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    #[must_use]
    pub const fn with_focus_order(mut self, focus_order: u32) -> Self {
        self.focus_order = Some(focus_order);
        self.focusable = true;
        self
    }

    #[must_use]
    pub fn with_action(mut self, action: AccessibilityAction) -> Self {
        self.actions.push(action);
        self.actions = canonicalize_actions(self.actions);
        self
    }

    #[must_use]
    pub const fn with_contrast_ratio(mut self, contrast_ratio: f32) -> Self {
        self.contrast_ratio = Some(contrast_ratio);
        self
    }

    #[must_use]
    pub fn with_shortcut(mut self, shortcut: impl Into<String>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    #[must_use]
    pub fn with_source_ref(mut self, source_ref: impl Into<String>) -> Self {
        self.source_ref = Some(source_ref.into());
        self
    }

    #[must_use]
    pub const fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FocusTransition {
    pub from_node_id: String,
    pub to_node_id: String,
    pub trigger: String,
}

impl FocusTransition {
    #[must_use]
    pub fn new(
        from_node_id: impl Into<String>,
        to_node_id: impl Into<String>,
        trigger: impl Into<String>,
    ) -> Self {
        Self {
            from_node_id: from_node_id.into(),
            to_node_id: to_node_id.into(),
            trigger: trigger.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssistiveAnnouncement {
    pub announcement_id: String,
    pub node_id: Option<String>,
    pub text: String,
    pub politeness: String,
}

impl AssistiveAnnouncement {
    #[must_use]
    pub fn new(
        announcement_id: impl Into<String>,
        node_id: Option<String>,
        text: impl Into<String>,
        politeness: impl Into<String>,
    ) -> Self {
        Self {
            announcement_id: announcement_id.into(),
            node_id,
            text: text.into(),
            politeness: politeness.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccessibilityRun {
    pub run_id: String,
    pub replay_command: Option<String>,
    pub nodes: Vec<AccessibilityNode>,
    pub focus_transitions: Vec<FocusTransition>,
    pub announcements: Vec<AssistiveAnnouncement>,
}

impl AccessibilityRun {
    #[must_use]
    pub fn new(run_id: impl Into<String>, nodes: Vec<AccessibilityNode>) -> Self {
        Self {
            run_id: run_id.into(),
            replay_command: None,
            nodes: canonicalize_nodes(nodes),
            focus_transitions: Vec::new(),
            announcements: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_focus_transitions(mut self, transitions: Vec<FocusTransition>) -> Self {
        self.focus_transitions = canonicalize_transitions(transitions);
        self
    }

    #[must_use]
    pub fn with_announcements(mut self, announcements: Vec<AssistiveAnnouncement>) -> Self {
        self.announcements = canonicalize_announcements(announcements);
        self
    }

    #[must_use]
    pub fn with_replay_command(mut self, replay_command: impl Into<String>) -> Self {
        self.replay_command = Some(replay_command.into());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccessibilityDiffConfig {
    pub minimum_contrast_ratio: f32,
    pub contrast_improvement_delta: f32,
    pub require_focus_reachability: bool,
}

impl Default for AccessibilityDiffConfig {
    fn default() -> Self {
        Self {
            minimum_contrast_ratio: 4.5,
            contrast_improvement_delta: 0.5,
            require_focus_reachability: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AccessibilityViolationKind {
    MissingNode,
    RoleChanged,
    FocusabilityDropped,
    DisabledReachableNode,
    MissingAction,
    DisabledAction,
    UnreachableFocusNode,
    MissingFocusTransition,
    ContrastBelowPolicy,
    MissingAnnouncement,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccessibilityViolation {
    pub violation_kind: AccessibilityViolationKind,
    pub node_id: Option<String>,
    pub policy_id: String,
    pub risk_level: TransformationRiskLevel,
    pub source_value: Option<String>,
    pub translated_value: Option<String>,
    pub remediation_hint: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AccessibilityImprovementKind {
    AddedAccessibleName,
    AddedDescription,
    AddedAction,
    AddedShortcut,
    ImprovedContrast,
    AddedAnnouncement,
    AddedReachableFocusTarget,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccessibilityImprovement {
    pub improvement_kind: AccessibilityImprovementKind,
    pub node_id: Option<String>,
    pub policy_id: String,
    pub baseline_ref: String,
    pub rationale: String,
    pub source_value: Option<String>,
    pub translated_value: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccessibilityDiffReport {
    pub validator_id: String,
    pub contract_id: String,
    pub source_run_id: String,
    pub translated_run_id: String,
    pub verdict: AccessibilityDiffVerdict,
    pub nodes_compared: usize,
    pub focus_edges_compared: usize,
    pub actions_compared: usize,
    pub violations: Vec<AccessibilityViolation>,
    pub improvements: Vec<AccessibilityImprovement>,
    pub covered_policy_ids: Vec<String>,
    pub violated_policy_ids: Vec<String>,
    pub risk_level: TransformationRiskLevel,
    pub risk_score: f64,
    pub expected_loss: ExpectedLossResult,
}

#[must_use]
pub fn compare_accessibility_runs(
    source_run: &AccessibilityRun,
    translated_run: &AccessibilityRun,
    config: &AccessibilityDiffConfig,
) -> AccessibilityDiffReport {
    let contract = load_builtin_semantic_contract().expect("built-in semantic contract parses");
    let source_nodes = node_map(&source_run.nodes);
    let translated_nodes = node_map(&translated_run.nodes);
    let source_actions = action_map(&source_run.nodes);
    let translated_actions = action_map(&translated_run.nodes);
    let source_announcements = announcement_map(&source_run.announcements);
    let translated_announcements = announcement_map(&translated_run.announcements);

    let mut violations = Vec::new();
    let mut improvements = Vec::new();
    let mut covered_policy_ids = BTreeSet::new();
    let mut violated_policy_ids = BTreeSet::new();
    let mut successes = 0_u32;
    let mut weighted_failures = 0_u32;
    let mut actions_compared = 0_usize;

    for (node_id, source_node) in &source_nodes {
        match translated_nodes.get(node_id) {
            Some(translated_node) => {
                let node_result = compare_node(
                    source_node,
                    translated_node,
                    &source_actions,
                    &translated_actions,
                    config,
                );
                actions_compared = actions_compared.saturating_add(node_result.actions_compared);
                successes = successes.saturating_add(node_result.successes);
                covered_policy_ids.extend(node_result.covered_policy_ids);
                for violation in node_result.violations {
                    weighted_failures =
                        weighted_failures.saturating_add(failure_weight(violation.risk_level));
                    violated_policy_ids.insert(violation.policy_id.clone());
                    violations.push(violation);
                }
                improvements.extend(node_result.improvements);
            }
            None => {
                let violation = missing_node(source_node);
                weighted_failures =
                    weighted_failures.saturating_add(failure_weight(violation.risk_level));
                violated_policy_ids.insert(violation.policy_id.clone());
                violations.push(violation);
            }
        }
    }

    for (node_id, translated_node) in &translated_nodes {
        if !source_nodes.contains_key(node_id)
            && translated_node.focusable
            && translated_node.enabled
        {
            improvements.push(added_focus_target(translated_node));
        }
    }

    let focus_result = compare_focus_graph(source_run, translated_run, &source_nodes, config);
    successes = successes.saturating_add(focus_result.successes);
    covered_policy_ids.extend(focus_result.covered_policy_ids);
    for violation in focus_result.violations {
        weighted_failures = weighted_failures.saturating_add(failure_weight(violation.risk_level));
        violated_policy_ids.insert(violation.policy_id.clone());
        violations.push(violation);
    }

    let mut announcement_state = AnnouncementCompareState {
        improvements: &mut improvements,
        violations: &mut violations,
        successes: &mut successes,
        weighted_failures: &mut weighted_failures,
        covered_policy_ids: &mut covered_policy_ids,
        violated_policy_ids: &mut violated_policy_ids,
    };
    compare_announcements(
        &source_announcements,
        &translated_announcements,
        &mut announcement_state,
    );

    sort_violations_by_severity(&mut violations);
    improvements.sort_by(|a, b| {
        a.node_id.cmp(&b.node_id).then_with(|| {
            format!("{:?}", a.improvement_kind).cmp(&format!("{:?}", b.improvement_kind))
        })
    });

    let verdict = if violations.is_empty() {
        if improvements.is_empty() {
            AccessibilityDiffVerdict::Equivalent
        } else {
            AccessibilityDiffVerdict::Improved
        }
    } else {
        AccessibilityDiffVerdict::Violation
    };
    let risk_level = violations
        .iter()
        .map(|violation| violation.risk_level)
        .max()
        .unwrap_or(TransformationRiskLevel::Low);
    let risk_score = risk_score(successes, weighted_failures);
    let first_violated_policy = violated_policy_ids.iter().next().cloned();
    let expected_loss = expected_loss(successes, weighted_failures, first_violated_policy);

    AccessibilityDiffReport {
        validator_id: ACCESSIBILITY_DIFF_VALIDATOR_ID.to_string(),
        contract_id: contract.contract_id,
        source_run_id: source_run.run_id.clone(),
        translated_run_id: translated_run.run_id.clone(),
        verdict,
        nodes_compared: source_nodes.len().max(translated_nodes.len()),
        focus_edges_compared: source_run
            .focus_transitions
            .len()
            .max(translated_run.focus_transitions.len()),
        actions_compared,
        violations,
        improvements,
        covered_policy_ids: covered_policy_ids.into_iter().collect(),
        violated_policy_ids: violated_policy_ids.into_iter().collect(),
        risk_level,
        risk_score,
        expected_loss,
    }
}

#[derive(Default)]
struct NodeCompareResult {
    violations: Vec<AccessibilityViolation>,
    improvements: Vec<AccessibilityImprovement>,
    covered_policy_ids: BTreeSet<String>,
    actions_compared: usize,
    successes: u32,
}

#[derive(Default)]
struct FocusCompareResult {
    violations: Vec<AccessibilityViolation>,
    covered_policy_ids: BTreeSet<String>,
    successes: u32,
}

fn compare_node(
    source_node: &AccessibilityNode,
    translated_node: &AccessibilityNode,
    source_actions: &BTreeMap<(String, String), AccessibilityAction>,
    translated_actions: &BTreeMap<(String, String), AccessibilityAction>,
    config: &AccessibilityDiffConfig,
) -> NodeCompareResult {
    let mut result = NodeCompareResult::default();

    if source_node.role != translated_node.role {
        result
            .violations
            .push(role_changed(source_node, translated_node));
    } else {
        result
            .covered_policy_ids
            .insert(A11Y_ASSISTIVE_POLICY_ID.to_string());
        result.successes = result.successes.saturating_add(1);
    }

    if source_node.focusable && !translated_node.focusable {
        result
            .violations
            .push(focusability_dropped(source_node, translated_node));
    } else if source_node.focusable {
        result
            .covered_policy_ids
            .insert(A11Y_FOCUS_POLICY_ID.to_string());
        result.successes = result.successes.saturating_add(1);
    }

    if source_node.enabled && !translated_node.enabled && source_node.focusable {
        result
            .violations
            .push(disabled_reachable_node(source_node, translated_node));
    }

    compare_names(source_node, translated_node, &mut result);
    compare_node_improvements(source_node, translated_node, config, &mut result);
    compare_actions(source_node, source_actions, translated_actions, &mut result);
    compare_contrast(source_node, translated_node, config, &mut result);

    result
}

fn compare_names(
    source_node: &AccessibilityNode,
    translated_node: &AccessibilityNode,
    result: &mut NodeCompareResult,
) {
    if source_node.name.is_some() && translated_node.name.is_none() {
        result
            .violations
            .push(missing_accessible_name(source_node, translated_node));
    } else {
        result
            .covered_policy_ids
            .insert(A11Y_ASSISTIVE_POLICY_ID.to_string());
        result.successes = result.successes.saturating_add(1);
    }

    if source_node.name.is_none() && translated_node.name.is_some() {
        result.improvements.push(improvement(
            AccessibilityImprovementKind::AddedAccessibleName,
            Some(translated_node.node_id.clone()),
            baseline_ref(source_node),
            "translated node adds a screen-reader accessible name absent from source",
            None,
            translated_node.name.clone(),
        ));
    }
}

fn compare_node_improvements(
    source_node: &AccessibilityNode,
    translated_node: &AccessibilityNode,
    config: &AccessibilityDiffConfig,
    result: &mut NodeCompareResult,
) {
    if source_node.description.is_none() && translated_node.description.is_some() {
        result.improvements.push(improvement(
            AccessibilityImprovementKind::AddedDescription,
            Some(translated_node.node_id.clone()),
            baseline_ref(source_node),
            "translated node adds assistive description without changing role or action contract",
            None,
            translated_node.description.clone(),
        ));
    }

    if source_node.shortcut.is_none() && translated_node.shortcut.is_some() {
        result.improvements.push(improvement(
            AccessibilityImprovementKind::AddedShortcut,
            Some(translated_node.node_id.clone()),
            baseline_ref(source_node),
            "translated node exposes a keyboard shortcut hint absent from source",
            None,
            translated_node.shortcut.clone(),
        ));
    }

    if let (Some(source_ratio), Some(translated_ratio)) =
        (source_node.contrast_ratio, translated_node.contrast_ratio)
        && translated_ratio - source_ratio >= config.contrast_improvement_delta
        && translated_ratio >= config.minimum_contrast_ratio
    {
        result.improvements.push(improvement(
            AccessibilityImprovementKind::ImprovedContrast,
            Some(translated_node.node_id.clone()),
            baseline_ref(source_node),
            "translated node improves contrast while remaining above policy threshold",
            Some(format!("{source_ratio:.2}")),
            Some(format!("{translated_ratio:.2}")),
        ));
    }
}

fn compare_actions(
    source_node: &AccessibilityNode,
    source_actions: &BTreeMap<(String, String), AccessibilityAction>,
    translated_actions: &BTreeMap<(String, String), AccessibilityAction>,
    result: &mut NodeCompareResult,
) {
    let source_node_actions = source_node
        .actions
        .iter()
        .map(|action| action.action_id.clone())
        .collect::<BTreeSet<_>>();
    let translated_node_actions = translated_actions
        .keys()
        .filter(|(node_id, _action_id)| node_id == &source_node.node_id)
        .map(|(_node_id, action_id)| action_id.clone())
        .collect::<BTreeSet<_>>();

    for action_id in &source_node_actions {
        result.actions_compared = result.actions_compared.saturating_add(1);
        let key = (source_node.node_id.clone(), action_id.clone());
        match (source_actions.get(&key), translated_actions.get(&key)) {
            (Some(source_action), Some(translated_action)) if translated_action.enabled => {
                if source_action.kind == translated_action.kind {
                    result
                        .covered_policy_ids
                        .insert(A11Y_ACTION_POLICY_ID.to_string());
                    result.successes = result.successes.saturating_add(1);
                } else {
                    result.violations.push(action_kind_changed(
                        source_node,
                        source_action,
                        translated_action,
                    ));
                }
            }
            (Some(source_action), Some(translated_action)) => {
                result.violations.push(disabled_action(
                    source_node,
                    source_action,
                    translated_action,
                ));
            }
            (Some(source_action), None) => {
                result
                    .violations
                    .push(missing_action(source_node, source_action));
            }
            (None, _) => {}
        }
    }

    for action_id in translated_node_actions.difference(&source_node_actions) {
        if let Some(action) =
            translated_actions.get(&(source_node.node_id.clone(), action_id.clone()))
        {
            result.improvements.push(improvement(
                AccessibilityImprovementKind::AddedAction,
                Some(source_node.node_id.clone()),
                baseline_ref(source_node),
                "translated node exposes an additional reachable assistive action",
                None,
                Some(format!("{}:{:?}", action.action_id, action.kind)),
            ));
        }
    }
}

fn compare_contrast(
    source_node: &AccessibilityNode,
    translated_node: &AccessibilityNode,
    config: &AccessibilityDiffConfig,
    result: &mut NodeCompareResult,
) {
    if source_node.role == AccessibilityRole::Presentation {
        return;
    }
    if let Some(translated_ratio) = translated_node.contrast_ratio {
        if translated_ratio < config.minimum_contrast_ratio {
            result
                .violations
                .push(contrast_below_policy(source_node, translated_node, config));
        } else {
            result
                .covered_policy_ids
                .insert(A11Y_CONTRAST_POLICY_ID.to_string());
            result.successes = result.successes.saturating_add(1);
        }
    }
}

fn compare_focus_graph(
    _source_run: &AccessibilityRun,
    translated_run: &AccessibilityRun,
    source_nodes: &BTreeMap<String, AccessibilityNode>,
    config: &AccessibilityDiffConfig,
) -> FocusCompareResult {
    let mut result = FocusCompareResult::default();
    if !config.require_focus_reachability {
        return result;
    }

    let source_order = focus_order(source_nodes.values());
    let translated_graph = focus_graph(&translated_run.focus_transitions);
    let translated_focus_ids = translated_run
        .nodes
        .iter()
        .filter(|node| node.focusable && node.enabled)
        .map(|node| node.node_id.clone())
        .collect::<BTreeSet<_>>();

    for source_node_id in &source_order {
        if !translated_focus_ids.contains(source_node_id) {
            let violation = unreachable_focus_node(
                source_node_id,
                "translated focus graph omits source focus target",
            );
            result.violations.push(violation);
        }
    }

    for [from, to] in source_order.array_windows::<2>() {
        if reachable(&translated_graph, from, to) {
            result
                .covered_policy_ids
                .insert(A11Y_FOCUS_POLICY_ID.to_string());
            result.successes = result.successes.saturating_add(1);
        } else {
            result.violations.push(missing_focus_transition(from, to));
        }
    }

    let source_focus = source_order.into_iter().collect::<BTreeSet<_>>();
    for node in translated_run
        .nodes
        .iter()
        .filter(|node| node.focusable && node.enabled && !source_focus.contains(&node.node_id))
    {
        result
            .covered_policy_ids
            .insert(A11Y_IMPROVEMENT_POLICY_ID.to_string());
        result.successes = result.successes.saturating_add(1);
        if reachable_from_any_source(&translated_graph, &source_focus, &node.node_id) {
            continue;
        }
        result.violations.push(unreachable_focus_node(
            &node.node_id,
            "translated focus target is not reachable from preserved source focus graph",
        ));
    }
    result
}

struct AnnouncementCompareState<'a> {
    improvements: &'a mut Vec<AccessibilityImprovement>,
    violations: &'a mut Vec<AccessibilityViolation>,
    successes: &'a mut u32,
    weighted_failures: &'a mut u32,
    covered_policy_ids: &'a mut BTreeSet<String>,
    violated_policy_ids: &'a mut BTreeSet<String>,
}

fn compare_announcements(
    source_announcements: &BTreeMap<String, AssistiveAnnouncement>,
    translated_announcements: &BTreeMap<String, AssistiveAnnouncement>,
    state: &mut AnnouncementCompareState<'_>,
) {
    for (announcement_id, source_announcement) in source_announcements {
        match translated_announcements.get(announcement_id) {
            Some(translated_announcement)
                if source_announcement.text == translated_announcement.text
                    && source_announcement.politeness == translated_announcement.politeness =>
            {
                *state.successes = state.successes.saturating_add(1);
                state
                    .covered_policy_ids
                    .insert(A11Y_ASSISTIVE_POLICY_ID.to_string());
            }
            Some(translated_announcement) => {
                let violation = announcement_changed(source_announcement, translated_announcement);
                *state.weighted_failures = state
                    .weighted_failures
                    .saturating_add(failure_weight(violation.risk_level));
                state
                    .violated_policy_ids
                    .insert(violation.policy_id.clone());
                state.violations.push(violation);
            }
            None => {
                let violation = missing_announcement(source_announcement);
                *state.weighted_failures = state
                    .weighted_failures
                    .saturating_add(failure_weight(violation.risk_level));
                state
                    .violated_policy_ids
                    .insert(violation.policy_id.clone());
                state.violations.push(violation);
            }
        }
    }

    for (announcement_id, translated_announcement) in translated_announcements {
        if !source_announcements.contains_key(announcement_id) {
            state.improvements.push(improvement(
                AccessibilityImprovementKind::AddedAnnouncement,
                translated_announcement.node_id.clone(),
                format!("announcement:{announcement_id}"),
                "translated run adds an assistive announcement absent from source baseline",
                None,
                Some(translated_announcement.text.clone()),
            ));
        }
    }
}

fn node_map(nodes: &[AccessibilityNode]) -> BTreeMap<String, AccessibilityNode> {
    nodes
        .iter()
        .cloned()
        .map(|node| (node.node_id.clone(), node))
        .collect()
}

fn action_map(nodes: &[AccessibilityNode]) -> BTreeMap<(String, String), AccessibilityAction> {
    nodes
        .iter()
        .flat_map(|node| {
            node.actions
                .iter()
                .cloned()
                .map(|action| ((node.node_id.clone(), action.action_id.clone()), action))
        })
        .collect()
}

fn announcement_map(
    announcements: &[AssistiveAnnouncement],
) -> BTreeMap<String, AssistiveAnnouncement> {
    announcements
        .iter()
        .cloned()
        .map(|announcement| (announcement.announcement_id.clone(), announcement))
        .collect()
}

fn focus_order<'a>(nodes: impl Iterator<Item = &'a AccessibilityNode>) -> Vec<String> {
    let mut focusable = nodes
        .filter(|node| node.focusable && node.enabled)
        .map(|node| (node.focus_order.unwrap_or(u32::MAX), node.node_id.clone()))
        .collect::<Vec<_>>();
    focusable.sort();
    focusable
        .into_iter()
        .map(|(_order, node_id)| node_id)
        .collect()
}

fn focus_graph(transitions: &[FocusTransition]) -> BTreeMap<String, BTreeSet<String>> {
    let mut graph: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for transition in transitions {
        graph
            .entry(transition.from_node_id.clone())
            .or_default()
            .insert(transition.to_node_id.clone());
    }
    graph
}

fn reachable(graph: &BTreeMap<String, BTreeSet<String>>, from: &str, to: &str) -> bool {
    if from == to {
        return true;
    }
    let mut visited = BTreeSet::new();
    let mut queue = VecDeque::from([from.to_string()]);
    while let Some(current) = queue.pop_front() {
        if !visited.insert(current.clone()) {
            continue;
        }
        if let Some(next_ids) = graph.get(&current) {
            for next_id in next_ids {
                if next_id == to {
                    return true;
                }
                queue.push_back(next_id.clone());
            }
        }
    }
    false
}

fn reachable_from_any_source(
    graph: &BTreeMap<String, BTreeSet<String>>,
    source_focus: &BTreeSet<String>,
    target: &str,
) -> bool {
    source_focus
        .iter()
        .any(|source_id| reachable(graph, source_id, target))
}

fn missing_node(node: &AccessibilityNode) -> AccessibilityViolation {
    violation(
        AccessibilityViolationKind::MissingNode,
        Some(node.node_id.clone()),
        A11Y_ASSISTIVE_POLICY_ID,
        TransformationRiskLevel::Critical,
        Some(node_summary(node)),
        None,
        "Restore the accessible node or attach an explicit waiver explaining why the source node is obsolete.",
        "translated run dropped a source accessibility node",
    )
}

fn role_changed(
    source_node: &AccessibilityNode,
    translated_node: &AccessibilityNode,
) -> AccessibilityViolation {
    violation(
        AccessibilityViolationKind::RoleChanged,
        Some(source_node.node_id.clone()),
        A11Y_ASSISTIVE_POLICY_ID,
        TransformationRiskLevel::High,
        Some(format!("{:?}", source_node.role)),
        Some(format!("{:?}", translated_node.role)),
        "Preserve the source role or add a migration policy waiver with assistive-output proof.",
        "translated node changed assistive role",
    )
}

fn focusability_dropped(
    source_node: &AccessibilityNode,
    translated_node: &AccessibilityNode,
) -> AccessibilityViolation {
    violation(
        AccessibilityViolationKind::FocusabilityDropped,
        Some(source_node.node_id.clone()),
        A11Y_FOCUS_POLICY_ID,
        TransformationRiskLevel::Critical,
        Some(source_node.focusable.to_string()),
        Some(translated_node.focusable.to_string()),
        "Keep the node keyboard-focusable or reroute focus to an equivalent reachable control.",
        "translated node is no longer keyboard focusable",
    )
}

fn disabled_reachable_node(
    source_node: &AccessibilityNode,
    translated_node: &AccessibilityNode,
) -> AccessibilityViolation {
    violation(
        AccessibilityViolationKind::DisabledReachableNode,
        Some(source_node.node_id.clone()),
        A11Y_ACTION_POLICY_ID,
        TransformationRiskLevel::High,
        Some(source_node.enabled.to_string()),
        Some(translated_node.enabled.to_string()),
        "Preserve enabled state for reachable controls or expose an equivalent enabled action.",
        "translated node disables a source reachable control",
    )
}

fn missing_accessible_name(
    source_node: &AccessibilityNode,
    translated_node: &AccessibilityNode,
) -> AccessibilityViolation {
    violation(
        AccessibilityViolationKind::MissingNode,
        Some(source_node.node_id.clone()),
        A11Y_ASSISTIVE_POLICY_ID,
        TransformationRiskLevel::High,
        source_node.name.clone(),
        translated_node.name.clone(),
        "Restore the accessible name so screen-reader output remains meaningful.",
        "translated node dropped source accessible name",
    )
}

fn missing_action(
    node: &AccessibilityNode,
    action: &AccessibilityAction,
) -> AccessibilityViolation {
    violation(
        AccessibilityViolationKind::MissingAction,
        Some(node.node_id.clone()),
        A11Y_ACTION_POLICY_ID,
        TransformationRiskLevel::Critical,
        Some(format!("{}:{:?}", action.action_id, action.kind)),
        None,
        "Expose the source action on the translated node or provide an equivalent reachable action.",
        "translated node is missing a source assistive action",
    )
}

fn disabled_action(
    node: &AccessibilityNode,
    source_action: &AccessibilityAction,
    translated_action: &AccessibilityAction,
) -> AccessibilityViolation {
    violation(
        AccessibilityViolationKind::DisabledAction,
        Some(node.node_id.clone()),
        A11Y_ACTION_POLICY_ID,
        TransformationRiskLevel::High,
        Some(source_action.enabled.to_string()),
        Some(translated_action.enabled.to_string()),
        "Keep the translated action enabled when the source action is enabled.",
        "translated action exists but is disabled",
    )
}

fn action_kind_changed(
    node: &AccessibilityNode,
    source_action: &AccessibilityAction,
    translated_action: &AccessibilityAction,
) -> AccessibilityViolation {
    violation(
        AccessibilityViolationKind::MissingAction,
        Some(node.node_id.clone()),
        A11Y_ACTION_POLICY_ID,
        TransformationRiskLevel::High,
        Some(format!("{:?}", source_action.kind)),
        Some(format!("{:?}", translated_action.kind)),
        "Preserve action semantics while adding labels or shortcuts as separate improvements.",
        "translated action changed assistive action semantics",
    )
}

fn contrast_below_policy(
    source_node: &AccessibilityNode,
    translated_node: &AccessibilityNode,
    config: &AccessibilityDiffConfig,
) -> AccessibilityViolation {
    violation(
        AccessibilityViolationKind::ContrastBelowPolicy,
        Some(source_node.node_id.clone()),
        A11Y_CONTRAST_POLICY_ID,
        TransformationRiskLevel::High,
        source_node
            .contrast_ratio
            .map(|ratio| format!("{ratio:.2}")),
        translated_node
            .contrast_ratio
            .map(|ratio| format!("{ratio:.2}")),
        "Raise translated contrast to meet or exceed the configured accessibility threshold.",
        &format!(
            "translated contrast is below policy threshold {:.2}",
            config.minimum_contrast_ratio
        ),
    )
}

fn unreachable_focus_node(node_id: &str, message: &str) -> AccessibilityViolation {
    violation(
        AccessibilityViolationKind::UnreachableFocusNode,
        Some(node_id.to_string()),
        A11Y_FOCUS_POLICY_ID,
        TransformationRiskLevel::Critical,
        Some("reachable".to_string()),
        Some("unreachable".to_string()),
        "Restore a deterministic keyboard path to this focus target.",
        message,
    )
}

fn missing_focus_transition(from: &str, to: &str) -> AccessibilityViolation {
    violation(
        AccessibilityViolationKind::MissingFocusTransition,
        Some(from.to_string()),
        A11Y_FOCUS_POLICY_ID,
        TransformationRiskLevel::Critical,
        Some(format!("{from}->{to}")),
        None,
        "Add a deterministic focus transition preserving source traversal order.",
        "translated focus graph cannot reach the next source focus target",
    )
}

fn missing_announcement(source_announcement: &AssistiveAnnouncement) -> AccessibilityViolation {
    violation(
        AccessibilityViolationKind::MissingAnnouncement,
        source_announcement.node_id.clone(),
        A11Y_ASSISTIVE_POLICY_ID,
        TransformationRiskLevel::Medium,
        Some(source_announcement.text.clone()),
        None,
        "Restore the source live-region or screen-reader announcement.",
        "translated run dropped an assistive announcement",
    )
}

fn announcement_changed(
    source_announcement: &AssistiveAnnouncement,
    translated_announcement: &AssistiveAnnouncement,
) -> AccessibilityViolation {
    violation(
        AccessibilityViolationKind::MissingAnnouncement,
        source_announcement.node_id.clone(),
        A11Y_ASSISTIVE_POLICY_ID,
        TransformationRiskLevel::Medium,
        Some(format!(
            "{}:{}",
            source_announcement.politeness, source_announcement.text
        )),
        Some(format!(
            "{}:{}",
            translated_announcement.politeness, translated_announcement.text
        )),
        "Preserve source announcement text and politeness unless an improvement rationale is logged.",
        "translated run changed an assistive announcement",
    )
}

fn added_focus_target(node: &AccessibilityNode) -> AccessibilityImprovement {
    improvement(
        AccessibilityImprovementKind::AddedReachableFocusTarget,
        Some(node.node_id.clone()),
        baseline_ref(node),
        "translated run adds an enabled focus target beyond the source baseline",
        None,
        Some(node_summary(node)),
    )
}

fn improvement(
    improvement_kind: AccessibilityImprovementKind,
    node_id: Option<String>,
    baseline_ref: String,
    rationale: &str,
    source_value: Option<String>,
    translated_value: Option<String>,
) -> AccessibilityImprovement {
    AccessibilityImprovement {
        improvement_kind,
        node_id,
        policy_id: A11Y_IMPROVEMENT_POLICY_ID.to_string(),
        baseline_ref,
        rationale: rationale.to_string(),
        source_value,
        translated_value,
    }
}

#[allow(clippy::too_many_arguments)]
fn violation(
    violation_kind: AccessibilityViolationKind,
    node_id: Option<String>,
    policy_id: &str,
    risk_level: TransformationRiskLevel,
    source_value: Option<String>,
    translated_value: Option<String>,
    remediation_hint: &str,
    message: &str,
) -> AccessibilityViolation {
    AccessibilityViolation {
        violation_kind,
        node_id,
        policy_id: policy_id.to_string(),
        risk_level,
        source_value,
        translated_value,
        remediation_hint: remediation_hint.to_string(),
        message: message.to_string(),
    }
}

fn node_summary(node: &AccessibilityNode) -> String {
    format!(
        "{:?}:{}:{}",
        node.role,
        node.node_id,
        node.name.as_deref().unwrap_or("unnamed")
    )
}

fn baseline_ref(node: &AccessibilityNode) -> String {
    node.source_ref
        .clone()
        .unwrap_or_else(|| format!("source-node:{}", node.node_id))
}

fn sort_violations_by_severity(violations: &mut [AccessibilityViolation]) {
    violations.sort_by(|a, b| {
        b.risk_level
            .cmp(&a.risk_level)
            .then_with(|| a.policy_id.cmp(&b.policy_id))
            .then_with(|| a.node_id.cmp(&b.node_id))
            .then_with(|| format!("{:?}", a.violation_kind).cmp(&format!("{:?}", b.violation_kind)))
    });
}

fn canonicalize_nodes(mut nodes: Vec<AccessibilityNode>) -> Vec<AccessibilityNode> {
    for node in &mut nodes {
        node.actions = canonicalize_actions(std::mem::take(&mut node.actions));
    }
    nodes.sort_by(|a, b| {
        a.focus_order
            .cmp(&b.focus_order)
            .then_with(|| a.node_id.cmp(&b.node_id))
            .then_with(|| a.role.cmp(&b.role))
    });
    nodes
}

fn canonicalize_actions(mut actions: Vec<AccessibilityAction>) -> Vec<AccessibilityAction> {
    actions.sort_by(|a, b| {
        a.action_id
            .cmp(&b.action_id)
            .then_with(|| a.kind.cmp(&b.kind))
            .then_with(|| a.label.cmp(&b.label))
    });
    actions
}

fn canonicalize_transitions(mut transitions: Vec<FocusTransition>) -> Vec<FocusTransition> {
    transitions.sort_by(|a, b| {
        a.from_node_id
            .cmp(&b.from_node_id)
            .then_with(|| a.to_node_id.cmp(&b.to_node_id))
            .then_with(|| a.trigger.cmp(&b.trigger))
    });
    transitions
}

fn canonicalize_announcements(
    mut announcements: Vec<AssistiveAnnouncement>,
) -> Vec<AssistiveAnnouncement> {
    announcements.sort_by(|a, b| {
        a.announcement_id
            .cmp(&b.announcement_id)
            .then_with(|| a.node_id.cmp(&b.node_id))
            .then_with(|| a.politeness.cmp(&b.politeness))
            .then_with(|| a.text.cmp(&b.text))
    });
    announcements
}

fn risk_score(successes: u32, weighted_failures: u32) -> f64 {
    if weighted_failures == 0 {
        return 0.0;
    }
    let total = successes.saturating_add(weighted_failures);
    f64::from(weighted_failures) / f64::from(total)
}

fn failure_weight(risk: TransformationRiskLevel) -> u32 {
    match risk {
        TransformationRiskLevel::Low => 1,
        TransformationRiskLevel::Medium => 2,
        TransformationRiskLevel::High => 4,
        TransformationRiskLevel::Critical => 8,
    }
}

fn expected_loss(
    successes: u32,
    weighted_failures: u32,
    claim_id: Option<String>,
) -> ExpectedLossResult {
    let confidence_model =
        load_builtin_confidence_model().expect("built-in confidence model must parse");
    let posterior = confidence_model.compute_posterior(successes, weighted_failures);
    confidence_model.expected_loss_decision(
        &posterior,
        claim_id,
        Some(ACCESSIBILITY_DIFF_VALIDATOR_ID.to_string()),
    )
}
