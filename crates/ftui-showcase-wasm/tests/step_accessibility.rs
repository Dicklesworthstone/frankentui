#![forbid(unsafe_code)]

//! Cross-component coverage using the actual showcase model and widget renders.
//! No synthetic accessibility nodes or direct announcement-helper calls.

use ftui_core::event::Event;
use ftui_demo_showcase::app::AppModel;
use ftui_web::step_program::StepProgram;

fn runner() -> StepProgram<AppModel> {
    StepProgram::new(AppModel::default(), 80, 24).with_accessibility(Default::default())
}

fn assert_current_tree(runner: &StepProgram<AppModel>) {
    let tree = runner.accessibility_tree().expect("collection enabled");
    assert!(!tree.is_empty(), "actual widgets must populate the tree");
    assert!(tree.root().is_some());
    assert!(!runner.accessibility_order().is_empty());
    for id in runner.accessibility_order() {
        assert!(tree.node(*id).is_some(), "reading order references a missing node");
    }
    let has_name = tree.nodes().any(|node| {
        node.name
            .as_deref()
            .is_some_and(|name| !name.trim().is_empty())
    });
    assert!(has_name);
    let mirror = runner.accessibility_mirror().expect("bounded host mirror");
    assert!(!mirror.lines.is_empty());
    assert!(mirror.lines.len() <= 128);
    assert!(mirror.lines.iter().all(|line| line.chars().count() <= 240));
}

#[test]
fn real_showcase_widgets_reach_the_step_runner_accessibility_channel() {
    let mut runner = runner();
    runner.init().expect("initialize the real showcase");
    assert_current_tree(&runner);
    assert!(runner.outputs().last_buffer.is_some());
    let initial = runner.take_accessibility_announcements();
    assert!(initial.announcements.len() <= 8);
    assert!(
        initial
            .announcements
            .iter()
            .all(|announcement| announcement.text.chars().count() <= 240)
    );
    assert!(
        runner
            .take_accessibility_announcements()
            .announcements
            .is_empty()
    );
    // The real Model::on_accessibility hook gets a chance to render its
    // diagnostics on a later host step, without a synthetic input event.
    let follow_up = runner.step().expect("callback-driven follow-up frame");
    assert!(follow_up.rendered);
    assert_eq!(follow_up.events_processed, 0);
    assert_current_tree(&runner);
}

#[test]
fn real_showcase_navigation_and_resize_refresh_the_semantic_snapshot() {
    let mut runner = runner();
    runner.init().unwrap();
    let before = runner.accessibility_tree().unwrap().clone();
    runner.take_accessibility_announcements();
    runner.take_outputs();

    assert!(runner.model_mut().goto_screen_index(1));
    runner
        .push_event(Event::Resize {
            width: 100,
            height: 30,
        })
        .unwrap();
    assert!(runner.step().unwrap().rendered);
    assert_current_tree(&runner);
    assert!(!runner.accessibility_tree().unwrap().diff(&before).is_empty());
    let output = runner.take_outputs();
    let buffer = output.last_buffer.expect("resized visual frame");
    assert_eq!(buffer.width(), 100);
    assert_eq!(buffer.height(), 30);
    assert!(output.last_full_repaint_hint);
    // Visual output consumption must not discard the semantic tree.
    assert_current_tree(&runner);

    runner.set_accessibility_policy(None);
    assert!(runner.step().unwrap().rendered);
    assert!(runner.accessibility_tree().is_none());
    assert!(runner.accessibility_mirror().is_none());
    assert!(runner.accessibility_order().is_empty());
    assert!(
        runner
            .take_accessibility_announcements()
            .announcements
            .is_empty()
    );
    runner.set_accessibility_policy(Some(Default::default()));
    assert!(runner.step().unwrap().rendered);
    assert_current_tree(&runner);
}
