use doctor_frankentui::accessibility_diff::{
    AccessibilityAction, AccessibilityActionKind, AccessibilityDiffConfig,
    AccessibilityDiffVerdict, AccessibilityImprovementKind, AccessibilityNode, AccessibilityRole,
    AccessibilityRun, AccessibilityViolationKind, AssistiveAnnouncement, FocusTransition,
    compare_accessibility_runs,
};
use doctor_frankentui::semantic_contract::TransformationRiskLevel;

fn activate() -> AccessibilityAction {
    AccessibilityAction::new("activate", AccessibilityActionKind::Activate, "Activate")
}

fn set_value() -> AccessibilityAction {
    AccessibilityAction::new("set-value", AccessibilityActionKind::SetValue, "Set value")
}

fn button(node_id: &str, order: u32, name: &str) -> AccessibilityNode {
    AccessibilityNode::new(node_id, AccessibilityRole::Button)
        .with_name(name)
        .with_focus_order(order)
        .with_action(activate())
        .with_contrast_ratio(5.0)
        .with_source_ref(format!("tsx:{node_id}"))
}

fn input(node_id: &str, order: u32, name: &str) -> AccessibilityNode {
    AccessibilityNode::new(node_id, AccessibilityRole::TextInput)
        .with_name(name)
        .with_focus_order(order)
        .with_action(set_value())
        .with_contrast_ratio(5.2)
        .with_source_ref(format!("tsx:{node_id}"))
}

fn run(run_id: &str, nodes: Vec<AccessibilityNode>) -> AccessibilityRun {
    AccessibilityRun::new(run_id, nodes)
        .with_focus_transitions(vec![
            FocusTransition::new("name", "save", "Tab"),
            FocusTransition::new("save", "name", "Shift+Tab"),
        ])
        .with_announcements(vec![AssistiveAnnouncement::new(
            "form-ready",
            Some("name".to_string()),
            "Form ready",
            "polite",
        )])
        .with_replay_command(format!("doctor_frankentui a11y-replay --run-id {run_id}"))
}

#[test]
fn equivalent_accessibility_runs_pass() {
    let source = run(
        "source-run",
        vec![input("name", 0, "Name"), button("save", 1, "Save")],
    );
    let translated = run(
        "translated-run",
        vec![input("name", 0, "Name"), button("save", 1, "Save")],
    );

    let report =
        compare_accessibility_runs(&source, &translated, &AccessibilityDiffConfig::default());

    assert_eq!(report.verdict, AccessibilityDiffVerdict::Equivalent);
    assert!(report.violations.is_empty());
    assert!(report.improvements.is_empty());
    assert!(report.covered_policy_ids.contains(&"AD-001".to_string()));
    assert!(report.covered_policy_ids.contains(&"AD-002".to_string()));
    assert_eq!(
        report.expected_loss.policy_id.as_deref(),
        Some("accessibility_diff_validator")
    );
}

#[test]
fn missing_action_is_critical_reachability_violation() {
    let source = run(
        "source-run",
        vec![input("name", 0, "Name"), button("save", 1, "Save")],
    );
    let translated_save = AccessibilityNode::new("save", AccessibilityRole::Button)
        .with_name("Save")
        .with_focus_order(1)
        .with_contrast_ratio(5.0);
    let translated = run(
        "translated-run",
        vec![input("name", 0, "Name"), translated_save],
    );

    let report =
        compare_accessibility_runs(&source, &translated, &AccessibilityDiffConfig::default());

    assert_eq!(report.verdict, AccessibilityDiffVerdict::Violation);
    assert_eq!(report.risk_level, TransformationRiskLevel::Critical);
    assert!(report.violated_policy_ids.contains(&"AD-002".to_string()));
    assert!(report.violations.iter().any(|violation| {
        violation.violation_kind == AccessibilityViolationKind::MissingAction
            && violation.node_id.as_deref() == Some("save")
            && violation
                .remediation_hint
                .contains("equivalent reachable action")
    }));
}

#[test]
fn missing_focus_transition_is_ranked_with_remediation() {
    let source = run(
        "source-run",
        vec![input("name", 0, "Name"), button("save", 1, "Save")],
    );
    let translated = AccessibilityRun::new(
        "translated-run",
        vec![input("name", 0, "Name"), button("save", 1, "Save")],
    )
    .with_focus_transitions(vec![FocusTransition::new("save", "name", "Shift+Tab")]);

    let report =
        compare_accessibility_runs(&source, &translated, &AccessibilityDiffConfig::default());

    assert_eq!(report.verdict, AccessibilityDiffVerdict::Violation);
    assert_eq!(
        report.violations[0].risk_level,
        TransformationRiskLevel::Critical
    );
    assert!(report.violations.iter().any(|violation| {
        violation.violation_kind == AccessibilityViolationKind::MissingFocusTransition
            && violation.remediation_hint.contains("focus transition")
    }));
}

#[test]
fn accessibility_improvements_include_baseline_rationale() {
    let source_name = AccessibilityNode::new("name", AccessibilityRole::TextInput)
        .with_focus_order(0)
        .with_action(set_value())
        .with_contrast_ratio(4.7)
        .with_source_ref("tsx:NameInput");
    let translated_name = AccessibilityNode::new("name", AccessibilityRole::TextInput)
        .with_name("Full name")
        .with_description("Required field")
        .with_shortcut("Ctrl+N")
        .with_focus_order(0)
        .with_action(set_value())
        .with_action(AccessibilityAction::new(
            "clear",
            AccessibilityActionKind::Custom("clear".to_string()),
            "Clear",
        ))
        .with_contrast_ratio(5.4)
        .with_source_ref("tsx:NameInput");
    let source = AccessibilityRun::new("source-run", vec![source_name]);
    let translated = AccessibilityRun::new("translated-run", vec![translated_name])
        .with_announcements(vec![AssistiveAnnouncement::new(
            "name-help",
            Some("name".to_string()),
            "Full name is required",
            "polite",
        )]);

    let report =
        compare_accessibility_runs(&source, &translated, &AccessibilityDiffConfig::default());

    assert_eq!(report.verdict, AccessibilityDiffVerdict::Improved);
    assert!(report.violations.is_empty());
    assert!(
        report
            .improvements
            .iter()
            .any(|item| item.improvement_kind == AccessibilityImprovementKind::AddedAccessibleName)
    );
    assert!(
        report
            .improvements
            .iter()
            .any(|item| item.improvement_kind == AccessibilityImprovementKind::ImprovedContrast)
    );
    assert!(
        report
            .improvements
            .iter()
            .all(|item| { !item.baseline_ref.is_empty() && item.rationale.contains("translated") })
    );
}

#[test]
fn contrast_regression_below_policy_fails() {
    let source = run(
        "source-run",
        vec![input("name", 0, "Name"), button("save", 1, "Save")],
    );
    let translated = run(
        "translated-run",
        vec![
            input("name", 0, "Name"),
            button("save", 1, "Save").with_contrast_ratio(3.1),
        ],
    );

    let report =
        compare_accessibility_runs(&source, &translated, &AccessibilityDiffConfig::default());

    assert_eq!(report.verdict, AccessibilityDiffVerdict::Violation);
    assert!(report.violated_policy_ids.contains(&"AD-004".to_string()));
    assert!(report.violations.iter().any(|violation| {
        violation.violation_kind == AccessibilityViolationKind::ContrastBelowPolicy
            && violation.remediation_hint.contains("contrast")
    }));
}

#[test]
fn missing_assistive_announcement_is_reported() {
    let source = run(
        "source-run",
        vec![input("name", 0, "Name"), button("save", 1, "Save")],
    );
    let translated = AccessibilityRun::new(
        "translated-run",
        vec![input("name", 0, "Name"), button("save", 1, "Save")],
    )
    .with_focus_transitions(vec![
        FocusTransition::new("name", "save", "Tab"),
        FocusTransition::new("save", "name", "Shift+Tab"),
    ]);

    let report =
        compare_accessibility_runs(&source, &translated, &AccessibilityDiffConfig::default());

    assert_eq!(report.verdict, AccessibilityDiffVerdict::Violation);
    assert!(report.violated_policy_ids.contains(&"AD-003".to_string()));
    assert!(report.violations.iter().any(|violation| {
        violation.violation_kind == AccessibilityViolationKind::MissingAnnouncement
            && violation.message.contains("announcement")
    }));
}
