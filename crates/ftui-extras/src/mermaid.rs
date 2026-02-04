#![forbid(unsafe_code)]

//! Mermaid parser core (tokenizer + AST).
//!
//! This module provides a minimal, deterministic parser for Mermaid fenced blocks.
//! It focuses on:
//! - Tokenization with stable spans (line/col)
//! - Diagram type detection
//! - AST for common diagram elements

use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line: usize,
    pub col: usize,
    pub byte: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: Position,
    pub end: Position,
}

impl Span {
    fn new(start: Position, end: Position) -> Self {
        Self { start, end }
    }

    fn at_line(line: usize, line_len: usize) -> Self {
        let start = Position {
            line,
            col: 1,
            byte: 0,
        };
        let end = Position {
            line,
            col: line_len.max(1),
            byte: 0,
        };
        Self::new(start, end)
    }
}

#[derive(Debug, Clone)]
pub struct MermaidError {
    pub message: String,
    pub span: Span,
    pub expected: Option<Vec<&'static str>>,
}

impl MermaidError {
    fn new(message: impl Into<String>, span: Span) -> Self {
        Self {
            message: message.into(),
            span,
            expected: None,
        }
    }

    fn with_expected(mut self, expected: Vec<&'static str>) -> Self {
        self.expected = Some(expected);
        self
    }
}

impl fmt::Display for MermaidError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} (line {}, col {})",
            self.message, self.span.start.line, self.span.start.col
        )?;
        if let Some(expected) = &self.expected {
            write!(f, "; expected: {}", expected.join(", "))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagramType {
    Graph,
    Sequence,
    State,
    Gantt,
    Class,
    Er,
    Mindmap,
    Pie,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphDirection {
    TB,
    TD,
    LR,
    RL,
    BT,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keyword {
    Graph,
    Flowchart,
    SequenceDiagram,
    StateDiagram,
    Gantt,
    ClassDiagram,
    ErDiagram,
    Mindmap,
    Pie,
    Subgraph,
    End,
    Title,
    Section,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind<'a> {
    Keyword(Keyword),
    Identifier(&'a str),
    Number(&'a str),
    String(&'a str),
    Arrow(&'a str),
    Punct(char),
    Directive(&'a str),
    Comment(&'a str),
    Newline,
    Eof,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token<'a> {
    pub kind: TokenKind<'a>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct MermaidAst {
    pub diagram_type: DiagramType,
    pub direction: Option<GraphDirection>,
    pub directives: Vec<Directive>,
    pub statements: Vec<Statement>,
}

#[derive(Debug, Clone)]
pub struct Directive {
    pub content: String,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub id: String,
    pub label: Option<String>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub arrow: String,
    pub label: Option<String>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct SequenceMessage {
    pub from: String,
    pub to: String,
    pub arrow: String,
    pub message: Option<String>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct GanttTask {
    pub title: String,
    pub meta: String,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct PieEntry {
    pub label: String,
    pub value: String,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct MindmapNode {
    pub depth: usize,
    pub text: String,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum Statement {
    Directive(Directive),
    Node(Node),
    Edge(Edge),
    SequenceMessage(SequenceMessage),
    ClassMember {
        class: String,
        member: String,
        span: Span,
    },
    GanttTitle {
        title: String,
        span: Span,
    },
    GanttSection {
        name: String,
        span: Span,
    },
    GanttTask(GanttTask),
    PieEntry(PieEntry),
    MindmapNode(MindmapNode),
    Raw {
        text: String,
        span: Span,
    },
}

pub struct Lexer<'a> {
    input: &'a str,
    bytes: &'a [u8],
    idx: usize,
    line: usize,
    col: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(input: &'a str) -> Self {
        Self {
            input,
            bytes: input.as_bytes(),
            idx: 0,
            line: 1,
            col: 1,
        }
    }

    pub fn tokenize(mut self) -> Vec<Token<'a>> {
        let mut out = Vec::new();
        loop {
            let lexeme = self.next_token();
            let is_eof = matches!(lexeme.kind, TokenKind::Eof);
            out.push(lexeme);
            if is_eof {
                break;
            }
        }
        out
    }

    fn next_token(&mut self) -> Token<'a> {
        self.skip_spaces();
        let start = self.position();
        if self.idx >= self.bytes.len() {
            return Token {
                kind: TokenKind::Eof,
                span: Span::new(start, start),
            };
        }
        let b = self.bytes[self.idx];
        if b == b'\n' {
            self.advance_byte();
            return Token {
                kind: TokenKind::Newline,
                span: Span::new(start, self.position()),
            };
        }
        if b == b'\r' {
            self.advance_byte();
            if self.peek_byte() == Some(b'\n') {
                self.advance_byte();
            }
            return Token {
                kind: TokenKind::Newline,
                span: Span::new(start, self.position()),
            };
        }
        if b == b'%' && self.peek_byte() == Some(b'%') {
            return self.lex_comment_or_directive(start);
        }
        if b == b'"' || b == b'\'' {
            return self.lex_string(start, b);
        }
        if is_digit(b) {
            return self.lex_number(start);
        }
        if is_arrow_char(b as char) {
            return self.lex_arrow_or_punct(start);
        }
        if is_ident_start(b as char) {
            return self.lex_identifier(start);
        }

        self.advance_byte();
        Token {
            kind: TokenKind::Punct(b as char),
            span: Span::new(start, self.position()),
        }
    }

    fn lex_comment_or_directive(&mut self, start: Position) -> Token<'a> {
        self.advance_byte(); // %
        self.advance_byte(); // %
        if self.peek_n_bytes(0) == Some(b'{') {
            self.advance_byte();
            let content_start = self.idx;
            while self.idx < self.bytes.len() {
                if self.bytes[self.idx] == b'}'
                    && self.peek_n_bytes(1) == Some(b'%')
                    && self.peek_n_bytes(2) == Some(b'%')
                {
                    let content = &self.input[content_start..self.idx];
                    self.advance_byte();
                    self.advance_byte();
                    self.advance_byte();
                    return Token {
                        kind: TokenKind::Directive(content),
                        span: Span::new(start, self.position()),
                    };
                }
                self.advance_byte();
            }
            return Token {
                kind: TokenKind::Directive(&self.input[content_start..self.idx]),
                span: Span::new(start, self.position()),
            };
        }

        let content_start = self.idx;
        while self.idx < self.bytes.len() {
            let b = self.bytes[self.idx];
            if b == b'\n' || b == b'\r' {
                break;
            }
            self.advance_byte();
        }
        Token {
            kind: TokenKind::Comment(&self.input[content_start..self.idx]),
            span: Span::new(start, self.position()),
        }
    }

    fn lex_string(&mut self, start: Position, quote: u8) -> Token<'a> {
        self.advance_byte();
        let content_start = self.idx;
        while self.idx < self.bytes.len() {
            let b = self.bytes[self.idx];
            if b == quote {
                let content = &self.input[content_start..self.idx];
                self.advance_byte();
                return Token {
                    kind: TokenKind::String(content),
                    span: Span::new(start, self.position()),
                };
            }
            if b == b'\\' {
                self.advance_byte();
                if self.idx < self.bytes.len() {
                    self.advance_byte();
                }
                continue;
            }
            if b == b'\n' || b == b'\r' {
                break;
            }
            self.advance_byte();
        }
        Token {
            kind: TokenKind::String(&self.input[content_start..self.idx]),
            span: Span::new(start, self.position()),
        }
    }

    fn lex_number(&mut self, start: Position) -> Token<'a> {
        let start_idx = self.idx;
        while self.idx < self.bytes.len() {
            let b = self.bytes[self.idx];
            if !is_digit(b) && b != b'.' {
                break;
            }
            self.advance_byte();
        }
        Token {
            kind: TokenKind::Number(&self.input[start_idx..self.idx]),
            span: Span::new(start, self.position()),
        }
    }

    fn lex_identifier(&mut self, start: Position) -> Token<'a> {
        let start_idx = self.idx;
        self.advance_byte();
        while self.idx < self.bytes.len() {
            let c = self.bytes[self.idx] as char;
            if !is_ident_continue(c) {
                break;
            }
            self.advance_byte();
        }
        let text = &self.input[start_idx..self.idx];
        let kind = match keyword_from(text) {
            Some(keyword) => TokenKind::Keyword(keyword),
            None => TokenKind::Identifier(text),
        };
        Token {
            kind,
            span: Span::new(start, self.position()),
        }
    }

    fn lex_arrow_or_punct(&mut self, start: Position) -> Token<'a> {
        let start_idx = self.idx;
        let mut count = 0usize;
        while self.idx < self.bytes.len() {
            let c = self.bytes[self.idx] as char;
            if !is_arrow_char(c) {
                break;
            }
            count += 1;
            self.advance_byte();
        }
        if count >= 2 {
            return Token {
                kind: TokenKind::Arrow(&self.input[start_idx..self.idx]),
                span: Span::new(start, self.position()),
            };
        }
        let ch = self.input[start_idx..self.idx]
            .chars()
            .next()
            .unwrap_or('-');
        Token {
            kind: TokenKind::Punct(ch),
            span: Span::new(start, self.position()),
        }
    }

    fn skip_spaces(&mut self) {
        while self.idx < self.bytes.len() {
            let b = self.bytes[self.idx];
            if b == b' ' || b == b'\t' {
                self.advance_byte();
            } else {
                break;
            }
        }
    }

    fn advance_byte(&mut self) {
        if self.idx >= self.bytes.len() {
            return;
        }
        let b = self.bytes[self.idx];
        self.idx += 1;
        if b == b'\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
    }

    fn position(&self) -> Position {
        Position {
            line: self.line,
            col: self.col,
            byte: self.idx,
        }
    }

    fn peek_byte(&self) -> Option<u8> {
        self.bytes.get(self.idx + 1).copied()
    }

    fn peek_n_bytes(&self, n: usize) -> Option<u8> {
        self.bytes.get(self.idx + n).copied()
    }
}

pub fn tokenize(input: &str) -> Vec<Token<'_>> {
    Lexer::new(input).tokenize()
}

pub fn parse(input: &str) -> Result<MermaidAst, MermaidError> {
    let mut diagram_type = DiagramType::Unknown;
    let mut direction = None;
    let mut directives = Vec::new();
    let mut statements = Vec::new();
    let mut saw_header = false;

    for (idx, raw_line) in input.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw_line.trim_end_matches('\r');
        let trimmed = strip_inline_comment(line).trim();
        if trimmed.is_empty() {
            continue;
        }

        if trimmed.starts_with("%%{") {
            let span = Span::at_line(line_no, line.len());
            if let Some(content) = trimmed
                .strip_prefix("%%{")
                .and_then(|v| v.strip_suffix("}%%"))
            {
                let dir = Directive {
                    content: content.trim().to_string(),
                    span,
                };
                directives.push(dir.clone());
                statements.push(Statement::Directive(dir));
                continue;
            }
            return Err(MermaidError::new("unterminated directive", span));
        }
        if trimmed.starts_with("%%") {
            continue;
        }

        if !saw_header {
            if let Some((dtype, dir)) = parse_header(trimmed) {
                diagram_type = dtype;
                direction = dir;
                saw_header = true;
                continue;
            }
            let span = Span::at_line(line_no, line.len());
            return Err(
                MermaidError::new("expected Mermaid diagram header", span).with_expected(vec![
                    "graph",
                    "flowchart",
                    "sequenceDiagram",
                    "stateDiagram",
                    "gantt",
                    "classDiagram",
                    "erDiagram",
                    "mindmap",
                    "pie",
                ]),
            );
        }

        let span = Span::at_line(line_no, line.len());
        match diagram_type {
            DiagramType::Graph | DiagramType::State | DiagramType::Class | DiagramType::Er => {
                if let Some(edge) = parse_edge(trimmed, span) {
                    if let Some(node) = edge_node(trimmed, span) {
                        statements.push(Statement::Node(node));
                    }
                    statements.push(Statement::Edge(edge));
                } else if let Some(member) = parse_class_member(trimmed, span) {
                    statements.push(member);
                } else if let Some(node) = parse_node(trimmed, span) {
                    statements.push(Statement::Node(node));
                } else {
                    statements.push(Statement::Raw {
                        text: normalize_ws(trimmed),
                        span,
                    });
                }
            }
            DiagramType::Sequence => {
                if let Some(msg) = parse_sequence(trimmed, span) {
                    statements.push(Statement::SequenceMessage(msg));
                } else {
                    statements.push(Statement::Raw {
                        text: normalize_ws(trimmed),
                        span,
                    });
                }
            }
            DiagramType::Gantt => {
                if let Some(stmt) = parse_gantt(trimmed, span) {
                    statements.push(stmt);
                } else {
                    statements.push(Statement::Raw {
                        text: normalize_ws(trimmed),
                        span,
                    });
                }
            }
            DiagramType::Mindmap => {
                if let Some(node) = parse_mindmap(trimmed, raw_line, span) {
                    statements.push(Statement::MindmapNode(node));
                } else {
                    statements.push(Statement::Raw {
                        text: normalize_ws(trimmed),
                        span,
                    });
                }
            }
            DiagramType::Pie => {
                if let Some(entry) = parse_pie(trimmed, span) {
                    statements.push(Statement::PieEntry(entry));
                } else {
                    statements.push(Statement::Raw {
                        text: normalize_ws(trimmed),
                        span,
                    });
                }
            }
            DiagramType::Unknown => {
                statements.push(Statement::Raw {
                    text: normalize_ws(trimmed),
                    span,
                });
            }
        }
    }

    Ok(MermaidAst {
        diagram_type,
        direction,
        directives,
        statements,
    })
}

fn strip_inline_comment(line: &str) -> &str {
    if let Some(idx) = line.find("%%") {
        if line[..idx].trim().is_empty() {
            return line;
        }
        &line[..idx]
    } else {
        line
    }
}

fn parse_header(line: &str) -> Option<(DiagramType, Option<GraphDirection>)> {
    let lower = line.trim().to_ascii_lowercase();
    if lower.starts_with("graph") || lower.starts_with("flowchart") {
        let mut parts = lower.split_whitespace();
        let _ = parts.next()?;
        let dir = parts.next().and_then(|d| match d {
            "tb" => Some(GraphDirection::TB),
            "td" => Some(GraphDirection::TD),
            "lr" => Some(GraphDirection::LR),
            "rl" => Some(GraphDirection::RL),
            "bt" => Some(GraphDirection::BT),
            _ => None,
        });
        return Some((DiagramType::Graph, dir));
    }
    if lower.starts_with("sequencediagram") {
        return Some((DiagramType::Sequence, None));
    }
    if lower.starts_with("statediagram") {
        return Some((DiagramType::State, None));
    }
    if lower.starts_with("gantt") {
        return Some((DiagramType::Gantt, None));
    }
    if lower.starts_with("classdiagram") {
        return Some((DiagramType::Class, None));
    }
    if lower.starts_with("erdiagram") {
        return Some((DiagramType::Er, None));
    }
    if lower.starts_with("mindmap") {
        return Some((DiagramType::Mindmap, None));
    }
    if lower.starts_with("pie") {
        return Some((DiagramType::Pie, None));
    }
    None
}

fn parse_edge(line: &str, span: Span) -> Option<Edge> {
    let (start, end, arrow) = find_arrow(line)?;
    let left = line[..start].trim();
    let right = line[end..].trim();
    if left.is_empty() || right.is_empty() {
        return None;
    }
    let (label, right_id) = split_label(right);
    let from = parse_node_id(left)?;
    let to = parse_node_id(right_id)?;
    Some(Edge {
        from,
        to,
        arrow: arrow.to_string(),
        label: label.map(normalize_ws),
        span,
    })
}

fn edge_node(line: &str, span: Span) -> Option<Node> {
    let (start, _, _) = find_arrow(line)?;
    let left = line[..start].trim();
    parse_node(left, span)
}

fn parse_node(line: &str, span: Span) -> Option<Node> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let (id, label) = parse_node_spec(line)?;
    Some(Node { id, label, span })
}

fn parse_node_spec(text: &str) -> Option<(String, Option<String>)> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut id = String::new();
    let mut label = None;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '[' || c == '(' || c == '{' {
            let closing = match c {
                '[' => ']',
                '(' => ')',
                '{' => '}',
                _ => ']',
            };
            let rest: String = chars.collect();
            if let Some(end) = rest.find(closing) {
                label = Some(normalize_ws(rest[..end].trim()));
            }
            break;
        }
        if c.is_whitespace() {
            break;
        }
        id.push(c);
    }
    if id.is_empty() {
        return None;
    }
    Some((normalize_ws(&id), label))
}

fn parse_class_member(line: &str, span: Span) -> Option<Statement> {
    if let Some(idx) = line.find(':') {
        let left = line[..idx].trim();
        let right = line[idx + 1..].trim();
        if !left.is_empty() && !right.is_empty() {
            return Some(Statement::ClassMember {
                class: normalize_ws(left),
                member: normalize_ws(right),
                span,
            });
        }
    }
    None
}

fn parse_sequence(line: &str, span: Span) -> Option<SequenceMessage> {
    let (start, end, arrow) = find_arrow(line)?;
    let left = line[..start].trim();
    let right = line[end..].trim();
    let (message, right_id) = if let Some(idx) = right.find(':') {
        (Some(right[idx + 1..].trim()), right[..idx].trim())
    } else {
        (None, right)
    };
    if left.is_empty() || right_id.is_empty() {
        return None;
    }
    Some(SequenceMessage {
        from: normalize_ws(left),
        to: normalize_ws(right_id),
        arrow: arrow.to_string(),
        message: message.map(normalize_ws),
        span,
    })
}

fn parse_gantt(line: &str, span: Span) -> Option<Statement> {
    let lower = line.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("title ") {
        return Some(Statement::GanttTitle {
            title: normalize_ws(rest),
            span,
        });
    }
    if let Some(rest) = lower.strip_prefix("section ") {
        return Some(Statement::GanttSection {
            name: normalize_ws(rest),
            span,
        });
    }
    if line.contains(':') {
        let mut parts = line.splitn(2, ':');
        let title = parts.next()?.trim();
        let meta = parts.next()?.trim();
        if !title.is_empty() && !meta.is_empty() {
            return Some(Statement::GanttTask(GanttTask {
                title: normalize_ws(title),
                meta: normalize_ws(meta),
                span,
            }));
        }
    }
    None
}

fn parse_pie(line: &str, span: Span) -> Option<PieEntry> {
    let mut parts = line.splitn(2, ':');
    let label = parts.next()?.trim();
    let value = parts.next()?.trim();
    if label.is_empty() || value.is_empty() {
        return None;
    }
    Some(PieEntry {
        label: normalize_ws(label.trim_matches(['"', '\''])),
        value: normalize_ws(value),
        span,
    })
}

fn parse_mindmap(trimmed: &str, raw_line: &str, span: Span) -> Option<MindmapNode> {
    if trimmed.is_empty() {
        return None;
    }
    let mut depth = 0usize;
    for ch in raw_line.chars() {
        if ch == ' ' {
            depth += 1;
        } else if ch == '\t' {
            depth += 2;
        } else {
            break;
        }
    }
    Some(MindmapNode {
        depth,
        text: normalize_ws(trimmed),
        span,
    })
}

fn split_label(text: &str) -> (Option<&str>, &str) {
    let trimmed = text.trim();
    if let Some(stripped) = trimmed.strip_prefix('|')
        && let Some(end) = stripped.find('|')
    {
        let label = &stripped[..end];
        let rest = stripped[end + 1..].trim();
        return (Some(label), rest);
    }
    if let Some(idx) = trimmed.find(':') {
        let label = trimmed[idx + 1..].trim();
        let rest = trimmed[..idx].trim();
        return (Some(label), rest);
    }
    (None, trimmed)
}

fn parse_node_id(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let (id, _) = parse_node_spec(text)?;
    Some(id)
}

fn find_arrow(line: &str) -> Option<(usize, usize, &str)> {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if is_arrow_char(chars[i]) {
            let start = i;
            let mut j = i + 1;
            while j < chars.len() && is_arrow_char(chars[j]) {
                j += 1;
            }
            if j - start >= 2 {
                let start_byte = line.char_indices().nth(start).map(|(idx, _)| idx)?;
                let end_byte = if j >= chars.len() {
                    line.len()
                } else {
                    line.char_indices().nth(j).map(|(idx, _)| idx)?
                };
                let arrow = &line[start_byte..end_byte];
                return Some((start_byte, end_byte, arrow));
            }
            i = j;
        } else {
            i += 1;
        }
    }
    None
}

fn normalize_ws(input: &str) -> String {
    input
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn keyword_from(text: &str) -> Option<Keyword> {
    match text.to_ascii_lowercase().as_str() {
        "graph" => Some(Keyword::Graph),
        "flowchart" => Some(Keyword::Flowchart),
        "sequencediagram" => Some(Keyword::SequenceDiagram),
        "statediagram" => Some(Keyword::StateDiagram),
        "gantt" => Some(Keyword::Gantt),
        "classdiagram" => Some(Keyword::ClassDiagram),
        "erdiagram" => Some(Keyword::ErDiagram),
        "mindmap" => Some(Keyword::Mindmap),
        "pie" => Some(Keyword::Pie),
        "subgraph" => Some(Keyword::Subgraph),
        "end" => Some(Keyword::End),
        "title" => Some(Keyword::Title),
        "section" => Some(Keyword::Section),
        _ => None,
    }
}

fn is_digit(b: u8) -> bool {
    b.is_ascii_digit()
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c == '$'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '$')
}

fn is_arrow_char(c: char) -> bool {
    matches!(c, '-' | '.' | '=' | '<' | '>' | 'o' | 'x' | '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_graph_header() {
        let tokens = tokenize("graph TD\nA-->B\n");
        assert!(
            tokens
                .iter()
                .any(|t| matches!(t.kind, TokenKind::Keyword(Keyword::Graph)))
        );
        assert!(
            tokens
                .iter()
                .any(|t| matches!(t.kind, TokenKind::Arrow("-->")))
        );
    }

    #[test]
    fn parse_graph_edges() {
        let ast = parse("graph TD\nA-->B\nB-->C\n").expect("parse");
        assert_eq!(ast.diagram_type, DiagramType::Graph);
        let edges = ast
            .statements
            .iter()
            .filter(|s| matches!(s, Statement::Edge(_)))
            .count();
        assert_eq!(edges, 2);
    }

    #[test]
    fn parse_sequence_messages() {
        let ast = parse("sequenceDiagram\nAlice->>Bob: Hello\n").expect("parse");
        let msgs = ast
            .statements
            .iter()
            .filter(|s| matches!(s, Statement::SequenceMessage(_)))
            .count();
        assert_eq!(msgs, 1);
    }

    #[test]
    fn parse_state_edges() {
        let ast = parse("stateDiagram\nS1-->S2\n").expect("parse");
        let edges = ast
            .statements
            .iter()
            .filter(|s| matches!(s, Statement::Edge(_)))
            .count();
        assert_eq!(edges, 1);
    }

    #[test]
    fn parse_gantt_lines() {
        let ast = parse(
            "gantt\n    title Project Plan\n    section Phase 1\n    Task A :done, 2024-01-01, 1d\n",
        )
        .expect("parse");
        assert!(
            ast.statements
                .iter()
                .any(|s| matches!(s, Statement::GanttTitle { .. }))
        );
        assert!(
            ast.statements
                .iter()
                .any(|s| matches!(s, Statement::GanttSection { .. }))
        );
        assert!(
            ast.statements
                .iter()
                .any(|s| matches!(s, Statement::GanttTask(_)))
        );
    }

    #[test]
    fn parse_class_member() {
        let ast = parse("classDiagram\nClassA : +int id\n").expect("parse");
        assert!(
            ast.statements
                .iter()
                .any(|s| matches!(s, Statement::ClassMember { .. }))
        );
    }

    #[test]
    fn parse_er_edge() {
        let ast = parse("erDiagram\nA ||--o{ B : relates\n").expect("parse");
        assert!(
            ast.statements
                .iter()
                .any(|s| matches!(s, Statement::Edge(_)))
        );
    }

    #[test]
    fn parse_mindmap_nodes() {
        let ast = parse("mindmap\n  root\n    child\n").expect("parse");
        let nodes = ast
            .statements
            .iter()
            .filter(|s| matches!(s, Statement::MindmapNode(_)))
            .count();
        assert_eq!(nodes, 2);
    }

    #[test]
    fn parse_pie_entries() {
        let ast = parse("pie\n  \"Dogs\" : 386\n  Cats : 85\n").expect("parse");
        let entries = ast
            .statements
            .iter()
            .filter(|s| matches!(s, Statement::PieEntry(_)))
            .count();
        assert_eq!(entries, 2);
    }

    #[test]
    fn tokenize_directive_block() {
        let tokens = tokenize("%%{init: {\"theme\":\"dark\"}}%%\n");
        assert!(
            tokens
                .iter()
                .any(|t| matches!(t.kind, TokenKind::Directive(_)))
        );
    }

    #[test]
    fn parse_directive_line() {
        let ast = parse("graph TD\n%%{init: {\"theme\":\"dark\"}}%%\nA-->B\n").expect("parse");
        assert!(
            ast.statements
                .iter()
                .any(|s| matches!(s, Statement::Directive(_)))
        );
    }
}
