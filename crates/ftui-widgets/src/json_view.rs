//! JSON view widget for pretty-printing JSON text.
//!
//! Renders formatted JSON with indentation and optional syntax highlighting.
//! Does not depend on serde; operates on raw JSON strings with a minimal
//! tokenizer.
//!
//! # Example
//!
//! ```
//! use ftui_widgets::json_view::JsonView;
//!
//! let json = r#"{"name": "Alice", "age": 30}"#;
//! let view = JsonView::new(json);
//! let lines = view.formatted_lines();
//! assert!(lines.len() > 1); // Pretty-printed across multiple lines
//! ```

use std::collections::HashSet;

use crate::{StatefulWidget, Widget, draw_text_span};
use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind};
use ftui_core::geometry::Rect;
use ftui_render::frame::Frame;
use ftui_style::Style;

/// A classified JSON token for rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonToken {
    /// Object key (string before colon).
    Key(String),
    /// String value.
    StringVal(String),
    /// Number value.
    Number(String),
    /// Boolean or null literal.
    Literal(String),
    /// Structural character: `{`, `}`, `[`, `]`, `:`, `,`.
    Punctuation(String),
    /// Whitespace / indentation.
    Whitespace(String),
    /// Newline.
    Newline,
    /// Error text (invalid JSON portion).
    Error(String),
}

/// One segment of a [`JsonPath`]: an object key or an array index.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum JsonPathSegment {
    /// Object member key.
    Key(String),
    /// Array element index.
    Index(usize),
}

/// A stable path to a foldable node, from the root (empty path) down. Object
/// members contribute a [`JsonPathSegment::Key`], array elements a
/// [`JsonPathSegment::Index`]. Folds are keyed by path so they survive a
/// [`set_source`](JsonView::set_source) that leaves the node in place.
pub type JsonPath = Vec<JsonPathSegment>;

/// A rendered line produced by [`JsonView::lines_with_state`]: its tokens plus
/// the fold metadata needed to draw and navigate the tree.
#[derive(Debug, Clone)]
pub struct FormattedLine {
    /// The tokens to draw (already the fold placeholder for a folded node).
    pub tokens: Vec<JsonToken>,
    /// The path of the foldable node this line opens, or `None` for a leaf or
    /// closing line.
    pub path: Option<JsonPath>,
    /// Whether this line opens a foldable object/array node.
    pub foldable: bool,
    /// Whether this line is currently rendered folded (a placeholder).
    pub folded: bool,
    /// Nesting depth (0 at the root).
    pub depth: usize,
}

/// Internal annotation of one line of [`JsonView::formatted_lines`].
#[derive(Debug, Clone)]
struct LineInfo {
    tokens: Vec<JsonToken>,
    path: Option<JsonPath>,
    foldable: bool,
    depth: usize,
    /// Index of the matching closing line (for a foldable opener).
    close_line: usize,
    /// Number of direct children (for the folded placeholder count).
    child_count: usize,
    /// Whether the closing line of this node carries a trailing comma.
    trailing_comma: bool,
}

/// Fold state and cursor for a [`JsonView`] rendered as a [`StatefulWidget`].
///
/// Owned by the caller (like `List`/`Table` state). Folds are keyed by
/// [`JsonPath`]. After each render the visible foldable paths are cached so
/// [`handle_event`](JsonViewState::handle_event) can resolve the cursor line to
/// a node without borrowing the view.
#[derive(Debug, Clone, Default)]
pub struct JsonViewState {
    folded: HashSet<JsonPath>,
    /// The highlighted line (index into the visible lines).
    pub cursor_line: usize,
    /// First visible line (vertical scroll offset).
    pub scroll: usize,
    /// Foldable path per visible line, cached on render for `handle_event`.
    visible_paths: Vec<Option<JsonPath>>,
}

impl JsonViewState {
    /// A fresh state with nothing folded and the cursor at the top.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Toggle the fold state of `path`.
    pub fn toggle(&mut self, path: &JsonPath) {
        if !self.folded.remove(path) {
            self.folded.insert(path.clone());
        }
    }

    /// Fold `path`.
    pub fn fold(&mut self, path: &JsonPath) {
        self.folded.insert(path.clone());
    }

    /// Unfold `path`.
    pub fn unfold(&mut self, path: &JsonPath) {
        self.folded.remove(path);
    }

    /// Whether `path` is folded.
    #[must_use]
    pub fn is_folded(&self, path: &JsonPath) -> bool {
        self.folded.contains(path)
    }

    /// Fold every foldable node in `view`.
    pub fn fold_all(&mut self, view: &JsonView) {
        for info in view.line_infos() {
            if info.foldable
                && let Some(path) = info.path
            {
                self.folded.insert(path);
            }
        }
    }

    /// Unfold everything.
    pub fn unfold_all(&mut self) {
        self.folded.clear();
    }

    /// Number of folded nodes.
    #[must_use]
    pub fn folded_count(&self) -> usize {
        self.folded.len()
    }

    /// Handle a key event against the last rendered layout: `Enter`/`Space`
    /// toggle the node on the cursor line (no-op on a leaf), `Up`/`Down` move
    /// the cursor, `PageUp`/`PageDown` move by ten, `Home`/`End` jump to the
    /// ends. Returns whether the state changed. Requires a prior render to have
    /// cached the visible layout.
    pub fn handle_event(&mut self, event: &Event) -> bool {
        let Event::Key(KeyEvent { code, kind, .. }) = event else {
            return false;
        };
        if *kind == KeyEventKind::Release {
            return false;
        }
        let count = self.visible_paths.len();
        if count == 0 {
            return false;
        }
        let last = count - 1;
        match code {
            KeyCode::Enter | KeyCode::Char(' ') => {
                if let Some(Some(path)) = self.visible_paths.get(self.cursor_line) {
                    let path = path.clone();
                    self.toggle(&path);
                    true
                } else {
                    false
                }
            }
            KeyCode::Up => {
                let next = self.cursor_line.saturating_sub(1);
                self.set_cursor(next)
            }
            KeyCode::Down => {
                let next = (self.cursor_line + 1).min(last);
                self.set_cursor(next)
            }
            KeyCode::PageUp => {
                let next = self.cursor_line.saturating_sub(10);
                self.set_cursor(next)
            }
            KeyCode::PageDown => {
                let next = (self.cursor_line + 10).min(last);
                self.set_cursor(next)
            }
            KeyCode::Home => self.set_cursor(0),
            KeyCode::End => self.set_cursor(last),
            _ => false,
        }
    }

    fn set_cursor(&mut self, line: usize) -> bool {
        if line == self.cursor_line {
            false
        } else {
            self.cursor_line = line;
            true
        }
    }
}

/// Widget that renders pretty-printed JSON with syntax coloring.
#[derive(Debug, Clone)]
pub struct JsonView {
    source: String,
    indent: usize,
    key_style: Style,
    string_style: Style,
    number_style: Style,
    literal_style: Style,
    punct_style: Style,
    error_style: Style,
    cursor_style: Style,
}

impl Default for JsonView {
    fn default() -> Self {
        Self::new("")
    }
}

impl JsonView {
    /// Create a new JSON view from a raw JSON string.
    #[must_use]
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            indent: 2,
            key_style: Style::new().bold(),
            string_style: Style::default(),
            number_style: Style::default(),
            literal_style: Style::default(),
            punct_style: Style::default(),
            error_style: Style::default(),
            cursor_style: Style::new().reverse(),
        }
    }

    /// Set the indentation width.
    #[must_use]
    pub fn with_indent(mut self, indent: usize) -> Self {
        self.indent = indent;
        self
    }

    /// Set style for object keys.
    #[must_use]
    pub fn with_key_style(mut self, style: Style) -> Self {
        self.key_style = style;
        self
    }

    /// Set style for string values.
    #[must_use]
    pub fn with_string_style(mut self, style: Style) -> Self {
        self.string_style = style;
        self
    }

    /// Set style for numbers.
    #[must_use]
    pub fn with_number_style(mut self, style: Style) -> Self {
        self.number_style = style;
        self
    }

    /// Set style for boolean/null literals.
    #[must_use]
    pub fn with_literal_style(mut self, style: Style) -> Self {
        self.literal_style = style;
        self
    }

    /// Set style for punctuation.
    #[must_use]
    pub fn with_punct_style(mut self, style: Style) -> Self {
        self.punct_style = style;
        self
    }

    /// Set style for error text.
    #[must_use]
    pub fn with_error_style(mut self, style: Style) -> Self {
        self.error_style = style;
        self
    }

    /// Set the style for the highlighted cursor line in the stateful
    /// (foldable) rendering. Defaults to reverse video.
    #[must_use]
    pub fn with_cursor_style(mut self, style: Style) -> Self {
        self.cursor_style = style;
        self
    }

    /// Set the source JSON.
    pub fn set_source(&mut self, source: impl Into<String>) {
        self.source = source.into();
    }

    /// Get the source JSON.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Pretty-format the JSON into lines of tokens for rendering.
    #[must_use]
    pub fn formatted_lines(&self) -> Vec<Vec<JsonToken>> {
        let trimmed = self.source.trim();
        if trimmed.is_empty() {
            return vec![];
        }

        let mut lines: Vec<Vec<JsonToken>> = Vec::new();
        let mut current_line: Vec<JsonToken> = Vec::new();
        let mut depth: usize = 0;
        let mut chars = trimmed.chars().peekable();

        while let Some(&ch) = chars.peek() {
            match ch {
                '{' | '[' => {
                    chars.next();
                    current_line.push(JsonToken::Punctuation(ch.to_string()));
                    // Check if next non-whitespace is closing bracket
                    skip_ws(&mut chars);
                    let next = chars.peek().copied();
                    if next == Some('}') || next == Some(']') {
                        // Empty object/array
                        let closing = chars.next().unwrap();
                        current_line.push(JsonToken::Punctuation(closing.to_string()));
                        // Check for comma
                        skip_ws(&mut chars);
                        if chars.peek() == Some(&',') {
                            chars.next();
                            current_line.push(JsonToken::Punctuation(",".to_string()));
                        }
                    } else {
                        depth += 1;
                        lines.push(current_line);
                        current_line = vec![JsonToken::Whitespace(make_indent(
                            depth.min(32),
                            self.indent,
                        ))];
                    }
                }
                '}' | ']' => {
                    chars.next();
                    depth = depth.saturating_sub(1);
                    lines.push(current_line);
                    current_line = vec![
                        JsonToken::Whitespace(make_indent(depth, self.indent)),
                        JsonToken::Punctuation(ch.to_string()),
                    ];
                    // Check for comma
                    skip_ws(&mut chars);
                    if chars.peek() == Some(&',') {
                        chars.next();
                        current_line.push(JsonToken::Punctuation(",".to_string()));
                    }
                }
                '"' => {
                    let s = read_string(&mut chars);
                    skip_ws(&mut chars);
                    if chars.peek() == Some(&':') {
                        // This is a key
                        current_line.push(JsonToken::Key(s));
                        chars.next();
                        current_line.push(JsonToken::Punctuation(": ".to_string()));
                        skip_ws(&mut chars);
                    } else {
                        current_line.push(JsonToken::StringVal(s));
                        // Check for comma
                        skip_ws(&mut chars);
                        if chars.peek() == Some(&',') {
                            chars.next();
                            current_line.push(JsonToken::Punctuation(",".to_string()));
                            lines.push(current_line);
                            current_line = vec![JsonToken::Whitespace(make_indent(
                                depth.min(32),
                                self.indent,
                            ))];
                        }
                    }
                }
                ',' => {
                    chars.next();
                    current_line.push(JsonToken::Punctuation(",".to_string()));
                    lines.push(current_line);
                    current_line = vec![JsonToken::Whitespace(make_indent(
                        depth.min(32),
                        self.indent,
                    ))];
                }
                ':' => {
                    chars.next();
                    current_line.push(JsonToken::Punctuation(": ".to_string()));
                    skip_ws(&mut chars);
                }
                ' ' | '\t' | '\r' | '\n' => {
                    chars.next();
                }
                _ => {
                    // Number, boolean, null, or error
                    let literal = read_literal(&mut chars);
                    let tok = classify_literal(&literal);
                    current_line.push(tok);
                    // Check for comma
                    skip_ws(&mut chars);
                    if chars.peek() == Some(&',') {
                        chars.next();
                        current_line.push(JsonToken::Punctuation(",".to_string()));
                        lines.push(current_line);
                        current_line = vec![JsonToken::Whitespace(make_indent(
                            depth.min(32),
                            self.indent,
                        ))];
                    }
                }
            }
        }

        if !current_line.is_empty() {
            lines.push(current_line);
        }

        lines
    }

    /// Annotate [`formatted_lines`](Self::formatted_lines) with fold metadata:
    /// each line's nesting depth, whether it opens a foldable object/array
    /// node, that node's [`JsonPath`], its matching closing line and its direct
    /// child count. Tokens are copied unchanged, so
    /// `lines_with_state(&JsonViewState::default())` reproduces the flat output
    /// exactly.
    fn line_infos(&self) -> Vec<LineInfo> {
        let lines = self.formatted_lines();

        struct Frame {
            child_count: usize,
            path: JsonPath,
            opener: usize,
        }

        let mut infos: Vec<LineInfo> = Vec::with_capacity(lines.len());
        let mut stack: Vec<Frame> = Vec::new();

        // The pretty-printer can put a closing bracket and the next sibling on
        // the same line (`},"k": {` or `},"k": 1`). Each line therefore may
        // close the innermost container (at its start) and then open or start a
        // new sibling (its rest). Both are handled here.
        for (i, tokens) in lines.into_iter().enumerate() {
            let closes = line_closes(&tokens);
            let opens = line_opens(&tokens);

            // A leading close pops and finalises the innermost container.
            if closes && let Some(frame) = stack.pop() {
                infos[frame.opener].close_line = i;
                infos[frame.opener].child_count = frame.child_count;
                infos[frame.opener].trailing_comma = line_close_has_comma(&tokens);
            }

            // Whatever remains on the line (an opener or a leaf value) is a new
            // direct child of the current parent frame; a bare closing line is
            // not.
            let contributes = opens || !closes || line_has_content_after_close(&tokens);
            let index = if contributes {
                stack.last().map(|f| f.child_count)
            } else {
                None
            };
            if contributes && let Some(frame) = stack.last_mut() {
                frame.child_count += 1;
            }

            let depth = stack.len();

            if opens {
                let mut path = stack.last().map(|f| f.path.clone()).unwrap_or_default();
                if let Some(key) = line_key(&tokens) {
                    path.push(JsonPathSegment::Key(key));
                } else if let Some(idx) = index {
                    path.push(JsonPathSegment::Index(idx));
                }
                // (no segment for the root container: its path stays empty)
                infos.push(LineInfo {
                    tokens,
                    path: Some(path.clone()),
                    foldable: true,
                    depth,
                    close_line: i,
                    child_count: 0,
                    trailing_comma: false,
                });
                stack.push(Frame {
                    child_count: 0,
                    path,
                    opener: i,
                });
            } else {
                infos.push(LineInfo {
                    tokens,
                    path: None,
                    foldable: false,
                    depth,
                    close_line: i,
                    child_count: 0,
                    trailing_comma: false,
                });
            }
        }

        // Unclosed containers (malformed JSON) are not foldable.
        for frame in stack {
            infos[frame.opener].foldable = false;
            infos[frame.opener].path = None;
        }

        infos
    }

    /// Format the JSON into display lines honouring `state`'s fold set: a folded
    /// node collapses to a single placeholder line (`key: ▸ {…} (n keys)` /
    /// `▸ […] (n items)`) and its descendants and closing line are hidden. With
    /// the default (empty) state this equals [`formatted_lines`](Self::formatted_lines).
    #[must_use]
    pub fn lines_with_state(&self, state: &JsonViewState) -> Vec<FormattedLine> {
        let infos = self.line_infos();
        let mut out = Vec::with_capacity(infos.len());
        let mut i = 0;
        // When the previous folded node's closing bracket shares a line with the
        // next sibling, that sibling line is emitted with the leading close
        // stripped (the bracket and its comma are already in the placeholder).
        let mut strip = false;
        while i < infos.len() {
            let info = &infos[i];
            let tokens = if strip {
                strip_close_prefix(&info.tokens)
            } else {
                info.tokens.clone()
            };
            strip = false;

            if info.foldable
                && let Some(path) = &info.path
                && state.is_folded(path)
            {
                out.push(placeholder_from_tokens(tokens, info));
                let close = info.close_line;
                if close > i && line_has_content_after_close(&infos[close].tokens) {
                    // The closing line also starts a sibling: strip its close.
                    i = close;
                    strip = true;
                } else {
                    // A bare closing line is fully represented by the placeholder.
                    i = close.max(i) + 1;
                }
                continue;
            }

            out.push(FormattedLine {
                tokens,
                path: info.path.clone(),
                foldable: info.foldable,
                folded: false,
                depth: info.depth,
            });
            i += 1;
        }
        out
    }

    /// Draw one line's tokens at row `y`, optionally highlighting it as the
    /// cursor line.
    fn draw_line(
        &self,
        frame: &mut Frame,
        area: Rect,
        y: u16,
        tokens: &[JsonToken],
        is_cursor: bool,
        styling: bool,
    ) {
        let max_x = area.right();
        let mut x = area.x;
        for token in tokens {
            let (text, style) = match token {
                JsonToken::Key(s) => (s.as_str(), self.key_style),
                JsonToken::StringVal(s) => (s.as_str(), self.string_style),
                JsonToken::Number(s) => (s.as_str(), self.number_style),
                JsonToken::Literal(s) => (s.as_str(), self.literal_style),
                JsonToken::Punctuation(s) => (s.as_str(), self.punct_style),
                JsonToken::Whitespace(s) => (s.as_str(), Style::default()),
                JsonToken::Error(s) => (s.as_str(), self.error_style),
                JsonToken::Newline => continue,
            };
            let style = if styling { style } else { Style::default() };
            x = draw_text_span(frame, x, y, text, style, max_x);
        }
        if is_cursor && styling {
            for cx in area.x..area.right() {
                if let Some(cell) = frame.buffer.get_mut(cx, y) {
                    crate::apply_style(cell, self.cursor_style);
                }
            }
        }
    }
}

/// Build the placeholder line for a folded node from its (possibly
/// close-stripped) opener tokens: drop the opening bracket and append a
/// `▸ {…} (n keys)` / `▸ […] (n items)` marker, carrying the node's trailing
/// comma if it had one.
fn placeholder_from_tokens(mut tokens: Vec<JsonToken>, info: &LineInfo) -> FormattedLine {
    let is_array = matches!(tokens.last(), Some(JsonToken::Punctuation(p)) if p == "[");
    tokens.pop(); // drop the opening bracket
    let (open, close, unit) = if is_array {
        ('[', ']', "items")
    } else {
        ('{', '}', "keys")
    };
    let comma = if info.trailing_comma { "," } else { "" };
    let summary = format!("▸ {open}…{close} ({} {unit}){comma}", info.child_count);
    tokens.push(JsonToken::Punctuation(summary));
    FormattedLine {
        tokens,
        path: info.path.clone(),
        foldable: true,
        folded: true,
        depth: info.depth,
    }
}

fn line_opens(tokens: &[JsonToken]) -> bool {
    matches!(tokens.last(), Some(JsonToken::Punctuation(p)) if p == "{" || p == "[")
}

fn line_closes(tokens: &[JsonToken]) -> bool {
    for t in tokens {
        match t {
            JsonToken::Whitespace(_) => {}
            JsonToken::Punctuation(p) => return p == "}" || p == "]",
            _ => return false,
        }
    }
    false
}

fn line_key(tokens: &[JsonToken]) -> Option<String> {
    tokens.iter().find_map(|t| {
        if let JsonToken::Key(k) = t {
            let unquoted = k
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .unwrap_or(k);
            Some(unquoted.to_string())
        } else {
            None
        }
    })
}

/// Whether the leading closing bracket of `tokens` is followed by a comma
/// (`},` or `],`) — the folded node's trailing comma.
fn line_close_has_comma(tokens: &[JsonToken]) -> bool {
    let mut it = tokens
        .iter()
        .skip_while(|t| matches!(t, JsonToken::Whitespace(_)));
    match it.next() {
        Some(JsonToken::Punctuation(p)) if p == "}" || p == "]" => {
            matches!(it.next(), Some(JsonToken::Punctuation(c)) if c == ",")
        }
        _ => false,
    }
}

/// Whether, after any leading close bracket and comma, the line still carries a
/// sibling (a key or value) — i.e. the closing bracket shares the line with the
/// next member.
fn line_has_content_after_close(tokens: &[JsonToken]) -> bool {
    strip_close_prefix(tokens)
        .iter()
        .any(|t| !matches!(t, JsonToken::Whitespace(_)))
}

/// Remove a leading closing bracket (and its trailing comma) from a line,
/// preserving the indentation, so a merged `},"k": {` becomes `"k": {`.
fn strip_close_prefix(tokens: &[JsonToken]) -> Vec<JsonToken> {
    let mut result = Vec::with_capacity(tokens.len());
    let mut idx = 0;
    while let Some(JsonToken::Whitespace(_)) = tokens.get(idx) {
        result.push(tokens[idx].clone());
        idx += 1;
    }
    if let Some(JsonToken::Punctuation(p)) = tokens.get(idx)
        && (p == "}" || p == "]")
    {
        idx += 1;
        if let Some(JsonToken::Punctuation(c)) = tokens.get(idx)
            && c == ","
        {
            idx += 1;
        }
    }
    result.extend_from_slice(&tokens[idx..]);
    result
}

fn make_indent(depth: usize, width: usize) -> String {
    " ".repeat(depth * width)
}

fn skip_ws(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while let Some(&ch) = chars.peek() {
        if ch == ' ' || ch == '\t' || ch == '\r' || ch == '\n' {
            chars.next();
        } else {
            break;
        }
    }
}

fn read_string(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut s = String::new();
    s.push('"');
    chars.next(); // consume opening quote
    let mut escaped = false;
    for ch in chars.by_ref() {
        s.push(ch);
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            break;
        }
    }
    s
}

fn read_literal(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut s = String::new();
    while let Some(&ch) = chars.peek() {
        if ch == ','
            || ch == '}'
            || ch == ']'
            || ch == ':'
            || ch == ' '
            || ch == '\n'
            || ch == '\r'
            || ch == '\t'
        {
            break;
        }
        s.push(ch);
        chars.next();
    }
    s
}

fn classify_literal(s: &str) -> JsonToken {
    match s {
        "true" | "false" | "null" => JsonToken::Literal(s.to_string()),
        _ => {
            // Try as number
            if s.bytes().all(|b| {
                b.is_ascii_digit() || b == b'.' || b == b'-' || b == b'+' || b == b'e' || b == b'E'
            }) && !s.is_empty()
            {
                JsonToken::Number(s.to_string())
            } else {
                JsonToken::Error(s.to_string())
            }
        }
    }
}

impl Widget for JsonView {
    fn render(&self, area: Rect, frame: &mut Frame) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let deg = frame.buffer.degradation;
        frame.buffer.fill(area, ftui_render::cell::Cell::default());
        if !deg.render_content() {
            return;
        }
        let lines = self.formatted_lines();
        let max_x = area.right();

        for (row_idx, tokens) in lines.iter().enumerate() {
            if row_idx >= area.height as usize {
                break;
            }

            let y = area.y.saturating_add(row_idx as u16);
            let mut x = area.x;

            for token in tokens {
                let (text, style) = match token {
                    JsonToken::Key(s) => (s.as_str(), self.key_style),
                    JsonToken::StringVal(s) => (s.as_str(), self.string_style),
                    JsonToken::Number(s) => (s.as_str(), self.number_style),
                    JsonToken::Literal(s) => (s.as_str(), self.literal_style),
                    JsonToken::Punctuation(s) => (s.as_str(), self.punct_style),
                    JsonToken::Whitespace(s) => (s.as_str(), Style::default()),
                    JsonToken::Error(s) => (s.as_str(), self.error_style),
                    JsonToken::Newline => continue,
                };

                if deg.apply_styling() {
                    x = draw_text_span(frame, x, y, text, style, max_x);
                } else {
                    x = draw_text_span(frame, x, y, text, Style::default(), max_x);
                }
            }
        }
    }

    fn is_essential(&self) -> bool {
        false
    }
}

impl StatefulWidget for JsonView {
    type State = JsonViewState;

    fn render(&self, area: Rect, frame: &mut Frame, state: &mut JsonViewState) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let deg = frame.buffer.degradation;
        frame.buffer.fill(area, ftui_render::cell::Cell::default());
        if !deg.render_content() {
            state.visible_paths.clear();
            return;
        }

        #[cfg(feature = "tracing")]
        let _span = tracing::debug_span!(
            "widget_render",
            widget = "JsonView",
            folded_count = state.folded_count(),
        )
        .entered();

        let lines = self.lines_with_state(state);
        state.visible_paths = lines
            .iter()
            .map(|l| if l.foldable { l.path.clone() } else { None })
            .collect();

        let visible = lines.len();
        if visible == 0 {
            state.cursor_line = 0;
            state.scroll = 0;
            return;
        }
        if state.cursor_line >= visible {
            state.cursor_line = visible - 1;
        }
        // Keep the cursor inside the viewport.
        let height = area.height as usize;
        if state.cursor_line < state.scroll {
            state.scroll = state.cursor_line;
        } else if state.cursor_line >= state.scroll + height {
            state.scroll = state.cursor_line + 1 - height;
        }
        if state.scroll >= visible {
            state.scroll = visible - 1;
        }

        for row in 0..height {
            let idx = state.scroll + row;
            if idx >= visible {
                break;
            }
            let y = area.y + row as u16;
            self.draw_line(
                frame,
                area,
                y,
                &lines[idx].tokens,
                idx == state.cursor_line,
                deg.apply_styling(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::budget::DegradationLevel;
    use ftui_render::cell::{CellAttrs, PackedRgba};
    use ftui_render::frame::Frame;
    use ftui_render::grapheme_pool::GraphemePool;

    #[test]
    fn empty_source() {
        let view = JsonView::new("");
        assert!(view.formatted_lines().is_empty());
    }

    #[test]
    fn simple_object() {
        let view = JsonView::new(r#"{"a": 1}"#);
        let lines = view.formatted_lines();
        assert!(lines.len() >= 3); // { + content + }
    }

    #[test]
    fn nested_object() {
        let view = JsonView::new(r#"{"a": {"b": 2}}"#);
        let lines = view.formatted_lines();
        assert!(lines.len() >= 3);
    }

    #[test]
    fn array() {
        let view = JsonView::new(r#"[1, 2, 3]"#);
        let lines = view.formatted_lines();
        assert!(lines.len() >= 3);
    }

    #[test]
    fn empty_object() {
        let view = JsonView::new(r#"{}"#);
        let lines = view.formatted_lines();
        assert!(!lines.is_empty());
        // Should be compact: single line with {}
    }

    #[test]
    fn empty_array() {
        let view = JsonView::new(r#"[]"#);
        let lines = view.formatted_lines();
        assert!(!lines.is_empty());
    }

    #[test]
    fn string_values() {
        let view = JsonView::new(r#"{"msg": "hello world"}"#);
        let lines = view.formatted_lines();
        // Should contain StringVal token with quoted string
        let has_string = lines.iter().any(|line| {
            line.iter()
                .any(|t| matches!(t, JsonToken::StringVal(s) if s.contains("hello")))
        });
        assert!(has_string);
    }

    #[test]
    fn boolean_and_null() {
        let view = JsonView::new(r#"{"a": true, "b": false, "c": null}"#);
        let lines = view.formatted_lines();
        let has_literal = lines.iter().any(|line| {
            line.iter()
                .any(|t| matches!(t, JsonToken::Literal(s) if s == "true"))
        });
        assert!(has_literal);
    }

    #[test]
    fn numbers() {
        let view = JsonView::new(r#"{"x": 42, "y": -3.14}"#);
        let lines = view.formatted_lines();
        let has_number = lines.iter().any(|line| {
            line.iter()
                .any(|t| matches!(t, JsonToken::Number(s) if s == "42"))
        });
        assert!(has_number);
    }

    #[test]
    fn escaped_string() {
        let view = JsonView::new(r#"{"msg": "hello \"world\""}"#);
        let lines = view.formatted_lines();
        let has_escaped = lines.iter().any(|line| {
            line.iter()
                .any(|t| matches!(t, JsonToken::StringVal(s) if s.contains("\\\"")))
        });
        assert!(has_escaped);
    }

    #[test]
    fn indent_width() {
        let view = JsonView::new(r#"{"a": 1}"#).with_indent(4);
        let lines = view.formatted_lines();
        let has_4_indent = lines.iter().any(|line| {
            line.iter()
                .any(|t| matches!(t, JsonToken::Whitespace(s) if s == "    "))
        });
        assert!(has_4_indent);
    }

    #[test]
    fn render_basic() {
        let view = JsonView::new(r#"{"key": "value"}"#);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 10, &mut pool);
        let area = Rect::new(0, 0, 40, 10);
        Widget::render(&view, area, &mut frame);

        // First char should be '{'
        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell.content.as_char(), Some('{'));
    }

    #[test]
    fn render_zero_area() {
        let view = JsonView::new(r#"{"a": 1}"#);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 10, &mut pool);
        Widget::render(&view, Rect::new(0, 0, 0, 0), &mut frame); // No panic
    }

    #[test]
    fn render_truncated_height() {
        let view = JsonView::new(r#"{"a": 1, "b": 2, "c": 3}"#);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 2, &mut pool);
        let area = Rect::new(0, 0, 40, 2);
        Widget::render(&view, area, &mut frame); // Only first 2 lines, no panic
    }

    #[test]
    fn is_not_essential() {
        let view = JsonView::new("");
        assert!(!view.is_essential());
    }

    #[test]
    fn default_impl() {
        let view = JsonView::default();
        assert!(view.source().is_empty());
    }

    #[test]
    fn set_source() {
        let mut view = JsonView::new("");
        view.set_source(r#"{"a": 1}"#);
        assert!(!view.formatted_lines().is_empty());
    }

    #[test]
    fn plain_literal() {
        let view = JsonView::new("42");
        let lines = view.formatted_lines();
        assert_eq!(lines.len(), 1);
    }

    // ─── Edge-case tests (bd-2agoi) ────────────────────────────────────

    #[test]
    fn whitespace_only_source() {
        let view = JsonView::new("   \n\t  ");
        assert!(view.formatted_lines().is_empty());
    }

    #[test]
    fn deeply_nested_objects() {
        // 35 levels deep — depth clamped at 32 for indent
        let open: String = "{\"a\": ".repeat(35);
        let close: String = "}".repeat(35);
        let json = format!("{open}1{close}");
        let view = JsonView::new(json);
        let lines = view.formatted_lines();
        // Should not panic and produce output
        assert!(lines.len() > 10);
    }

    #[test]
    fn scientific_notation_number() {
        let view = JsonView::new(r#"{"x": 1.23e+10}"#);
        let lines = view.formatted_lines();
        let has_sci = lines.iter().any(|line| {
            line.iter()
                .any(|t| matches!(t, JsonToken::Number(s) if s.contains("e+")))
        });
        assert!(has_sci, "scientific notation should be Number: {lines:?}");
    }

    #[test]
    fn empty_string_key_and_value() {
        let view = JsonView::new(r#"{"": ""}"#);
        let lines = view.formatted_lines();
        let has_empty_key = lines.iter().any(|line| {
            line.iter()
                .any(|t| matches!(t, JsonToken::Key(s) if s == "\"\""))
        });
        assert!(has_empty_key, "empty key should be present: {lines:?}");
    }

    #[test]
    fn unicode_in_strings() {
        let view = JsonView::new(r#"{"emoji": "🎉🚀"}"#);
        let lines = view.formatted_lines();
        let has_emoji = lines.iter().any(|line| {
            line.iter()
                .any(|t| matches!(t, JsonToken::StringVal(s) if s.contains('🎉')))
        });
        assert!(has_emoji);
    }

    #[test]
    fn unclosed_string() {
        // Missing closing quote — tokenizer reads until EOF
        let view = JsonView::new(r#"{"key": "val"#);
        let lines = view.formatted_lines();
        // Should not panic; produces some output
        assert!(!lines.is_empty());
    }

    #[test]
    fn unclosed_object() {
        let view = JsonView::new(r#"{"a": 1"#);
        let lines = view.formatted_lines();
        assert!(!lines.is_empty());
    }

    #[test]
    fn unclosed_array() {
        let view = JsonView::new(r#"[1, 2, 3"#);
        let lines = view.formatted_lines();
        assert!(!lines.is_empty());
    }

    #[test]
    fn nested_empty_containers() {
        let view = JsonView::new(r#"{"a": [], "b": {}}"#);
        let lines = view.formatted_lines();
        // [] and {} should appear compact
        let flat = lines
            .iter()
            .map(|line| {
                line.iter()
                    .filter_map(|t| match t {
                        JsonToken::Punctuation(s) => Some(s.as_str()),
                        _ => None,
                    })
                    .collect::<String>()
            })
            .collect::<String>();
        assert!(flat.contains("[]"), "empty array should be compact: {flat}");
        assert!(
            flat.contains("{}"),
            "empty object should be compact: {flat}"
        );
    }

    #[test]
    fn array_of_mixed_types() {
        let view = JsonView::new(r#"[1, "two", true, null]"#);
        let lines = view.formatted_lines();
        let all_tokens: Vec<&JsonToken> = lines.iter().flat_map(|l| l.iter()).collect();
        assert!(all_tokens.iter().any(|t| matches!(t, JsonToken::Number(_))));
        assert!(
            all_tokens
                .iter()
                .any(|t| matches!(t, JsonToken::StringVal(_)))
        );
        assert!(
            all_tokens
                .iter()
                .any(|t| matches!(t, JsonToken::Literal(s) if s == "true"))
        );
        assert!(
            all_tokens
                .iter()
                .any(|t| matches!(t, JsonToken::Literal(s) if s == "null"))
        );
    }

    #[test]
    fn zero_indent_width() {
        let view = JsonView::new(r#"{"a": 1}"#).with_indent(0);
        let lines = view.formatted_lines();
        // Indentation should be empty strings
        for line in &lines {
            for token in line {
                if let JsonToken::Whitespace(s) = token {
                    assert!(s.is_empty(), "zero indent should produce empty whitespace");
                }
            }
        }
    }

    #[test]
    fn bare_string_top_level() {
        let view = JsonView::new(r#""hello""#);
        let lines = view.formatted_lines();
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0]
                .iter()
                .any(|t| matches!(t, JsonToken::StringVal(s) if s.contains("hello")))
        );
    }

    #[test]
    fn error_token_for_invalid_literal() {
        let view = JsonView::new(r#"{"a": undefined}"#);
        let lines = view.formatted_lines();
        let has_error = lines
            .iter()
            .any(|line| line.iter().any(|t| matches!(t, JsonToken::Error(_))));
        assert!(has_error, "undefined should produce Error token");
    }

    #[test]
    fn clone_independence() {
        let view = JsonView::new(r#"{"a": 1}"#);
        let cloned = view.clone();
        assert_eq!(view.source(), cloned.source());
    }

    #[test]
    fn debug_format() {
        let view = JsonView::new("{}");
        let dbg = format!("{view:?}");
        assert!(dbg.contains("JsonView"));
    }

    #[test]
    fn style_builders_chain() {
        let view = JsonView::new("{}")
            .with_indent(4)
            .with_key_style(Style::new().bold())
            .with_string_style(Style::default())
            .with_number_style(Style::default())
            .with_literal_style(Style::default())
            .with_punct_style(Style::default())
            .with_error_style(Style::default());
        assert_eq!(view.indent, 4);
    }

    #[test]
    fn render_width_one() {
        let view = JsonView::new(r#"{"a": 1}"#);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 10, &mut pool);
        Widget::render(&view, Rect::new(0, 0, 1, 10), &mut frame);
        // Should render first char of each line without panic
        let cell = frame.buffer.get(0, 0).unwrap();
        assert_eq!(cell.content.as_char(), Some('{'));
    }

    #[test]
    fn render_no_styling_drops_token_styles() {
        let key_color = PackedRgba::rgb(1, 2, 3);
        let number_color = PackedRgba::rgb(4, 5, 6);
        let view = JsonView::new(r#"{"key": 1}"#)
            .with_key_style(Style::new().fg(key_color).bold())
            .with_number_style(Style::new().fg(number_color).italic());
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 10, &mut pool);
        frame.buffer.degradation = DegradationLevel::NoStyling;
        Widget::render(&view, Rect::new(0, 0, 40, 10), &mut frame);

        let key_cell = frame.buffer.get(2, 1).unwrap();
        assert_eq!(key_cell.content.as_char(), Some('"'));
        assert_ne!(key_cell.fg, key_color);
        assert_eq!(key_cell.attrs, CellAttrs::NONE);

        let number_cell = frame.buffer.get(9, 1).unwrap();
        assert_eq!(number_cell.content.as_char(), Some('1'));
        assert_ne!(number_cell.fg, number_color);
        assert_eq!(number_cell.attrs, CellAttrs::NONE);
    }

    #[test]
    fn render_skeleton_is_noop() {
        let view = JsonView::new(r#"{"key": "value"}"#);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 10, &mut pool);
        let area = Rect::new(0, 0, 40, 10);
        Widget::render(&view, area, &mut frame);
        frame.buffer.degradation = DegradationLevel::Skeleton;
        Widget::render(&view, area, &mut frame);

        for y in 0..10 {
            for x in 0..40 {
                assert_eq!(
                    frame.buffer.get(x, y),
                    Some(&ftui_render::cell::Cell::default())
                );
            }
        }
    }

    #[test]
    fn render_shorter_json_clears_stale_suffix_and_rows() {
        let long = JsonView::new(r#"{"alpha": 1000, "beta": 2000}"#);
        let short = JsonView::new(r#"{"a": 1}"#);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 10, &mut pool);
        let area = Rect::new(0, 0, 40, 10);

        Widget::render(&long, area, &mut frame);
        Widget::render(&short, area, &mut frame);

        for y in 0..10u16 {
            for x in 0..40u16 {
                if y >= 3 {
                    assert_eq!(
                        frame.buffer.get(x, y),
                        Some(&ftui_render::cell::Cell::default())
                    );
                }
            }
        }
    }

    #[test]
    fn json_token_eq() {
        assert_eq!(JsonToken::Key("a".into()), JsonToken::Key("a".into()));
        assert_ne!(JsonToken::Key("a".into()), JsonToken::StringVal("a".into()));
        assert_ne!(JsonToken::Newline, JsonToken::Whitespace("".into()));
    }

    #[test]
    fn json_token_clone_and_debug() {
        let tokens = vec![
            JsonToken::Key("k".into()),
            JsonToken::StringVal("s".into()),
            JsonToken::Number("1".into()),
            JsonToken::Literal("true".into()),
            JsonToken::Punctuation("{".into()),
            JsonToken::Whitespace("  ".into()),
            JsonToken::Newline,
            JsonToken::Error("bad".into()),
        ];
        for tok in &tokens {
            let cloned = tok.clone();
            assert_eq!(tok, &cloned);
            let _ = format!("{tok:?}");
        }
    }

    #[test]
    fn classify_literal_empty_string() {
        // Empty literal should be Error (not a number or keyword)
        let result = classify_literal("");
        assert!(matches!(result, JsonToken::Error(s) if s.is_empty()));
    }

    #[test]
    fn negative_number() {
        assert_eq!(
            classify_literal("-42"),
            JsonToken::Number("-42".to_string())
        );
    }

    #[test]
    fn number_with_exponent() {
        assert_eq!(
            classify_literal("5E-3"),
            JsonToken::Number("5E-3".to_string())
        );
    }

    // ─── End edge-case tests (bd-2agoi) ──────────────────────────────

    #[test]
    fn classify_literal_types() {
        assert_eq!(
            classify_literal("true"),
            JsonToken::Literal("true".to_string())
        );
        assert_eq!(
            classify_literal("false"),
            JsonToken::Literal("false".to_string())
        );
        assert_eq!(
            classify_literal("null"),
            JsonToken::Literal("null".to_string())
        );
        assert_eq!(classify_literal("42"), JsonToken::Number("42".to_string()));
        assert_eq!(
            classify_literal("-3.14"),
            JsonToken::Number("-3.14".to_string())
        );
        assert!(matches!(classify_literal("invalid!"), JsonToken::Error(_)));
    }

    mod fold_tests {
        use super::*;

        fn key(k: &str) -> JsonPath {
            vec![JsonPathSegment::Key(k.to_string())]
        }

        fn line_text(tokens: &[JsonToken]) -> String {
            tokens
                .iter()
                .filter_map(|t| match t {
                    JsonToken::Key(s)
                    | JsonToken::StringVal(s)
                    | JsonToken::Number(s)
                    | JsonToken::Literal(s)
                    | JsonToken::Punctuation(s)
                    | JsonToken::Whitespace(s)
                    | JsonToken::Error(s) => Some(s.as_str()),
                    JsonToken::Newline => None,
                })
                .collect()
        }

        fn texts(view: &JsonView, state: &JsonViewState) -> Vec<String> {
            view.lines_with_state(state)
                .iter()
                .map(|l| line_text(&l.tokens))
                .collect()
        }

        #[test]
        fn default_state_matches_formatted_lines() {
            // The unfolded stateful output is byte-identical to the flat one.
            let view = JsonView::new(r#"{"a": 1, "b": {"c": 2}, "arr": [1, 2]}"#);
            let flat = view.formatted_lines();
            let stateful: Vec<Vec<JsonToken>> = view
                .lines_with_state(&JsonViewState::default())
                .into_iter()
                .map(|l| l.tokens)
                .collect();
            assert_eq!(flat, stateful);
        }

        #[test]
        fn toggle_folds_object_and_array() {
            let view = JsonView::new(r#"{"obj": {"x": 1}, "arr": [1, 2]}"#);
            let mut state = JsonViewState::new();

            state.toggle(&key("obj"));
            assert!(state.is_folded(&key("obj")));
            let t = texts(&view, &state);
            assert!(
                t.iter().any(|l| l.contains("obj") && l.contains('▸')),
                "obj folded to a placeholder: {t:?}"
            );
            assert!(!t.iter().any(|l| l.contains("\"x\"")), "descendants hidden");
            assert!(t.iter().any(|l| l.trim() == "1,"), "arr still expanded");

            state.toggle(&key("obj"));
            assert!(!state.is_folded(&key("obj")));
            assert!(texts(&view, &state).iter().any(|l| l.contains("\"x\"")));
        }

        #[test]
        fn folded_node_renders_placeholder_with_count() {
            let view = JsonView::new(r#"{"o": {"a": 1, "b": 2, "c": 3}, "arr": [10, 20, 30, 40]}"#);
            let mut state = JsonViewState::new();
            state.fold(&key("o"));
            state.fold(&key("arr"));
            let t = texts(&view, &state);
            assert!(t.iter().any(|l| l.contains("▸ {…} (3 keys)")), "{t:?}");
            assert!(t.iter().any(|l| l.contains("▸ […] (4 items)")), "{t:?}");
        }

        #[test]
        fn nested_fold_hides_descendants() {
            let view = JsonView::new(r#"{"a": {"b": {"c": 1}}}"#);
            let mut state = JsonViewState::new();
            state.fold(&key("a"));
            let t = texts(&view, &state);
            assert!(!t.iter().any(|l| l.contains("\"b\"")));
            assert!(!t.iter().any(|l| l.contains("\"c\"")));
            // root open, folded "a", root close.
            assert_eq!(t.len(), 3, "{t:?}");
        }

        #[test]
        fn fold_survives_set_source_when_path_exists() {
            let mut view = JsonView::new(r#"{"a": {"x": 1}, "b": 2}"#);
            let mut state = JsonViewState::new();
            state.fold(&key("a"));

            // Change the contents of "a" but keep it an object at the same path.
            view.set_source(r#"{"a": {"x": 99, "y": 100}, "b": 3}"#);
            let lines = view.lines_with_state(&state);
            assert!(
                lines
                    .iter()
                    .any(|l| l.folded && l.path.as_deref() == Some(key("a").as_slice())),
                "a stays folded across set_source"
            );
            assert!(!texts(&view, &state).iter().any(|l| l.contains("\"x\"")));
        }

        #[test]
        fn invalid_json_keeps_error_tokens_when_folding() {
            let view = JsonView::new(r#"{"a": nope}"#);
            let flat = view.formatted_lines();
            assert!(
                flat.iter()
                    .flatten()
                    .any(|t| matches!(t, JsonToken::Error(e) if e == "nope")),
                "invalid literal is an Error token"
            );
            // Folding then unfolding must not corrupt the tokens.
            let mut state = JsonViewState::new();
            state.fold_all(&view);
            state.unfold_all();
            let restored: Vec<Vec<JsonToken>> = view
                .lines_with_state(&state)
                .into_iter()
                .map(|l| l.tokens)
                .collect();
            assert_eq!(flat, restored);
        }

        #[test]
        fn fold_all_then_unfold_all_restores_formatted_lines() {
            let cases = [
                r#"{"a": 1}"#,
                r#"{"a": {"b": [1, 2, {"c": 3}]}, "d": "e"}"#,
                r#"[1, [2, [3, [4]]], {}]"#,
                r#"{"x": [], "y": {}, "z": [1]}"#,
                r#"42"#,
                r#""bare""#,
            ];
            for src in cases {
                let view = JsonView::new(src);
                let flat = view.formatted_lines();
                let mut state = JsonViewState::new();
                state.fold_all(&view);
                state.unfold_all();
                let restored: Vec<Vec<JsonToken>> = view
                    .lines_with_state(&state)
                    .into_iter()
                    .map(|l| l.tokens)
                    .collect();
                assert_eq!(flat, restored, "src={src}");
            }
        }

        #[test]
        fn foldable_paths_are_unique() {
            let view = JsonView::new(r#"{"a": {"b": 1}, "c": [{"d": 2}, {"d": 3}]}"#);
            let lines = view.lines_with_state(&JsonViewState::default());
            let paths: Vec<JsonPath> = lines
                .iter()
                .filter(|l| l.foldable)
                .filter_map(|l| l.path.clone())
                .collect();
            let unique: HashSet<JsonPath> = paths.iter().cloned().collect();
            assert_eq!(paths.len(), unique.len(), "paths not unique: {paths:?}");
            // The two `{"d": ..}` array elements have distinct index paths.
            assert!(paths.contains(&vec![
                JsonPathSegment::Key("c".into()),
                JsonPathSegment::Index(0)
            ]));
            assert!(paths.contains(&vec![
                JsonPathSegment::Key("c".into()),
                JsonPathSegment::Index(1)
            ]));
        }

        #[test]
        fn enter_toggles_node_at_cursor() {
            let view = JsonView::new(r#"{"a": {"x": 1}}"#);
            let mut state = JsonViewState::new();
            state.cursor_line = 1; // the `"a": {` opener line

            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(40, 12, &mut pool);
            StatefulWidget::render(&view, Rect::new(0, 0, 40, 12), &mut frame, &mut state);

            assert!(state.handle_event(&Event::Key(KeyEvent::new(KeyCode::Enter))));
            assert!(state.is_folded(&key("a")));
            // Space toggles it back.
            assert!(state.handle_event(&Event::Key(KeyEvent::new(KeyCode::Char(' ')))));
            assert!(!state.is_folded(&key("a")));
        }

        #[test]
        fn arrows_move_cursor_and_leaf_toggle_is_noop() {
            let view = JsonView::new(r#"{"a": 1, "b": 2}"#);
            let mut state = JsonViewState::new();
            let mut pool = GraphemePool::new();
            let mut frame = Frame::new(40, 12, &mut pool);
            StatefulWidget::render(&view, Rect::new(0, 0, 40, 12), &mut frame, &mut state);

            assert!(state.handle_event(&Event::Key(KeyEvent::new(KeyCode::Down))));
            assert_eq!(state.cursor_line, 1);
            // Line 1 is `"a": 1,` — a leaf; Enter changes nothing.
            assert!(!state.handle_event(&Event::Key(KeyEvent::new(KeyCode::Enter))));
            assert_eq!(state.folded_count(), 0);
            // Home/End.
            assert!(state.handle_event(&Event::Key(KeyEvent::new(KeyCode::End))));
            assert!(state.cursor_line > 0);
            assert!(state.handle_event(&Event::Key(KeyEvent::new(KeyCode::Home))));
            assert_eq!(state.cursor_line, 0);
        }

        #[test]
        fn stateful_render_equals_widget_when_unfolded() {
            // The default stateful render draws the same glyphs as the stateless
            // Widget (no cursor styling difference in the content itself).
            let view = JsonView::new(r#"{"a": {"b": 1}}"#);
            let area = Rect::new(0, 0, 20, 6);

            let mut pool_a = GraphemePool::new();
            let mut fa = Frame::new(20, 6, &mut pool_a);
            Widget::render(&view, area, &mut fa);

            let mut pool_b = GraphemePool::new();
            let mut fb = Frame::new(20, 6, &mut pool_b);
            let mut state = JsonViewState::new();
            // Use a no-op cursor style so the cursor row is not reverse-video.
            let view_plain = view.clone().with_cursor_style(Style::default());
            StatefulWidget::render(&view_plain, area, &mut fb, &mut state);

            for y in 0..6 {
                for x in 0..20 {
                    assert_eq!(
                        fa.buffer.get(x, y).unwrap().content.as_char(),
                        fb.buffer.get(x, y).unwrap().content.as_char(),
                        "cell ({x},{y})"
                    );
                }
            }
        }
    }
}
