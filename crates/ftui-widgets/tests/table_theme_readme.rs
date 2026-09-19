use ftui_style::color::Color;
use ftui_style::{Style, TableTheme};

#[test]
fn readme_table_theme_snippet_compiles() {
    let _theme = TableTheme::modern()
        .with_stripe_period(2)
        .with_header_style(Style::new().bold().fg(Color::Cyan))
        .with_selection_style(Style::new().bg(Color::DarkGray));
}
