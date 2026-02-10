#![forbid(unsafe_code)]

//! Integration tests: snapshot testing for core widgets.
//!
//! Run `BLESS=1 cargo test --package ftui-harness` to create/update snapshots.

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, Modifiers};
use ftui_core::geometry::{Rect, Sides};
use ftui_harness::{assert_snapshot, assert_snapshot_ansi};
use ftui_layout::Constraint;
use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::frame::{Frame, HitId, HitRegion};
use ftui_render::grapheme_pool::GraphemePool;
use ftui_style::{
    Style, TableEffect, TableEffectRule, TableEffectTarget, TablePresetId, TableTheme,
};
use ftui_text::{Span, Text, WrapMode};
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::BorderType;
use ftui_widgets::borders::Borders;
use ftui_widgets::columns::Columns;
use ftui_widgets::command_palette::CommandPalette;
use ftui_widgets::inspector::{InspectorMode, InspectorOverlay, InspectorState, WidgetInfo};
use ftui_widgets::list::{List, ListItem, ListState};
use ftui_widgets::modal::{BackdropConfig, Modal, ModalPosition, ModalSizeConstraints};
use ftui_widgets::padding::Padding;
use ftui_widgets::panel::Panel;
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::scrollbar::{Scrollbar, ScrollbarOrientation, ScrollbarState};
use ftui_widgets::table::{Row, Table, TableState};
use ftui_widgets::{StatefulWidget, Widget};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Table snapshot helpers
// ---------------------------------------------------------------------------

fn preset_label(preset: Option<TablePresetId>) -> &'static str {
    match preset {
        Some(TablePresetId::Aurora) => "aurora",
        Some(TablePresetId::Graphite) => "graphite",
        Some(TablePresetId::Neon) => "neon",
        Some(TablePresetId::Slate) => "slate",
        Some(TablePresetId::Solar) => "solar",
        Some(TablePresetId::Orchard) => "orchard",
        Some(TablePresetId::Paper) => "paper",
        Some(TablePresetId::Midnight) => "midnight",
        Some(TablePresetId::TerminalClassic) => "terminal_classic",
        None => "custom",
    }
}

fn assert_table_snapshot_ansi(name: &str, buf: &Buffer, theme: &TableTheme, phase: f32) {
    let diagnostics = theme.diagnostics();
    let preset = preset_label(diagnostics.preset_id);
    let result = std::panic::catch_unwind(|| {
        assert_snapshot_ansi!(name, buf);
    });
    if let Err(err) = result {
        eprintln!(
            "Table snapshot meta: preset={preset} phase={phase:.2} \
style_hash={:#x} effects_hash={:#x} padding={} column_gap={} row_height={}",
            diagnostics.style_hash,
            diagnostics.effects_hash,
            diagnostics.padding,
            diagnostics.column_gap,
            diagnostics.row_height,
        );
        std::panic::resume_unwind(err);
    }
}

// ============================================================================
// Block
// ============================================================================

#[test]
fn snapshot_block_plain() {
    let block = Block::default().borders(Borders::ALL).title("Box");
    let area = Rect::new(0, 0, 12, 5);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(12, 5, &mut pool);
    block.render(area, &mut frame);
    assert_snapshot!("block_plain", &frame.buffer);
}

#[test]
fn snapshot_block_no_borders() {
    let block = Block::default().title("Hello");
    let area = Rect::new(0, 0, 10, 3);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(10, 3, &mut pool);
    block.render(area, &mut frame);
    assert_snapshot!("block_no_borders", &frame.buffer);
}

// ============================================================================
// Paragraph
// ============================================================================

#[test]
fn snapshot_paragraph_simple() {
    let para = Paragraph::new(Text::raw("Hello, FrankenTUI!"));
    let area = Rect::new(0, 0, 20, 1);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(20, 1, &mut pool);
    para.render(area, &mut frame);
    assert_snapshot!("paragraph_simple", &frame.buffer);
}

#[test]
fn snapshot_paragraph_multiline() {
    let para = Paragraph::new(Text::raw("Line 1\nLine 2\nLine 3"));
    let area = Rect::new(0, 0, 10, 3);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(10, 3, &mut pool);
    para.render(area, &mut frame);
    assert_snapshot!("paragraph_multiline", &frame.buffer);
}

#[test]
fn snapshot_paragraph_centered() {
    let para = Paragraph::new(Text::raw("Hi")).alignment(Alignment::Center);
    let area = Rect::new(0, 0, 10, 1);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(10, 1, &mut pool);
    para.render(area, &mut frame);
    assert_snapshot!("paragraph_centered", &frame.buffer);
}

#[test]
fn snapshot_paragraph_in_block() {
    let para = Paragraph::new(Text::raw("Inner"))
        .block(Block::default().borders(Borders::ALL).title("Frame"));
    let area = Rect::new(0, 0, 15, 5);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(15, 5, &mut pool);
    para.render(area, &mut frame);
    assert_snapshot!("paragraph_in_block", &frame.buffer);
}

#[test]
fn snapshot_paragraph_wrapped_styles() {
    let text = Text::from_spans([
        Span::styled("Hello ", Style::new().bold()),
        Span::styled("world", Style::new().italic()),
    ]);
    let para = Paragraph::new(text).wrap(WrapMode::Word);
    let area = Rect::new(0, 0, 6, 2);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(6, 2, &mut pool);
    para.render(area, &mut frame);
    assert_snapshot_ansi!("paragraph_wrapped_styles", &frame.buffer);
}

// ============================================================================
// List
// ============================================================================

#[test]
fn snapshot_list_basic() {
    let items = vec![
        ListItem::new("Apple"),
        ListItem::new("Banana"),
        ListItem::new("Cherry"),
    ];
    let list = List::new(items);
    let area = Rect::new(0, 0, 12, 3);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(12, 3, &mut pool);
    let mut state = ListState::default();
    StatefulWidget::render(&list, area, &mut frame, &mut state);
    assert_snapshot!("list_basic", &frame.buffer);
}

#[test]
fn snapshot_list_with_selection() {
    let items = vec![
        ListItem::new("One"),
        ListItem::new("Two"),
        ListItem::new("Three"),
    ];
    let list = List::new(items).highlight_symbol(">");
    let area = Rect::new(0, 0, 12, 3);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(12, 3, &mut pool);
    let mut state = ListState::default();
    state.select(Some(1));
    StatefulWidget::render(&list, area, &mut frame, &mut state);
    assert_snapshot!("list_with_selection", &frame.buffer);
}

// ============================================================================
// Scrollbar
// ============================================================================

#[test]
fn snapshot_scrollbar_vertical() {
    let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight);
    let area = Rect::new(0, 0, 1, 10);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 10, &mut pool);
    let mut state = ScrollbarState::new(100, 0, 10);
    StatefulWidget::render(&sb, area, &mut frame, &mut state);
    assert_snapshot!("scrollbar_vertical_top", &frame.buffer);
}

#[test]
fn snapshot_scrollbar_vertical_mid() {
    let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight);
    let area = Rect::new(0, 0, 1, 10);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(1, 10, &mut pool);
    let mut state = ScrollbarState::new(100, 45, 10);
    StatefulWidget::render(&sb, area, &mut frame, &mut state);
    assert_snapshot!("scrollbar_vertical_mid", &frame.buffer);
}

#[test]
fn snapshot_scrollbar_horizontal() {
    let sb = Scrollbar::new(ScrollbarOrientation::HorizontalBottom);
    let area = Rect::new(0, 0, 20, 1);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(20, 1, &mut pool);
    let mut state = ScrollbarState::new(100, 0, 20);
    StatefulWidget::render(&sb, area, &mut frame, &mut state);
    assert_snapshot!("scrollbar_horizontal", &frame.buffer);
}

// ============================================================================
// Columns
// ============================================================================

#[test]
fn snapshot_columns_equal() {
    let columns = Columns::new()
        .add(Paragraph::new(Text::raw("Left")))
        .add(Paragraph::new(Text::raw("Center")))
        .add(Paragraph::new(Text::raw("Right")))
        .gap(1);

    let area = Rect::new(0, 0, 20, 3);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(20, 3, &mut pool);
    columns.render(area, &mut frame);
    assert_snapshot!("columns_equal", &frame.buffer);
}

#[test]
fn snapshot_columns_padding() {
    let columns = Columns::new()
        .add(Padding::new(
            Paragraph::new(Text::raw("Pad")),
            Sides::all(1),
        ))
        .add(Paragraph::new(Text::raw("Plain")))
        .gap(1);

    let area = Rect::new(0, 0, 17, 5);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(17, 5, &mut pool);
    columns.render(area, &mut frame);
    assert_snapshot!("columns_padding", &frame.buffer);
}

// ============================================================================
// Table
// ============================================================================

#[test]
fn snapshot_table_theme_aurora_header_zebra() {
    let theme = TableTheme::aurora();
    let phase = 0.0;
    let table = Table::new(
        [
            Row::new(["Mercury", "0.39", "rocky"]),
            Row::new(["Venus", "0.72", "rocky"]),
            Row::new(["Earth", "1.00", "life"]),
        ],
        [
            Constraint::Fixed(10),
            Constraint::Fixed(6),
            Constraint::Fixed(6),
        ],
    )
    .header(Row::new(["Name", "AU", "Class"]))
    .block(
        Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded),
    )
    .theme(theme.clone())
    .theme_phase(phase)
    .column_spacing(1);

    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(32, 7, &mut pool);
    Widget::render(&table, Rect::new(0, 0, 32, 7), &mut frame);
    assert_table_snapshot_ansi(
        "table_theme_aurora_header_zebra_phase_0_00",
        &frame.buffer,
        &theme,
        phase,
    );
}

#[test]
fn snapshot_table_theme_graphite_selected_hover() {
    let theme = TableTheme::graphite();
    let phase = 0.0;
    let table = Table::new(
        [
            Row::new(["Alpha", "A"]),
            Row::new(["Beta", "B"]),
            Row::new(["Gamma", "C"]),
        ],
        [Constraint::Fixed(10), Constraint::Fixed(6)],
    )
    .theme(theme.clone())
    .theme_phase(phase)
    .column_spacing(1);

    let mut state = TableState::default();
    state.selected = Some(1);
    state.hovered = Some(2);

    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(20, 4, &mut pool);
    StatefulWidget::render(&table, Rect::new(0, 0, 20, 4), &mut frame, &mut state);
    assert_table_snapshot_ansi(
        "table_theme_graphite_selected_hover_phase_0_00",
        &frame.buffer,
        &theme,
        phase,
    );
}

#[test]
fn snapshot_table_theme_pulse_phase() {
    let theme = TableTheme::aurora().with_effect(TableEffectRule::new(
        TableEffectTarget::Row(1),
        TableEffect::Pulse {
            fg_a: PackedRgba::rgb(240, 245, 255),
            fg_b: PackedRgba::rgb(255, 255, 255),
            bg_a: PackedRgba::rgb(24, 36, 54),
            bg_b: PackedRgba::rgb(60, 90, 140),
            speed: 1.0,
            phase_offset: 0.0,
        },
    ));
    let phase = 0.25;
    let table = Table::new(
        [
            Row::new(["Cold", "0.2"]),
            Row::new(["Warm", "0.6"]),
            Row::new(["Hot", "0.9"]),
        ],
        [Constraint::Fixed(8), Constraint::Fixed(6)],
    )
    .theme(theme.clone())
    .theme_phase(phase)
    .column_spacing(1);

    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(20, 4, &mut pool);
    Widget::render(&table, Rect::new(0, 0, 20, 4), &mut frame);
    assert_table_snapshot_ansi(
        "table_theme_aurora_pulse_phase_0_25",
        &frame.buffer,
        &theme,
        phase,
    );
}

#[test]
fn snapshot_table_layout_spacing_padding_row_height() {
    let theme = TableTheme::terminal_classic()
        .with_padding(1)
        .with_column_gap(2)
        .with_row_height(2);
    let column_gap = theme.column_gap;
    let row_height = theme.row_height;
    let pad = " ".repeat(theme.padding as usize);
    let padded = |text: &str| Text::raw(format!("{pad}{text}{pad}"));

    let table = Table::new(
        [
            Row::new([
                Text::raw(format!("{pad}ID{pad}\n{pad}--{pad}")),
                Text::raw(format!("{pad}Name{pad}\n{pad}----{pad}")),
            ])
            .height(row_height as u16),
            Row::new([padded("01"), padded("Alice")]).height(row_height as u16),
        ],
        [Constraint::Fixed(8), Constraint::Fixed(10)],
    )
    .theme(theme)
    .theme_phase(0.0)
    .column_spacing(column_gap as u16);

    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(22, 5, &mut pool);
    Widget::render(&table, Rect::new(0, 0, 22, 5), &mut frame);
    assert_snapshot!("table_layout_padding_row_height", &frame.buffer);
}

#[test]
fn snapshot_table_fit_content_wide_glyphs() {
    let table = Table::new(
        [
            Row::new(["ID", "Label"]),
            Row::new(["🦀", "Rustacean"]),
            Row::new(["漢", "漢字"]),
        ],
        [Constraint::FitContent, Constraint::FitContent],
    )
    .column_spacing(2);

    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(24, 4, &mut pool);
    Widget::render(&table, Rect::new(0, 0, 24, 4), &mut frame);
    assert_snapshot!("table_fit_content_wide_glyphs", &frame.buffer);
}

// ============================================================================
// Raw Buffer
// ============================================================================

#[test]
fn snapshot_raw_buffer_pattern() {
    let mut buf = Buffer::new(8, 4);
    // Checkerboard pattern
    for y in 0..4u16 {
        for x in 0..8u16 {
            if (x + y) % 2 == 0 {
                buf.set(x, y, Cell::from_char('#'));
            } else {
                buf.set(x, y, Cell::from_char('.'));
            }
        }
    }
    assert_snapshot!("raw_checkerboard", &buf);
}

// ============================================================================
// Panel
// ============================================================================

#[test]
fn snapshot_panel_square() {
    let child = Paragraph::new(Text::raw("Inner"));
    let panel = Panel::new(child)
        .title("Panel")
        .padding(ftui_core::geometry::Sides::all(1));
    let area = Rect::new(0, 0, 14, 7);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(14, 7, &mut pool);
    panel.render(area, &mut frame);
    assert_snapshot!("panel_square", &frame.buffer);
}

#[test]
fn snapshot_panel_rounded_with_subtitle() {
    let child = Paragraph::new(Text::raw("Hello"));
    let panel = Panel::new(child)
        .border_type(BorderType::Rounded)
        .title("Top")
        .subtitle("Bottom")
        .title_alignment(Alignment::Center)
        .subtitle_alignment(Alignment::Center)
        .padding(ftui_core::geometry::Sides::all(1));
    let area = Rect::new(0, 0, 16, 7);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(16, 7, &mut pool);
    panel.render(area, &mut frame);
    assert_snapshot!("panel_rounded_subtitle", &frame.buffer);
}

#[test]
fn snapshot_panel_ascii_borders() {
    let child = Paragraph::new(Text::raw("ASCII"));
    let panel = Panel::new(child)
        .border_type(BorderType::Ascii)
        .title("Box")
        .padding(ftui_core::geometry::Sides::all(1));
    let area = Rect::new(0, 0, 12, 5);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(12, 5, &mut pool);
    panel.render(area, &mut frame);
    assert_snapshot!("panel_ascii", &frame.buffer);
}

#[test]
fn snapshot_panel_title_truncates_with_ellipsis() {
    let child = Paragraph::new(Text::raw("X"));
    let panel = Panel::new(child)
        .border_type(BorderType::Square)
        .title("VeryLongTitle")
        .padding(ftui_core::geometry::Sides::all(0));
    let area = Rect::new(0, 0, 10, 3);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(10, 3, &mut pool);
    panel.render(area, &mut frame);
    assert_snapshot!("panel_title_ellipsis", &frame.buffer);
}

// ============================================================================
// Modal
// ============================================================================

#[test]
fn snapshot_modal_center_80x24() {
    let content = Paragraph::new(Text::raw("Modal Content"))
        .block(Block::default().borders(Borders::ALL).title("Dialog"));
    let modal = Modal::new(content).size(
        ModalSizeConstraints::new()
            .min_width(20)
            .max_width(20)
            .min_height(5)
            .max_height(5),
    );
    let area = Rect::new(0, 0, 80, 24);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    modal.render(area, &mut frame);
    assert_snapshot!("modal_center_80x24", &frame.buffer);
}

#[test]
fn snapshot_modal_offset_80x24() {
    let content = Paragraph::new(Text::raw("Offset Modal"))
        .block(Block::default().borders(Borders::ALL).title("Offset"));
    let modal = Modal::new(content)
        .size(
            ModalSizeConstraints::new()
                .min_width(16)
                .max_width(16)
                .min_height(4)
                .max_height(4),
        )
        .position(ModalPosition::CenterOffset { x: -10, y: -3 });
    let area = Rect::new(0, 0, 80, 24);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(80, 24, &mut pool);
    modal.render(area, &mut frame);
    assert_snapshot!("modal_offset_80x24", &frame.buffer);
}

#[test]
fn snapshot_modal_constrained_120x40() {
    let content = Paragraph::new(Text::raw("Constrained\nWith max size"))
        .block(Block::default().borders(Borders::ALL).title("Constrained"));
    let modal = Modal::new(content).size(
        ModalSizeConstraints::new()
            .min_width(10)
            .max_width(30)
            .min_height(3)
            .max_height(8),
    );
    let area = Rect::new(0, 0, 120, 40);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(120, 40, &mut pool);
    modal.render(area, &mut frame);
    assert_snapshot!("modal_constrained_120x40", &frame.buffer);
}

#[test]
fn snapshot_modal_backdrop_opacity() {
    // Fill background with pattern to show backdrop effect
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(40, 12, &mut pool);
    for y in 0..12u16 {
        for x in 0..40u16 {
            frame.buffer.set(
                x,
                y,
                Cell::from_char(if (x + y) % 2 == 0 { '#' } else { '.' }),
            );
        }
    }

    let content =
        Paragraph::new(Text::raw("Content")).block(Block::default().borders(Borders::ALL));
    let modal = Modal::new(content)
        .size(
            ModalSizeConstraints::new()
                .min_width(12)
                .max_width(12)
                .min_height(4)
                .max_height(4),
        )
        .backdrop(BackdropConfig::new(
            ftui_render::cell::PackedRgba::rgb(0, 0, 0),
            0.8,
        ));
    let area = Rect::new(0, 0, 40, 12);
    modal.render(area, &mut frame);
    assert_snapshot!("modal_backdrop_opacity", &frame.buffer);
}

// ============================================================================
// UI Inspector
// ============================================================================

#[test]
fn snapshot_inspector_hit_regions_with_panel() {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::with_hit_grid(50, 16, &mut pool);
    let area = Rect::new(0, 0, 50, 16);

    let shell = Block::new().title(" Inspector Demo ").borders(Borders::ALL);
    let inner = shell.inner(area);
    shell.render(area, &mut frame);

    let content =
        Paragraph::new(Text::raw("Hit regions\n- Button\n- Content")).alignment(Alignment::Left);
    content.render(inner, &mut frame);

    let button_hit = Rect::new(inner.x + 2, inner.y + 2, 12, 3);
    let list_hit = Rect::new(inner.x + 2, inner.y + 6, 18, 2);

    frame.register_hit(button_hit, HitId::new(1), HitRegion::Button, 7);
    frame.register_hit(list_hit, HitId::new(2), HitRegion::Content, 0);

    let mut state = InspectorState::new();
    state.mode = InspectorMode::HitRegions;
    state.show_detail_panel = true;
    state.selected = Some(HitId::new(1));
    state.hover_pos = Some((list_hit.x + 1, list_hit.y));

    InspectorOverlay::new(&state).render(area, &mut frame);

    assert_snapshot_ansi!("inspector_hit_regions_with_panel", &frame.buffer);
}

#[test]
fn snapshot_inspector_widget_bounds_tree() {
    let mut pool = GraphemePool::new();
    let mut frame = Frame::with_hit_grid(60, 18, &mut pool);
    let area = Rect::new(0, 0, 60, 18);

    let shell = Block::new().title(" Layout ").borders(Borders::ALL);
    shell.render(area, &mut frame);

    let mut state = InspectorState::new();
    state.mode = InspectorMode::WidgetBounds;

    let mut root = WidgetInfo::new("Root", Rect::new(1, 1, 58, 16)).with_depth(0);
    let mut left = WidgetInfo::new("LeftPane", Rect::new(2, 2, 26, 12))
        .with_depth(1)
        .with_hit_id(HitId::new(10));
    let right = WidgetInfo::new("RightPane", Rect::new(31, 2, 26, 12))
        .with_depth(1)
        .with_hit_id(HitId::new(11));
    left.add_child(WidgetInfo::new("SearchBox", Rect::new(4, 4, 20, 3)).with_depth(2));
    left.add_child(WidgetInfo::new("Results", Rect::new(4, 8, 20, 5)).with_depth(2));

    root.add_child(left);
    root.add_child(right);
    state.register_widget(root);

    InspectorOverlay::new(&state).render(area, &mut frame);

    assert_snapshot_ansi!("inspector_widget_bounds_tree", &frame.buffer);
}

#[test]
fn inspector_overlay_stress_perf() {
    let cols: u16 = 24;
    let rows: u16 = 14;
    let max_depth: u8 = 3;
    let area = Rect::new(0, 0, 160, 56);

    let mut pool = GraphemePool::new();
    let mut frame = Frame::with_hit_grid(area.width, area.height, &mut pool);

    let mut state = InspectorState::new();
    state.mode = InspectorMode::Full;
    state.show_detail_panel = true;

    let cell_width = (area.width / cols).max(1);
    let cell_height = (area.height / rows).max(1);
    let mut root = WidgetInfo::new("StressRoot", area).with_depth(0);
    let mut id_counter: u32 = 1;

    for row in 0..rows {
        let y = area.y.saturating_add(row.saturating_mul(cell_height));
        if y >= area.bottom() {
            break;
        }
        let height = area.bottom().saturating_sub(y).min(cell_height);
        if height == 0 {
            continue;
        }

        for col in 0..cols {
            let x = area.x.saturating_add(col.saturating_mul(cell_width));
            if x >= area.right() {
                break;
            }
            let width = area.right().saturating_sub(x).min(cell_width);
            if width == 0 {
                continue;
            }

            let rect = Rect::new(x, y, width, height);
            let mut widget = build_inspector_chain(format!("Cell {col},{row}"), rect, 1, max_depth);
            widget.hit_id = Some(HitId::new(id_counter));

            frame.register_hit(
                rect,
                HitId::new(id_counter),
                HitRegion::Content,
                u64::from(id_counter),
            );

            if id_counter == 1 {
                state.selected = Some(HitId::new(id_counter));
                state.hover_pos = Some((
                    rect.x.saturating_add(rect.width / 2),
                    rect.y.saturating_add(rect.height / 2),
                ));
            }

            root.add_child(widget);
            id_counter = id_counter.saturating_add(1);
        }
    }

    state.register_widget(root);

    let overlay = InspectorOverlay::new(&state);
    let start = Instant::now();
    overlay.render(area, &mut frame);
    let duration = start.elapsed();

    let budget_ms = std::env::var("INSPECTOR_PERF_BUDGET_MS")
        .ok()
        .and_then(|value| value.parse::<u128>().ok())
        .unwrap_or(50);
    let budget_ms = u64::try_from(budget_ms).unwrap_or(u64::MAX);
    let budget = Duration::from_millis(budget_ms);

    log_inspector_perf(
        "inspector_overlay_stress",
        cols,
        rows,
        max_depth,
        duration,
        budget,
    );

    assert!(
        duration <= budget,
        "Inspector overlay stress render took {:?}, budget {:?}",
        duration,
        budget
    );
}

fn build_inspector_chain(name: String, area: Rect, depth: u8, max_depth: u8) -> WidgetInfo {
    let mut widget = WidgetInfo::new(name, area).with_depth(depth);

    if depth < max_depth {
        let next_depth = depth.saturating_add(1);
        let child_area = Rect::new(
            area.x.saturating_add(1),
            area.y.saturating_add(1),
            area.width.saturating_sub(2),
            area.height.saturating_sub(2),
        );
        if !child_area.is_empty() {
            let child = build_inspector_chain(
                format!("Depth {}", next_depth),
                child_area,
                next_depth,
                max_depth,
            );
            widget.add_child(child);
        }
    }

    widget
}

fn log_inspector_perf(
    case: &str,
    cols: u16,
    rows: u16,
    max_depth: u8,
    duration: Duration,
    budget: Duration,
) {
    if std::env::var("INSPECTOR_PERF_LOG").is_ok() || std::env::var("PERF_LOG").is_ok() {
        let duration_us = duration.as_micros();
        let budget_us = budget.as_micros();
        let result = if duration <= budget { "pass" } else { "fail" };
        println!(
            r#"{{"event":"perf_test","case":"{}","cols":{},"rows":{},"depth":{},"duration_us":{},"budget_us":{},"result":"{}"}}"#,
            case, cols, rows, max_depth, duration_us, budget_us, result
        );
    }
}

// ============================================================================
// Command Palette
// ============================================================================

#[test]
fn snapshot_palette_empty() {
    let mut palette = CommandPalette::new();
    palette.open();

    let area = Rect::new(0, 0, 60, 10);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(60, 10, &mut pool);

    // Fill background to see overlay clearly
    for y in 0..10u16 {
        for x in 0..60u16 {
            frame.buffer.set(x, y, Cell::from_char('.'));
        }
    }

    palette.render(area, &mut frame);
    assert_snapshot!("palette_empty", &frame.buffer);
}

#[test]
fn snapshot_palette_results() {
    let mut palette = CommandPalette::new();
    palette.register("Open File", Some("Open a file"), &[]);
    palette.register("Save File", Some("Save current file"), &[]);
    palette.open();

    // Simulate typing "file"
    for ch in "file".chars() {
        let k = Event::Key(KeyEvent {
            code: KeyCode::Char(ch),
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        let _ = palette.handle_event(&k);
    }

    let area = Rect::new(0, 0, 60, 10);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(60, 10, &mut pool);
    palette.render(area, &mut frame);
    assert_snapshot!("palette_results", &frame.buffer);
}

#[test]
fn snapshot_palette_long_list() {
    let mut palette = CommandPalette::new();
    for i in 0..20 {
        palette.register(format!("Action {:02}", i), None, &[]);
    }
    palette.open();

    // Select item 5 to show scrolling/selection
    for _ in 0..5 {
        let down = Event::Key(KeyEvent {
            code: KeyCode::Down,
            modifiers: Modifiers::empty(),
            kind: KeyEventKind::Press,
        });
        let _ = palette.handle_event(&down);
    }

    let area = Rect::new(0, 0, 40, 10); // Narrower/shorter to force scroll
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(40, 10, &mut pool);
    palette.render(area, &mut frame);
    assert_snapshot!("palette_long_list", &frame.buffer);
}

#[test]
fn snapshot_palette_no_results() {
    let mut palette = CommandPalette::new();
    palette.register("Alpha", None, &[]);
    palette.open();

    // Type "z" (no match)
    let z = Event::Key(KeyEvent {
        code: KeyCode::Char('z'),
        modifiers: Modifiers::empty(),
        kind: KeyEventKind::Press,
    });
    let _ = palette.handle_event(&z);

    let area = Rect::new(0, 0, 60, 10);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(60, 10, &mut pool);
    palette.render(area, &mut frame);
    assert_snapshot!("palette_no_results", &frame.buffer);
}

// ============================================================================
// List: ANSI selection snapshot (bd-iuvb.17.3)
// ============================================================================

#[test]
fn snapshot_list_selection_styled_ansi() {
    let items = vec![
        ListItem::new("Apple"),
        ListItem::new("Banana"),
        ListItem::new("Cherry"),
    ];
    let list = List::new(items)
        .highlight_symbol("> ")
        .highlight_style(Style::new().bold());
    let area = Rect::new(0, 0, 14, 3);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(14, 3, &mut pool);
    let mut state = ListState::default();
    state.select(Some(1));
    StatefulWidget::render(&list, area, &mut frame, &mut state);
    assert_snapshot_ansi!("list_selection_styled", &frame.buffer);
}

// ============================================================================
// List: ANSI hover + selection snapshot (bd-iuvb.17.3)
// ============================================================================

#[test]
fn snapshot_list_selected_hover_ansi() {
    let items = vec![
        ListItem::new("One"),
        ListItem::new("Two"),
        ListItem::new("Three"),
    ];
    let list = List::new(items)
        .highlight_symbol(">")
        .highlight_style(Style::new().bold())
        .hover_style(Style::new().reverse());
    let area = Rect::new(0, 0, 12, 3);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::new(12, 3, &mut pool);
    let mut state = ListState::default();
    state.select(Some(1));
    state.hovered = Some(2);
    StatefulWidget::render(&list, area, &mut frame, &mut state);
    assert_snapshot_ansi!("list_selected_hover", &frame.buffer);
}

// ============================================================================
// List: hit region with selection (bd-iuvb.17.3)
// ============================================================================

#[test]
fn snapshot_list_with_hit_id_and_selection() {
    let items = vec![
        ListItem::new("First"),
        ListItem::new("Second"),
        ListItem::new("Third"),
    ];
    let list = List::new(items)
        .highlight_symbol("> ")
        .hit_id(HitId::new(42));
    let area = Rect::new(0, 0, 14, 3);
    let mut pool = GraphemePool::new();
    let mut frame = Frame::with_hit_grid(14, 3, &mut pool);
    let mut state = ListState::default();
    state.select(Some(0));
    StatefulWidget::render(&list, area, &mut frame, &mut state);

    // Verify snapshot
    assert_snapshot!("list_hit_id_selected", &frame.buffer);

    // Verify hit regions are registered
    for y in 0..3u16 {
        let hit = frame.hit_test(0, y).expect("expected hit");
        assert_eq!(hit.0, HitId::new(42));
        assert_eq!(hit.2, y as u64, "hit data should encode row index");
    }
}
