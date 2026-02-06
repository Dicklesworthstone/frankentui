//! DOT (GraphViz) format parser that produces [`MermaidDiagramIr`].
//!
//! Supports the core DOT subset:
//! - `graph`/`digraph` declarations
//! - Node declarations with `label`, `shape`, `color`, `style`, `fillcolor` attributes
//! - Edge declarations (`->` for digraph, `--` for graph)
//! - `subgraph`/`cluster` declarations
//! - DOT shape → [`NodeShape`] mapping
//!
//! # Example
//!
//! ```ignore
//! use ftui_extras::dot_parser::parse_dot;
//!
//! let ir = parse_dot("digraph G { A -> B; B -> C; }").unwrap();
//! assert_eq!(ir.ir.nodes.len(), 3);
//! assert_eq!(ir.ir.edges.len(), 2);
//! ```

use std::collections::HashMap;

use crate::mermaid::{
    DiagramType, GraphDirection, IrCluster, IrClusterId, IrEdge, IrEndpoint, IrLabel, IrLabelId,
    IrNode, IrNodeId, MermaidDiagramIr, MermaidDiagramMeta, MermaidError, MermaidIrParse,
    MermaidWarning, NodeShape, Position, Span,
};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Returns `true` if `input` looks like DOT format rather than Mermaid.
///
/// Heuristic: first non-whitespace, non-comment token is `graph`, `digraph`,
/// or `strict`.
#[must_use]
pub fn looks_like_dot(input: &str) -> bool {
    let trimmed = skip_leading_comments(input);
    let lower = trimmed.trim_start().to_ascii_lowercase();
    if lower.starts_with("digraph") || lower.starts_with("strict ") {
        return true;
    }
    if !lower.starts_with("graph") {
        return false;
    }
    // After "graph", the next char must be whitespace or '{' (not an ident continuation)
    let after_graph = &lower[5..];
    if after_graph.is_empty() {
        return false;
    }
    let next = after_graph.as_bytes()[0];
    if next == b'{' || next == b'\n' || next == b'\r' || next == b'\t' {
        return true;
    }
    if next != b' ' {
        return false;
    }
    // "graph " — check if the next token is a Mermaid direction keyword
    let after_space = after_graph[1..].trim_start();
    let mermaid_directions = ["td", "tb", "lr", "rl", "bt"];
    for dir in &mermaid_directions {
        if let Some(rest) = after_space.strip_prefix(dir) {
            // Must be a complete token: followed by whitespace, newline, ';', or EOF
            if rest.is_empty() || rest.starts_with(|c: char| c.is_ascii_whitespace() || c == ';') {
                return false;
            }
        }
    }
    true
}

/// Parse a DOT string into a [`MermaidIrParse`].
///
/// # Errors
///
/// Returns `Err` if the input cannot be parsed at all (e.g. missing opening
/// brace). Recoverable issues are returned as warnings inside the result.
pub fn parse_dot(input: &str) -> Result<MermaidIrParse, DotParseError> {
    let mut parser = DotParser::new(input);
    parser.parse()
}

/// Error type for unrecoverable DOT parse failures.
#[derive(Debug, Clone)]
pub struct DotParseError {
    pub message: String,
    pub line: usize,
    pub col: usize,
}

impl std::fmt::Display for DotParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "DOT parse error at {}:{}: {}",
            self.line, self.col, self.message
        )
    }
}

impl std::error::Error for DotParseError {}

// ---------------------------------------------------------------------------
// DOT shape → NodeShape mapping
// ---------------------------------------------------------------------------

fn dot_shape_to_node_shape(shape: &str) -> NodeShape {
    match shape.to_ascii_lowercase().as_str() {
        "box" | "rect" | "rectangle" | "square" => NodeShape::Rect,
        "ellipse" | "oval" => NodeShape::Rounded,
        "circle" | "point" | "doublecircle" => NodeShape::Circle,
        "diamond" => NodeShape::Diamond,
        "hexagon" => NodeShape::Hexagon,
        "parallelogram" => NodeShape::Asymmetric,
        "record" | "mrecord" => NodeShape::Subroutine,
        "tab" | "folder" | "box3d" | "component" | "cylinder" | "note" => NodeShape::Rect,
        "plaintext" | "plain" | "none" => NodeShape::Rect,
        _ => NodeShape::Rect,
    }
}

// ---------------------------------------------------------------------------
// Internal parser
// ---------------------------------------------------------------------------

struct DotParser<'a> {
    input: &'a str,
    bytes: &'a [u8],
    pos: usize,
    line: usize,
    col: usize,
    is_digraph: bool,

    // Accumulation
    nodes: Vec<IrNode>,
    edges: Vec<IrEdge>,
    labels: Vec<IrLabel>,
    clusters: Vec<IrCluster>,
    warnings: Vec<MermaidWarning>,
    errors: Vec<MermaidError>,

    // Lookup
    node_map: HashMap<String, IrNodeId>,
    cluster_counter: usize,
}

impl<'a> DotParser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input,
            bytes: input.as_bytes(),
            pos: 0,
            line: 1,
            col: 1,
            is_digraph: false,
            nodes: Vec::new(),
            edges: Vec::new(),
            labels: Vec::new(),
            clusters: Vec::new(),
            warnings: Vec::new(),
            errors: Vec::new(),
            node_map: HashMap::new(),
            cluster_counter: 0,
        }
    }

    // -- Position helpers --

    fn current_pos(&self) -> Position {
        Position {
            line: self.line,
            col: self.col,
            byte: self.pos,
        }
    }

    fn span_from(&self, start: Position) -> Span {
        Span {
            start,
            end: self.current_pos(),
        }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn advance(&mut self) -> Option<u8> {
        if self.at_end() {
            return None;
        }
        let b = self.bytes[self.pos];
        self.pos += 1;
        if b == b'\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(b)
    }

    fn skip_whitespace(&mut self) {
        while !self.at_end() {
            let b = self.bytes[self.pos];
            if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' {
                self.advance();
            } else if b == b'/' && self.pos + 1 < self.bytes.len() {
                if self.bytes[self.pos + 1] == b'/' {
                    // Line comment
                    while !self.at_end() && self.peek() != Some(b'\n') {
                        self.advance();
                    }
                } else if self.bytes[self.pos + 1] == b'*' {
                    // Block comment
                    self.advance(); // /
                    self.advance(); // *
                    loop {
                        if self.at_end() {
                            break;
                        }
                        if self.peek() == Some(b'*')
                            && self.pos + 1 < self.bytes.len()
                            && self.bytes[self.pos + 1] == b'/'
                        {
                            self.advance(); // *
                            self.advance(); // /
                            break;
                        }
                        self.advance();
                    }
                } else {
                    break;
                }
            } else if b == b'#' {
                // Hash-style line comment (some DOT dialects)
                while !self.at_end() && self.peek() != Some(b'\n') {
                    self.advance();
                }
            } else {
                break;
            }
        }
    }

    fn expect_char(&mut self, c: u8) -> Result<(), DotParseError> {
        self.skip_whitespace();
        if self.peek() == Some(c) {
            self.advance();
            Ok(())
        } else {
            Err(DotParseError {
                message: format!(
                    "expected '{}', found '{}'",
                    c as char,
                    self.peek()
                        .map_or("EOF".to_string(), |b| (b as char).to_string())
                ),
                line: self.line,
                col: self.col,
            })
        }
    }

    /// Read a DOT identifier: bare word or quoted string.
    fn read_id(&mut self) -> Option<String> {
        self.skip_whitespace();
        if self.at_end() {
            return None;
        }
        match self.peek()? {
            b'"' => self.read_quoted_string(),
            b'<' => self.read_html_label(),
            _ => self.read_bare_id(),
        }
    }

    fn read_bare_id(&mut self) -> Option<String> {
        let start = self.pos;
        while !self.at_end() {
            let b = self.bytes[self.pos];
            if b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-' {
                self.advance();
            } else {
                break;
            }
        }
        if self.pos == start {
            return None;
        }
        Some(self.input[start..self.pos].to_string())
    }

    fn read_quoted_string(&mut self) -> Option<String> {
        if self.peek() != Some(b'"') {
            return None;
        }
        self.advance(); // opening quote
        let mut s = String::new();
        loop {
            match self.advance()? {
                b'\\' => {
                    if let Some(next) = self.advance() {
                        match next {
                            b'n' => s.push('\n'),
                            b't' => s.push('\t'),
                            b'"' => s.push('"'),
                            b'\\' => s.push('\\'),
                            other => {
                                s.push('\\');
                                s.push(other as char);
                            }
                        }
                    }
                }
                b'"' => break,
                other => s.push(other as char),
            }
        }
        Some(s)
    }

    fn read_html_label(&mut self) -> Option<String> {
        if self.peek() != Some(b'<') {
            return None;
        }
        self.advance(); // <
        let mut depth = 1u32;
        let mut s = String::new();
        loop {
            let b = self.advance()?;
            match b {
                b'<' => {
                    depth += 1;
                    s.push('<');
                }
                b'>' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    s.push('>');
                }
                _ => s.push(b as char),
            }
        }
        // Strip HTML tags for the label text
        let plain = strip_html_tags(&s);
        Some(plain)
    }

    /// Read attribute list `[key=val, key=val, ...]`.
    fn read_attrs(&mut self) -> HashMap<String, String> {
        let mut attrs = HashMap::new();
        self.skip_whitespace();
        if self.peek() != Some(b'[') {
            return attrs;
        }
        self.advance(); // [

        loop {
            self.skip_whitespace();
            if self.at_end() || self.peek() == Some(b']') {
                break;
            }
            let Some(key) = self.read_id() else { break };
            self.skip_whitespace();
            if self.peek() == Some(b'=') {
                self.advance();
                if let Some(val) = self.read_id() {
                    attrs.insert(key.to_ascii_lowercase(), val);
                }
            } else {
                // Boolean attribute
                attrs.insert(key.to_ascii_lowercase(), String::new());
            }
            self.skip_whitespace();
            // Consume separator (, or ;)
            if self.peek() == Some(b',') || self.peek() == Some(b';') {
                self.advance();
            }
        }

        if self.peek() == Some(b']') {
            self.advance();
        }
        attrs
    }

    // -- Label interning --

    fn intern_label(&mut self, text: &str) -> IrLabelId {
        let id = IrLabelId(self.labels.len());
        self.labels.push(IrLabel {
            text: text.to_string(),
            span: Span {
                start: Position {
                    line: 0,
                    col: 0,
                    byte: 0,
                },
                end: Position {
                    line: 0,
                    col: 0,
                    byte: 0,
                },
            },
        });
        id
    }

    // -- Node resolution --

    fn ensure_node(&mut self, id: &str, span: Span) -> IrNodeId {
        if let Some(&node_id) = self.node_map.get(id) {
            return node_id;
        }
        let node_id = IrNodeId(self.nodes.len());
        let label_id = self.intern_label(id);
        self.nodes.push(IrNode {
            id: id.to_string(),
            label: Some(label_id),
            shape: NodeShape::Rect,
            classes: Vec::new(),
            style_ref: None,
            span_primary: span,
            span_all: vec![span],
            implicit: true,
            members: Vec::new(),
        });
        self.node_map.insert(id.to_string(), node_id);
        node_id
    }

    fn apply_node_attrs(&mut self, node_id: IrNodeId, attrs: &HashMap<String, String>) {
        if let Some(label) = attrs.get("label") {
            let label_id = self.intern_label(label);
            self.nodes[node_id.0].label = Some(label_id);
        }
        if let Some(shape) = attrs.get("shape") {
            self.nodes[node_id.0].shape = dot_shape_to_node_shape(shape);
        }
        self.nodes[node_id.0].implicit = false;
    }

    // -- Main parse --

    fn parse(&mut self) -> Result<MermaidIrParse, DotParseError> {
        self.skip_whitespace();

        // Optional "strict"
        if self.input[self.pos..]
            .to_ascii_lowercase()
            .starts_with("strict")
        {
            for _ in 0..6 {
                self.advance();
            }
            self.skip_whitespace();
        }

        // graph or digraph
        let lower = self.input[self.pos..].to_ascii_lowercase();
        if lower.starts_with("digraph") {
            self.is_digraph = true;
            for _ in 0..7 {
                self.advance();
            }
        } else if lower.starts_with("graph") {
            self.is_digraph = false;
            for _ in 0..5 {
                self.advance();
            }
        } else {
            return Err(DotParseError {
                message: "expected 'graph' or 'digraph'".to_string(),
                line: self.line,
                col: self.col,
            });
        }

        self.skip_whitespace();

        // Optional graph name
        let _graph_name = if self.peek() != Some(b'{') {
            self.read_id()
        } else {
            None
        };

        self.expect_char(b'{')?;
        self.parse_body(None)?;

        let meta = MermaidDiagramMeta {
            diagram_type: DiagramType::Graph,
            direction: if self.is_digraph {
                GraphDirection::TB
            } else {
                GraphDirection::LR
            },
            support_level: crate::mermaid::MermaidSupportLevel::Supported,
            init: crate::mermaid::MermaidInitParse::default(),
            theme_overrides: crate::mermaid::MermaidThemeOverrides::default(),
            guard: crate::mermaid::MermaidGuardReport::default(),
        };

        let ir = MermaidDiagramIr {
            diagram_type: DiagramType::Graph,
            direction: meta.direction,
            nodes: std::mem::take(&mut self.nodes),
            edges: std::mem::take(&mut self.edges),
            ports: Vec::new(),
            clusters: std::mem::take(&mut self.clusters),
            labels: std::mem::take(&mut self.labels),
            pie_entries: Vec::new(),
            pie_title: None,
            pie_show_data: false,
            style_refs: Vec::new(),
            links: Vec::new(),
            meta,
            constraints: Vec::new(),
        };

        Ok(MermaidIrParse {
            ir,
            warnings: std::mem::take(&mut self.warnings),
            errors: std::mem::take(&mut self.errors),
        })
    }

    fn parse_body(&mut self, cluster_id: Option<IrClusterId>) -> Result<(), DotParseError> {
        let mut cluster_members: Vec<IrNodeId> = Vec::new();

        loop {
            self.skip_whitespace();
            if self.at_end() || self.peek() == Some(b'}') {
                break;
            }

            let start = self.current_pos();

            // Check for subgraph
            let lower_rest = self.input[self.pos..].to_ascii_lowercase();
            if lower_rest.starts_with("subgraph") {
                self.parse_subgraph()?;
                continue;
            }

            // Check for graph/node/edge attribute statements
            if lower_rest.starts_with("graph ")
                || lower_rest.starts_with("graph\t")
                || lower_rest.starts_with("graph[")
            {
                // Skip "graph" keyword
                for _ in 0..5 {
                    self.advance();
                }
                let _attrs = self.read_attrs();
                self.consume_optional_semicolon();
                continue;
            }
            if lower_rest.starts_with("node ")
                || lower_rest.starts_with("node\t")
                || lower_rest.starts_with("node[")
            {
                for _ in 0..4 {
                    self.advance();
                }
                let _attrs = self.read_attrs();
                self.consume_optional_semicolon();
                continue;
            }
            if lower_rest.starts_with("edge ")
                || lower_rest.starts_with("edge\t")
                || lower_rest.starts_with("edge[")
            {
                for _ in 0..4 {
                    self.advance();
                }
                let _attrs = self.read_attrs();
                self.consume_optional_semicolon();
                continue;
            }

            // Try to read a node ID
            let Some(first_id) = self.read_id() else {
                // Skip unknown character
                self.advance();
                continue;
            };

            self.skip_whitespace();

            // Check if this is an edge statement
            let edge_op = if self.is_digraph { "->" } else { "--" };
            if self.input[self.pos..].starts_with(edge_op) {
                // Edge chain: A -> B -> C [attrs]
                let mut chain = vec![first_id];
                while self.input[self.pos..].starts_with(edge_op) {
                    // Consume edge operator
                    self.advance();
                    self.advance();
                    self.skip_whitespace();
                    if let Some(next_id) = self.read_id() {
                        chain.push(next_id);
                    } else {
                        break;
                    }
                    self.skip_whitespace();
                }

                let attrs = self.read_attrs();
                let edge_label = attrs.get("label").map(|l| self.intern_label(l));

                let span = self.span_from(start);
                let arrow = if self.is_digraph {
                    "-->".to_string()
                } else {
                    "---".to_string()
                };

                // Create edges for the chain
                for pair in chain.windows(2) {
                    let from_id = self.ensure_node(&pair[0], span);
                    let to_id = self.ensure_node(&pair[1], span);
                    cluster_members.push(from_id);
                    cluster_members.push(to_id);
                    self.edges.push(IrEdge {
                        from: IrEndpoint::Node(from_id),
                        to: IrEndpoint::Node(to_id),
                        arrow: arrow.clone(),
                        label: edge_label,
                        style_ref: None,
                        span,
                    });
                }
            } else {
                // Node declaration
                let attrs = self.read_attrs();
                let span = self.span_from(start);
                let node_id = self.ensure_node(&first_id, span);
                self.apply_node_attrs(node_id, &attrs);
                cluster_members.push(node_id);
            }

            self.consume_optional_semicolon();
        }

        // Consume closing brace
        if self.peek() == Some(b'}') {
            self.advance();
        }

        // If we're inside a cluster, register the members
        if let Some(cid) = cluster_id {
            // Deduplicate members
            let mut deduped = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for m in cluster_members {
                if seen.insert(m.0) {
                    deduped.push(m);
                }
            }
            self.clusters[cid.0].members = deduped;
        }

        Ok(())
    }

    fn parse_subgraph(&mut self) -> Result<(), DotParseError> {
        let start = self.current_pos();

        // Skip "subgraph"
        for _ in 0..8 {
            self.advance();
        }
        self.skip_whitespace();

        // Optional subgraph name
        let name = if self.peek() != Some(b'{') {
            self.read_id()
        } else {
            None
        };

        let cluster_id = IrClusterId(self.clusters.len());
        let title = name.as_ref().map(|n| {
            // Strip "cluster_" prefix for display
            let display = n.strip_prefix("cluster_").unwrap_or(n);
            self.intern_label(display)
        });

        self.clusters.push(IrCluster {
            id: cluster_id,
            title,
            members: Vec::new(),
            span: self.span_from(start),
        });

        self.cluster_counter += 1;

        self.expect_char(b'{')?;
        self.parse_body(Some(cluster_id))?;

        Ok(())
    }

    fn consume_optional_semicolon(&mut self) {
        self.skip_whitespace();
        if self.peek() == Some(b';') {
            self.advance();
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Strip HTML tags from a string (for HTML labels).
fn strip_html_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        if c == '<' {
            in_tag = true;
        } else if c == '>' {
            in_tag = false;
        } else if !in_tag {
            out.push(c);
        }
    }
    out
}

/// Skip leading C-style and hash comments.
fn skip_leading_comments(input: &str) -> &str {
    let mut s = input.trim_start();
    loop {
        if s.starts_with("//") {
            if let Some(newline) = s.find('\n') {
                s = s[newline + 1..].trim_start();
            } else {
                return "";
            }
        } else if s.starts_with("/*") {
            if let Some(end) = s.find("*/") {
                s = s[end + 2..].trim_start();
            } else {
                return "";
            }
        } else if s.starts_with('#') {
            if let Some(newline) = s.find('\n') {
                s = s[newline + 1..].trim_start();
            } else {
                return "";
            }
        } else {
            return s;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_digraph() {
        let input = r#"digraph G {
            A -> B;
            B -> C;
        }"#;
        let result = parse_dot(input).unwrap();
        assert_eq!(result.ir.nodes.len(), 3);
        assert_eq!(result.ir.edges.len(), 2);
        assert_eq!(result.ir.nodes[0].id, "A");
        assert_eq!(result.ir.nodes[1].id, "B");
        assert_eq!(result.ir.nodes[2].id, "C");
    }

    #[test]
    fn parse_simple_graph() {
        let input = r#"graph {
            A -- B;
            B -- C;
        }"#;
        let result = parse_dot(input).unwrap();
        assert_eq!(result.ir.nodes.len(), 3);
        assert_eq!(result.ir.edges.len(), 2);
    }

    #[test]
    fn parse_node_attributes() {
        let input = r#"digraph {
            A [label="Node A" shape=diamond];
            B [label="Node B" shape=circle];
            A -> B;
        }"#;
        let result = parse_dot(input).unwrap();
        assert_eq!(result.ir.nodes.len(), 2);
        assert_eq!(result.ir.nodes[0].shape, NodeShape::Diamond);
        assert_eq!(result.ir.nodes[1].shape, NodeShape::Circle);
        // Check labels
        let a_label = result.ir.nodes[0].label.unwrap();
        assert_eq!(result.ir.labels[a_label.0].text, "Node A");
    }

    #[test]
    fn parse_edge_chain() {
        let input = r#"digraph {
            A -> B -> C -> D;
        }"#;
        let result = parse_dot(input).unwrap();
        assert_eq!(result.ir.nodes.len(), 4);
        assert_eq!(result.ir.edges.len(), 3);
    }

    #[test]
    fn parse_edge_with_label() {
        let input = r#"digraph {
            A -> B [label="connects to"];
        }"#;
        let result = parse_dot(input).unwrap();
        assert_eq!(result.ir.edges.len(), 1);
        let edge_label = result.ir.edges[0].label.unwrap();
        assert_eq!(result.ir.labels[edge_label.0].text, "connects to");
    }

    #[test]
    fn parse_subgraph_cluster() {
        let input = r#"digraph {
            subgraph cluster_0 {
                A; B;
            }
            subgraph cluster_1 {
                C; D;
            }
            A -> C;
        }"#;
        let result = parse_dot(input).unwrap();
        assert_eq!(result.ir.clusters.len(), 2);
        assert_eq!(result.ir.clusters[0].members.len(), 2);
        assert_eq!(result.ir.clusters[1].members.len(), 2);
        // Title should strip "cluster_" prefix
        let title0 = result.ir.clusters[0].title.unwrap();
        assert_eq!(result.ir.labels[title0.0].text, "0");
    }

    #[test]
    fn parse_strict_digraph() {
        let input = r#"strict digraph {
            A -> B;
        }"#;
        let result = parse_dot(input).unwrap();
        assert_eq!(result.ir.nodes.len(), 2);
        assert_eq!(result.ir.edges.len(), 1);
    }

    #[test]
    fn parse_quoted_node_ids() {
        let input = r#"digraph {
            "node 1" -> "node 2";
        }"#;
        let result = parse_dot(input).unwrap();
        assert_eq!(result.ir.nodes[0].id, "node 1");
        assert_eq!(result.ir.nodes[1].id, "node 2");
    }

    #[test]
    fn parse_comments() {
        let input = r#"
        // This is a comment
        digraph G {
            /* block comment */
            A -> B; // inline
            # hash comment
            B -> C;
        }"#;
        let result = parse_dot(input).unwrap();
        assert_eq!(result.ir.nodes.len(), 3);
        assert_eq!(result.ir.edges.len(), 2);
    }

    #[test]
    fn parse_node_default_attrs() {
        // Global node/edge/graph attribute statements should be skipped
        let input = r#"digraph {
            node [shape=box];
            edge [color=red];
            graph [rankdir=LR];
            A -> B;
        }"#;
        let result = parse_dot(input).unwrap();
        assert_eq!(result.ir.nodes.len(), 2);
        assert_eq!(result.ir.edges.len(), 1);
    }

    #[test]
    fn dot_shape_mapping() {
        assert_eq!(dot_shape_to_node_shape("box"), NodeShape::Rect);
        assert_eq!(dot_shape_to_node_shape("rectangle"), NodeShape::Rect);
        assert_eq!(dot_shape_to_node_shape("ellipse"), NodeShape::Rounded);
        assert_eq!(dot_shape_to_node_shape("circle"), NodeShape::Circle);
        assert_eq!(dot_shape_to_node_shape("diamond"), NodeShape::Diamond);
        assert_eq!(dot_shape_to_node_shape("hexagon"), NodeShape::Hexagon);
        assert_eq!(dot_shape_to_node_shape("record"), NodeShape::Subroutine);
        assert_eq!(dot_shape_to_node_shape("unknown"), NodeShape::Rect);
    }

    #[test]
    fn looks_like_dot_detection() {
        assert!(looks_like_dot("digraph G { }"));
        assert!(looks_like_dot("strict digraph { }"));
        assert!(looks_like_dot("  digraph { }"));
        assert!(looks_like_dot("// comment\ndigraph { }"));
        assert!(looks_like_dot("graph G { }"));
        assert!(looks_like_dot("graph { }"));
        assert!(looks_like_dot("graph\t{ }"));
        assert!(looks_like_dot("graph\n{ }"));

        // DOT graph names that START WITH a Mermaid direction should still be DOT
        assert!(looks_like_dot("graph tdb { }"));
        assert!(looks_like_dot("graph btree { }"));
        assert!(looks_like_dot("graph lrp { }"));
        assert!(looks_like_dot("graph rlx { }"));

        // Mermaid formats should NOT match
        assert!(!looks_like_dot("graph TD"));
        assert!(!looks_like_dot("graph LR"));
        assert!(!looks_like_dot("graph RL"));
        assert!(!looks_like_dot("graph BT"));
        assert!(!looks_like_dot("graph TB"));
        assert!(!looks_like_dot("graph td;"));
        assert!(!looks_like_dot("flowchart LR"));

        // Not valid DOT or Mermaid
        assert!(!looks_like_dot("graphFoo { }"));
        assert!(!looks_like_dot("random text"));
    }

    #[test]
    fn parse_empty_graph() {
        let input = "digraph {}";
        let result = parse_dot(input).unwrap();
        assert_eq!(result.ir.nodes.len(), 0);
        assert_eq!(result.ir.edges.len(), 0);
    }

    #[test]
    fn parse_escaped_quotes_in_labels() {
        let input = r#"digraph {
            A [label="say \"hello\""];
        }"#;
        let result = parse_dot(input).unwrap();
        let label_id = result.ir.nodes[0].label.unwrap();
        assert_eq!(result.ir.labels[label_id.0].text, "say \"hello\"");
    }

    #[test]
    fn parse_complex_graph() {
        // Realistic cargo dependency graph snippet
        let input = r#"digraph {
            "ftui-core" [label="ftui-core\n0.1.0"];
            "ftui-render" [label="ftui-render\n0.1.0"];
            "ftui-style" [label="ftui-style\n0.1.0"];
            "ftui-text" [label="ftui-text\n0.1.0"];
            "ftui-layout" [label="ftui-layout\n0.1.0"];
            "ftui-render" -> "ftui-core";
            "ftui-render" -> "ftui-style";
            "ftui-text" -> "ftui-style";
            "ftui-layout" -> "ftui-core";
        }"#;
        let result = parse_dot(input).unwrap();
        assert_eq!(result.ir.nodes.len(), 5);
        assert_eq!(result.ir.edges.len(), 4);
    }

    #[test]
    fn parse_error_missing_brace() {
        let err = parse_dot("digraph G").unwrap_err();
        assert!(err.message.contains("expected '{'"), "got: {}", err.message);
    }

    #[test]
    fn nodes_are_deduped() {
        let input = r#"digraph {
            A -> B;
            A -> C;
            B -> C;
        }"#;
        let result = parse_dot(input).unwrap();
        assert_eq!(result.ir.nodes.len(), 3);
        assert_eq!(result.ir.edges.len(), 3);
    }

    #[test]
    fn node_with_multiple_attrs() {
        let input = r#"digraph {
            A [label="Alpha", shape=diamond, color=red, style=filled];
        }"#;
        let result = parse_dot(input).unwrap();
        assert_eq!(result.ir.nodes[0].shape, NodeShape::Diamond);
        let label_id = result.ir.nodes[0].label.unwrap();
        assert_eq!(result.ir.labels[label_id.0].text, "Alpha");
    }

    #[test]
    fn parse_html_label() {
        let input = r#"digraph {
            A [label=<Hello <b>World</b>>];
        }"#;
        let result = parse_dot(input).unwrap();
        let label_id = result.ir.nodes[0].label.unwrap();
        // HTML tags should be stripped
        assert_eq!(result.ir.labels[label_id.0].text, "Hello World");
    }

    #[test]
    fn parse_escape_sequences_in_labels() {
        let input = "digraph { A [label=\"line1\\nline2\\ttab\\\\back\"]; }";
        let result = parse_dot(input).unwrap();
        let label_id = result.ir.nodes[0].label.unwrap();
        assert_eq!(result.ir.labels[label_id.0].text, "line1\nline2\ttab\\back");
    }

    #[test]
    fn parse_boolean_attrs_without_value() {
        let input = r#"digraph {
            A [filled; bold; label="X"];
        }"#;
        let result = parse_dot(input).unwrap();
        // Should parse without error and pick up the label
        let label_id = result.ir.nodes[0].label.unwrap();
        assert_eq!(result.ir.labels[label_id.0].text, "X");
    }

    #[test]
    fn parse_named_graph() {
        let input = r#"digraph MyGraph {
            A -> B;
        }"#;
        let result = parse_dot(input).unwrap();
        assert_eq!(result.ir.nodes.len(), 2);
        assert_eq!(result.ir.edges.len(), 1);
    }

    #[test]
    fn dot_parse_error_display() {
        let err = DotParseError {
            message: "bad token".to_string(),
            line: 3,
            col: 7,
        };
        let s = format!("{err}");
        assert!(s.contains("3:7"));
        assert!(s.contains("bad token"));
    }

    #[test]
    fn dot_shape_mapping_case_insensitive() {
        assert_eq!(dot_shape_to_node_shape("BOX"), NodeShape::Rect);
        assert_eq!(dot_shape_to_node_shape("Diamond"), NodeShape::Diamond);
        assert_eq!(dot_shape_to_node_shape("ELLIPSE"), NodeShape::Rounded);
    }

    #[test]
    fn dot_shape_parallelogram_and_plain() {
        assert_eq!(
            dot_shape_to_node_shape("parallelogram"),
            NodeShape::Asymmetric
        );
        assert_eq!(dot_shape_to_node_shape("plaintext"), NodeShape::Rect);
        assert_eq!(dot_shape_to_node_shape("plain"), NodeShape::Rect);
        assert_eq!(dot_shape_to_node_shape("none"), NodeShape::Rect);
        assert_eq!(dot_shape_to_node_shape("mrecord"), NodeShape::Subroutine);
    }

    #[test]
    fn strip_html_tags_nested() {
        assert_eq!(strip_html_tags("<a><b>text</b></a>"), "text");
        assert_eq!(strip_html_tags("no tags"), "no tags");
        assert_eq!(strip_html_tags("<br/>"), "");
        assert_eq!(strip_html_tags("a<i>b</i>c"), "abc");
    }

    #[test]
    fn skip_leading_comments_strips_all_comment_types() {
        assert_eq!(skip_leading_comments("// line\ndigraph"), "digraph");
        assert_eq!(skip_leading_comments("/* block */digraph"), "digraph");
        assert_eq!(skip_leading_comments("# hash\ndigraph"), "digraph");
        // Unclosed comments
        assert_eq!(skip_leading_comments("// no newline"), "");
        assert_eq!(skip_leading_comments("/* never closed"), "");
    }

    #[test]
    fn parse_missing_closing_brace() {
        // Missing } should still parse without panicking
        let input = "digraph { A -> B;";
        let result = parse_dot(input);
        // Should succeed (parser is lenient about missing })
        assert!(result.is_ok());
    }

    #[test]
    fn parse_semicolon_separated_attrs() {
        let input = r#"digraph {
            A [label="one"; shape=circle];
        }"#;
        let result = parse_dot(input).unwrap();
        assert_eq!(result.ir.nodes[0].shape, NodeShape::Circle);
        let label_id = result.ir.nodes[0].label.unwrap();
        assert_eq!(result.ir.labels[label_id.0].text, "one");
    }

    #[test]
    fn looks_like_dot_block_comment_before_digraph() {
        assert!(looks_like_dot("/* header */\ndigraph { }"));
    }

    #[test]
    fn parse_subgraph_without_name() {
        let input = r#"digraph {
            subgraph {
                X; Y;
            }
            X -> Y;
        }"#;
        let result = parse_dot(input).unwrap();
        assert_eq!(result.ir.nodes.len(), 2);
        // Cluster should exist but without a title
        assert_eq!(result.ir.clusters.len(), 1);
        assert!(result.ir.clusters[0].title.is_none());
    }
}
