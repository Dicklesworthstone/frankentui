#![forbid(unsafe_code)]

//! Regressions through actual Dialog rendering, events, and Frame finalization.

use ftui_a11y::node::{A11yNodeInfo, A11yRole, LiveRegion};
use ftui_a11y::tree::{A11yChange, A11yTree, A11yTreeBuilder, AnnouncementReason, ScreenReaderPolicy};
use ftui_core::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, Modifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ftui_core::geometry::Rect;
use ftui_render::frame::{Frame, HitId};
use ftui_render::grapheme_pool::GraphemePool;
use ftui_widgets::StatefulWidget;
use ftui_widgets::modal::{
    DIALOG_HIT_BUTTON, DIALOG_HIT_INPUT, Dialog, DialogResult, DialogState,
};

const AREA: Rect = Rect::new(0, 0, 80, 24);

fn key(code: KeyCode, modifiers: Modifiers, kind: KeyEventKind) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers,
        kind,
    })
}

fn press(dialog: &Dialog, state: &mut DialogState, code: KeyCode, modifiers: Modifiers) {
    assert!(
        dialog
            .handle_event(&key(code, modifiers, KeyEventKind::Press), state, None)
            .is_none()
    );
}

fn snapshot(dialog: &Dialog, state: &mut DialogState, area: Rect) -> (A11yTree, Vec<u64>) {
    let mut builder = A11yTreeBuilder::new();
    let order;
    {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        frame.set_a11y(&mut builder);
        dialog.render(area, &mut frame, state);
        frame.finish_a11y();
        order = frame.take_a11y_order();
    }
    (builder.build(), order)
}

fn assert_silent(current: &A11yTree, previous: &A11yTree) {
    let batch = current.screen_reader_announcements_since(previous, ScreenReaderPolicy::default());
    assert!(batch.announcements.is_empty());
    assert_eq!(batch.dropped_count, 0);
}

#[test]
fn dialog_presets_emit_named_scoped_controls_without_decorative_duplicates() {
    for (dialog, expected) in [
        (Dialog::alert("Title", "Message"), 2),
        (Dialog::confirm("Title", "Message"), 3),
        (Dialog::prompt("Title", "Message"), 4),
        (Dialog::custom("Title", "Message").ok_button().build(), 2),
    ] {
        let dialog = dialog.hit_id(HitId::new(700));
        let (tree, order) = snapshot(&dialog, &mut DialogState::new(), AREA);
        assert_eq!(tree.node_count(), expected);
        assert_eq!(order.len(), expected);
        let root = tree.root().unwrap();
        assert_eq!(root.id, 700);
        assert_eq!(root.role, A11yRole::Dialog);
        assert_eq!(root.name.as_deref(), Some("Title"));
        assert_eq!(root.description.as_deref(), Some("Message"));
        assert_eq!(root.children, order[1..]);
        assert!(!root.state.focused);
        for id in &root.children {
            let child = tree.node(*id).unwrap();
            assert_eq!(child.parent, Some(root.id));
            assert!(!child.name.as_deref().unwrap().trim().is_empty());
            assert!(!child.bounds.is_empty());
        }
        assert!(tree.nodes().all(|node| node.live_region.is_none()));
        assert!(!tree.nodes().any(|node| node.role == A11yRole::Group));
    }
}

#[test]
fn prompt_controls_use_exact_hit_bounds_and_expose_full_value() {
    let dialog = Dialog::prompt("Profile", "Enter a name").hit_id(HitId::new(700));
    let mut state = DialogState::new();
    state.input_value = "Jos\u{00e9} \u{754c}".to_owned();
    let mut builder = A11yTreeBuilder::new();
    let mut pool = GraphemePool::new();
    let mut frame = Frame::with_hit_grid(80, 24, &mut pool);
    frame.set_a11y(&mut builder);
    dialog.render(AREA, &mut frame, &mut state);
    frame.finish_a11y();
    // Inspect a snapshot without moving the builder borrowed by the live frame.
    let tree = frame.a11y.as_deref().unwrap().clone().build();
    let root = tree.root().unwrap();
    for (index, id) in root.children.iter().enumerate() {
        let node = tree.node(*id).unwrap();
        let expected = if index == 0 {
            assert_eq!(node.role, A11yRole::TextInput);
            assert_eq!(
                node.state.value_text.as_deref(),
                Some(state.input_value.as_str())
            );
            (HitId::new(700), DIALOG_HIT_INPUT, 0)
        } else {
            assert_eq!(node.role, A11yRole::Button);
            (HitId::new(700), DIALOG_HIT_BUTTON, (index - 1) as u64)
        };
        let mut hit_cells = 0;
        for y in 0..24 {
            for x in 0..80 {
                if frame.hit_test(x, y) == Some(expected) {
                    hit_cells += 1;
                    assert!(x >= node.bounds.x && x < node.bounds.right());
                    assert!(y >= node.bounds.y && y < node.bounds.bottom());
                }
            }
        }
        assert_eq!(
            hit_cells,
            usize::from(node.bounds.width) * usize::from(node.bounds.height)
        );
    }
}

#[test]
fn prompt_tab_and_both_reverse_tab_encodings_announce_each_destination_once() {
    let dialog = Dialog::prompt("Profile", "Enter a name").hit_id(HitId::new(700));
    let mut state = DialogState::new();
    let (mut previous, _) = snapshot(&dialog, &mut state, AREA);
    assert_eq!(previous.focused().unwrap().role, A11yRole::TextInput);
    let policy = ScreenReaderPolicy {
        max_announcements: 1,
        ..ScreenReaderPolicy::default()
    };
    for (code, modifiers, name, role) in [
        (KeyCode::Tab, Modifiers::empty(), "OK", A11yRole::Button),
        (KeyCode::Tab, Modifiers::empty(), "Cancel", A11yRole::Button),
        (KeyCode::Tab, Modifiers::empty(), "Profile", A11yRole::TextInput),
        (KeyCode::BackTab, Modifiers::empty(), "Cancel", A11yRole::Button),
        (KeyCode::BackTab, Modifiers::empty(), "OK", A11yRole::Button),
        (
            KeyCode::BackTab,
            Modifiers::empty(),
            "Profile",
            A11yRole::TextInput,
        ),
        (KeyCode::Tab, Modifiers::SHIFT, "Cancel", A11yRole::Button),
    ] {
        press(&dialog, &mut state, code, modifiers);
        let (current, _) = snapshot(&dialog, &mut state, AREA);
        let focused = current.focused().unwrap();
        assert_eq!(focused.name.as_deref(), Some(name));
        assert_eq!(focused.role, role);
        let batch = current.screen_reader_announcements_since(&previous, policy);
        assert_eq!(batch.announcements.len(), 1);
        assert_eq!(batch.dropped_count, 0);
        assert_eq!(batch.announcements[0].reason, AnnouncementReason::FocusChanged);
        assert_eq!(batch.announcements[0].node_id, Some(focused.id));
        assert_eq!(batch.announcements[0].urgency, LiveRegion::Polite);
        let (unchanged, _) = snapshot(&dialog, &mut state, AREA);
        assert!(unchanged.diff(&current).is_empty());
        assert_silent(&unchanged, &current);
        previous = current;
    }
    assert_eq!(
        dialog.handle_event(
            &key(KeyCode::Enter, Modifiers::empty(), KeyEventKind::Press),
            &mut state,
            None,
        ),
        Some(DialogResult::Cancel)
    );
    let (closed, _) = snapshot(&dialog, &mut state, AREA);
    assert!(closed.is_empty());
    assert_silent(&closed, &previous);
}

#[test]
fn retained_prompt_edit_updates_value_without_new_identity_or_full_value_speech() {
    let dialog = Dialog::prompt("Profile", "Enter a name").hit_id(HitId::new(700));
    let mut state = DialogState::new();
    let (before, _) = snapshot(&dialog, &mut state, AREA);
    press(&dialog, &mut state, KeyCode::Char('x'), Modifiers::empty());
    let (after, _) = snapshot(&dialog, &mut state, AREA);
    assert_eq!(before.focused_id(), after.focused_id());
    assert_eq!(after.focused().unwrap().state.value_text.as_deref(), Some("x"));
    let diff = after.diff(&before);
    assert!(diff.added.is_empty() && diff.removed.is_empty());
    assert!(diff.focus_changed.is_none());
    assert!(diff.changed.iter().any(|(_, changes)| changes.iter().any(|change| {
        matches!(change, A11yChange::StateChanged { field, .. } if field == "value_text")
    })));
    assert_silent(&after, &before);
}

#[test]
fn dialog_hit_identity_survives_moves_resizes_and_button_label_changes() {
    let before_dialog = Dialog::custom("Before", "Message")
        .custom_button("Save", "save")
        .cancel_button()
        .hit_id(HitId::new(700))
        .build();
    let after_dialog = Dialog::custom("After", "New message")
        .custom_button("Save changes", "save")
        .cancel_button()
        .hit_id(HitId::new(700))
        .build();
    let mut state = DialogState::new();
    state.input_focused = false;
    state.focused_button = Some(0);
    let (before, before_order) = snapshot(&before_dialog, &mut state, AREA);
    let (after, after_order) = snapshot(&after_dialog, &mut state, Rect::new(4, 2, 60, 18));
    assert_eq!(before_order, after_order);
    assert_eq!(before.focused_id(), after.focused_id());
    let diff = after.diff(&before);
    assert!(diff.added.is_empty() && diff.removed.is_empty());
    assert!(diff.focus_changed.is_none());
    assert_silent(&after, &before);
    assert_ne!(before.root().unwrap().bounds, after.root().unwrap().bounds);
}

#[test]
fn duplicate_button_keys_and_multiple_dialogs_do_not_collapse_nodes() {
    let dialog = Dialog::custom("Title", "Message")
        .custom_button("Same", "same")
        .custom_button("Same", "same")
        .build();
    let first = dialog.clone().hit_id(HitId::new(700));
    let second = dialog.hit_id(HitId::new(701));
    let mut builder = A11yTreeBuilder::new();
    {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        frame.set_a11y(&mut builder);
        first.render(Rect::new(0, 0, 40, 12), &mut frame, &mut DialogState::new());
        second.render(Rect::new(40, 0, 40, 12), &mut frame, &mut DialogState::new());
        frame.finish_a11y();
        assert_eq!(frame.a11y_order().len(), 6);
    }
    let tree = builder.build();
    assert_eq!(tree.node_count(), 6);
    let a = &tree.node(700).unwrap().children;
    let b = &tree.node(701).unwrap().children;
    assert_eq!(a.len(), 2);
    assert_eq!(b.len(), 2);
    assert_ne!(a[0], a[1]);
    assert_ne!(b[0], b[1]);
    assert!(a.iter().all(|id| !b.contains(id)));
}

#[test]
fn dialog_scoping_preserves_outer_parent_siblings_and_explicit_focus() {
    let dialog = Dialog::prompt("Title", "Message").hit_id(HitId::new(700));
    let mut builder = A11yTreeBuilder::new();
    builder.set_focused(Some(901));
    {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        frame.set_a11y(&mut builder);
        frame.with_a11y_scope(A11yNodeInfo::new(900, A11yRole::Group, AREA), |frame| {
            dialog.render(AREA, frame, &mut DialogState::new());
        });
        frame.push_a11y(
            A11yNodeInfo::new(901, A11yRole::Button, Rect::new(0, 0, 1, 1))
                .with_name("Outside"),
        );
        frame.finish_a11y();
    }
    let tree = builder.build();
    assert_eq!(tree.node(700).unwrap().parent, Some(900));
    assert_eq!(tree.node(900).unwrap().children, vec![700]);
    assert_eq!(tree.node(901).unwrap().parent, None);
    assert_eq!(tree.focused_id(), Some(901));
}

#[test]
fn closed_empty_and_fully_clipped_dialogs_emit_no_nodes() {
    let dialog = Dialog::prompt("Title", "Message").hit_id(HitId::new(700));
    let mut closed = DialogState::new();
    closed.close(DialogResult::Dismissed);
    assert!(snapshot(&dialog, &mut closed, AREA).0.is_empty());
    assert!(
        snapshot(&dialog, &mut DialogState::new(), Rect::new(0, 0, 0, 0))
            .0
            .is_empty()
    );
    let mut builder = A11yTreeBuilder::new();
    {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        frame.set_a11y(&mut builder);
        frame.buffer.push_scissor(Rect::new(0, 0, 1, 1));
        dialog.render(AREA, &mut frame, &mut DialogState::new());
        frame.finish_a11y();
        assert!(frame.a11y_order().is_empty());
    }
    assert!(builder.build().is_empty());
}

#[test]
fn clipped_controls_are_not_exposed_as_phantom_focus_targets() {
    let dialog = Dialog::prompt("Title", "Message").hit_id(HitId::new(700));
    let mut state = DialogState::new();
    let (full, _) = snapshot(&dialog, &mut state, AREA);
    let input_bounds = full.focused().unwrap().bounds;
    let mut builder = A11yTreeBuilder::new();
    {
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        frame.set_a11y(&mut builder);
        frame.buffer.push_scissor(input_bounds);
        dialog.render(AREA, &mut frame, &mut state);
        frame.finish_a11y();
    }
    let clipped = builder.build();
    assert_eq!(clipped.node_count(), 2); // dialog and the input only
    assert_eq!(clipped.focused_id(), full.focused_id());
    assert!(clipped.nodes().all(|node| node.bounds == input_bounds));
    let (tiny, _) = snapshot(&dialog, &mut state, Rect::new(0, 0, 2, 2));
    assert!(tiny.nodes().all(|node| node.role == A11yRole::Dialog));
    assert!(tiny.focused().is_none());
}

#[test]
fn a11y_collection_preserves_cells_hits_and_cursor() {
    let dialog = Dialog::prompt("Title", "Message").hit_id(HitId::new(700));
    let mut state = DialogState::new();
    state.input_value = "value".to_owned();
    let mut builder = A11yTreeBuilder::new();
    let mut a_pool = GraphemePool::new();
    let mut b_pool = GraphemePool::new();
    let mut plain = Frame::with_hit_grid(80, 24, &mut a_pool);
    let mut accessible = Frame::with_hit_grid(80, 24, &mut b_pool);
    accessible.set_a11y(&mut builder);
    dialog.render(AREA, &mut plain, &mut state);
    dialog.render(AREA, &mut accessible, &mut state);
    accessible.finish_a11y();
    assert!(plain.a11y_order().is_empty());
    assert_eq!(accessible.a11y_order().len(), 4);
    assert_eq!(plain.cursor_position, accessible.cursor_position);
    assert_eq!(plain.cursor_visible, accessible.cursor_visible);
    for y in 0..24 {
        for x in 0..80 {
            assert_eq!(plain.buffer.get(x, y), accessible.buffer.get(x, y));
            assert_eq!(plain.hit_test(x, y), accessible.hit_test(x, y));
        }
    }
}

#[test]
fn prompt_input_focus_wins_over_a_stale_button_focus_flag() {
    let dialog = Dialog::prompt("Title", "Message").hit_id(HitId::new(700));
    let mut state = DialogState::new();
    state.focused_button = Some(0);
    let (tree, _) = snapshot(&dialog, &mut state, AREA);
    let focused: Vec<_> = tree.nodes().filter(|node| node.state.focused).collect();
    assert_eq!(focused.len(), 1);
    assert_eq!(focused[0].role, A11yRole::TextInput);
    let batch = tree.screen_reader_announcements_since(
        &A11yTree::empty(),
        ScreenReaderPolicy::default(),
    );
    assert_eq!(batch.announcements.len(), 1);
    assert_eq!(batch.dropped_count, 0);
}

#[test]
fn backtab_cancels_a_pressed_button_and_ignores_release_and_repeat() {
    let dialog = Dialog::prompt("Title", "Message").hit_id(HitId::new(700));
    let mut state = DialogState::new();
    let hit = Some((HitId::new(700), DIALOG_HIT_BUTTON, 0));
    let down = Event::Mouse(MouseEvent::new(
        MouseEventKind::Down(MouseButton::Left),
        0,
        0,
    ));
    assert!(dialog.handle_event(&down, &mut state, hit).is_none());
    assert_eq!(state.focused_button, Some(0));
    press(&dialog, &mut state, KeyCode::BackTab, Modifiers::empty());
    assert!(state.input_focused);
    let up = Event::Mouse(MouseEvent::new(MouseEventKind::Up(MouseButton::Left), 0, 0));
    assert!(dialog.handle_event(&up, &mut state, hit).is_none());
    assert!(state.is_open());
    for kind in [KeyEventKind::Release, KeyEventKind::Repeat] {
        assert!(
            dialog
                .handle_event(
                    &key(KeyCode::BackTab, Modifiers::empty(), kind),
                    &mut state,
                    None,
                )
                .is_none()
        );
        assert!(state.input_focused);
        assert_eq!(state.focused_button, None);
    }
}

#[test]
fn keyboard_navigation_recovers_out_of_range_public_focus_state() {
    let dialog = Dialog::confirm("Title", "Message").hit_id(HitId::new(700));
    for (code, expected) in [
        (KeyCode::Tab, 0),
        (KeyCode::BackTab, 1),
        (KeyCode::Right, 0),
        (KeyCode::Left, 1),
    ] {
        let mut state = DialogState::new();
        state.input_focused = false;
        state.focused_button = Some(usize::MAX);
        press(&dialog, &mut state, code, Modifiers::empty());
        assert_eq!(state.focused_button, Some(expected));
        let (tree, _) = snapshot(&dialog, &mut state, AREA);
        assert_eq!(tree.focused().unwrap().role, A11yRole::Button);
    }
}
