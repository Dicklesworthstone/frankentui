use ftui_core::geometry::Rect;
use ftui_layout::pane::{PaneConstraints, PaneId, PaneTree};

#[test]
fn visual_rect_uses_defaults() {
    let tree = PaneTree::singleton("editor");
    let outer = Rect::new(10, 20, 30, 12);
    let layout = tree.solve_layout(outer).expect("valid pane layout");

    assert_eq!(layout.rect(tree.root()), Some(outer));
    assert_eq!(
        layout.visual_rect(tree.root()),
        Some(Rect::new(12, 22, 26, 8))
    );
    assert_eq!(
        layout.visual_rect_with_constraints(tree.root(), &PaneConstraints::default()),
        layout.visual_rect(tree.root())
    );
}

#[test]
fn custom_insets_and_explicit_zero_are_respected() {
    let tree = PaneTree::singleton("editor");
    let outer = Rect::new(10, 20, 30, 12);
    let layout = tree.solve_layout(outer).expect("valid pane layout");
    for (margin, padding, expected) in [
        (Some(2), Some(3), Rect::new(15, 25, 20, 2)),
        (Some(0), Some(0), outer),
        (Some(0), Some(2), Rect::new(12, 22, 26, 8)),
        (Some(2), Some(0), Rect::new(12, 22, 26, 8)),
        (None, Some(2), Rect::new(13, 23, 24, 6)),
        (Some(2), None, Rect::new(13, 23, 24, 6)),
        (None, None, Rect::new(12, 22, 26, 8)),
    ] {
        let constraints = PaneConstraints {
            margin,
            padding,
            ..PaneConstraints::default()
        };
        assert_eq!(
            layout.visual_rect_with_constraints(tree.root(), &constraints),
            Some(expected),
            "margin={margin:?}, padding={padding:?}"
        );
        // Visual insets must never change the solver's outer allocation.
        assert_eq!(layout.rect(tree.root()), Some(outer));
    }
}

#[test]
fn tiny_panes_drop_padding_when_either_content_dimension_would_be_empty() {
    let tree = PaneTree::singleton("editor");
    for (width, height, expected) in [
        (3, 8, Rect::new(11, 21, 1, 6)),
        (8, 3, Rect::new(11, 21, 6, 1)),
        (4, 4, Rect::new(11, 21, 2, 2)),
        (5, 5, Rect::new(12, 22, 1, 1)),
        (1, 1, Rect::new(11, 21, 0, 0)),
        (2, 2, Rect::new(11, 21, 0, 0)),
    ] {
        let layout = tree
            .solve_layout(Rect::new(10, 20, width, height))
            .expect("small pane layout");
        assert_eq!(layout.visual_rect(tree.root()), Some(expected));
        assert_eq!(
            layout.visual_rect_with_constraints(tree.root(), &PaneConstraints::default()),
            Some(expected)
        );
    }
}

#[test]
fn oversized_custom_insets_do_not_wrap() {
    let tree = PaneTree::singleton("editor");
    let layout = tree
        .solve_layout(Rect::new(10, 20, 30, 12))
        .expect("valid pane layout");
    for (margin, padding, expected) in [
        (1, u16::MAX, Rect::new(11, 21, 28, 10)),
        (u16::MAX, 1, Rect::new(u16::MAX, u16::MAX, 0, 0)),
    ] {
        let constraints = PaneConstraints {
            margin: Some(margin),
            padding: Some(padding),
            ..PaneConstraints::default()
        };
        assert_eq!(
            layout.visual_rect_with_constraints(tree.root(), &constraints),
            Some(expected)
        );
    }
}

#[test]
fn missing_pane_has_no_visual_rectangle() {
    let tree = PaneTree::singleton("editor");
    let layout = tree
        .solve_layout(Rect::new(0, 0, 10, 10))
        .expect("valid pane layout");
    let absent = PaneId::new(99).expect("nonzero pane id");
    assert_eq!(layout.visual_rect(absent), None);
    assert_eq!(
        layout.visual_rect_with_constraints(absent, &PaneConstraints::default()),
        None
    );
}

#[test]
fn serialized_constraints_preserve_defaults_and_explicit_insets() {
    let tree = PaneTree::singleton("editor");
    let layout = tree
        .solve_layout(Rect::new(10, 20, 30, 12))
        .expect("valid pane layout");
    for (fields, margin, padding, expected) in [
        ("", None, None, Rect::new(12, 22, 26, 8)),
        (
            r#", "margin": null, "padding": null"#,
            None,
            None,
            Rect::new(12, 22, 26, 8),
        ),
        (
            r#", "margin": 0, "padding": 0"#,
            Some(0),
            Some(0),
            Rect::new(10, 20, 30, 12),
        ),
        (
            r#", "margin": 2, "padding": 3"#,
            Some(2),
            Some(3),
            Rect::new(15, 25, 20, 2),
        ),
    ] {
        let json = format!(
            r#"{{"min_width":1,"min_height":1,"max_width":null,"max_height":null,"collapsible":false{fields}}}"#
        );
        let constraints: PaneConstraints = serde_json::from_str(&json).expect("valid constraints");
        assert_eq!(constraints.margin, margin);
        assert_eq!(constraints.padding, padding);
        assert_eq!(
            layout.visual_rect_with_constraints(tree.root(), &constraints),
            Some(expected)
        );
        let encoded = serde_json::to_string(&constraints).expect("serialize constraints");
        assert_eq!(
            serde_json::from_str::<PaneConstraints>(&encoded).expect("deserialize constraints"),
            constraints
        );
    }
}

#[test]
fn serialized_insets_reject_negative_and_out_of_range_values() {
    for field in ["margin", "padding"] {
        for value in [-1, 65_536] {
            let json = format!(
                r#"{{"min_width":1,"min_height":1,"max_width":null,"max_height":null,"collapsible":false,"{field}":{value}}}"#
            );
            assert!(
                serde_json::from_str::<PaneConstraints>(&json).is_err(),
                "invalid {field}={value} must be rejected"
            );
        }
    }
}
