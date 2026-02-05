#![forbid(unsafe_code)]

//! Mermaid showcase screen — state + command handling scaffold.

use std::cell::RefCell;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind};
use ftui_core::geometry::Rect;
use ftui_extras::mermaid;
use ftui_extras::mermaid::{
    MermaidCompatibilityMatrix, MermaidConfig, MermaidDiagramIr, MermaidError,
    MermaidFallbackPolicy, MermaidGlyphMode, MermaidRenderMode, MermaidTier, MermaidWrapMode,
};
use ftui_extras::mermaid_layout;
use ftui_extras::mermaid_render;
use ftui_layout::{Constraint, Flex};
use ftui_render::buffer::Buffer;
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::Style;
use ftui_text::{Line, Span, Text};
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::paragraph::Paragraph;

use super::{HelpEntry, Screen};
use crate::determinism;
use crate::test_logging::{TEST_JSONL_SCHEMA, escape_json, jsonl_enabled};
use crate::theme;

const ZOOM_STEP: f32 = 0.1;
const ZOOM_MIN: f32 = 0.2;
const ZOOM_MAX: f32 = 3.0;
const VIEWPORT_OVERRIDE_DEFAULT_COLS: u16 = 80;
const VIEWPORT_OVERRIDE_DEFAULT_ROWS: u16 = 24;
const VIEWPORT_OVERRIDE_MIN_COLS: u16 = 1;
const VIEWPORT_OVERRIDE_MIN_ROWS: u16 = 1;
const VIEWPORT_OVERRIDE_STEP_COLS: i16 = 4;
const VIEWPORT_OVERRIDE_STEP_ROWS: i16 = 2;

const MERMAID_JSONL_EVENT: &str = "mermaid_render";
static MERMAID_JSONL_SEQ: AtomicU64 = AtomicU64::new(0);

// ── Performance thresholds (good / ok / bad) ────────────────────────
/// Parse time thresholds in milliseconds.
const PARSE_MS_GOOD: f32 = 1.0;
const PARSE_MS_OK: f32 = 5.0;
/// Layout time thresholds in milliseconds.
const LAYOUT_MS_GOOD: f32 = 5.0;
const LAYOUT_MS_OK: f32 = 20.0;
/// Render time thresholds in milliseconds.
const RENDER_MS_GOOD: f32 = 8.0;
const RENDER_MS_OK: f32 = 16.0;
/// Objective score thresholds (lower is better).
const SCORE_GOOD: f32 = 5.0;
const SCORE_OK: f32 = 15.0;
/// Edge crossing thresholds (lower is better).
const CROSSINGS_GOOD: u32 = 0;
const CROSSINGS_OK: u32 = 3;
/// Symmetry thresholds (higher is better, 0.0-1.0).
const SYMMETRY_GOOD: f32 = 0.7;
const SYMMETRY_OK: f32 = 0.4;
/// Compactness thresholds (higher is better, 0.0-1.0).
const COMPACTNESS_GOOD: f32 = 0.3;
const COMPACTNESS_OK: f32 = 0.1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MetricLevel {
    Good,
    Ok,
    Bad,
}

impl MetricLevel {
    /// Return the foreground color for this metric level.
    fn color(self) -> theme::ColorToken {
        match self {
            Self::Good => theme::accent::SUCCESS,
            Self::Ok => theme::accent::WARNING,
            Self::Bad => theme::accent::ERROR,
        }
    }
}

/// Classify a "lower is better" metric.
fn classify_lower(value: f32, good: f32, ok: f32) -> MetricLevel {
    if value <= good {
        MetricLevel::Good
    } else if value <= ok {
        MetricLevel::Ok
    } else {
        MetricLevel::Bad
    }
}

/// Classify a "lower is better" integer metric.
fn classify_lower_u32(value: u32, good: u32, ok: u32) -> MetricLevel {
    if value <= good {
        MetricLevel::Good
    } else if value <= ok {
        MetricLevel::Ok
    } else {
        MetricLevel::Bad
    }
}

/// Classify a "higher is better" metric.
fn classify_higher(value: f32, good: f32, ok: f32) -> MetricLevel {
    if value >= good {
        MetricLevel::Good
    } else if value >= ok {
        MetricLevel::Ok
    } else {
        MetricLevel::Bad
    }
}

fn push_opt_f32(json: &mut String, key: &str, value: Option<f32>) {
    json.push_str(&format!(",\"{key}\":"));
    if let Some(v) = value
        && v.is_finite()
    {
        json.push_str(&format!("{v:.3}"));
    } else {
        json.push_str("null");
    }
}

fn push_opt_u32(json: &mut String, key: &str, value: Option<u32>) {
    json.push_str(&format!(",\"{key}\":"));
    if let Some(v) = value {
        json.push_str(&v.to_string());
    } else {
        json.push_str("null");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LayoutMode {
    Auto,
    Dense,
    Spacious,
}

impl LayoutMode {
    const fn next(self) -> Self {
        match self {
            Self::Auto => Self::Dense,
            Self::Dense => Self::Spacious,
            Self::Spacious => Self::Auto,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Dense => "Dense",
            Self::Spacious => "Spacious",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct MermaidSample {
    name: &'static str,
    kind: &'static str,
    complexity: &'static str,
    tags: &'static [&'static str],
    features: &'static [&'static str],
    edge_cases: &'static [&'static str],
    source: &'static str,
}

// =============================================================================
// Feature Matrix: Mermaid capabilities → demo coverage
// =============================================================================
//
// This matrix maps every supported diagram family and syntax feature to the
// sample(s) that exercise it. Gaps are noted as TODOs for future samples.
//
// ## Diagram Families
//
// | Family       | Type Enum   | Samples                              | Coverage |
// |-------------|-------------|--------------------------------------|----------|
// | Flowchart   | Graph       | Flow Basic, Subgraphs, Dense,        | Good     |
// |             |             | Long Labels, Unicode, Styles         |          |
// | Sequence    | Sequence    | Seq Mini, Seq Checkout, Seq Dense    | Good     |
// | Class       | Class       | Class Basic, Class Members           | Moderate |
// | State       | State       | State Basic, State Composite         | Moderate |
// | ER          | Er          | ER Basic                             | Minimal  |
// | Gantt       | Gantt       | Gantt Basic                          | Minimal  |
// | Mindmap     | Mindmap     | Mindmap Seed, Mindmap Deep           | Moderate |
// | Pie         | Pie         | Pie Basic, Pie Many                  | Good     |
// | Gitgraph    | (unsupported) | Gitgraph Basic (fallback test)     | N/A      |
// | Journey     | (unsupported) | Journey Basic (fallback test)       | N/A      |
// | Requirement | (unsupported) | Requirement Basic (fallback test)   | N/A      |
//
// ## Syntax Features
//
// | Feature               | Samples That Exercise It          | Gaps/TODOs                    |
// |----------------------|-----------------------------------|-------------------------------|
// | Node shapes: []      | Flow Basic, all flow samples      | —                             |
// | Node shapes: {}      | Flow Basic (decision diamond)     | —                             |
// | Node shapes: ()      | (none explicitly)                 | TODO: rounded node sample     |
// | Node shapes: ([])    | (none explicitly)                 | TODO: stadium shape sample    |
// | Node shapes: [[]]    | (none explicitly)                 | TODO: subroutine sample       |
// | Node shapes: {{}}    | (none explicitly)                 | TODO: hexagon sample          |
// | Node shapes: (())    | (none explicitly)                 | TODO: circle shape sample     |
// | Node shapes: >]      | (none explicitly)                 | TODO: asymmetric shape sample |
// | Edge labels          | Flow Basic, Flow Long Labels,     | —                             |
// |                      | Flow Subgraphs                    |                               |
// | Dotted edges -.->    | Sequence Checkout                 | TODO: flow sample with dotted |
// | Thick edges ==>      | (none explicitly)                 | TODO: thick edge sample       |
// | Bidir edges <-->     | (none explicitly)                 | TODO: bidirectional sample    |
// | Endpoint markers o/x | (none explicitly)                 | TODO: marker endpoint sample  |
// | Subgraphs            | Flow Subgraphs                    | —                             |
// | Nested subgraphs     | Flow Subgraphs                    | —                             |
// | classDef             | Flow Styles                       | —                             |
// | style directive      | Flow Styles                       | —                             |
// | linkStyle            | (none explicitly)                 | TODO: linkStyle sample        |
// | init directives      | (off by default)                  | TODO: init directive sample   |
// | click/link           | (off by default)                  | TODO: link sample             |
// | Unicode labels       | Flow Unicode                      | —                             |
// | Long/wrap labels     | Flow Long Labels                  | —                             |
// | ER cardinality       | ER Basic                          | —                             |
// | Class members        | Class Members                     | —                             |
// | State composites     | State Composite                   | —                             |
// | State notes          | State Composite                   | —                             |
// | Gantt sections       | Gantt Basic                       | —                             |
// | Gantt tasks          | Gantt Basic                       | —                             |
// | Pie showData         | Pie Basic                         | —                             |
// | Pie many slices      | Pie Many                          | —                             |
// | Mindmap deep nesting | Mindmap Deep                      | —                             |
//
// ## Layout Directions
//
// | Direction | Samples                  | Gaps/TODOs                          |
// |-----------|--------------------------|-------------------------------------|
// | LR        | Flow Basic               | —                                   |
// | TB        | Flow Subgraphs, most     | —                                   |
// | RL        | (none explicitly)        | TODO: RL direction sample           |
// | BT        | (none explicitly)        | TODO: BT direction sample           |
//
// ## Rendering Features
//
// | Feature         | Exercised By        | Notes                               |
// |----------------|---------------------|-------------------------------------|
// | Braille mode   | Runtime toggle (r)  | All samples via render_mode cycling  |
// | Block mode     | Runtime toggle (r)  | All samples via render_mode cycling  |
// | HalfBlock mode | Runtime toggle (r)  | All samples via render_mode cycling  |
// | CellOnly mode  | Runtime toggle (r)  | All samples via render_mode cycling  |
// | Zoom in/out    | Runtime toggle (+/-) | All samples                         |
// | Palette cycle  | Runtime toggle (s)  | All samples                         |
// | Tier control   | Runtime toggle (t)  | All samples                         |
// | Wrap mode      | Runtime toggle (w)  | All samples via wrap_mode cycling    |
//
// ## Stress/Performance Features
//
// | Feature           | Samples                 | Notes                           |
// |------------------|-------------------------|----------------------------------|
// | Large graph      | Flow Dense (>20 nodes)  | Edge crossing stress             |
// | Many messages    | Sequence Dense          | Tight vertical spacing           |
// | Deep hierarchy   | Mindmap Deep (5 levels) | Layout depth stress              |
// | Many pie slices  | Pie Many (6+ slices)    | Small-slice rendering            |
// | Long labels      | Flow Long Labels        | Wrapping/truncation stress       |
// | Unicode width    | Flow Unicode            | CJK/emoji width handling         |
//
// ## Sample Purpose Registry
//
// Every sample below has a declared purpose and feature coverage tag.
// The `features` field lists syntax features exercised.
// The `edge_cases` field lists rendering edge cases tested.
// The `tags` field provides searchable categories.
// =============================================================================

/// Known diagram feature tag for coverage tracking.
///
/// Each tag represents a specific Mermaid syntax or rendering capability.
/// Samples declare which tags they exercise via their `features` field.
/// Gaps (features with no sample) are listed below as TODOs.
const KNOWN_FEATURE_TAGS: &[&str] = &[
    // Syntax features
    "basic-nodes",
    "edge-labels",
    "subgraph",
    "classDef",
    "style",
    "unicode-labels",
    "long-labels",
    "many-nodes",
    "many-edges",
    // Sequence features
    "messages",
    "responses",
    "round-trip",
    "multi-actor",
    "many-messages",
    // Class features
    "relations",
    "class-members",
    // State features
    "state-edges",
    "substates",
    "notes",
    // ER features
    "er-arrows",
    // Gantt features
    "title",
    "sections",
    // Mindmap features
    "indent",
    "multi-level",
    // Pie features
    "showData",
    "labels",
    // Unsupported diagram fallback
    "branches",
    "commits",
    "scores",
    "requirements",
];

/// Features known to be supported but lacking dedicated samples.
/// Each entry is (feature_tag, description).
const FEATURE_GAPS: &[(&str, &str)] = &[
    (
        "node-rounded",
        "Rounded node shape () — no sample exercises this",
    ),
    (
        "node-stadium",
        "Stadium node shape ([]) — no sample exercises this",
    ),
    (
        "node-subroutine",
        "Subroutine node shape [[]] — no sample exercises this",
    ),
    (
        "node-hexagon",
        "Hexagon node shape {{}} — no sample exercises this",
    ),
    (
        "node-circle",
        "Circle node shape (()) — no sample exercises this",
    ),
    (
        "node-asymmetric",
        "Asymmetric node shape >] — no sample exercises this",
    ),
    (
        "dotted-edges",
        "Dotted edges -.-> in flowcharts — only used in sequence",
    ),
    ("thick-edges", "Thick edges ==> — no sample exercises this"),
    (
        "bidir-edges",
        "Bidirectional edges <--> — no sample exercises this",
    ),
    (
        "endpoint-markers",
        "Endpoint markers o--o, x--x — no sample exercises this",
    ),
    (
        "linkStyle",
        "linkStyle directive — no sample exercises this",
    ),
    (
        "direction-rl",
        "Right-to-left layout — no sample exercises this",
    ),
    (
        "direction-bt",
        "Bottom-to-top layout — no sample exercises this",
    ),
];

const DEFAULT_SAMPLES: &[MermaidSample] = &[
    MermaidSample {
        name: "Flow Basic",
        kind: "flow",
        complexity: "S",
        tags: &["branch", "decision"],
        features: &["edge-labels", "basic-nodes"],
        edge_cases: &[],
        source: r#"graph LR
A[Start] --> B{Check}
B -->|Yes| C[OK]
B -->|No| D[Fix]"#,
    },
    MermaidSample {
        name: "Flow Subgraphs",
        kind: "flow",
        complexity: "M",
        tags: &["subgraph", "clusters"],
        features: &["subgraph", "edge-labels"],
        edge_cases: &["nested-grouping"],
        source: r#"graph TB
  subgraph Cluster_A
    A1[Ingress] --> A2[Parse]
  end
  subgraph Cluster_B
    B1[Store] --> B2[Report]
  end
  A2 -->|ok| B1
  A2 -->|err| B2"#,
    },
    MermaidSample {
        name: "Flow Dense",
        kind: "flow",
        complexity: "L",
        tags: &["dense", "dag"],
        features: &["many-nodes", "many-edges"],
        edge_cases: &["edge-crossing"],
        source: r#"graph LR
  A-->B
  A-->C
  B-->D
  C-->D
  D-->E
  E-->F
  F-->G
  C-->H
  H-->I
  I-->J
  J-->K"#,
    },
    MermaidSample {
        name: "Flow Long Labels",
        kind: "flow",
        complexity: "M",
        tags: &["labels", "wrap"],
        features: &["long-labels", "edge-labels"],
        edge_cases: &["long-text"],
        source: r#"graph TD
  A[This is a very long label that should wrap or truncate neatly] --> B[Another extremely verbose node label]
  B --> C{Decision with a long question that should still render}
  C -->|Yes| D[Proceed to the next step]
  C -->|No| E[Abort with a meaningful explanation]"#,
    },
    MermaidSample {
        name: "Flow Unicode",
        kind: "flow",
        complexity: "S",
        tags: &["unicode", "labels"],
        features: &["unicode-labels"],
        edge_cases: &["non-ascii"],
        source: r#"graph LR
  A[Δ Start] --> B[β-Compute]
  B --> C[東京]
  C --> D[naïve café]"#,
    },
    MermaidSample {
        name: "Flow Styles",
        kind: "flow",
        complexity: "M",
        tags: &["classdef", "style"],
        features: &["classDef", "style"],
        edge_cases: &["style-lines"],
        source: r#"graph LR
  A[Primary] --> B[Secondary]
  B --> C[Accent]
  classDef hot fill:#ff6b6b,stroke:#333,stroke-width:2px;
  class A hot;
  style C fill:#6bc5ff,stroke:#333,stroke-width:2px;"#,
    },
    MermaidSample {
        name: "Sequence Mini",
        kind: "sequence",
        complexity: "S",
        tags: &["compact"],
        features: &["messages", "responses"],
        edge_cases: &[],
        source: r#"sequenceDiagram
  Alice->>Bob: Hello
  Bob-->>Alice: Hi!"#,
    },
    MermaidSample {
        name: "Sequence Checkout",
        kind: "sequence",
        complexity: "M",
        tags: &["multi-hop", "api"],
        features: &["round-trip", "multi-actor"],
        edge_cases: &["mixed-arrows"],
        source: r#"sequenceDiagram
  Client->>API: POST /checkout
  API->>Auth: Validate token
  Auth-->>API: OK
  API->>DB: Create order
  DB-->>API: id=42
  API-->>Client: 201 Created"#,
    },
    MermaidSample {
        name: "Sequence Dense",
        kind: "sequence",
        complexity: "L",
        tags: &["dense", "timing"],
        features: &["many-messages"],
        edge_cases: &["tight-spacing"],
        source: r#"sequenceDiagram
  User->>UI: Click
  UI->>API: Fetch
  API-->>UI: 200 OK
  UI-->>User: Render
  User->>UI: Scroll
  UI->>API: Prefetch
  API-->>UI: 204
  UI-->>User: Update"#,
    },
    MermaidSample {
        name: "Class Basic",
        kind: "class",
        complexity: "S",
        tags: &["inheritance", "association"],
        features: &["relations"],
        edge_cases: &[],
        source: r#"classDiagram
  class User
  class Admin
  class Order
  User <|-- Admin
  User --> Order"#,
    },
    MermaidSample {
        name: "Class Members",
        kind: "class",
        complexity: "M",
        tags: &["fields", "methods"],
        features: &["class-members"],
        edge_cases: &["long-member-lines"],
        source: r#"classDiagram
  class Account
  class Ledger
  Account : +id: UUID
  Account : +balance: f64
  Account : +deposit(amount)
  Ledger : +entries: Vec
  Account --> Ledger"#,
    },
    MermaidSample {
        name: "State Basic",
        kind: "state",
        complexity: "S",
        tags: &["start-end"],
        features: &["state-edges"],
        edge_cases: &[],
        source: r#"stateDiagram-v2
  [*] --> Idle
  Idle --> Busy: start
  Busy --> Idle: done
  Busy --> [*]: exit"#,
    },
    MermaidSample {
        name: "State Composite",
        kind: "state",
        complexity: "M",
        tags: &["composite", "notes"],
        features: &["substates", "notes"],
        edge_cases: &["nested-blocks"],
        source: r#"stateDiagram-v2
  [*] --> Working
  state Working {
    Draft --> Review
    Review --> Approved
    Review --> Rejected
  }
  Working --> [*]
  note right of Review: ensure QA"#,
    },
    MermaidSample {
        name: "ER Basic",
        kind: "er",
        complexity: "M",
        tags: &["cardinality", "relations"],
        features: &["er-arrows"],
        edge_cases: &[],
        source: r#"erDiagram
  CUSTOMER ||--o{ ORDER : places
  ORDER ||--|{ LINE_ITEM : contains
  PRODUCT ||--o{ LINE_ITEM : in"#,
    },
    MermaidSample {
        name: "Gantt Basic",
        kind: "gantt",
        complexity: "M",
        tags: &["sections", "tasks"],
        features: &["title", "sections"],
        edge_cases: &["date-meta"],
        source: r#"gantt
  title Release Plan
  section Build
  Design :a1, 2024-01-01, 5d
  Implement :after a1, 7d
  section Launch
  QA : 2024-01-10, 3d
  Release : milestone, 2024-01-14, 1d"#,
    },
    MermaidSample {
        name: "Mindmap Seed",
        kind: "mindmap",
        complexity: "S",
        tags: &["tree"],
        features: &["indent"],
        edge_cases: &[],
        source: r#"mindmap
  root
    alpha
    beta
      beta-1
      beta-2"#,
    },
    MermaidSample {
        name: "Mindmap Deep",
        kind: "mindmap",
        complexity: "L",
        tags: &["deep", "wide"],
        features: &["multi-level"],
        edge_cases: &["many-nodes"],
        source: r#"mindmap
  roadmap
    discover
      interviews
      audit
        perf
        ux
    build
      api
        auth
        data
      ui
        shell
        widgets
    launch
      beta
      ga"#,
    },
    MermaidSample {
        name: "Pie Basic",
        kind: "pie",
        complexity: "S",
        tags: &["title", "showdata"],
        features: &["title", "showData"],
        edge_cases: &[],
        source: r#"pie showData
  title Market Share
  "Alpha": 38
  "Beta": 27
  "Gamma": 20
  "Delta": 15"#,
    },
    MermaidSample {
        name: "Pie Many",
        kind: "pie",
        complexity: "M",
        tags: &["many-slices"],
        features: &["labels"],
        edge_cases: &["small-slices"],
        source: r#"pie
  title Segments
  Core: 35
  Edge: 22
  Mobile: 18
  Infra: 12
  Labs: 8
  Other: 5"#,
    },
    MermaidSample {
        name: "Gitgraph Basic",
        kind: "gitgraph",
        complexity: "M",
        tags: &["unsupported"],
        features: &["branches", "commits"],
        edge_cases: &["unsupported-diagram"],
        source: r#"gitGraph
  commit
  branch feature
  checkout feature
  commit
  checkout main
  merge feature"#,
    },
    MermaidSample {
        name: "Journey Basic",
        kind: "journey",
        complexity: "M",
        tags: &["unsupported"],
        features: &["sections", "scores"],
        edge_cases: &["unsupported-diagram"],
        source: r#"journey
  title User Onboarding
  section Discover
    Landing: 5: User
    Signup: 4: User
  section Activate
    Tutorial: 3: User
    First task: 5: User"#,
    },
    MermaidSample {
        name: "Requirement Basic",
        kind: "requirement",
        complexity: "M",
        tags: &["unsupported"],
        features: &["requirements"],
        edge_cases: &["unsupported-diagram"],
        source: r#"requirementDiagram
  requirement req1 {
    id: 1
    text: Must render diagrams
    risk: high
    verifyMethod: test
  }"#,
    },
];

#[derive(Debug, Clone, Copy, Default)]
struct MermaidMetricsSnapshot {
    parse_ms: Option<f32>,
    layout_ms: Option<f32>,
    render_ms: Option<f32>,
    layout_iterations: Option<u32>,
    /// Composite layout quality score (lower is better).
    objective_score: Option<f32>,
    /// Edge crossing count (lower is better).
    constraint_violations: Option<u32>,
    /// Total edge bends/waypoints (lower is better).
    bends: Option<u32>,
    /// Symmetry across center axis (0.0-1.0, higher is better).
    symmetry: Option<f32>,
    /// Compactness: node area / bounding box area (0.0-1.0, higher is better).
    compactness: Option<f32>,
    /// Edge length variance (lower = more uniform).
    edge_length_variance: Option<f32>,
    /// Label collision count (lower is better).
    label_collisions: Option<u32>,
    fallback_tier: Option<MermaidTier>,
    fallback_reason: Option<&'static str>,
    /// Number of parse/IR errors encountered.
    error_count: Option<u32>,
}

impl MermaidMetricsSnapshot {
    /// Populate layout quality metrics from a `DiagramLayout` result.
    ///
    /// Uses `evaluate_layout` to compute the composite LayoutObjective,
    /// then stores each metric field for consistent display.
    fn from_layout(layout: &ftui_extras::mermaid_layout::DiagramLayout) -> Self {
        let objective = ftui_extras::mermaid_layout::evaluate_layout(layout);
        Self {
            layout_iterations: Some(layout.stats.iterations_used as u32),
            objective_score: Some(objective.score as f32),
            constraint_violations: Some(objective.crossings as u32),
            bends: Some(objective.bends as u32),
            symmetry: Some(objective.symmetry as f32),
            compactness: Some(objective.compactness as f32),
            edge_length_variance: Some(objective.edge_length_variance as f32),
            label_collisions: Some(objective.label_collisions as u32),
            ..Self::default()
        }
    }

    /// Populate fallback fields from a degradation plan.
    fn set_fallback(
        &mut self,
        tier: MermaidTier,
        plan: &ftui_extras::mermaid::MermaidDegradationPlan,
    ) {
        self.fallback_tier = Some(tier);
        self.fallback_reason = Some(if plan.collapse_clusters {
            "clusters_collapsed"
        } else if plan.simplify_routing {
            "routing_simplified"
        } else if plan.hide_labels {
            "labels_hidden"
        } else if plan.reduce_decoration {
            "decoration_reduced"
        } else if plan.force_glyph_mode.is_some() {
            "glyph_mode_forced"
        } else {
            "layout_budget_exceeded"
        });
    }
}

/// A single entry in the status log.
#[derive(Debug, Clone)]
struct StatusLogEntry {
    action: &'static str,
    detail: String,
}

const STATUS_LOG_CAP: usize = 50;

#[derive(Debug)]
struct MermaidShowcaseState {
    samples: Vec<MermaidSample>,
    selected_index: usize,
    layout_mode: LayoutMode,
    tier: MermaidTier,
    glyph_mode: MermaidGlyphMode,
    render_mode: MermaidRenderMode,
    wrap_mode: MermaidWrapMode,
    styles_enabled: bool,
    metrics_visible: bool,
    controls_visible: bool,
    viewport_zoom: f32,
    viewport_pan: (i16, i16),
    viewport_size_override: Option<(u16, u16)>,
    analysis_epoch: u64,
    layout_epoch: u64,
    render_epoch: u64,
    metrics: MermaidMetricsSnapshot,
    status_log: Vec<StatusLogEntry>,
    status_log_visible: bool,
}

#[derive(Debug)]
struct MermaidRenderCache {
    analysis_epoch: u64,
    layout_epoch: u64,
    render_epoch: u64,
    viewport: (u16, u16),
    zoom: f32,
    ir: Option<MermaidDiagramIr>,
    layout: Option<mermaid_layout::DiagramLayout>,
    buffer: Buffer,
    metrics: MermaidMetricsSnapshot,
    errors: Vec<MermaidError>,
    cache_hits: u64,
    cache_misses: u64,
    last_cache_hit: bool,
}

impl MermaidRenderCache {
    fn empty() -> Self {
        Self {
            analysis_epoch: u64::MAX,
            layout_epoch: u64::MAX,
            render_epoch: u64::MAX,
            viewport: (0, 0),
            zoom: 1.0,
            ir: None,
            layout: None,
            buffer: Buffer::new(1, 1),
            metrics: MermaidMetricsSnapshot::default(),
            errors: Vec::new(),
            cache_hits: 0,
            cache_misses: 0,
            last_cache_hit: false,
        }
    }
}

impl MermaidShowcaseState {
    fn new() -> Self {
        let mut state = Self {
            samples: DEFAULT_SAMPLES.to_vec(),
            selected_index: 0,
            layout_mode: LayoutMode::Auto,
            tier: MermaidTier::Auto,
            glyph_mode: MermaidGlyphMode::Unicode,
            render_mode: MermaidRenderMode::Auto,
            wrap_mode: MermaidWrapMode::WordChar,
            styles_enabled: true,
            metrics_visible: true,
            controls_visible: true,
            viewport_zoom: 1.0,
            viewport_pan: (0, 0),
            viewport_size_override: None,
            analysis_epoch: 0,
            layout_epoch: 0,
            render_epoch: 0,
            metrics: MermaidMetricsSnapshot::default(),
            status_log: Vec::new(),
            status_log_visible: false,
        };
        state.recompute_metrics();
        state
    }

    fn log_action(&mut self, action: &'static str, detail: String) {
        if self.status_log.len() >= STATUS_LOG_CAP {
            self.status_log.remove(0);
        }
        self.status_log.push(StatusLogEntry { action, detail });
    }

    fn selected_sample(&self) -> Option<MermaidSample> {
        self.samples.get(self.selected_index).copied()
    }

    fn normalize(&mut self) {
        self.viewport_zoom = self.viewport_zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        self.clamp_viewport_override();
        self.recompute_metrics();
    }
    /// Run the parse/layout pipeline for the current sample and populate metrics.
    fn recompute_metrics(&mut self) {
        let sample = match self.selected_sample() {
            Some(s) => s,
            None => {
                self.metrics = MermaidMetricsSnapshot::default();
                return;
            }
        };
        let source = sample.source;

        let config = MermaidConfig {
            glyph_mode: self.glyph_mode,
            render_mode: self.render_mode,
            tier_override: self.tier,
            wrap_mode: self.wrap_mode,
            enable_styles: self.styles_enabled,
            ..MermaidConfig::default()
        };
        let matrix = MermaidCompatibilityMatrix::default();
        let policy = MermaidFallbackPolicy::default();

        let t0 = std::time::Instant::now();
        let ast = match ftui_extras::mermaid::parse(source) {
            Ok(ast) => ast,
            Err(_) => {
                self.metrics = MermaidMetricsSnapshot::default();
                return;
            }
        };
        let parse_elapsed = t0.elapsed();

        let ir_parse = ftui_extras::mermaid::normalize_ast_to_ir(&ast, &config, &matrix, &policy);

        let t1 = std::time::Instant::now();
        let spacing = match self.layout_mode {
            LayoutMode::Dense => mermaid_layout::LayoutSpacing {
                rank_gap: 2.0,
                node_gap: 2.0,
                ..mermaid_layout::LayoutSpacing::default()
            },
            LayoutMode::Spacious => mermaid_layout::LayoutSpacing {
                rank_gap: 6.0,
                node_gap: 5.0,
                ..mermaid_layout::LayoutSpacing::default()
            },
            LayoutMode::Auto => mermaid_layout::LayoutSpacing::default(),
        };
        let layout = mermaid_layout::layout_diagram_with_spacing(&ir_parse.ir, &config, &spacing);
        let layout_elapsed = t1.elapsed();

        let mut snap = MermaidMetricsSnapshot::from_layout(&layout);
        snap.parse_ms = Some(parse_elapsed.as_secs_f32() * 1000.0);
        snap.layout_ms = Some(layout_elapsed.as_secs_f32() * 1000.0);
        if let Some(ref plan) = layout.degradation {
            snap.set_fallback(self.tier, plan);
        }
        snap.error_count = Some(ir_parse.errors.len() as u32);
        self.metrics = snap;
        self.emit_metrics_jsonl(sample);
    }

    fn emit_metrics_jsonl(&self, sample: MermaidSample) {
        if !jsonl_enabled() {
            return;
        }
        let seq = MERMAID_JSONL_SEQ.fetch_add(1, Ordering::Relaxed);
        let run_id = determinism::demo_run_id();
        let seed = determinism::demo_seed(0);
        let screen_mode = determinism::demo_screen_mode();
        let line = self.metrics_jsonl_line(sample, seq, run_id.as_deref(), seed, &screen_mode);
        let _ = writeln!(std::io::stderr(), "{line}");
    }

    fn metrics_jsonl_line(
        &self,
        sample: MermaidSample,
        seq: u64,
        run_id: Option<&str>,
        seed: u64,
        screen_mode: &str,
    ) -> String {
        let mut json = String::new();
        json.push('{');
        json.push_str(&format!("\"schema_version\":\"{}\"", TEST_JSONL_SCHEMA));
        json.push_str(&format!(",\"event\":\"{}\"", MERMAID_JSONL_EVENT));
        json.push_str(&format!(",\"seq\":{seq}"));
        if let Some(run_id) = run_id {
            json.push_str(&format!(",\"run_id\":\"{}\"", escape_json(run_id)));
        }
        json.push_str(&format!(",\"seed\":{seed}"));
        json.push_str(&format!(
            ",\"screen_mode\":\"{}\"",
            escape_json(screen_mode)
        ));
        json.push_str(&format!(",\"sample\":\"{}\"", escape_json(sample.name)));
        json.push_str(&format!(
            ",\"layout_mode\":\"{}\"",
            self.layout_mode.as_str()
        ));
        json.push_str(&format!(",\"tier\":\"{}\"", self.tier));
        json.push_str(&format!(",\"glyph_mode\":\"{}\"", self.glyph_mode));
        json.push_str(&format!(",\"wrap_mode\":\"{}\"", self.wrap_mode));
        json.push_str(&format!(",\"render_epoch\":{}", self.render_epoch));
        push_opt_f32(&mut json, "parse_ms", self.metrics.parse_ms);
        push_opt_f32(&mut json, "layout_ms", self.metrics.layout_ms);
        push_opt_f32(&mut json, "render_ms", self.metrics.render_ms);
        push_opt_u32(
            &mut json,
            "layout_iterations",
            self.metrics.layout_iterations,
        );
        push_opt_f32(&mut json, "objective_score", self.metrics.objective_score);
        push_opt_u32(
            &mut json,
            "constraint_violations",
            self.metrics.constraint_violations,
        );
        push_opt_u32(&mut json, "bends", self.metrics.bends);
        push_opt_f32(&mut json, "symmetry", self.metrics.symmetry);
        push_opt_f32(&mut json, "compactness", self.metrics.compactness);
        push_opt_f32(
            &mut json,
            "edge_length_variance",
            self.metrics.edge_length_variance,
        );
        push_opt_u32(&mut json, "label_collisions", self.metrics.label_collisions);
        push_opt_u32(&mut json, "error_count", self.metrics.error_count);
        if let Some(tier) = self.metrics.fallback_tier {
            json.push_str(&format!(",\"fallback_tier\":\"{tier}\""));
        }
        if let Some(reason) = self.metrics.fallback_reason {
            json.push_str(&format!(",\"fallback_reason\":\"{}\"", escape_json(reason)));
        }
        json.push('}');
        json
    }

    fn clamp_viewport_override(&mut self) {
        if let Some((cols, rows)) = self.viewport_size_override {
            let cols = cols.max(VIEWPORT_OVERRIDE_MIN_COLS);
            let rows = rows.max(VIEWPORT_OVERRIDE_MIN_ROWS);
            self.viewport_size_override = Some((cols, rows));
        }
    }

    fn adjust_viewport_override(&mut self, delta_cols: i16, delta_rows: i16) {
        let (cols, rows) = self.viewport_size_override.unwrap_or((
            VIEWPORT_OVERRIDE_DEFAULT_COLS,
            VIEWPORT_OVERRIDE_DEFAULT_ROWS,
        ));
        let cols = (cols as i32 + delta_cols as i32)
            .clamp(VIEWPORT_OVERRIDE_MIN_COLS as i32, u16::MAX as i32) as u16;
        let rows = (rows as i32 + delta_rows as i32)
            .clamp(VIEWPORT_OVERRIDE_MIN_ROWS as i32, u16::MAX as i32) as u16;
        let next = Some((cols, rows));
        if self.viewport_size_override != next {
            self.viewport_size_override = next;
            self.bump_render();
        }
    }

    fn bump_analysis(&mut self) {
        self.analysis_epoch = self.analysis_epoch.saturating_add(1);
    }

    fn bump_layout(&mut self) {
        self.layout_epoch = self.layout_epoch.saturating_add(1);
    }

    fn bump_render(&mut self) {
        self.render_epoch = self.render_epoch.saturating_add(1);
    }

    fn bump_all(&mut self) {
        self.bump_analysis();
        self.bump_layout();
        self.bump_render();
    }

    fn apply_action(&mut self, action: MermaidShowcaseAction) {
        match action {
            MermaidShowcaseAction::NextSample => {
                if !self.samples.is_empty() {
                    self.selected_index = (self.selected_index + 1) % self.samples.len();
                    self.bump_all();
                    let name = self.samples[self.selected_index].name;
                    self.log_action("sample", name.to_string());
                }
            }
            MermaidShowcaseAction::PrevSample => {
                if !self.samples.is_empty() {
                    self.selected_index =
                        (self.selected_index + self.samples.len() - 1) % self.samples.len();
                    self.bump_all();
                    let name = self.samples[self.selected_index].name;
                    self.log_action("sample", name.to_string());
                }
            }
            MermaidShowcaseAction::FirstSample => {
                if !self.samples.is_empty() {
                    self.selected_index = 0;
                    self.bump_all();
                    self.log_action("sample", "first".to_string());
                }
            }
            MermaidShowcaseAction::LastSample => {
                if !self.samples.is_empty() {
                    self.selected_index = self.samples.len() - 1;
                    self.bump_all();
                    self.log_action("sample", "last".to_string());
                }
            }
            MermaidShowcaseAction::Refresh => {
                self.bump_all();
                self.log_action("refresh", String::new());
            }
            MermaidShowcaseAction::ZoomIn => {
                self.viewport_zoom += ZOOM_STEP;
                self.log_action("zoom", format!("{:.0}%", self.viewport_zoom * 100.0));
            }
            MermaidShowcaseAction::ZoomOut => {
                self.viewport_zoom -= ZOOM_STEP;
                self.log_action("zoom", format!("{:.0}%", self.viewport_zoom * 100.0));
            }
            MermaidShowcaseAction::ZoomReset => {
                self.viewport_zoom = 1.0;
                self.viewport_pan = (0, 0);
                self.log_action("zoom", "reset".to_string());
            }
            MermaidShowcaseAction::FitToView => {
                self.viewport_zoom = 1.0;
                self.viewport_pan = (0, 0);
                self.log_action("fit", String::new());
            }
            MermaidShowcaseAction::ToggleLayoutMode => {
                self.layout_mode = self.layout_mode.next();
                self.bump_layout();
                self.bump_render();
                self.log_action("layout", self.layout_mode.as_str().to_string());
            }
            MermaidShowcaseAction::ForceRelayout => {
                self.bump_layout();
                self.bump_render();
                self.log_action("relayout", String::new());
            }
            MermaidShowcaseAction::ToggleMetrics => {
                self.metrics_visible = !self.metrics_visible;
                self.log_action(
                    "metrics",
                    if self.metrics_visible { "on" } else { "off" }.to_string(),
                );
            }
            MermaidShowcaseAction::ToggleControls => {
                self.controls_visible = !self.controls_visible;
                self.log_action(
                    "controls",
                    if self.controls_visible { "on" } else { "off" }.to_string(),
                );
            }
            MermaidShowcaseAction::CycleTier => {
                self.tier = match self.tier {
                    MermaidTier::Auto => MermaidTier::Rich,
                    MermaidTier::Rich => MermaidTier::Normal,
                    MermaidTier::Normal => MermaidTier::Compact,
                    MermaidTier::Compact => MermaidTier::Auto,
                };
                self.bump_all();
                self.log_action("tier", self.tier.to_string());
            }
            MermaidShowcaseAction::ToggleGlyphMode => {
                self.glyph_mode = match self.glyph_mode {
                    MermaidGlyphMode::Unicode => MermaidGlyphMode::Ascii,
                    MermaidGlyphMode::Ascii => MermaidGlyphMode::Unicode,
                };
                self.bump_render();
                self.log_action("glyph", self.glyph_mode.to_string());
            }
            MermaidShowcaseAction::CycleRenderMode => {
                self.render_mode = match self.render_mode {
                    MermaidRenderMode::Auto => MermaidRenderMode::Braille,
                    MermaidRenderMode::Braille => MermaidRenderMode::Block,
                    MermaidRenderMode::Block => MermaidRenderMode::HalfBlock,
                    MermaidRenderMode::HalfBlock => MermaidRenderMode::CellOnly,
                    MermaidRenderMode::CellOnly => MermaidRenderMode::Auto,
                };
                self.bump_render();
                self.log_action("render", self.render_mode.to_string());
            }
            MermaidShowcaseAction::ToggleStyles => {
                self.styles_enabled = !self.styles_enabled;
                self.bump_analysis();
                self.bump_render();
                self.log_action(
                    "styles",
                    if self.styles_enabled { "on" } else { "off" }.to_string(),
                );
            }
            MermaidShowcaseAction::CycleWrapMode => {
                self.wrap_mode = match self.wrap_mode {
                    MermaidWrapMode::None => MermaidWrapMode::Word,
                    MermaidWrapMode::Word => MermaidWrapMode::Char,
                    MermaidWrapMode::Char => MermaidWrapMode::WordChar,
                    MermaidWrapMode::WordChar => MermaidWrapMode::None,
                };
                self.bump_layout();
                self.bump_render();
                self.log_action("wrap", self.wrap_mode.to_string());
            }
            MermaidShowcaseAction::IncreaseViewportWidth => {
                self.adjust_viewport_override(VIEWPORT_OVERRIDE_STEP_COLS, 0);
                self.log_action("viewport", "width +".to_string());
            }
            MermaidShowcaseAction::DecreaseViewportWidth => {
                self.adjust_viewport_override(-VIEWPORT_OVERRIDE_STEP_COLS, 0);
                self.log_action("viewport", "width -".to_string());
            }
            MermaidShowcaseAction::IncreaseViewportHeight => {
                self.adjust_viewport_override(0, VIEWPORT_OVERRIDE_STEP_ROWS);
                self.log_action("viewport", "height +".to_string());
            }
            MermaidShowcaseAction::DecreaseViewportHeight => {
                self.adjust_viewport_override(0, -VIEWPORT_OVERRIDE_STEP_ROWS);
                self.log_action("viewport", "height -".to_string());
            }
            MermaidShowcaseAction::ResetViewportOverride => {
                if self.viewport_size_override.is_some() {
                    self.viewport_size_override = None;
                    self.bump_render();
                    self.log_action("viewport", "reset".to_string());
                }
            }
            MermaidShowcaseAction::CollapsePanels => {
                self.controls_visible = false;
                self.metrics_visible = false;
                self.status_log_visible = false;
                self.log_action("panels", "collapsed".to_string());
            }
            MermaidShowcaseAction::ToggleStatusLog => {
                self.status_log_visible = !self.status_log_visible;
            }
        }
        self.normalize();
    }
}

#[derive(Debug, Clone, Copy)]
enum MermaidShowcaseAction {
    NextSample,
    PrevSample,
    FirstSample,
    LastSample,
    Refresh,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    FitToView,
    ToggleLayoutMode,
    ForceRelayout,
    ToggleMetrics,
    ToggleControls,
    CycleTier,
    ToggleGlyphMode,
    CycleRenderMode,
    ToggleStyles,
    CycleWrapMode,
    IncreaseViewportWidth,
    DecreaseViewportWidth,
    IncreaseViewportHeight,
    DecreaseViewportHeight,
    ResetViewportOverride,
    CollapsePanels,
    ToggleStatusLog,
}

/// Mermaid showcase screen scaffold (state + key handling).
pub struct MermaidShowcaseScreen {
    state: MermaidShowcaseState,
    cache: RefCell<MermaidRenderCache>,
}

impl Default for MermaidShowcaseScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl MermaidShowcaseScreen {
    pub fn new() -> Self {
        Self {
            state: MermaidShowcaseState::new(),
            cache: RefCell::new(MermaidRenderCache::empty()),
        }
    }

    fn build_config(&self) -> MermaidConfig {
        let mut config = MermaidConfig {
            glyph_mode: self.state.glyph_mode,
            tier_override: self.state.tier,
            render_mode: self.state.render_mode,
            wrap_mode: self.state.wrap_mode,
            enable_styles: self.state.styles_enabled,
            ..MermaidConfig::default()
        };

        match self.state.layout_mode {
            LayoutMode::Auto => {}
            LayoutMode::Dense => {
                config.layout_iteration_budget = 400;
                config.route_budget = 8_000;
            }
            LayoutMode::Spacious => {
                config.layout_iteration_budget = 140;
                config.route_budget = 3_000;
            }
        }

        config
    }

    fn layout_spacing(&self) -> mermaid_layout::LayoutSpacing {
        match self.state.layout_mode {
            LayoutMode::Dense => mermaid_layout::LayoutSpacing {
                rank_gap: 2.0,
                node_gap: 2.0,
                ..mermaid_layout::LayoutSpacing::default()
            },
            LayoutMode::Spacious => mermaid_layout::LayoutSpacing {
                rank_gap: 6.0,
                node_gap: 5.0,
                ..mermaid_layout::LayoutSpacing::default()
            },
            LayoutMode::Auto => mermaid_layout::LayoutSpacing::default(),
        }
    }

    fn target_viewport_size(&self, inner: Rect) -> (u16, u16) {
        if let Some((cols, rows)) = self.state.viewport_size_override {
            (cols.max(1), rows.max(1))
        } else {
            (inner.width.max(1), inner.height.max(1))
        }
    }

    fn has_render_error(&self) -> bool {
        !self.cache.borrow().errors.is_empty()
    }

    fn ensure_render_cache(&self, inner: Rect) {
        let (width, height) = self.target_viewport_size(inner);
        let zoom = self.state.viewport_zoom;
        let render_width = (f32::from(width) * zoom)
            .round()
            .clamp(1.0, f32::from(u16::MAX)) as u16;
        let render_height = (f32::from(height) * zoom)
            .round()
            .clamp(1.0, f32::from(u16::MAX)) as u16;

        let mut cache = self.cache.borrow_mut();
        let zoom_matches = (cache.zoom - zoom).abs() <= f32::EPSILON;
        let mut analysis_needed = cache.analysis_epoch != self.state.analysis_epoch;
        let mut layout_needed = cache.layout_epoch != self.state.layout_epoch;
        let mut render_needed = cache.render_epoch != self.state.render_epoch
            || cache.viewport != (width, height)
            || !zoom_matches;

        if cache.ir.is_none() {
            analysis_needed = true;
        }
        if cache.layout.is_none() {
            layout_needed = true;
        }

        if !analysis_needed && !layout_needed && !render_needed {
            cache.cache_hits = cache.cache_hits.saturating_add(1);
            cache.last_cache_hit = true;
            return;
        }
        cache.cache_misses = cache.cache_misses.saturating_add(1);
        cache.last_cache_hit = false;

        if self.state.selected_sample().is_none() {
            if render_needed {
                let msg = "No samples loaded.";
                let area = Rect::new(0, 0, render_width, render_height);
                let mut pool = ftui_render::grapheme_pool::GraphemePool::new();
                let mut tmp_frame = Frame::new(render_width, render_height, &mut pool);
                Paragraph::new(msg)
                    .style(Style::new().fg(theme::fg::MUTED))
                    .render(area, &mut tmp_frame);
                let mut buffer = Buffer::new(render_width, render_height);
                buffer.copy_from(&tmp_frame.buffer, area, 0, 0);
                cache.buffer = buffer;
                cache.viewport = (width, height);
                cache.zoom = zoom;
                cache.render_epoch = self.state.render_epoch;
            }
            cache.analysis_epoch = self.state.analysis_epoch;
            cache.layout_epoch = self.state.layout_epoch;
            cache.ir = None;
            cache.layout = None;
            cache.metrics = MermaidMetricsSnapshot::default();
            cache.errors.clear();
            return;
        }

        let sample = self.state.selected_sample().expect("sample present");
        let config = self.build_config();
        let matrix = MermaidCompatibilityMatrix::default();
        let policy = MermaidFallbackPolicy::default();
        let mut metrics = cache.metrics;

        if analysis_needed {
            let parse_start = Instant::now();
            let parsed = mermaid::parse_with_diagnostics(sample.source);
            metrics.parse_ms = Some(parse_start.elapsed().as_secs_f32() * 1000.0);

            let ir_parse = mermaid::normalize_ast_to_ir(&parsed.ast, &config, &matrix, &policy);
            let mut errors = Vec::new();
            errors.extend(parsed.errors);
            errors.extend(ir_parse.errors);
            metrics.error_count = Some(errors.len() as u32);
            cache.errors = errors;
            cache.ir = Some(ir_parse.ir);
            cache.analysis_epoch = self.state.analysis_epoch;
            layout_needed = true;
            render_needed = true;
        }

        if layout_needed {
            let ir = cache.ir.as_ref().expect("layout requires cached IR");
            let spacing = self.layout_spacing();
            let layout_start = Instant::now();
            let layout = mermaid_layout::layout_diagram_with_spacing(ir, &config, &spacing);
            let parse_ms = metrics.parse_ms;
            let error_count = metrics.error_count;
            let mut snap = MermaidMetricsSnapshot::from_layout(&layout);
            snap.parse_ms = parse_ms;
            snap.error_count = error_count;
            snap.layout_ms = Some(layout_start.elapsed().as_secs_f32() * 1000.0);
            if let Some(ref plan) = layout.degradation {
                snap.set_fallback(self.state.tier, plan);
            }
            metrics = snap;
            cache.layout = Some(layout);
            cache.layout_epoch = self.state.layout_epoch;
            render_needed = true;
        }

        if render_needed {
            let ir = cache.ir.as_ref().expect("render requires cached IR");
            let layout = cache
                .layout
                .as_ref()
                .expect("render requires cached layout");
            let mut buffer = Buffer::new(render_width, render_height);
            let area = Rect::new(0, 0, render_width, render_height);
            let render_start = Instant::now();
            let _plan =
                mermaid_render::render_diagram_adaptive(layout, ir, &config, area, &mut buffer);
            metrics.render_ms = Some(render_start.elapsed().as_secs_f32() * 1000.0);

            if !cache.errors.is_empty() {
                let has_content = !ir.nodes.is_empty()
                    || !ir.edges.is_empty()
                    || !ir.labels.is_empty()
                    || !ir.clusters.is_empty();
                if has_content {
                    mermaid_render::render_mermaid_error_overlay(
                        &cache.errors,
                        sample.source,
                        &config,
                        area,
                        &mut buffer,
                    );
                } else {
                    mermaid_render::render_mermaid_error_panel(
                        &cache.errors,
                        sample.source,
                        &config,
                        area,
                        &mut buffer,
                    );
                }
            }

            cache.buffer = buffer;
            cache.viewport = (width, height);
            cache.zoom = zoom;
            cache.render_epoch = self.state.render_epoch;
        }

        cache.metrics = metrics;
    }

    fn blit_buffer(&self, frame: &mut Frame, area: Rect, buf: &Buffer, pan: (i16, i16)) {
        let view_w = area.width;
        let view_h = area.height;
        let buf_w = buf.width();
        let buf_h = buf.height();
        if view_w == 0 || view_h == 0 || buf_w == 0 || buf_h == 0 {
            return;
        }

        let pan_x = i32::from(pan.0);
        let pan_y = i32::from(pan.1);

        let (src_x, dst_x, copy_w) = if buf_w >= view_w {
            let center = ((buf_w - view_w) / 2) as i32;
            let max_src = (buf_w - view_w) as i32;
            let src = (center + pan_x).clamp(0, max_src);
            (src as u16, area.x, view_w)
        } else {
            let center = ((view_w - buf_w) / 2) as i32;
            let min_dst = i32::from(area.x);
            let max_dst = min_dst + (view_w - buf_w) as i32;
            let dst = (min_dst + center + pan_x).clamp(min_dst, max_dst);
            (0, dst as u16, buf_w)
        };

        let (src_y, dst_y, copy_h) = if buf_h >= view_h {
            let center = ((buf_h - view_h) / 2) as i32;
            let max_src = (buf_h - view_h) as i32;
            let src = (center + pan_y).clamp(0, max_src);
            (src as u16, area.y, view_h)
        } else {
            let center = ((view_h - buf_h) / 2) as i32;
            let min_dst = i32::from(area.y);
            let max_dst = min_dst + (view_h - buf_h) as i32;
            let dst = (min_dst + center + pan_y).clamp(min_dst, max_dst);
            (0, dst as u16, buf_h)
        };

        if copy_w == 0 || copy_h == 0 {
            return;
        }

        frame
            .buffer
            .copy_from(buf, Rect::new(src_x, src_y, copy_w, copy_h), dst_x, dst_y);
    }

    fn handle_key(&self, event: &KeyEvent) -> Option<MermaidShowcaseAction> {
        if event.kind != KeyEventKind::Press {
            return None;
        }

        match event.code {
            KeyCode::Down | KeyCode::Char('j') => Some(MermaidShowcaseAction::NextSample),
            KeyCode::Up | KeyCode::Char('k') => Some(MermaidShowcaseAction::PrevSample),
            KeyCode::Home => Some(MermaidShowcaseAction::FirstSample),
            KeyCode::End => Some(MermaidShowcaseAction::LastSample),
            KeyCode::Enter => Some(MermaidShowcaseAction::Refresh),
            KeyCode::Char('+') | KeyCode::Char('=') => Some(MermaidShowcaseAction::ZoomIn),
            KeyCode::Char('-') => Some(MermaidShowcaseAction::ZoomOut),
            KeyCode::Char('0') => Some(MermaidShowcaseAction::ZoomReset),
            KeyCode::Char('f') => Some(MermaidShowcaseAction::FitToView),
            KeyCode::Char('l') => Some(MermaidShowcaseAction::ToggleLayoutMode),
            KeyCode::Char('r') => Some(MermaidShowcaseAction::ForceRelayout),
            KeyCode::Char('m') => Some(MermaidShowcaseAction::ToggleMetrics),
            KeyCode::Char('c') => Some(MermaidShowcaseAction::ToggleControls),
            KeyCode::Char('t') => Some(MermaidShowcaseAction::CycleTier),
            KeyCode::Char('g') => Some(MermaidShowcaseAction::ToggleGlyphMode),
            KeyCode::Char('b') => Some(MermaidShowcaseAction::CycleRenderMode),
            KeyCode::Char('s') => Some(MermaidShowcaseAction::ToggleStyles),
            KeyCode::Char('w') => Some(MermaidShowcaseAction::CycleWrapMode),
            KeyCode::Char(']') => Some(MermaidShowcaseAction::IncreaseViewportWidth),
            KeyCode::Char('[') => Some(MermaidShowcaseAction::DecreaseViewportWidth),
            KeyCode::Char('}') => Some(MermaidShowcaseAction::IncreaseViewportHeight),
            KeyCode::Char('{') => Some(MermaidShowcaseAction::DecreaseViewportHeight),
            KeyCode::Char('o') => Some(MermaidShowcaseAction::ResetViewportOverride),
            KeyCode::Escape => Some(MermaidShowcaseAction::CollapsePanels),
            KeyCode::Char('i') => Some(MermaidShowcaseAction::ToggleStatusLog),
            _ => None,
        }
    }

    fn split_header_body_footer(&self, area: Rect) -> (Rect, Rect, Rect) {
        if area.height >= 3 {
            let rows = Flex::vertical()
                .constraints([
                    Constraint::Fixed(1),
                    Constraint::Min(1),
                    Constraint::Fixed(1),
                ])
                .split(area);
            return (rows[0], rows[1], rows[2]);
        }

        let empty = Rect::new(area.x, area.y, area.width, 0);
        (empty, area, empty)
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let sample = self
            .state
            .selected_sample()
            .map(|s| s.name)
            .unwrap_or("None");
        let total = self.state.samples.len();
        let index = self.state.selected_index.saturating_add(1).min(total);
        let score_str = self
            .state
            .metrics
            .objective_score
            .map_or_else(|| "-".to_string(), |s| format!("{s:.1}"));
        let viewport = if let Some((cols, rows)) = self.state.viewport_size_override {
            format!("Viewport: {cols}x{rows} (override)")
        } else {
            "Viewport: auto".to_string()
        };
        let status = if self.has_render_error() {
            "ERR"
        } else if self.state.metrics.fallback_tier.is_some() {
            "WARN"
        } else {
            "OK"
        };
        let text = format!(
            "Mermaid Showcase | {} ({}/{}) | Layout: {} | Score: {} | {} | {}",
            sample,
            index,
            total,
            self.state.layout_mode.as_str(),
            score_str,
            viewport,
            status
        );
        Paragraph::new(text)
            .style(Style::new().fg(theme::fg::PRIMARY).bg(theme::bg::DEEP))
            .render(area, frame);
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let hint = "j/k sample  l layout  r relayout  b render  +/- zoom  []/{} size  o reset  m metrics  t tier";
        let metrics = if self.state.metrics_visible {
            format!(
                "parse {}ms | layout {}ms | render {}ms",
                self.state.metrics.parse_ms.unwrap_or(0.0),
                self.state.metrics.layout_ms.unwrap_or(0.0),
                self.state.metrics.render_ms.unwrap_or(0.0)
            )
        } else {
            "metrics hidden (m)".to_string()
        };
        let text = format!("{hint} | {metrics}");
        Paragraph::new(text)
            .style(Style::new().fg(theme::fg::MUTED).bg(theme::bg::BASE))
            .render(area, frame);
    }

    fn render_samples(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Samples")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::fg::PRIMARY).bg(theme::bg::DEEP));

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let mut lines = Vec::new();
        for (idx, sample) in self.state.samples.iter().enumerate() {
            let prefix = if idx == self.state.selected_index {
                "> "
            } else {
                "  "
            };
            let mut meta_parts: Vec<&str> = Vec::with_capacity(2 + sample.tags.len());
            meta_parts.push(sample.kind);
            meta_parts.push(sample.complexity);
            meta_parts.extend_from_slice(sample.tags);
            let tag_str = if meta_parts.is_empty() {
                String::new()
            } else {
                format!(" [{}]", meta_parts.join(", "))
            };
            lines.push(format!("{prefix}{}{}", sample.name, tag_str));
        }

        Paragraph::new(lines.join("\n"))
            .style(Style::new().fg(theme::fg::MUTED))
            .render(inner, frame);
    }

    fn render_viewport(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Viewport")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::fg::PRIMARY).bg(theme::bg::BASE));

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        self.ensure_render_cache(inner);
        let cache = self.cache.borrow();
        self.blit_buffer(frame, inner, &cache.buffer, self.state.viewport_pan);
    }

    fn render_controls_panel(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Controls")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::fg::PRIMARY).bg(theme::bg::DEEP));

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let lines = [
            format!("Layout: {} (l)", self.state.layout_mode.as_str()),
            format!("Tier: {} (t)", self.state.tier),
            format!("Glyphs: {} (g)", self.state.glyph_mode),
            format!("Render: {} (b)", self.state.render_mode),
            format!("Wrap: {} (w)", self.state.wrap_mode),
            format!(
                "Styles: {} (s)",
                if self.state.styles_enabled {
                    "on"
                } else {
                    "off"
                }
            ),
            format!("Zoom: {:.0}% (+/-)", self.state.viewport_zoom * 100.0),
            "Fit: f".to_string(),
            format!(
                "Metrics: {} (m)",
                if self.state.metrics_visible {
                    "on"
                } else {
                    "off"
                }
            ),
        ];

        Paragraph::new(lines.join("\n"))
            .style(Style::new().fg(theme::fg::MUTED))
            .render(inner, frame);
    }

    fn render_metrics_panel(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Metrics")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::fg::PRIMARY).bg(theme::bg::DEEP));

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let metrics = &self.state.metrics;
        let mut lines: Vec<Line> = Vec::new();
        if self.state.metrics_visible {
            let muted = Style::new().fg(theme::fg::MUTED);

            // Parse timing.
            let parse_val = metrics.parse_ms.unwrap_or(0.0);
            let parse_level = classify_lower(parse_val, PARSE_MS_GOOD, PARSE_MS_OK);
            lines.push(Line::from_spans(vec![
                Span::styled("Parse: ", muted),
                Span::styled(
                    format!("{parse_val:.2} ms"),
                    Style::new().fg(parse_level.color()),
                ),
            ]));

            // Layout timing.
            let layout_val = metrics.layout_ms.unwrap_or(0.0);
            let layout_level = classify_lower(layout_val, LAYOUT_MS_GOOD, LAYOUT_MS_OK);
            lines.push(Line::from_spans(vec![
                Span::styled("Layout: ", muted),
                Span::styled(
                    format!("{layout_val:.2} ms"),
                    Style::new().fg(layout_level.color()),
                ),
            ]));

            // Render timing.
            let render_val = metrics.render_ms.unwrap_or(0.0);
            let render_level = classify_lower(render_val, RENDER_MS_GOOD, RENDER_MS_OK);
            lines.push(Line::from_spans(vec![
                Span::styled("Render: ", muted),
                Span::styled(
                    format!("{render_val:.2} ms"),
                    Style::new().fg(render_level.color()),
                ),
            ]));

            // Viewport info (neutral).
            if let Some((cols, rows)) = self.state.viewport_size_override {
                lines.push(Line::from_spans(vec![Span::styled(
                    format!("Viewport: {cols}x{rows}"),
                    muted,
                )]));
            } else {
                lines.push(Line::from_spans(vec![Span::styled(
                    "Viewport: auto",
                    muted,
                )]));
            }
            lines.push(Line::from_spans(vec![Span::styled(
                format!("Zoom: {:.0}%", self.state.viewport_zoom * 100.0),
                muted,
            )]));

            // Iterations (neutral).
            lines.push(Line::from_spans(vec![Span::styled(
                format!("Iters: {}", metrics.layout_iterations.unwrap_or(0)),
                muted,
            )]));

            // Objective score.
            let score_val = metrics.objective_score.unwrap_or(0.0);
            let score_level = classify_lower(score_val, SCORE_GOOD, SCORE_OK);
            lines.push(Line::from_spans(vec![
                Span::styled("Score: ", muted),
                Span::styled(
                    format!("{score_val:.2}"),
                    Style::new().fg(score_level.color()),
                ),
            ]));

            // Crossings.
            let cross_val = metrics.constraint_violations.unwrap_or(0);
            let cross_level = classify_lower_u32(cross_val, CROSSINGS_GOOD, CROSSINGS_OK);
            lines.push(Line::from_spans(vec![
                Span::styled("Cross: ", muted),
                Span::styled(format!("{cross_val}"), Style::new().fg(cross_level.color())),
            ]));

            // Bends (neutral).
            lines.push(Line::from_spans(vec![Span::styled(
                format!("Bends: {}", metrics.bends.unwrap_or(0)),
                muted,
            )]));

            // Symmetry (higher is better).
            let sym_val = metrics.symmetry.unwrap_or(0.0);
            let sym_level = classify_higher(sym_val, SYMMETRY_GOOD, SYMMETRY_OK);
            lines.push(Line::from_spans(vec![
                Span::styled("Sym: ", muted),
                Span::styled(format!("{sym_val:.2}"), Style::new().fg(sym_level.color())),
            ]));

            // Compactness (higher is better).
            let comp_val = metrics.compactness.unwrap_or(0.0);
            let comp_level = classify_higher(comp_val, COMPACTNESS_GOOD, COMPACTNESS_OK);
            lines.push(Line::from_spans(vec![
                Span::styled("Comp: ", muted),
                Span::styled(
                    format!("{comp_val:.2}"),
                    Style::new().fg(comp_level.color()),
                ),
            ]));

            // Edge length variance (neutral).
            lines.push(Line::from_spans(vec![Span::styled(
                format!(
                    "Edge var: {:.2}",
                    metrics.edge_length_variance.unwrap_or(0.0)
                ),
                muted,
            )]));

            // Label collisions (neutral).
            lines.push(Line::from_spans(vec![Span::styled(
                format!("Label col: {}", metrics.label_collisions.unwrap_or(0)),
                muted,
            )]));

            // Fallback info (warning color).
            if let Some(tier) = metrics.fallback_tier {
                lines.push(Line::from_spans(vec![
                    Span::styled("Fallback: ", muted),
                    Span::styled(format!("{tier}"), Style::new().fg(theme::accent::WARNING)),
                ]));
            }
            if let Some(reason) = metrics.fallback_reason {
                lines.push(Line::from_spans(vec![
                    Span::styled("Reason: ", muted),
                    Span::styled(reason, Style::new().fg(theme::accent::WARNING)),
                ]));
            }

            // Error diagnostics section.
            if let Some(ec) = metrics.error_count
                && ec > 0
            {
                lines.push(Line::from_spans(vec![
                    Span::styled("Errors: ", muted),
                    Span::styled(format!("{ec}"), Style::new().fg(theme::accent::ERROR)),
                ]));
                // Show first error message if available.
                let errors = &self.cache.borrow().errors;
                if let Some(first) = errors.first() {
                    let msg = if first.message.len() > 40 {
                        format!("{}...", &first.message[..37])
                    } else {
                        first.message.clone()
                    };
                    lines.push(Line::from_spans(vec![
                        Span::styled("  ", muted),
                        Span::styled(msg, Style::new().fg(theme::accent::ERROR)),
                    ]));
                }
            }
        } else {
            lines.push(Line::from_spans(vec![Span::styled(
                "Metrics hidden (press m)",
                Style::new().fg(theme::fg::MUTED),
            )]));
        }

        let text = Text::from_lines(lines);
        Paragraph::new(text).render(inner, frame);
    }

    fn render_status_log(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Status Log")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::fg::PRIMARY).bg(theme::bg::DEEP));

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let max_lines = inner.height as usize;
        let start = self.state.status_log.len().saturating_sub(max_lines);
        let mut lines = Vec::new();
        for entry in &self.state.status_log[start..] {
            if entry.detail.is_empty() {
                lines.push(entry.action.to_string());
            } else {
                lines.push(format!("{}: {}", entry.action, entry.detail));
            }
        }
        if lines.is_empty() {
            lines.push("No events yet.".to_string());
        }

        Paragraph::new(lines.join(
            "
",
        ))
        .style(Style::new().fg(theme::fg::MUTED))
        .render(inner, frame);
    }
}

impl Screen for MermaidShowcaseScreen {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if let Event::Key(key) = event
            && let Some(action) = self.handle_key(key)
        {
            self.state.apply_action(action);
        }
        Cmd::None
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let (header, body, footer) = self.split_header_body_footer(area);
        self.render_header(frame, header);
        self.render_footer(frame, footer);

        if body.is_empty() {
            return;
        }

        if body.width >= 120 {
            let columns = Flex::horizontal()
                .constraints([
                    Constraint::Percentage(26.0),
                    Constraint::Percentage(52.0),
                    Constraint::Percentage(22.0),
                ])
                .split(body);
            self.render_samples(frame, columns[0]);
            self.render_viewport(frame, columns[1]);
            let right = columns[2];
            if right.is_empty() {
                return;
            }

            // Collect which panels are visible.
            let show_controls = self.state.controls_visible;
            let show_metrics = self.state.metrics_visible;
            let show_log = self.state.status_log_visible;
            let panel_count = show_controls as u8 + show_metrics as u8 + show_log as u8;

            match panel_count {
                0 => {}
                1 => {
                    if show_controls {
                        self.render_controls_panel(frame, right);
                    } else if show_metrics {
                        self.render_metrics_panel(frame, right);
                    } else {
                        self.render_status_log(frame, right);
                    }
                }
                2 if right.height >= 12 => {
                    let rows = Flex::vertical()
                        .constraints([Constraint::Percentage(55.0), Constraint::Min(5)])
                        .split(right);
                    let mut slot = 0;
                    if show_controls {
                        self.render_controls_panel(frame, rows[slot]);
                        slot += 1;
                    }
                    if show_metrics {
                        self.render_metrics_panel(frame, rows[slot]);
                        slot += 1;
                    }
                    if show_log && slot < 2 {
                        self.render_status_log(frame, rows[slot]);
                    }
                }
                2 => {
                    // Not enough height for two panels; show first visible one.
                    if show_controls {
                        self.render_controls_panel(frame, right);
                    } else if show_metrics {
                        self.render_metrics_panel(frame, right);
                    } else {
                        self.render_status_log(frame, right);
                    }
                }
                _ if right.height >= 18 => {
                    // All three panels.
                    let rows = Flex::vertical()
                        .constraints([
                            Constraint::Percentage(40.0),
                            Constraint::Percentage(35.0),
                            Constraint::Min(4),
                        ])
                        .split(right);
                    self.render_controls_panel(frame, rows[0]);
                    self.render_metrics_panel(frame, rows[1]);
                    self.render_status_log(frame, rows[2]);
                }
                _ => {
                    // All visible but not enough height; show controls + metrics.
                    if right.height >= 12 {
                        let rows = Flex::vertical()
                            .constraints([Constraint::Percentage(55.0), Constraint::Min(5)])
                            .split(right);
                        self.render_controls_panel(frame, rows[0]);
                        self.render_metrics_panel(frame, rows[1]);
                    } else {
                        self.render_controls_panel(frame, right);
                    }
                }
            }
            return;
        }

        if body.width >= 80 {
            let columns = Flex::horizontal()
                .constraints([Constraint::Percentage(30.0), Constraint::Percentage(70.0)])
                .split(body);
            self.render_samples(frame, columns[0]);
            let right = columns[1];
            if self.state.metrics_visible && right.height >= 10 {
                let rows = Flex::vertical()
                    .constraints([Constraint::Min(1), Constraint::Fixed(8)])
                    .split(right);
                self.render_viewport(frame, rows[0]);
                self.render_metrics_panel(frame, rows[1]);
            } else {
                self.render_viewport(frame, right);
            }
            return;
        }

        let rows = Flex::vertical()
            .constraints([Constraint::Fixed(6), Constraint::Min(1)])
            .split(body);
        self.render_samples(frame, rows[0]);
        self.render_viewport(frame, rows[1]);
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "j / Down",
                action: "Next sample",
            },
            HelpEntry {
                key: "k / Up",
                action: "Previous sample",
            },
            HelpEntry {
                key: "Enter",
                action: "Re-render sample",
            },
            HelpEntry {
                key: "l",
                action: "Toggle layout mode",
            },
            HelpEntry {
                key: "r",
                action: "Force re-layout",
            },
            HelpEntry {
                key: "+ / -",
                action: "Zoom in/out",
            },
            HelpEntry {
                key: "] / [",
                action: "Viewport width +/-",
            },
            HelpEntry {
                key: "} / {",
                action: "Viewport height +/-",
            },
            HelpEntry {
                key: "o",
                action: "Reset viewport override",
            },
            HelpEntry {
                key: "f",
                action: "Fit to viewport",
            },
            HelpEntry {
                key: "m",
                action: "Toggle metrics",
            },
            HelpEntry {
                key: "c",
                action: "Toggle controls",
            },
            HelpEntry {
                key: "t",
                action: "Cycle tier",
            },
            HelpEntry {
                key: "g",
                action: "Toggle glyph mode",
            },
            HelpEntry {
                key: "b",
                action: "Cycle render mode",
            },
            HelpEntry {
                key: "s",
                action: "Toggle styles",
            },
            HelpEntry {
                key: "w",
                action: "Cycle wrap mode",
            },
            HelpEntry {
                key: "i",
                action: "Toggle status log",
            },
            HelpEntry {
                key: "Esc",
                action: "Collapse panels",
            },
        ]
    }

    fn title(&self) -> &'static str {
        "Mermaid Showcase"
    }

    fn tab_label(&self) -> &'static str {
        "Mermaid"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_core::event::{KeyEventKind, Modifiers};
    use serde_json::Value;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Press,
        }
    }

    fn new_state() -> MermaidShowcaseState {
        MermaidShowcaseState::new()
    }

    fn new_screen() -> MermaidShowcaseScreen {
        MermaidShowcaseScreen::new()
    }

    // --- State initialization ---

    #[test]
    fn state_defaults() {
        let s = new_state();
        assert_eq!(s.selected_index, 0);
        assert_eq!(s.layout_mode, LayoutMode::Auto);
        assert_eq!(s.viewport_zoom, 1.0);
        assert_eq!(s.viewport_pan, (0, 0));
        assert!(s.viewport_size_override.is_none());
        assert!(s.styles_enabled);
        assert!(s.metrics_visible);
        assert!(s.controls_visible);
        assert_eq!(s.render_epoch, 0);
        assert!(!s.samples.is_empty());
    }

    #[test]
    fn screen_default_impl() {
        let screen = MermaidShowcaseScreen::default();
        assert_eq!(screen.state.selected_index, 0);
    }

    // --- Sample navigation ---

    #[test]
    fn next_sample_wraps() {
        let mut s = new_state();
        let len = s.samples.len();
        s.selected_index = len - 1;
        let epoch = s.render_epoch;
        s.apply_action(MermaidShowcaseAction::NextSample);
        assert_eq!(s.selected_index, 0);
        assert_eq!(s.render_epoch, epoch + 1);
    }

    #[test]
    fn prev_sample_wraps() {
        let mut s = new_state();
        let epoch = s.render_epoch;
        s.apply_action(MermaidShowcaseAction::PrevSample);
        assert_eq!(s.selected_index, s.samples.len() - 1);
        assert_eq!(s.render_epoch, epoch + 1);
    }

    #[test]
    fn next_prev_roundtrip() {
        let mut s = new_state();
        s.apply_action(MermaidShowcaseAction::NextSample);
        s.apply_action(MermaidShowcaseAction::NextSample);
        s.apply_action(MermaidShowcaseAction::PrevSample);
        assert_eq!(s.selected_index, 1);
    }

    #[test]
    fn first_sample() {
        let mut s = new_state();
        s.selected_index = 5;
        s.apply_action(MermaidShowcaseAction::FirstSample);
        assert_eq!(s.selected_index, 0);
    }

    #[test]
    fn last_sample() {
        let mut s = new_state();
        s.apply_action(MermaidShowcaseAction::LastSample);
        assert_eq!(s.selected_index, s.samples.len() - 1);
    }

    #[test]
    fn selected_sample_returns_current() {
        let s = new_state();
        let sample = s.selected_sample().unwrap();
        assert_eq!(sample.name, "Flow Basic");
    }

    // --- Refresh ---

    #[test]
    fn refresh_bumps_epoch() {
        let mut s = new_state();
        let epoch = s.render_epoch;
        s.apply_action(MermaidShowcaseAction::Refresh);
        assert_eq!(s.render_epoch, epoch + 1);
    }

    // --- Zoom controls ---

    #[test]
    fn zoom_in() {
        let mut s = new_state();
        s.apply_action(MermaidShowcaseAction::ZoomIn);
        assert!((s.viewport_zoom - 1.1).abs() < 0.01);
    }

    #[test]
    fn zoom_out() {
        let mut s = new_state();
        s.apply_action(MermaidShowcaseAction::ZoomOut);
        assert!((s.viewport_zoom - 0.9).abs() < 0.01);
    }

    #[test]
    fn zoom_clamps_max() {
        let mut s = new_state();
        s.viewport_zoom = ZOOM_MAX;
        s.apply_action(MermaidShowcaseAction::ZoomIn);
        assert!((s.viewport_zoom - ZOOM_MAX).abs() < 0.01);
    }

    #[test]
    fn zoom_clamps_min() {
        let mut s = new_state();
        s.viewport_zoom = ZOOM_MIN;
        s.apply_action(MermaidShowcaseAction::ZoomOut);
        assert!((s.viewport_zoom - ZOOM_MIN).abs() < 0.01);
    }

    #[test]
    fn zoom_reset() {
        let mut s = new_state();
        s.viewport_zoom = 2.5;
        s.viewport_pan = (10, 20);
        s.apply_action(MermaidShowcaseAction::ZoomReset);
        assert!((s.viewport_zoom - 1.0).abs() < f32::EPSILON);
        assert_eq!(s.viewport_pan, (0, 0));
    }

    #[test]
    fn fit_to_view() {
        let mut s = new_state();
        s.viewport_zoom = 2.0;
        s.viewport_pan = (5, 5);
        s.apply_action(MermaidShowcaseAction::FitToView);
        assert!((s.viewport_zoom - 1.0).abs() < f32::EPSILON);
        assert_eq!(s.viewport_pan, (0, 0));
    }

    #[test]
    fn viewport_override_increase_sets_default() {
        let mut s = new_state();
        let epoch = s.render_epoch;
        s.apply_action(MermaidShowcaseAction::IncreaseViewportWidth);
        let expected_cols =
            (VIEWPORT_OVERRIDE_DEFAULT_COLS as i32 + VIEWPORT_OVERRIDE_STEP_COLS as i32) as u16;
        let expected_rows = VIEWPORT_OVERRIDE_DEFAULT_ROWS;
        assert_eq!(
            s.viewport_size_override,
            Some((expected_cols, expected_rows))
        );
        assert_eq!(s.render_epoch, epoch + 1);
    }

    #[test]
    fn viewport_override_reset_clears() {
        let mut s = new_state();
        s.apply_action(MermaidShowcaseAction::IncreaseViewportHeight);
        assert!(s.viewport_size_override.is_some());
        let epoch = s.render_epoch;
        s.apply_action(MermaidShowcaseAction::ResetViewportOverride);
        assert!(s.viewport_size_override.is_none());
        assert_eq!(s.render_epoch, epoch + 1);
    }

    // --- Layout mode ---

    #[test]
    fn layout_mode_cycles() {
        let mut s = new_state();
        assert_eq!(s.layout_mode, LayoutMode::Auto);
        s.apply_action(MermaidShowcaseAction::ToggleLayoutMode);
        assert_eq!(s.layout_mode, LayoutMode::Dense);
        s.apply_action(MermaidShowcaseAction::ToggleLayoutMode);
        assert_eq!(s.layout_mode, LayoutMode::Spacious);
        s.apply_action(MermaidShowcaseAction::ToggleLayoutMode);
        assert_eq!(s.layout_mode, LayoutMode::Auto);
    }

    #[test]
    fn layout_mode_bumps_epoch() {
        let mut s = new_state();
        let epoch = s.render_epoch;
        s.apply_action(MermaidShowcaseAction::ToggleLayoutMode);
        assert_eq!(s.render_epoch, epoch + 1);
    }

    #[test]
    fn layout_mode_as_str() {
        assert_eq!(LayoutMode::Auto.as_str(), "Auto");
        assert_eq!(LayoutMode::Dense.as_str(), "Dense");
        assert_eq!(LayoutMode::Spacious.as_str(), "Spacious");
    }

    // --- Force relayout ---

    #[test]
    fn force_relayout_bumps_epoch() {
        let mut s = new_state();
        let epoch = s.render_epoch;
        s.apply_action(MermaidShowcaseAction::ForceRelayout);
        assert_eq!(s.render_epoch, epoch + 1);
    }

    // --- Metrics toggle ---

    #[test]
    fn toggle_metrics() {
        let mut s = new_state();
        assert!(s.metrics_visible);
        s.apply_action(MermaidShowcaseAction::ToggleMetrics);
        assert!(!s.metrics_visible);
        s.apply_action(MermaidShowcaseAction::ToggleMetrics);
        assert!(s.metrics_visible);
    }

    // --- Controls toggle ---

    #[test]
    fn toggle_controls() {
        let mut s = new_state();
        assert!(s.controls_visible);
        s.apply_action(MermaidShowcaseAction::ToggleControls);
        assert!(!s.controls_visible);
        s.apply_action(MermaidShowcaseAction::ToggleControls);
        assert!(s.controls_visible);
    }

    // --- Tier cycling ---

    #[test]
    fn tier_cycles() {
        let mut s = new_state();
        assert_eq!(s.tier, MermaidTier::Auto);
        s.apply_action(MermaidShowcaseAction::CycleTier);
        assert_eq!(s.tier, MermaidTier::Rich);
        s.apply_action(MermaidShowcaseAction::CycleTier);
        assert_eq!(s.tier, MermaidTier::Normal);
        s.apply_action(MermaidShowcaseAction::CycleTier);
        assert_eq!(s.tier, MermaidTier::Compact);
        s.apply_action(MermaidShowcaseAction::CycleTier);
        assert_eq!(s.tier, MermaidTier::Auto);
    }

    #[test]
    fn tier_bumps_epoch() {
        let mut s = new_state();
        let epoch = s.render_epoch;
        s.apply_action(MermaidShowcaseAction::CycleTier);
        assert_eq!(s.render_epoch, epoch + 1);
    }

    // --- Glyph mode ---

    #[test]
    fn glyph_mode_toggles() {
        let mut s = new_state();
        assert_eq!(s.glyph_mode, MermaidGlyphMode::Unicode);
        s.apply_action(MermaidShowcaseAction::ToggleGlyphMode);
        assert_eq!(s.glyph_mode, MermaidGlyphMode::Ascii);
        s.apply_action(MermaidShowcaseAction::ToggleGlyphMode);
        assert_eq!(s.glyph_mode, MermaidGlyphMode::Unicode);
    }

    #[test]
    fn glyph_mode_bumps_epoch() {
        let mut s = new_state();
        let epoch = s.render_epoch;
        s.apply_action(MermaidShowcaseAction::ToggleGlyphMode);
        assert_eq!(s.render_epoch, epoch + 1);
    }

    // --- Render mode ---

    #[test]
    fn render_mode_cycles() {
        let mut s = new_state();
        assert_eq!(s.render_mode, MermaidRenderMode::Auto);
        s.apply_action(MermaidShowcaseAction::CycleRenderMode);
        assert_eq!(s.render_mode, MermaidRenderMode::Braille);
        s.apply_action(MermaidShowcaseAction::CycleRenderMode);
        assert_eq!(s.render_mode, MermaidRenderMode::Block);
        s.apply_action(MermaidShowcaseAction::CycleRenderMode);
        assert_eq!(s.render_mode, MermaidRenderMode::HalfBlock);
        s.apply_action(MermaidShowcaseAction::CycleRenderMode);
        assert_eq!(s.render_mode, MermaidRenderMode::CellOnly);
        s.apply_action(MermaidShowcaseAction::CycleRenderMode);
        assert_eq!(s.render_mode, MermaidRenderMode::Auto);
    }

    #[test]
    fn render_mode_bumps_epoch() {
        let mut s = new_state();
        let epoch = s.render_epoch;
        s.apply_action(MermaidShowcaseAction::CycleRenderMode);
        assert_eq!(s.render_epoch, epoch + 1);
    }

    // --- Styles ---

    #[test]
    fn styles_toggle() {
        let mut s = new_state();
        assert!(s.styles_enabled);
        s.apply_action(MermaidShowcaseAction::ToggleStyles);
        assert!(!s.styles_enabled);
        s.apply_action(MermaidShowcaseAction::ToggleStyles);
        assert!(s.styles_enabled);
    }

    #[test]
    fn styles_bumps_epoch() {
        let mut s = new_state();
        let epoch = s.render_epoch;
        s.apply_action(MermaidShowcaseAction::ToggleStyles);
        assert_eq!(s.render_epoch, epoch + 1);
    }

    // --- Wrap mode ---

    #[test]
    fn wrap_mode_cycles() {
        let mut s = new_state();
        assert_eq!(s.wrap_mode, MermaidWrapMode::WordChar);
        s.apply_action(MermaidShowcaseAction::CycleWrapMode);
        assert_eq!(s.wrap_mode, MermaidWrapMode::None);
        s.apply_action(MermaidShowcaseAction::CycleWrapMode);
        assert_eq!(s.wrap_mode, MermaidWrapMode::Word);
        s.apply_action(MermaidShowcaseAction::CycleWrapMode);
        assert_eq!(s.wrap_mode, MermaidWrapMode::Char);
        s.apply_action(MermaidShowcaseAction::CycleWrapMode);
        assert_eq!(s.wrap_mode, MermaidWrapMode::WordChar);
    }

    #[test]
    fn wrap_mode_bumps_epoch() {
        let mut s = new_state();
        let epoch = s.render_epoch;
        s.apply_action(MermaidShowcaseAction::CycleWrapMode);
        assert_eq!(s.render_epoch, epoch + 1);
    }

    // --- Collapse panels (Esc) ---

    #[test]
    fn collapse_panels() {
        let mut s = new_state();
        assert!(s.controls_visible);
        assert!(s.metrics_visible);
        s.apply_action(MermaidShowcaseAction::CollapsePanels);
        assert!(!s.controls_visible);
        assert!(!s.metrics_visible);
    }

    #[test]
    fn collapse_panels_idempotent() {
        let mut s = new_state();
        s.controls_visible = false;
        s.metrics_visible = false;
        s.apply_action(MermaidShowcaseAction::CollapsePanels);
        assert!(!s.controls_visible);
        assert!(!s.metrics_visible);
    }

    // --- Key mapping ---

    #[test]
    fn key_j_maps_to_next() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('j')));
        assert!(matches!(action, Some(MermaidShowcaseAction::NextSample)));
    }

    #[test]
    fn key_down_maps_to_next() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Down));
        assert!(matches!(action, Some(MermaidShowcaseAction::NextSample)));
    }

    #[test]
    fn key_k_maps_to_prev() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('k')));
        assert!(matches!(action, Some(MermaidShowcaseAction::PrevSample)));
    }

    #[test]
    fn key_up_maps_to_prev() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Up));
        assert!(matches!(action, Some(MermaidShowcaseAction::PrevSample)));
    }

    #[test]
    fn key_home_maps_to_first() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Home));
        assert!(matches!(action, Some(MermaidShowcaseAction::FirstSample)));
    }

    #[test]
    fn key_end_maps_to_last() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::End));
        assert!(matches!(action, Some(MermaidShowcaseAction::LastSample)));
    }

    #[test]
    fn key_enter_maps_to_refresh() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Enter));
        assert!(matches!(action, Some(MermaidShowcaseAction::Refresh)));
    }

    #[test]
    fn key_plus_maps_to_zoom_in() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('+')));
        assert!(matches!(action, Some(MermaidShowcaseAction::ZoomIn)));
    }

    #[test]
    fn key_equals_maps_to_zoom_in() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('=')));
        assert!(matches!(action, Some(MermaidShowcaseAction::ZoomIn)));
    }

    #[test]
    fn key_minus_maps_to_zoom_out() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('-')));
        assert!(matches!(action, Some(MermaidShowcaseAction::ZoomOut)));
    }

    #[test]
    fn key_zero_maps_to_zoom_reset() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('0')));
        assert!(matches!(action, Some(MermaidShowcaseAction::ZoomReset)));
    }

    #[test]
    fn key_f_maps_to_fit() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('f')));
        assert!(matches!(action, Some(MermaidShowcaseAction::FitToView)));
    }

    #[test]
    fn key_l_maps_to_layout() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('l')));
        assert!(matches!(
            action,
            Some(MermaidShowcaseAction::ToggleLayoutMode)
        ));
    }

    #[test]
    fn key_r_maps_to_relayout() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('r')));
        assert!(matches!(action, Some(MermaidShowcaseAction::ForceRelayout)));
    }

    #[test]
    fn key_m_maps_to_metrics() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('m')));
        assert!(matches!(action, Some(MermaidShowcaseAction::ToggleMetrics)));
    }

    #[test]
    fn key_c_maps_to_controls() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('c')));
        assert!(matches!(
            action,
            Some(MermaidShowcaseAction::ToggleControls)
        ));
    }

    #[test]
    fn key_t_maps_to_tier() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('t')));
        assert!(matches!(action, Some(MermaidShowcaseAction::CycleTier)));
    }

    #[test]
    fn key_g_maps_to_glyph() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('g')));
        assert!(matches!(
            action,
            Some(MermaidShowcaseAction::ToggleGlyphMode)
        ));
    }

    #[test]
    fn key_b_maps_to_render_mode() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('b')));
        assert!(matches!(
            action,
            Some(MermaidShowcaseAction::CycleRenderMode)
        ));
    }

    #[test]
    fn key_s_maps_to_styles() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('s')));
        assert!(matches!(action, Some(MermaidShowcaseAction::ToggleStyles)));
    }

    #[test]
    fn key_w_maps_to_wrap() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('w')));
        assert!(matches!(action, Some(MermaidShowcaseAction::CycleWrapMode)));
    }

    #[test]
    fn key_right_bracket_maps_to_width_increase() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char(']')));
        assert!(matches!(
            action,
            Some(MermaidShowcaseAction::IncreaseViewportWidth)
        ));
    }

    #[test]
    fn key_left_bracket_maps_to_width_decrease() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('[')));
        assert!(matches!(
            action,
            Some(MermaidShowcaseAction::DecreaseViewportWidth)
        ));
    }

    #[test]
    fn key_right_brace_maps_to_height_increase() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('}')));
        assert!(matches!(
            action,
            Some(MermaidShowcaseAction::IncreaseViewportHeight)
        ));
    }

    #[test]
    fn key_left_brace_maps_to_height_decrease() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('{')));
        assert!(matches!(
            action,
            Some(MermaidShowcaseAction::DecreaseViewportHeight)
        ));
    }

    #[test]
    fn key_o_maps_to_reset_viewport() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('o')));
        assert!(matches!(
            action,
            Some(MermaidShowcaseAction::ResetViewportOverride)
        ));
    }

    #[test]
    fn key_esc_maps_to_collapse() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Escape));
        assert!(matches!(
            action,
            Some(MermaidShowcaseAction::CollapsePanels)
        ));
    }

    #[test]
    fn unknown_key_returns_none() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('x')));
        assert!(action.is_none());
    }

    #[test]
    fn release_event_ignored() {
        let screen = new_screen();
        let event = KeyEvent {
            code: KeyCode::Char('j'),
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Release,
        };
        assert!(screen.handle_key(&event).is_none());
    }

    // --- Screen trait ---

    #[test]
    fn keybindings_list_not_empty() {
        let screen = new_screen();
        let bindings = screen.keybindings();
        assert!(bindings.len() >= 15);
    }

    #[test]
    fn keybindings_include_esc() {
        let screen = new_screen();
        let bindings = screen.keybindings();
        assert!(bindings.iter().any(|h| h.key == "Esc"));
    }

    #[test]
    fn title_and_tab_label() {
        let screen = new_screen();
        assert_eq!(screen.title(), "Mermaid Showcase");
        assert_eq!(screen.tab_label(), "Mermaid");
    }

    // --- Integration: key press through update ---

    #[test]
    fn update_applies_key_action() {
        let mut screen = new_screen();
        let event = Event::Key(press(KeyCode::Char('j')));
        screen.update(&event);
        assert_eq!(screen.state.selected_index, 1);
    }

    #[test]
    fn update_ignores_non_key_events() {
        let mut screen = new_screen();
        let event = Event::Tick;
        screen.update(&event);
        assert_eq!(screen.state.selected_index, 0);
    }

    // --- Sample library ---

    #[test]
    fn default_samples_non_empty() {
        assert!(!DEFAULT_SAMPLES.is_empty());
    }

    #[test]
    fn each_sample_has_source() {
        for sample in DEFAULT_SAMPLES {
            assert!(
                !sample.source.is_empty(),
                "sample {} has empty source",
                sample.name
            );
        }
    }

    #[test]
    fn each_sample_has_kind() {
        for sample in DEFAULT_SAMPLES {
            assert!(
                !sample.kind.is_empty(),
                "sample {} has empty kind",
                sample.name
            );
        }
    }
    // --- Metrics integration ---

    #[test]
    fn metrics_populated_on_init() {
        let s = new_state();
        assert!(s.metrics.parse_ms.is_some(), "parse_ms should be set");
        assert!(s.metrics.layout_ms.is_some(), "layout_ms should be set");
        assert!(
            s.metrics.objective_score.is_some(),
            "objective_score should be set"
        );
        assert!(
            s.metrics.layout_iterations.is_some(),
            "layout_iterations should be set"
        );
    }

    #[test]
    fn metrics_update_on_sample_change() {
        let mut s = new_state();
        let score_before = s.metrics.objective_score;
        s.apply_action(MermaidShowcaseAction::NextSample);
        assert!(
            s.metrics.objective_score.is_some(),
            "score should be set after sample change"
        );
        assert!(score_before.is_some());
    }

    #[test]
    fn metrics_update_on_layout_mode_change() {
        let mut s = new_state();
        s.apply_action(MermaidShowcaseAction::ToggleLayoutMode);
        assert!(
            s.metrics.layout_iterations.is_some(),
            "iterations should be set after layout mode change"
        );
        assert_eq!(s.layout_mode, LayoutMode::Dense);
    }

    #[test]
    fn metrics_quality_fields_populated() {
        let s = new_state();
        assert!(s.metrics.constraint_violations.is_some(), "crossings");
        assert!(s.metrics.bends.is_some(), "bends");
        assert!(s.metrics.symmetry.is_some(), "symmetry");
        assert!(s.metrics.compactness.is_some(), "compactness");
        assert!(
            s.metrics.edge_length_variance.is_some(),
            "edge_length_variance"
        );
        assert!(s.metrics.label_collisions.is_some(), "label_collisions");
    }

    #[test]
    fn metrics_recomputed_for_all_samples() {
        let mut s = new_state();
        let mut populated = 0usize;
        for _ in 0..s.samples.len() {
            // Some diagram types may not fully support layout yet;
            // check that at least parse_ms or layout_ms is populated.
            if s.metrics.parse_ms.is_some() || s.metrics.layout_ms.is_some() {
                populated += 1;
            }
            s.apply_action(MermaidShowcaseAction::NextSample);
        }
        // At least half the samples should produce valid metrics.
        assert!(
            populated > s.samples.len() / 2,
            "expected most samples to produce metrics, got {}/{}",
            populated,
            s.samples.len()
        );
    }

    #[test]
    fn metrics_jsonl_line_includes_required_fields() {
        let s = new_state();
        let sample = s.selected_sample().expect("sample");
        let line = s.metrics_jsonl_line(sample, 7, Some("run-1"), 123, "alt");
        let value: Value = serde_json::from_str(&line).expect("json parse");

        assert_eq!(
            value["schema_version"].as_str().unwrap_or_default(),
            TEST_JSONL_SCHEMA
        );
        assert_eq!(
            value["event"].as_str().unwrap_or_default(),
            MERMAID_JSONL_EVENT
        );
        assert_eq!(value["seq"].as_u64().unwrap_or_default(), 7);
        assert_eq!(value["run_id"].as_str().unwrap_or_default(), "run-1");
        assert_eq!(value["seed"].as_u64().unwrap_or_default(), 123);
        assert_eq!(value["screen_mode"].as_str().unwrap_or_default(), "alt");
        assert_eq!(value["sample"].as_str().unwrap_or_default(), sample.name);
        assert_eq!(
            value["layout_mode"].as_str().unwrap_or_default(),
            s.layout_mode.as_str()
        );
        assert_eq!(
            value["tier"].as_str().unwrap_or_default(),
            s.tier.to_string()
        );
        assert_eq!(
            value["glyph_mode"].as_str().unwrap_or_default(),
            s.glyph_mode.to_string()
        );
        assert_eq!(
            value["wrap_mode"].as_str().unwrap_or_default(),
            s.wrap_mode.to_string()
        );
        assert!(value["parse_ms"].is_number());
        assert!(value["layout_ms"].is_number());
        // render_ms is only set during actual view() rendering; may be null here.
        assert!(value["render_ms"].is_number() || value["render_ms"].is_null());

        // Validate quality metric fields are present and typed correctly.
        // All are emitted as number or null by push_opt_f32/push_opt_u32.
        assert!(value["render_epoch"].is_number(), "render_epoch missing");
        assert!(
            value["layout_iterations"].is_number() || value["layout_iterations"].is_null(),
            "layout_iterations must be number or null, got {:?}",
            value["layout_iterations"]
        );
        assert!(
            value["objective_score"].is_number() || value["objective_score"].is_null(),
            "objective_score must be number or null, got {:?}",
            value["objective_score"]
        );
        assert!(
            value["constraint_violations"].is_number() || value["constraint_violations"].is_null(),
            "constraint_violations must be number or null, got {:?}",
            value["constraint_violations"]
        );
        assert!(
            value["bends"].is_number() || value["bends"].is_null(),
            "bends must be number or null, got {:?}",
            value["bends"]
        );
        assert!(
            value["symmetry"].is_number() || value["symmetry"].is_null(),
            "symmetry must be number or null, got {:?}",
            value["symmetry"]
        );
        assert!(
            value["compactness"].is_number() || value["compactness"].is_null(),
            "compactness must be number or null, got {:?}",
            value["compactness"]
        );
        assert!(
            value["edge_length_variance"].is_number() || value["edge_length_variance"].is_null(),
            "edge_length_variance must be number or null, got {:?}",
            value["edge_length_variance"]
        );
        assert!(
            value["label_collisions"].is_number() || value["label_collisions"].is_null(),
            "label_collisions must be number or null, got {:?}",
            value["label_collisions"]
        );
    }

    /// Validate that JSONL quality metrics are populated (non-null) for a valid sample.
    #[test]
    fn metrics_jsonl_quality_fields_populated() {
        let s = new_state();
        let sample = s.selected_sample().expect("sample");
        let line = s.metrics_jsonl_line(sample, 0, None, 0, "test");
        let value: Value = serde_json::from_str(&line).expect("valid json");

        // For a valid flowchart sample, quality fields should be populated.
        assert!(
            value["layout_iterations"].is_number(),
            "layout_iterations should be non-null for valid sample"
        );
        assert!(
            value["objective_score"].is_number(),
            "objective_score should be non-null for valid sample"
        );
        assert!(
            value["bends"].is_number(),
            "bends should be non-null for valid sample"
        );
        assert!(
            value["symmetry"].is_number(),
            "symmetry should be non-null for valid sample"
        );
        assert!(
            value["compactness"].is_number(),
            "compactness should be non-null for valid sample"
        );
    }

    // --- Error UX + diagnostics (bd-20fop) ---

    #[test]
    fn error_count_zero_for_valid_sample() {
        let s = new_state();
        // Valid samples should have error_count == 0.
        assert_eq!(
            s.metrics.error_count.unwrap_or(0),
            0,
            "valid sample should have zero errors"
        );
    }

    #[test]
    fn error_count_in_jsonl_output() {
        let s = new_state();
        let sample = s.selected_sample().expect("sample");
        let line = s.metrics_jsonl_line(sample, 0, None, 0, "test");
        let value: Value = serde_json::from_str(&line).expect("valid json");
        // error_count field should be present and typed correctly.
        assert!(
            value["error_count"].is_number() || value["error_count"].is_null(),
            "error_count must be number or null, got {:?}",
            value["error_count"]
        );
    }

    #[test]
    fn error_count_jsonl_zero_for_valid_sample() {
        let s = new_state();
        let sample = s.selected_sample().expect("sample");
        let line = s.metrics_jsonl_line(sample, 0, None, 0, "test");
        let value: Value = serde_json::from_str(&line).expect("valid json");
        assert_eq!(
            value["error_count"].as_u64().unwrap_or(999),
            0,
            "valid sample JSONL should report error_count=0"
        );
    }

    #[test]
    fn has_render_error_false_for_valid_sample() {
        let screen = new_screen();
        assert!(
            !screen.has_render_error(),
            "valid sample should not have render errors"
        );
    }

    #[test]
    fn status_ok_for_valid_sample() {
        let s = new_state();
        // Valid sample with no errors and no fallback should be "OK".
        assert!(
            s.metrics.fallback_tier.is_none(),
            "valid sample should not trigger fallback"
        );
        assert_eq!(
            s.metrics.error_count.unwrap_or(0),
            0,
            "valid sample should have no errors"
        );
    }

    #[test]
    fn metrics_error_count_field_survives_layout_rebuild() {
        let mut s = new_state();
        // Set error count manually and verify it persists across normalize.
        s.metrics.error_count = Some(3);
        // After changing layout mode, metrics are rebuilt; error_count should
        // be re-derived from the actual parse (not the manual value).
        s.layout_mode = LayoutMode::Dense;
        s.render_epoch += 1;
        s.normalize();
        // After normalize + recompute, error_count should reflect the actual
        // parse result (0 for valid sample, not our injected 3).
        assert_eq!(
            s.metrics.error_count.unwrap_or(999),
            0,
            "error_count should be refreshed from actual parse, not stale"
        );
    }

    // --- Status log ---

    #[test]
    fn status_log_starts_empty() {
        let s = new_state();
        assert!(s.status_log.is_empty());
        assert!(!s.status_log_visible);
    }

    #[test]
    fn toggle_status_log() {
        let mut s = new_state();
        assert!(!s.status_log_visible);
        s.apply_action(MermaidShowcaseAction::ToggleStatusLog);
        assert!(s.status_log_visible);
        s.apply_action(MermaidShowcaseAction::ToggleStatusLog);
        assert!(!s.status_log_visible);
    }

    #[test]
    fn actions_produce_log_entries() {
        let mut s = new_state();
        s.apply_action(MermaidShowcaseAction::NextSample);
        assert_eq!(s.status_log.len(), 1);
        assert_eq!(s.status_log[0].action, "sample");

        s.apply_action(MermaidShowcaseAction::ZoomIn);
        assert_eq!(s.status_log.len(), 2);
        assert_eq!(s.status_log[1].action, "zoom");

        s.apply_action(MermaidShowcaseAction::ToggleLayoutMode);
        assert_eq!(s.status_log.len(), 3);
        assert_eq!(s.status_log[2].action, "layout");
    }

    #[test]
    fn status_log_capped_at_limit() {
        let mut s = new_state();
        for _ in 0..(STATUS_LOG_CAP + 10) {
            s.apply_action(MermaidShowcaseAction::ZoomIn);
        }
        assert!(s.status_log.len() <= STATUS_LOG_CAP);
    }

    #[test]
    fn collapse_panels_hides_status_log() {
        let mut s = new_state();
        s.status_log_visible = true;
        s.apply_action(MermaidShowcaseAction::CollapsePanels);
        assert!(!s.status_log_visible);
    }

    #[test]
    fn key_i_maps_to_status_log() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('i')));
        assert!(matches!(
            action,
            Some(MermaidShowcaseAction::ToggleStatusLog)
        ));
    }

    #[test]
    fn classify_lower_boundaries() {
        assert_eq!(classify_lower(0.5, 1.0, 5.0), MetricLevel::Good);
        assert_eq!(classify_lower(1.0, 1.0, 5.0), MetricLevel::Good);
        assert_eq!(classify_lower(3.0, 1.0, 5.0), MetricLevel::Ok);
        assert_eq!(classify_lower(5.0, 1.0, 5.0), MetricLevel::Ok);
        assert_eq!(classify_lower(6.0, 1.0, 5.0), MetricLevel::Bad);
    }

    #[test]
    fn classify_lower_u32_boundaries() {
        assert_eq!(classify_lower_u32(0, 0, 3), MetricLevel::Good);
        assert_eq!(classify_lower_u32(1, 0, 3), MetricLevel::Ok);
        assert_eq!(classify_lower_u32(3, 0, 3), MetricLevel::Ok);
        assert_eq!(classify_lower_u32(4, 0, 3), MetricLevel::Bad);
    }

    #[test]
    fn classify_higher_boundaries() {
        assert_eq!(classify_higher(0.8, 0.7, 0.4), MetricLevel::Good);
        assert_eq!(classify_higher(0.7, 0.7, 0.4), MetricLevel::Good);
        assert_eq!(classify_higher(0.5, 0.7, 0.4), MetricLevel::Ok);
        assert_eq!(classify_higher(0.4, 0.7, 0.4), MetricLevel::Ok);
        assert_eq!(classify_higher(0.3, 0.7, 0.4), MetricLevel::Bad);
    }

    #[test]
    fn metric_level_colors_distinct() {
        let g = MetricLevel::Good.color();
        let o = MetricLevel::Ok.color();
        let b = MetricLevel::Bad.color();
        assert_ne!(g, o);
        assert_ne!(g, b);
        assert_ne!(o, b);
    }
}
