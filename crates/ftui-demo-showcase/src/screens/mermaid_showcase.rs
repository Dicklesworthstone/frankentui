#![forbid(unsafe_code)]

//! Mermaid showcase screen — state + command handling scaffold.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEventKind};
use ftui_core::geometry::Rect;
use ftui_extras::mermaid;
use ftui_extras::mermaid::{
    DiagramPalettePreset, MermaidCompatibilityMatrix, MermaidConfig, MermaidDiagramIr,
    MermaidError, MermaidErrorMode, MermaidFallbackPolicy, MermaidGlyphMode, MermaidLinkMode,
    MermaidRenderMode, MermaidTier, MermaidWrapMode, ShowcaseMode,
};
use ftui_extras::mermaid_layout;
use ftui_extras::mermaid_render;
use ftui_extras::mermaid_render::SelectionState;
use ftui_layout::{Constraint, Flex};
use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::Style;
use ftui_text::{Line, Span, Text};
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::paragraph::Paragraph;
use std::cell::Cell as StdCell;

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

const INIT_DIRECTIVE_DEMO: &str = r##"%%{init: {"theme":"base","themeVariables":{"primaryColor":"#ffcc00","primaryTextColor":"#111111","primaryBorderColor":"#ff9900"},"flowchart":{"direction":"TB"}}}%%"##;

const LINK_DEMO_FLOW_BASIC: &str = r#"click C "https://example.com/ok" "OK"
click D "https://example.com/fix" "Fix""#;

const MERMAID_JSONL_EVENT: &str = "mermaid_render";
static MERMAID_JSONL_SEQ: AtomicU64 = AtomicU64::new(0);

fn hash64_str(value: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

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

const PALETTE_ORDER: &[DiagramPalettePreset] = &[
    DiagramPalettePreset::Default,
    DiagramPalettePreset::Corporate,
    DiagramPalettePreset::Neon,
    DiagramPalettePreset::Monochrome,
    DiagramPalettePreset::Pastel,
    DiagramPalettePreset::HighContrast,
];

fn next_palette(current: DiagramPalettePreset) -> DiagramPalettePreset {
    let idx = PALETTE_ORDER
        .iter()
        .position(|&p| p == current)
        .unwrap_or(0);
    PALETTE_ORDER[(idx + 1) % PALETTE_ORDER.len()]
}

fn prev_palette(current: DiagramPalettePreset) -> DiagramPalettePreset {
    let idx = PALETTE_ORDER
        .iter()
        .position(|&p| p == current)
        .unwrap_or(0);
    PALETTE_ORDER[(idx + PALETTE_ORDER.len() - 1) % PALETTE_ORDER.len()]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuardProfile {
    Default,
    Tight,
}

impl GuardProfile {
    const fn next(self) -> Self {
        match self {
            Self::Default => Self::Tight,
            Self::Tight => Self::Default,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::Tight => "Tight",
        }
    }
}

/// Diagram family for type-safe filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum SampleFamily {
    Flow,
    Sequence,
    Class,
    State,
    Er,
    Gantt,
    Mindmap,
    Pie,
    GitGraph,
    Journey,
    Requirement,
    BlockBeta,
    Unsupported,
}

impl SampleFamily {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Flow => "flow",
            Self::Sequence => "sequence",
            Self::Class => "class",
            Self::State => "state",
            Self::Er => "er",
            Self::Gantt => "gantt",
            Self::Mindmap => "mindmap",
            Self::Pie => "pie",
            Self::GitGraph => "gitgraph",
            Self::Journey => "journey",
            Self::Requirement => "requirement",
            Self::BlockBeta => "block-beta",
            Self::Unsupported => "unsupported",
        }
    }

    const ALL: &[Self] = &[
        Self::Flow,
        Self::Sequence,
        Self::Class,
        Self::State,
        Self::Er,
        Self::Gantt,
        Self::Mindmap,
        Self::Pie,
        Self::GitGraph,
        Self::Journey,
        Self::Requirement,
        Self::BlockBeta,
        Self::Unsupported,
    ];
}

/// Sample complexity tier for filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SampleComplexity {
    /// Small: 1-5 nodes/entities.
    S,
    /// Medium: 6-20 nodes/entities.
    M,
    /// Large: 20+ nodes/entities or deep nesting.
    L,
}

impl SampleComplexity {
    const fn as_str(self) -> &'static str {
        match self {
            Self::S => "S",
            Self::M => "M",
            Self::L => "L",
        }
    }
}

/// Default viewport size hint for a sample (width, height in terminal cells).
#[derive(Debug, Clone, Copy)]
struct SampleSizeHint {
    width: u16,
    height: u16,
}

#[derive(Debug, Clone, Copy)]
struct MermaidSample {
    /// Stable identifier for referencing this sample.
    id: &'static str,
    /// Human-readable display name.
    name: &'static str,
    /// Diagram family.
    family: SampleFamily,
    /// Complexity tier.
    complexity: SampleComplexity,
    /// Searchable category tags.
    tags: &'static [&'static str],
    /// Feature coverage tags (must be in KNOWN_FEATURE_TAGS).
    features: &'static [&'static str],
    /// Rendering edge cases this sample exercises.
    edge_cases: &'static [&'static str],
    /// Default viewport size hint (minimum comfortable rendering area).
    default_size: SampleSizeHint,
    /// Optional notes about this sample's purpose or quirks.
    notes: &'static str,
    /// Raw Mermaid source text.
    source: &'static str,
}

/// Sample registry with selection and filtering.
struct SampleRegistry;

impl SampleRegistry {
    /// All registered samples.
    fn all() -> &'static [MermaidSample] {
        DEFAULT_SAMPLES
    }

    /// Filter samples by diagram family.
    fn by_family(family: SampleFamily) -> Vec<&'static MermaidSample> {
        DEFAULT_SAMPLES
            .iter()
            .filter(|s| s.family == family)
            .collect()
    }

    /// Filter samples by minimum complexity.
    fn by_min_complexity(min: SampleComplexity) -> Vec<&'static MermaidSample> {
        DEFAULT_SAMPLES
            .iter()
            .filter(|s| s.complexity >= min)
            .collect()
    }

    /// Filter samples by exact complexity tier.
    fn by_complexity(tier: SampleComplexity) -> Vec<&'static MermaidSample> {
        DEFAULT_SAMPLES
            .iter()
            .filter(|s| s.complexity == tier)
            .collect()
    }

    /// Filter samples that fit within a given viewport size.
    fn by_max_size(width: u16, height: u16) -> Vec<&'static MermaidSample> {
        DEFAULT_SAMPLES
            .iter()
            .filter(|s| s.default_size.width <= width && s.default_size.height <= height)
            .collect()
    }

    /// Filter samples that exercise a specific feature tag.
    fn by_feature(tag: &str) -> Vec<&'static MermaidSample> {
        DEFAULT_SAMPLES
            .iter()
            .filter(|s| s.features.contains(&tag))
            .collect()
    }

    /// Filter samples matching any of the given tags.
    fn by_any_tag(tags: &[&str]) -> Vec<&'static MermaidSample> {
        DEFAULT_SAMPLES
            .iter()
            .filter(|s| s.tags.iter().any(|t| tags.contains(t)))
            .collect()
    }

    /// Find a sample by its stable id.
    fn by_id(id: &str) -> Option<&'static MermaidSample> {
        DEFAULT_SAMPLES.iter().find(|s| s.id == id)
    }

    /// Combined filter: family + complexity + max size.
    fn select(
        family: Option<SampleFamily>,
        complexity: Option<SampleComplexity>,
        max_width: Option<u16>,
        max_height: Option<u16>,
    ) -> Vec<&'static MermaidSample> {
        DEFAULT_SAMPLES
            .iter()
            .filter(|s| {
                family.is_none_or(|f| s.family == f)
                    && complexity.is_none_or(|c| s.complexity == c)
                    && max_width.is_none_or(|w| s.default_size.width <= w)
                    && max_height.is_none_or(|h| s.default_size.height <= h)
            })
            .collect()
    }
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
// | Class       | Class       | Class Basic, Class Members,          | Good     |
// |             |             | Class Hierarchy                      |          |
// | State       | State       | State Basic, State Composite,        | Good     |
// |             |             | State Concurrent                     |          |
// | ER          | Er          | ER Basic, ER Multi-Entity            | Moderate |
// | Gantt       | Gantt       | Gantt Basic, Gantt Dependencies      | Moderate |
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
// | Node shapes: ()      | Flow Node Shapes                  | —                             |
// | Node shapes: ([])    | Flow Node Shapes                  | —                             |
// | Node shapes: [[]]    | Flow Node Shapes                  | —                             |
// | Node shapes: {{}}    | Flow Node Shapes                  | —                             |
// | Node shapes: (())    | Flow Node Shapes                  | —                             |
// | Node shapes: >]      | Flow Node Shapes                  | —                             |
// | Edge labels          | Flow Basic, Flow Long Labels,     | —                             |
// |                      | Flow Subgraphs                    |                               |
// | Dotted edges -.->    | Flow Dense                        | —                             |
// | Thick edges ==>      | Flow Dense                        | —                             |
// | Bidir edges <-->     | Flow Dense                        | —                             |
// | Endpoint markers o/x | Flow Dense                        | —                             |
// | Subgraphs            | Flow Subgraphs                    | —                             |
// | Nested subgraphs     | Flow Subgraphs                    | —                             |
// | classDef             | Flow Styles                       | —                             |
// | style directive      | Flow Styles                       | —                             |
// | linkStyle            | Flow Basic (links toggle)         | —                             |
// | init directives      | Flow Basic (init toggle)          | —                             |
// | click/link           | Flow Basic (links toggle)         | —                             |
// | Unicode labels       | Flow Unicode                      | —                             |
// | Long/wrap labels     | Flow Long Labels                  | —                             |
// | ER cardinality       | ER Basic, ER Multi-Entity        | —                             |
// | ER many entities     | ER Multi-Entity                  | —                             |
// | Class members        | Class Members, Class Hierarchy   | —                             |
// | Class inheritance    | Class Hierarchy                  | —                             |
// | Class visibility     | Class Hierarchy                  | —                             |
// | State composites     | State Composite, State Concurrent| —                             |
// | State notes          | State Composite                   | —                             |
// | State fork/join      | State Concurrent                  | —                             |
// | State choices        | State Concurrent                  | —                             |
// | Gantt sections       | Gantt Basic, Gantt Dependencies  | —                             |
// | Gantt tasks          | Gantt Basic, Gantt Dependencies  | —                             |
// | Gantt deps/milestone | Gantt Dependencies                | —                             |
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
// | RL        | Flow Dense               | —                                   |
// | BT        | Flow Subgraphs           | —                                   |
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
    "node-rounded",
    "node-stadium",
    "node-subroutine",
    "node-hexagon",
    "node-circle",
    "node-asymmetric",
    "edge-labels",
    "subgraph",
    "classDef",
    "style",
    "linkStyle",
    "init-directives",
    "click-link",
    "unicode-labels",
    "long-labels",
    "many-nodes",
    "many-edges",
    "dotted-edges",
    "thick-edges",
    "bidir-edges",
    "endpoint-markers",
    "direction-rl",
    "direction-bt",
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
    // Block-beta features
    "block-columns",
    "block-spans",
    "block-nesting",
    // ER expanded features
    "er-many-entity",
    "er-varied-cardinality",
    // Gantt expanded features
    "task-deps",
    "milestones",
    "many-tasks",
    // Class expanded features
    "inheritance-chain",
    "visibility-modifiers",
    // State expanded features
    "fork-join",
    "choice-pseudostate",
];

/// Features known to be supported but lacking dedicated samples.
/// Each entry is (feature_tag, description).
const FEATURE_GAPS: &[(&str, &str)] = &[];

const DEFAULT_SAMPLES: &[MermaidSample] = &[
    MermaidSample {
        id: "flow-basic",
        name: "Flow Basic",
        family: SampleFamily::Flow,
        complexity: SampleComplexity::S,
        tags: &["branch", "decision"],
        features: &[
            "edge-labels",
            "basic-nodes",
            "linkStyle",
            "init-directives",
            "click-link",
        ],
        edge_cases: &[],
        default_size: SampleSizeHint {
            width: 40,
            height: 10,
        },
        notes: "Minimal branching with decision node, covers LR direction",
        source: r#"graph LR
A[Start] --> B{Check}
B -->|Yes| C[OK]
B -->|No| D[Fix]"#,
    },
    MermaidSample {
        id: "flow-subgraphs",
        name: "Flow Subgraphs",
        family: SampleFamily::Flow,
        complexity: SampleComplexity::M,
        tags: &["subgraph", "clusters"],
        features: &["subgraph", "edge-labels", "direction-bt"],
        edge_cases: &["nested-grouping"],
        default_size: SampleSizeHint {
            width: 60,
            height: 20,
        },
        notes: "Tests cluster rendering and cross-cluster edges in BT layout",
        source: r#"graph BT
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
        id: "flow-dense",
        name: "Flow Dense",
        family: SampleFamily::Flow,
        complexity: SampleComplexity::L,
        tags: &["dense", "dag"],
        features: &[
            "many-nodes",
            "many-edges",
            "dotted-edges",
            "thick-edges",
            "bidir-edges",
            "endpoint-markers",
            "direction-rl",
        ],
        edge_cases: &["edge-crossing"],
        default_size: SampleSizeHint {
            width: 80,
            height: 30,
        },
        notes: "Stress test for edge crossing minimization and edge-style variants",
        source: r#"graph RL
  A[Start] -.-> B[Queue]
  B ==> C[Compute]
  C <--> D[Cache]
  D --> E[Fanout]
  E --> F[Sink]
  F --> G[Audit]
  C --> H[Branch]
  H --> I[Merge]
  I --> J[Commit]
  J --> K[Done]
  K o--o L[Open]
  L x--x M[Closed]"#,
    },
    MermaidSample {
        id: "flow-long-labels",
        name: "Flow Long Labels",
        family: SampleFamily::Flow,
        complexity: SampleComplexity::M,
        tags: &["labels", "wrap"],
        features: &["long-labels", "edge-labels"],
        edge_cases: &["long-text"],
        default_size: SampleSizeHint {
            width: 60,
            height: 20,
        },
        notes: "Tests label wrapping and truncation",
        source: r#"graph TD
  A[This is a very long label that should wrap or truncate neatly] --> B[Another extremely verbose node label]
  B --> C{Decision with a long question that should still render}
  C -->|Yes| D[Proceed to the next step]
  C -->|No| E[Abort with a meaningful explanation]"#,
    },
    MermaidSample {
        id: "flow-unicode",
        name: "Flow Unicode",
        family: SampleFamily::Flow,
        complexity: SampleComplexity::S,
        tags: &["unicode", "labels"],
        features: &["unicode-labels"],
        edge_cases: &["non-ascii"],
        default_size: SampleSizeHint {
            width: 40,
            height: 10,
        },
        notes: "Non-ASCII label rendering (CJK, Greek, accented)",
        source: r#"graph LR
  A[Δ Start] --> B[β-Compute]
  B --> C[東京]
  C --> D[naïve café]"#,
    },
    MermaidSample {
        id: "flow-styles",
        name: "Flow Styles",
        family: SampleFamily::Flow,
        complexity: SampleComplexity::M,
        tags: &["classdef", "style"],
        features: &["classDef", "style"],
        edge_cases: &["style-lines"],
        default_size: SampleSizeHint {
            width: 60,
            height: 20,
        },
        notes: "classDef and style directives for custom node appearance",
        source: r#"graph LR
  A[Primary] --> B[Secondary]
  B --> C[Accent]
  classDef hot fill:#ff6b6b,stroke:#333,stroke-width:2px;
  class A hot;
  style C fill:#6bc5ff,stroke:#333,stroke-width:2px;"#,
    },
    MermaidSample {
        id: "sequence-mini",
        name: "Sequence Mini",
        family: SampleFamily::Sequence,
        complexity: SampleComplexity::S,
        tags: &["compact"],
        features: &["messages", "responses"],
        edge_cases: &[],
        default_size: SampleSizeHint {
            width: 40,
            height: 12,
        },
        notes: "Minimal sequence: two actors, one exchange",
        source: r#"sequenceDiagram
  Alice->>Bob: Hello
  Bob-->>Alice: Hi!"#,
    },
    MermaidSample {
        id: "sequence-checkout",
        name: "Sequence Checkout",
        family: SampleFamily::Sequence,
        complexity: SampleComplexity::M,
        tags: &["multi-hop", "api"],
        features: &["round-trip", "multi-actor"],
        edge_cases: &["mixed-arrows"],
        default_size: SampleSizeHint {
            width: 60,
            height: 20,
        },
        notes: "Multi-actor API flow with mixed arrow types",
        source: r#"sequenceDiagram
  Client->>API: POST /checkout
  API->>Auth: Validate token
  Auth-->>API: OK
  API->>DB: Create order
  DB-->>API: id=42
  API-->>Client: 201 Created"#,
    },
    MermaidSample {
        id: "sequence-dense",
        name: "Sequence Dense",
        family: SampleFamily::Sequence,
        complexity: SampleComplexity::L,
        tags: &["dense", "timing"],
        features: &["many-messages"],
        edge_cases: &["tight-spacing"],
        default_size: SampleSizeHint {
            width: 80,
            height: 30,
        },
        notes: "Stress test for tight vertical message spacing",
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
        id: "class-basic",
        name: "Class Basic",
        family: SampleFamily::Class,
        complexity: SampleComplexity::S,
        tags: &["inheritance", "association"],
        features: &["relations"],
        edge_cases: &[],
        default_size: SampleSizeHint {
            width: 50,
            height: 15,
        },
        notes: "Inheritance and association relations",
        source: r#"classDiagram
  class User
  class Admin
  class Order
  User <|-- Admin
  User --> Order"#,
    },
    MermaidSample {
        id: "class-members",
        name: "Class Members",
        family: SampleFamily::Class,
        complexity: SampleComplexity::M,
        tags: &["fields", "methods"],
        features: &["class-members"],
        edge_cases: &["long-member-lines"],
        default_size: SampleSizeHint {
            width: 60,
            height: 20,
        },
        notes: "Field and method compartment rendering",
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
        id: "state-basic",
        name: "State Basic",
        family: SampleFamily::State,
        complexity: SampleComplexity::S,
        tags: &["start-end"],
        features: &["state-edges"],
        edge_cases: &[],
        default_size: SampleSizeHint {
            width: 40,
            height: 12,
        },
        notes: "Start/end pseudo-nodes with simple transitions",
        source: r#"stateDiagram-v2
  [*] --> Idle
  Idle --> Busy: start
  Busy --> Idle: done
  Busy --> [*]: exit"#,
    },
    MermaidSample {
        id: "state-composite",
        name: "State Composite",
        family: SampleFamily::State,
        complexity: SampleComplexity::M,
        tags: &["composite", "notes"],
        features: &["substates", "notes"],
        edge_cases: &["nested-blocks"],
        default_size: SampleSizeHint {
            width: 60,
            height: 20,
        },
        notes: "Nested substates and note annotations",
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
        id: "er-basic",
        name: "ER Basic",
        family: SampleFamily::Er,
        complexity: SampleComplexity::M,
        tags: &["cardinality", "relations"],
        features: &["er-arrows"],
        edge_cases: &[],
        default_size: SampleSizeHint {
            width: 60,
            height: 15,
        },
        notes: "Entity-relationship cardinality notation",
        source: r#"erDiagram
  CUSTOMER ||--o{ ORDER : places
  ORDER ||--|{ LINE_ITEM : contains
  PRODUCT ||--o{ LINE_ITEM : in"#,
    },
    MermaidSample {
        id: "gantt-basic",
        name: "Gantt Basic",
        family: SampleFamily::Gantt,
        complexity: SampleComplexity::M,
        tags: &["sections", "tasks"],
        features: &["title", "sections"],
        edge_cases: &["date-meta"],
        default_size: SampleSizeHint {
            width: 60,
            height: 15,
        },
        notes: "Title, sections, and dated task bars",
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
        id: "mindmap-seed",
        name: "Mindmap Seed",
        family: SampleFamily::Mindmap,
        complexity: SampleComplexity::S,
        tags: &["tree"],
        features: &["indent"],
        edge_cases: &[],
        default_size: SampleSizeHint {
            width: 40,
            height: 12,
        },
        notes: "Simple indentation-based tree",
        source: r#"mindmap
  root
    alpha
    beta
      beta-1
      beta-2"#,
    },
    MermaidSample {
        id: "mindmap-deep",
        name: "Mindmap Deep",
        family: SampleFamily::Mindmap,
        complexity: SampleComplexity::L,
        tags: &["deep", "wide"],
        features: &["multi-level"],
        edge_cases: &["many-nodes"],
        default_size: SampleSizeHint {
            width: 80,
            height: 25,
        },
        notes: "Multi-level deep hierarchy stress test",
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
        id: "pie-basic",
        name: "Pie Basic",
        family: SampleFamily::Pie,
        complexity: SampleComplexity::S,
        tags: &["title", "showdata"],
        features: &["title", "showData"],
        edge_cases: &[],
        default_size: SampleSizeHint {
            width: 40,
            height: 15,
        },
        notes: "Title and showData flag with basic slices",
        source: r#"pie showData
  title Market Share
  "Alpha": 38
  "Beta": 27
  "Gamma": 20
  "Delta": 15"#,
    },
    MermaidSample {
        id: "pie-many",
        name: "Pie Many",
        family: SampleFamily::Pie,
        complexity: SampleComplexity::M,
        tags: &["many-slices"],
        features: &["labels"],
        edge_cases: &["small-slices"],
        default_size: SampleSizeHint {
            width: 50,
            height: 18,
        },
        notes: "Six+ slices testing small-slice rendering",
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
        id: "gitgraph-basic",
        name: "Gitgraph Basic",
        family: SampleFamily::GitGraph,
        complexity: SampleComplexity::M,
        tags: &["gitgraph", "branches"],
        features: &["branches", "commits"],
        edge_cases: &[],
        default_size: SampleSizeHint {
            width: 60,
            height: 15,
        },
        notes: "Basic gitGraph with branches and merges",
        source: r#"gitGraph
  commit
  branch feature
  checkout feature
  commit
  checkout main
  merge feature"#,
    },
    MermaidSample {
        id: "journey-basic",
        name: "Journey Basic",
        family: SampleFamily::Journey,
        complexity: SampleComplexity::M,
        tags: &["journey", "sections"],
        features: &["sections", "scores"],
        edge_cases: &[],
        default_size: SampleSizeHint {
            width: 60,
            height: 15,
        },
        notes: "User journey with sections and task scores",
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
        id: "requirement-basic",
        name: "Requirement Basic",
        family: SampleFamily::Requirement,
        complexity: SampleComplexity::M,
        tags: &["requirement", "traceability"],
        features: &["requirements"],
        edge_cases: &[],
        default_size: SampleSizeHint {
            width: 60,
            height: 15,
        },
        notes: "Requirements with traceability relations",
        source: r#"requirementDiagram
  requirement req1 {
    id: 1
    text: Must render diagrams
    risk: high
    verifyMethod: test
  }"#,
    },
    MermaidSample {
        id: "block-beta-basic",
        name: "Block Beta Basic",
        family: SampleFamily::BlockBeta,
        complexity: SampleComplexity::S,
        tags: &["block-beta", "columns"],
        features: &["block-columns", "block-spans"],
        edge_cases: &[],
        default_size: SampleSizeHint {
            width: 60,
            height: 15,
        },
        notes: "Columns + spans in a simple grid",
        source: r#"block-beta
  columns 3
  a["Frontend"] b["Backend"] c["Database"]
  space
  d["Load Balancer"]:3"#,
    },
    MermaidSample {
        id: "block-beta-nested",
        name: "Block Beta Nested",
        family: SampleFamily::BlockBeta,
        complexity: SampleComplexity::M,
        tags: &["block-beta", "nested"],
        features: &["block-columns", "block-spans", "block-nesting"],
        edge_cases: &["nested-blocks"],
        default_size: SampleSizeHint {
            width: 80,
            height: 25,
        },
        notes: "Nested blocks and many spans (stress)",
        source: r#"block-beta
  columns 4
  a["Service A"]:2 b["Service B"]:2
  block:inner1:2
    columns 2
    c["Cache"] d["Queue"]
  end
  block:inner2:2
    columns 2
    e["Worker 1"] f["Worker 2"]
  end
	  g["Load Balancer"]:4
	  space:2
	  h["Database"]:2"#,
    },
    MermaidSample {
        id: "flow-node-shapes",
        name: "Flow Node Shapes",
        family: SampleFamily::Flow,
        complexity: SampleComplexity::S,
        tags: &["shapes", "node-shapes"],
        features: &[
            "basic-nodes",
            "node-rounded",
            "node-stadium",
            "node-subroutine",
            "node-hexagon",
            "node-circle",
            "node-asymmetric",
        ],
        edge_cases: &[],
        default_size: SampleSizeHint {
            width: 60,
            height: 12,
        },
        notes: "Flowchart node-shape syntax coverage: rounded, stadium, subroutine, hexagon, circle, asymmetric",
        source: r#"graph LR
  A(Rounded) --> B([Stadium])
  B --> C[[Subroutine]]
  C --> D{{Hexagon}}
  D --> E((Circle))
  E --> F>Asymmetric]"#,
    },
    MermaidSample {
        id: "er-multi",
        name: "ER Multi-Entity",
        family: SampleFamily::Er,
        complexity: SampleComplexity::L,
        tags: &["multi-entity", "complex-relations"],
        features: &["er-arrows", "er-many-entity", "er-varied-cardinality"],
        edge_cases: &["many-relations"],
        default_size: SampleSizeHint {
            width: 80,
            height: 25,
        },
        notes: "Multiple entities with varied cardinality styles and attribute lists",
        source: r#"erDiagram
  CUSTOMER ||--o{ ORDER : places
  CUSTOMER {
    string name
    int id
  }
  ORDER ||--|{ LINE_ITEM : contains
  ORDER {
    int orderId
    date created
  }
  PRODUCT ||--o{ LINE_ITEM : includes
  PRODUCT {
    string name
    float price
  }
  WAREHOUSE }|--|| PRODUCT : stocks
  SUPPLIER }o--o{ PRODUCT : supplies"#,
    },
    MermaidSample {
        id: "gantt-deps",
        name: "Gantt Dependencies",
        family: SampleFamily::Gantt,
        complexity: SampleComplexity::L,
        tags: &["dependencies", "milestones", "multi-section"],
        features: &["title", "sections", "task-deps", "milestones", "many-tasks"],
        edge_cases: &["overlapping-tasks"],
        default_size: SampleSizeHint {
            width: 80,
            height: 25,
        },
        notes: "Gantt with task dependencies, milestones, and multiple sections",
        source: r#"gantt
  title Project Alpha
  dateFormat YYYY-MM-DD
  section Planning
  Requirements :a1, 2024-01-01, 7d
  Architecture :a2, after a1, 5d
  section Build
  Backend :b1, after a2, 10d
  Frontend :b2, after a2, 12d
  Integration :b3, after b1, 5d
  section QA
  Testing :c1, after b3, 7d
  Staging :c2, after c1, 3d
  Release :milestone, after c2, 0d"#,
    },
    MermaidSample {
        id: "class-hierarchy",
        name: "Class Hierarchy",
        family: SampleFamily::Class,
        complexity: SampleComplexity::L,
        tags: &["inheritance", "abstract", "visibility"],
        features: &[
            "relations",
            "class-members",
            "inheritance-chain",
            "visibility-modifiers",
        ],
        edge_cases: &["deep-chain"],
        default_size: SampleSizeHint {
            width: 80,
            height: 25,
        },
        notes: "Deep inheritance chain with visibility modifiers and abstract types",
        source: r#"classDiagram
  class Shape {
    <<abstract>>
    +area() float
    +perimeter() float
  }
  class Polygon {
    +int sides
    +vertices() List
  }
  class Rectangle {
    -float width
    -float height
    +area() float
  }
  class Square {
    +Square(float side)
  }
  Shape <|-- Polygon
  Polygon <|-- Rectangle
  Rectangle <|-- Square
  Shape <|.. Drawable"#,
    },
    MermaidSample {
        id: "state-concurrent",
        name: "State Concurrent",
        family: SampleFamily::State,
        complexity: SampleComplexity::L,
        tags: &["concurrent", "fork", "choice"],
        features: &[
            "state-edges",
            "substates",
            "fork-join",
            "choice-pseudostate",
        ],
        edge_cases: &["parallel-regions"],
        default_size: SampleSizeHint {
            width: 80,
            height: 25,
        },
        notes: "Fork/join regions and choice pseudostates for concurrency coverage",
        source: r#"stateDiagram-v2
  [*] --> Validate
  Validate --> Processing: valid
  Validate --> Rejected: invalid
  state Processing {
    [*] --> FetchData
    FetchData --> Transform
    Transform --> Store
    Store --> [*]
  }
  Processing --> Review
  state choice_state <<choice>>
  Review --> choice_state
  choice_state --> Approved: score > 80
  choice_state --> NeedsWork: score <= 80
  NeedsWork --> Validate
  Approved --> [*]
  Rejected --> [*]"#,
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
    /// Number of warnings encountered (unsupported features, guards, etc).
    warning_count: Option<u32>,
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

/// Granular debug overlay toggles for visualizing layout internals.
#[derive(Debug, Clone, Copy, Default)]
struct DebugOverlayFlags {
    /// Show node bounding boxes.
    bounds: bool,
    /// Show edge routing waypoints.
    routes: bool,
    /// Show port/connection markers.
    ports: bool,
    /// Show alignment grid.
    grid: bool,
}

impl DebugOverlayFlags {
    fn any_active(self) -> bool {
        self.bounds || self.routes || self.ports || self.grid
    }

    fn toggle_all(&mut self) {
        if self.any_active() {
            *self = Self::default();
        } else {
            self.bounds = true;
            self.routes = true;
            self.ports = true;
            self.grid = true;
        }
    }
}

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
    link_mode: MermaidLinkMode,
    init_directives_enabled: bool,
    error_mode: MermaidErrorMode,
    guard_profile: GuardProfile,
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
    /// Current interaction mode (Normal/Inspect/Search).
    mode: ShowcaseMode,
    /// Index of the focused node in Inspect mode.
    selected_node_idx: Option<usize>,
    /// Active search query text.
    search_query: String,
    /// Index into search_matches for the current highlighted match.
    search_match_idx: usize,
    /// Node indices matching the current search query.
    search_matches: Vec<usize>,
    /// Active color palette preset.
    palette: DiagramPalettePreset,
    /// Whether the help overlay is visible.
    help_visible: bool,
    /// Whether the minimap overlay is visible.
    show_minimap: bool,
    /// Whether the debug overlay is visible.
    /// Debug overlay flags (node bounds, edge routes, ports, grid).
    debug_overlay: DebugOverlayFlags,
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
    /// Effective Mermaid source used for the cached analysis (includes injected init/link directives).
    source: Option<String>,
    cache_hits: u64,
    cache_misses: u64,
    last_cache_hit: bool,
    /// Adjacency list for node navigation (rebuilt on layout change).
    adjacency: Vec<Vec<(usize, usize, bool)>>,
    /// Last rendered selection (for cache invalidation).
    selected_node_idx: Option<usize>,
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
            source: None,
            cache_hits: 0,
            cache_misses: 0,
            last_cache_hit: false,
            adjacency: Vec::new(),
            selected_node_idx: None,
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
            render_mode: MermaidRenderMode::Braille,
            wrap_mode: MermaidWrapMode::WordChar,
            styles_enabled: true,
            link_mode: MermaidLinkMode::Off,
            init_directives_enabled: false,
            error_mode: MermaidErrorMode::Panel,
            guard_profile: GuardProfile::Default,
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
            mode: ShowcaseMode::Normal,
            selected_node_idx: None,
            search_query: String::new(),
            search_match_idx: 0,
            search_matches: Vec::new(),
            palette: DiagramPalettePreset::Default,
            help_visible: false,
            show_minimap: false,
            debug_overlay: DebugOverlayFlags::default(),
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

    fn build_config(&self) -> MermaidConfig {
        let mut config = MermaidConfig {
            glyph_mode: self.glyph_mode,
            tier_override: self.tier,
            render_mode: self.render_mode,
            wrap_mode: self.wrap_mode,
            enable_styles: self.styles_enabled,
            enable_init_directives: self.init_directives_enabled,
            enable_links: self.link_mode != MermaidLinkMode::Off,
            link_mode: self.link_mode,
            error_mode: self.error_mode,
            palette: self.palette,
            ..MermaidConfig::default()
        };

        // Optional E2E logging hook. This is intentionally env-driven so PTY/E2E
        // tests can assert mermaid_prepare JSONL without adding CLI flags.
        if let Ok(path) = std::env::var("FTUI_MERMAID_LOG_PATH") {
            let trimmed = path.trim();
            if !trimmed.is_empty() {
                config.log_path = Some(trimmed.to_string());
            }
        }

        match self.guard_profile {
            GuardProfile::Default => {}
            GuardProfile::Tight => {
                config.max_nodes = 40;
                config.max_edges = 80;
                config.max_label_chars = 32;
                config.max_label_lines = 2;
            }
        }

        match self.layout_mode {
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

    fn effective_source(&self, sample: MermaidSample) -> Cow<'static, str> {
        let inject_init = self.init_directives_enabled;
        let inject_links = self.link_mode != MermaidLinkMode::Off && sample.id == "flow-basic";

        if !inject_init && !inject_links {
            return Cow::Borrowed(sample.source);
        }

        let mut out = String::new();
        if inject_init {
            out.push_str(INIT_DIRECTIVE_DEMO);
            out.push('\n');
        }
        out.push_str(sample.source);
        if inject_links {
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(LINK_DEMO_FLOW_BASIC);
        }
        Cow::Owned(out)
    }

    fn normalize(&mut self) {
        self.viewport_zoom = self.viewport_zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        self.clamp_viewport_override();
        self.recompute_metrics();
    }

    /// Get the number of nodes from the last cached layout (0 if no layout).
    fn cache_node_count(&self) -> usize {
        // Access through MermaidShowcaseScreen's cache is not possible here,
        // so estimate from the current sample's IR node count.
        // The actual node count will be refined when the cache is available.
        self.selected_sample()
            .map(|s| {
                // Simple heuristic: count lines that look like node definitions
                s.source
                    .lines()
                    .filter(|l| {
                        let l = l.trim();
                        !l.is_empty()
                            && !l.starts_with("graph")
                            && !l.starts_with("flowchart")
                            && !l.starts_with("subgraph")
                            && !l.starts_with("end")
                            && !l.starts_with("%%")
                            && !l.starts_with("click")
                            && !l.starts_with("classDef")
                            && !l.starts_with("style")
                            && !l.starts_with("linkStyle")
                    })
                    .count()
            })
            .unwrap_or(0)
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
        let source = self.effective_source(sample);
        let config = self.build_config();
        let matrix = MermaidCompatibilityMatrix::default();
        let policy = MermaidFallbackPolicy::default();

        let parse_start = Instant::now();
        let parsed = mermaid::parse_with_diagnostics(source.as_ref());
        let parse_elapsed = parse_start.elapsed();
        let diagram_type = parsed.ast.diagram_type;

        let ir_parse = mermaid::normalize_ast_to_ir(&parsed.ast, &config, &matrix, &policy);
        let warning_count = ir_parse.warnings.len() as u32;
        let error_count = parsed.errors.len().saturating_add(ir_parse.errors.len()) as u32;

        let layout_start = Instant::now();
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
        let layout_elapsed = layout_start.elapsed();

        let mut snap = MermaidMetricsSnapshot::from_layout(&layout);
        snap.parse_ms = Some(parse_elapsed.as_secs_f32() * 1000.0);
        snap.layout_ms = Some(layout_elapsed.as_secs_f32() * 1000.0);
        if let Some(ref plan) = layout.degradation {
            snap.set_fallback(self.tier, plan);
        }
        snap.warning_count = Some(warning_count);
        snap.error_count = Some(error_count);
        self.metrics = snap;
        self.emit_metrics_jsonl(sample, diagram_type);
    }

    fn emit_metrics_jsonl(&self, sample: MermaidSample, diagram_type: mermaid::DiagramType) {
        if !jsonl_enabled() {
            return;
        }
        let seq = MERMAID_JSONL_SEQ.fetch_add(1, Ordering::Relaxed);
        let run_id = determinism::demo_run_id();
        let seed = determinism::demo_seed(0);
        let screen_mode = determinism::demo_screen_mode();
        let line = self.metrics_jsonl_line(
            sample,
            diagram_type,
            seq,
            run_id.as_deref(),
            seed,
            &screen_mode,
        );
        let _ = writeln!(std::io::stderr(), "{line}");
    }

    fn metrics_jsonl_line(
        &self,
        sample: MermaidSample,
        diagram_type: mermaid::DiagramType,
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
        json.push_str(&format!(",\"sample_id\":\"{}\"", escape_json(sample.id)));
        json.push_str(&format!(
            ",\"sample_family\":\"{}\"",
            sample.family.as_str()
        ));
        json.push_str(&format!(
            ",\"diagram_type\":\"{}\"",
            escape_json(diagram_type.as_str())
        ));
        json.push_str(&format!(
            ",\"layout_mode\":\"{}\"",
            self.layout_mode.as_str()
        ));
        json.push_str(&format!(",\"tier\":\"{}\"", self.tier));
        json.push_str(&format!(",\"glyph_mode\":\"{}\"", self.glyph_mode));
        json.push_str(&format!(",\"render_mode\":\"{}\"", self.render_mode));
        json.push_str(&format!(",\"wrap_mode\":\"{}\"", self.wrap_mode));
        json.push_str(&format!(",\"styles_enabled\":{}", self.styles_enabled));
        json.push_str(&format!(
            ",\"enable_init_directives\":{}",
            self.init_directives_enabled
        ));
        json.push_str(&format!(
            ",\"enable_links\":{}",
            self.link_mode != MermaidLinkMode::Off
        ));
        json.push_str(&format!(",\"link_mode\":\"{}\"", self.link_mode));
        json.push_str(&format!(",\"error_mode\":\"{}\"", self.error_mode));
        json.push_str(&format!(
            ",\"guard_profile\":\"{}\"",
            self.guard_profile.as_str()
        ));
        json.push_str(&format!(",\"palette\":\"{}\"", self.palette));
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
        push_opt_u32(&mut json, "warning_count", self.metrics.warning_count);
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
            MermaidShowcaseAction::CycleLinkMode => {
                let was_enabled = self.link_mode != MermaidLinkMode::Off;
                self.link_mode = match self.link_mode {
                    MermaidLinkMode::Off => MermaidLinkMode::Inline,
                    MermaidLinkMode::Inline => MermaidLinkMode::Footnote,
                    MermaidLinkMode::Footnote => MermaidLinkMode::Off,
                };
                let enabled = self.link_mode != MermaidLinkMode::Off;
                if was_enabled != enabled {
                    self.bump_analysis();
                }
                self.bump_render();
                self.log_action("links", self.link_mode.to_string());
            }
            MermaidShowcaseAction::ToggleInitDirectives => {
                self.init_directives_enabled = !self.init_directives_enabled;
                self.bump_analysis();
                self.bump_render();
                self.log_action(
                    "init",
                    if self.init_directives_enabled {
                        "on"
                    } else {
                        "off"
                    }
                    .to_string(),
                );
            }
            MermaidShowcaseAction::CycleErrorMode => {
                self.error_mode = match self.error_mode {
                    MermaidErrorMode::Panel => MermaidErrorMode::Raw,
                    MermaidErrorMode::Raw => MermaidErrorMode::Both,
                    MermaidErrorMode::Both => MermaidErrorMode::Panel,
                };
                self.bump_render();
                self.log_action("errors", self.error_mode.to_string());
            }
            MermaidShowcaseAction::ToggleGuardProfile => {
                self.guard_profile = self.guard_profile.next();
                self.bump_all();
                self.log_action("guard", self.guard_profile.as_str().to_string());
            }
            MermaidShowcaseAction::CycleViewportPreset => {
                self.viewport_size_override = match self.viewport_size_override {
                    None => Some((80, 24)),
                    Some((80, 24)) => Some((120, 40)),
                    Some((120, 40)) => Some((200, 60)),
                    Some((200, 60)) => None,
                    Some(_) => Some((80, 24)),
                };
                self.bump_render();
                let detail = match self.viewport_size_override {
                    Some((cols, rows)) => format!("preset {cols}x{rows}"),
                    None => "preset auto".to_string(),
                };
                self.log_action("viewport", detail);
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
            MermaidShowcaseAction::CyclePalette => {
                self.palette = next_palette(self.palette);
                self.bump_render();
                self.log_action("palette", format!("{:?}", self.palette));
            }
            MermaidShowcaseAction::PrevPalette => {
                self.palette = prev_palette(self.palette);
                self.bump_render();
                self.log_action("palette", format!("{:?}", self.palette));
            }
            MermaidShowcaseAction::ToggleHelp => {
                self.help_visible = !self.help_visible;
                self.log_action(
                    "help",
                    if self.help_visible { "show" } else { "hide" }.to_string(),
                );
            }
            MermaidShowcaseAction::ToggleMinimap => {
                self.show_minimap = !self.show_minimap;
            }
            MermaidShowcaseAction::ToggleDebugOverlay => {
                self.debug_overlay.toggle_all();
                self.bump_render();
                self.log_action(
                    "debug",
                    if self.debug_overlay.any_active() {
                        "on"
                    } else {
                        "off"
                    }
                    .to_string(),
                );
            }
            MermaidShowcaseAction::ToggleDebugBounds => {
                self.debug_overlay.bounds = !self.debug_overlay.bounds;
                self.bump_render();
                self.log_action(
                    "debug:bounds",
                    if self.debug_overlay.bounds {
                        "on"
                    } else {
                        "off"
                    }
                    .to_string(),
                );
            }
            MermaidShowcaseAction::ToggleDebugRoutes => {
                self.debug_overlay.routes = !self.debug_overlay.routes;
                self.bump_render();
                self.log_action(
                    "debug:routes",
                    if self.debug_overlay.routes {
                        "on"
                    } else {
                        "off"
                    }
                    .to_string(),
                );
            }
            MermaidShowcaseAction::ToggleDebugPorts => {
                self.debug_overlay.ports = !self.debug_overlay.ports;
                self.bump_render();
                self.log_action(
                    "debug:ports",
                    if self.debug_overlay.ports {
                        "on"
                    } else {
                        "off"
                    }
                    .to_string(),
                );
            }
            MermaidShowcaseAction::ToggleDebugGrid => {
                self.debug_overlay.grid = !self.debug_overlay.grid;
                self.bump_render();
                self.log_action(
                    "debug:grid",
                    if self.debug_overlay.grid { "on" } else { "off" }.to_string(),
                );
            }
            MermaidShowcaseAction::SelectNextNode => {
                let node_count = self.cache_node_count();
                if node_count > 0 {
                    let idx = self.selected_node_idx.map_or(0, |i| (i + 1) % node_count);
                    self.selected_node_idx = Some(idx);
                    self.mode = ShowcaseMode::Inspect;
                    self.log_action("inspect", format!("node {idx}"));
                }
            }
            MermaidShowcaseAction::SelectPrevNode => {
                let node_count = self.cache_node_count();
                if node_count > 0 {
                    let idx = self
                        .selected_node_idx
                        .map_or(node_count - 1, |i| (i + node_count - 1) % node_count);
                    self.selected_node_idx = Some(idx);
                    self.mode = ShowcaseMode::Inspect;
                    self.log_action("inspect", format!("node {idx}"));
                }
            }
            MermaidShowcaseAction::EnterSearchMode => {
                self.mode = ShowcaseMode::Search;
                self.search_query.clear();
                self.search_matches.clear();
                self.search_match_idx = 0;
                self.log_action("search", "enter".to_string());
            }
            MermaidShowcaseAction::ExitMode => match self.mode {
                ShowcaseMode::Inspect => {
                    self.selected_node_idx = None;
                    self.mode = ShowcaseMode::Normal;
                    self.log_action("mode", "normal".to_string());
                }
                ShowcaseMode::Search => {
                    self.search_query.clear();
                    self.search_matches.clear();
                    self.search_match_idx = 0;
                    self.mode = ShowcaseMode::Normal;
                    self.log_action("mode", "normal".to_string());
                }
                ShowcaseMode::Normal => {}
            },
            MermaidShowcaseAction::NextSearchMatch => {
                if !self.search_matches.is_empty() {
                    self.search_match_idx = (self.search_match_idx + 1) % self.search_matches.len();
                    self.log_action(
                        "search",
                        format!(
                            "{}/{}",
                            self.search_match_idx + 1,
                            self.search_matches.len()
                        ),
                    );
                }
            }
            MermaidShowcaseAction::PrevSearchMatch => {
                if !self.search_matches.is_empty() {
                    self.search_match_idx = (self.search_match_idx + self.search_matches.len() - 1)
                        % self.search_matches.len();
                    self.log_action(
                        "search",
                        format!(
                            "{}/{}",
                            self.search_match_idx + 1,
                            self.search_matches.len()
                        ),
                    );
                }
            }
            // Navigation and search input are handled at the Screen level (need cache access).
            MermaidShowcaseAction::NavigateUp
            | MermaidShowcaseAction::NavigateDown
            | MermaidShowcaseAction::NavigateLeft
            | MermaidShowcaseAction::NavigateRight
            | MermaidShowcaseAction::SearchInput(_)
            | MermaidShowcaseAction::SearchBackspace => {}
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
    CycleLinkMode,
    ToggleInitDirectives,
    CycleErrorMode,
    ToggleGuardProfile,
    CycleViewportPreset,
    IncreaseViewportWidth,
    DecreaseViewportWidth,
    IncreaseViewportHeight,
    DecreaseViewportHeight,
    ResetViewportOverride,
    CollapsePanels,
    ToggleStatusLog,
    // Mega screen actions
    CyclePalette,
    PrevPalette,
    ToggleHelp,
    ToggleMinimap,
    ToggleDebugOverlay,
    ToggleDebugBounds,
    ToggleDebugRoutes,
    ToggleDebugPorts,
    ToggleDebugGrid,
    SelectNextNode,
    SelectPrevNode,
    NavigateUp,
    NavigateDown,
    NavigateLeft,
    NavigateRight,
    EnterSearchMode,
    ExitMode,
    NextSearchMatch,
    PrevSearchMatch,
    SearchInput(char),
    SearchBackspace,
}

/// Mermaid showcase screen scaffold (state + key handling).
pub struct MermaidShowcaseScreen {
    state: MermaidShowcaseState,
    cache: RefCell<MermaidRenderCache>,
    /// Cached samples panel area for mouse hit-testing.
    layout_samples: StdCell<Rect>,
    /// Cached viewport area for mouse hit-testing.
    layout_viewport: StdCell<Rect>,
    /// Cached controls/right panel area for mouse hit-testing.
    layout_right: StdCell<Rect>,
}

#[derive(Debug, Clone)]
pub struct MermaidHarnessFrameTelemetry {
    pub sample_id: String,
    pub sample_family: String,
    pub diagram_type: String,
    pub tier: String,
    pub glyph_mode: String,
    pub cache_hit: bool,
    pub checksum: u64,
    pub render_time_ms: Option<f32>,
    pub warnings: u32,
    pub guard_triggers: bool,
    pub config_hash: u64,
    pub init_config_hash: u64,
    pub capability_profile: String,
    pub link_count: u64,
    pub link_mode: String,
    pub legend_height: u16,
    pub parse_ms: Option<f32>,
    pub layout_ms: Option<f32>,
    pub route_ms: Option<f32>,
    pub render_ms: Option<f32>,
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
            layout_samples: StdCell::new(Rect::new(0, 0, 0, 0)),
            layout_viewport: StdCell::new(Rect::new(0, 0, 0, 0)),
            layout_right: StdCell::new(Rect::new(0, 0, 0, 0)),
        }
    }

    /// Number of built-in samples (used by the E2E harness).
    pub fn sample_count(&self) -> usize {
        self.state.samples.len()
    }

    /// Telemetry snapshot used by the Mermaid PTY harness JSONL stream.
    pub fn harness_frame_telemetry(&self, checksum: u64) -> MermaidHarnessFrameTelemetry {
        let selected = self.state.selected_sample();
        let sample_id = selected.map_or_else(String::new, |s| s.id.to_string());
        let sample_family = selected.map_or_else(String::new, |s| s.family.as_str().to_string());

        let cache = self.cache.borrow();
        let diagram_type = cache
            .ir
            .as_ref()
            .map(|ir| ir.diagram_type.as_str().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let link_count = cache.ir.as_ref().map_or(0_u64, |ir| ir.links.len() as u64);
        let guard_triggers = cache.ir.as_ref().is_some_and(|ir| {
            let guard = &ir.meta.guard;
            guard.limits_exceeded
                || guard.budget_exceeded
                || guard.node_limit_exceeded
                || guard.edge_limit_exceeded
                || guard.label_limit_exceeded
                || guard.route_budget_exceeded
                || guard.layout_budget_exceeded
                || guard.label_chars_over > 0
                || guard.label_lines_over > 0
        });
        let cache_hit = cache.last_cache_hit;
        let metrics = self.state.metrics;
        drop(cache);

        let config_hash = hash64_str(&format!("{:?}", self.build_config()));
        let init_config_hash = selected.map_or(0, |sample| {
            let source = self.state.effective_source(sample);
            hash64_str(source.as_ref())
        });
        let capability_profile = format!(
            "glyph:{};render:{};wrap:{};links:{};styles:{};init:{}",
            self.state.glyph_mode,
            self.state.render_mode,
            self.state.wrap_mode,
            self.state.link_mode,
            self.state.styles_enabled,
            self.state.init_directives_enabled,
        );
        let legend_height = match self.state.link_mode {
            MermaidLinkMode::Off => 0,
            MermaidLinkMode::Inline => u16::from(link_count > 0),
            MermaidLinkMode::Footnote => {
                if link_count == 0 {
                    0
                } else {
                    (link_count as u16).min(10).saturating_add(1)
                }
            }
        };

        MermaidHarnessFrameTelemetry {
            sample_id,
            sample_family,
            diagram_type,
            tier: self.state.tier.to_string(),
            glyph_mode: self.state.glyph_mode.to_string(),
            cache_hit,
            checksum,
            render_time_ms: metrics.render_ms,
            warnings: metrics.warning_count.unwrap_or(0),
            guard_triggers,
            config_hash,
            init_config_hash,
            capability_profile,
            link_count,
            link_mode: self.state.link_mode.to_string(),
            legend_height,
            parse_ms: metrics.parse_ms,
            layout_ms: metrics.layout_ms,
            route_ms: None,
            render_ms: metrics.render_ms,
        }
    }

    /// Zero out timing-dependent metrics so that snapshots are deterministic.
    ///
    /// Call this before `view()` in snapshot tests to avoid flaky timing diffs.
    #[doc(hidden)]
    pub fn stabilize_metrics_for_snapshot(&mut self) {
        self.state.metrics.parse_ms = Some(0.0);
        self.state.metrics.layout_ms = Some(0.0);
        self.state.metrics.render_ms = Some(0.0);
    }

    /// Select a sample by stable id (used by snapshot/E2E harnesses).
    ///
    /// This avoids brittle index-based navigation when new samples are inserted.
    #[doc(hidden)]
    pub fn select_sample_by_id_for_test(&mut self, id: &str) -> bool {
        let Some(pos) = self.state.samples.iter().position(|s| s.id == id) else {
            return false;
        };

        self.state.selected_index = pos;
        self.state.selected_node_idx = None;
        self.state.mode = ShowcaseMode::Normal;
        self.state.search_query.clear();
        self.state.search_matches.clear();
        self.state.search_match_idx = 0;
        self.state.bump_all();
        self.state.log_action("sample", format!("id:{id}"));
        self.state.normalize();
        true
    }

    /// Override the currently selected sample (used by deterministic snapshot tests).
    #[doc(hidden)]
    pub fn override_selected_sample_for_test(
        &mut self,
        id: &'static str,
        name: &'static str,
        source: &'static str,
    ) {
        if self.state.samples.is_empty() {
            return;
        }
        let idx = self
            .state
            .selected_index
            .min(self.state.samples.len().saturating_sub(1));
        let sample = &mut self.state.samples[idx];
        sample.id = id;
        sample.name = name;
        sample.source = source;

        self.state.selected_node_idx = None;
        self.state.mode = ShowcaseMode::Normal;
        self.state.search_query.clear();
        self.state.search_matches.clear();
        self.state.search_match_idx = 0;
        self.state.bump_all();
        self.state.normalize();
    }

    fn build_config(&self) -> MermaidConfig {
        self.state.build_config()
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
            || !zoom_matches
            || cache.selected_node_idx != self.state.selected_node_idx;

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
            cache.source = None;
            return;
        }

        let sample = self.state.selected_sample().expect("sample present");
        let config = self.build_config();
        let matrix = MermaidCompatibilityMatrix::default();
        let policy = MermaidFallbackPolicy::default();
        let mut metrics = cache.metrics;

        if analysis_needed {
            let source = self.state.effective_source(sample);
            let parse_start = Instant::now();
            let parsed = mermaid::parse_with_diagnostics(source.as_ref());
            metrics.parse_ms = Some(parse_start.elapsed().as_secs_f32() * 1000.0);

            // E2E-only: emit a mermaid_prepare JSONL record (init hash + warnings/errors)
            // when a log path is configured.
            if config.log_path.is_some() {
                let _prepared =
                    mermaid::prepare_with_policy(source.as_ref(), &config, &matrix, &policy);
            }

            let ir_parse = mermaid::normalize_ast_to_ir(&parsed.ast, &config, &matrix, &policy);
            metrics.warning_count = Some(ir_parse.warnings.len() as u32);
            let mut errors = Vec::new();
            errors.extend(parsed.errors);
            errors.extend(ir_parse.errors);
            metrics.error_count = Some(errors.len() as u32);
            cache.errors = errors;
            cache.ir = Some(ir_parse.ir);
            cache.source = match source {
                Cow::Borrowed(_) => None,
                Cow::Owned(text) => Some(text),
            };
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
            let warning_count = metrics.warning_count;
            let error_count = metrics.error_count;
            let mut snap = MermaidMetricsSnapshot::from_layout(&layout);
            snap.parse_ms = parse_ms;
            snap.warning_count = warning_count;
            snap.error_count = error_count;
            snap.layout_ms = Some(layout_start.elapsed().as_secs_f32() * 1000.0);
            if let Some(ref plan) = layout.degradation {
                snap.set_fallback(self.state.tier, plan);
            }
            metrics = snap;
            // Rebuild adjacency list for node navigation (compute before mutating cache).
            let adjacency = mermaid_render::build_adjacency(ir);
            cache.layout = Some(layout);
            cache.layout_epoch = self.state.layout_epoch;
            cache.adjacency = adjacency;
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
            let plan =
                mermaid_render::render_diagram_adaptive(layout, ir, &config, area, &mut buffer);
            metrics.render_ms = Some(render_start.elapsed().as_secs_f32() * 1000.0);

            // Apply selection highlighting overlay.
            if let Some(node_idx) = self.state.selected_node_idx
                && node_idx < ir.nodes.len()
            {
                let renderer = mermaid_render::MermaidRenderer::new(&config);
                let selection = SelectionState::from_selected(node_idx, ir);
                renderer.render_with_selection(layout, ir, &plan, &selection, &mut buffer);
            }
            // Apply debug overlays if active.
            if self.state.debug_overlay.any_active()
                && let Some(layout_ref) = cache.layout.as_ref()
            {
                Self::render_debug_overlays(
                    &mut buffer,
                    layout_ref,
                    self.state.debug_overlay,
                    render_width,
                    render_height,
                );
            }

            // Apply search dimming to non-matching nodes.
            if self.state.mode == ShowcaseMode::Search && !self.state.search_matches.is_empty() {
                Self::apply_search_dimming(
                    &mut buffer,
                    layout,
                    &self.state.search_matches,
                    render_width,
                    render_height,
                );
            }

            // Compute content flags before mutating cache (avoids borrow conflict with ir).
            let has_content = !ir.nodes.is_empty()
                || !ir.edges.is_empty()
                || !ir.labels.is_empty()
                || !ir.clusters.is_empty();
            cache.selected_node_idx = self.state.selected_node_idx;

            if !cache.errors.is_empty() {
                let source_for_errors = cache.source.as_deref().unwrap_or(sample.source);
                if has_content {
                    mermaid_render::render_mermaid_error_overlay(
                        &cache.errors,
                        source_for_errors,
                        &config,
                        area,
                        &mut buffer,
                    );
                } else {
                    mermaid_render::render_mermaid_error_panel(
                        &cache.errors,
                        source_for_errors,
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

    /// Recompute search matches against the current IR.
    fn recompute_search_matches(&mut self) {
        let cache = self.cache.borrow();
        let Some(ir) = cache.ir.as_ref() else {
            return;
        };
        let query = &self.state.search_query;
        if query.is_empty() {
            self.state.search_matches.clear();
            self.state.search_match_idx = 0;
            return;
        }
        let query_lower = query.to_lowercase();
        let mut matches = Vec::new();
        for (idx, node) in ir.nodes.iter().enumerate() {
            let id_match = node.id.to_lowercase().contains(&query_lower);
            let label_match = node.label.is_some_and(|lid| {
                ir.labels
                    .get(lid.0)
                    .is_some_and(|l| format!("{l:?}").to_lowercase().contains(&query_lower))
            });
            if id_match || label_match {
                matches.push(idx);
            }
        }
        drop(cache);
        self.state.search_matches = matches;
        if self.state.search_match_idx >= self.state.search_matches.len().max(1) {
            self.state.search_match_idx = 0;
        }
        // Auto-select the current match for highlighting.
        if !self.state.search_matches.is_empty() {
            self.state.selected_node_idx =
                Some(self.state.search_matches[self.state.search_match_idx]);
        } else {
            self.state.selected_node_idx = None;
        }
        self.state.bump_render();
    }

    fn apply_search_input(&mut self, ch: char) {
        self.state.search_query.push(ch);
        self.recompute_search_matches();
    }

    fn apply_search_backspace(&mut self) {
        self.state.search_query.pop();
        self.recompute_search_matches();
    }

    /// Render debug overlays for layout visualization.
    fn render_debug_overlays(
        buffer: &mut Buffer,
        layout: &mermaid_layout::DiagramLayout,
        flags: DebugOverlayFlags,
        render_width: u16,
        render_height: u16,
    ) {
        let bb = &layout.bounding_box;
        let margin = 1.0_f64;
        let avail_w = f64::from(render_width).max(1.0) - 2.0 * margin;
        let avail_h = f64::from(render_height).max(1.0) - 2.0 * margin;
        let bb_w = bb.width.max(1.0);
        let bb_h = bb.height.max(1.0);
        let scale = (avail_w / bb_w).min(avail_h / bb_h).max(0.1);
        let offset_x = margin + (avail_w - bb_w * scale) / 2.0 - bb.x * scale;
        let offset_y = margin + (avail_h - bb_h * scale) / 2.0 - bb.y * scale;

        let to_cell = |x: f64, y: f64| -> (u16, u16) {
            let cx = (x * scale + offset_x).round().max(0.0) as u16;
            let cy = (y * scale + offset_y).round().max(0.0) as u16;
            (
                cx.min(render_width.saturating_sub(1)),
                cy.min(render_height.saturating_sub(1)),
            )
        };

        // Grid overlay: draw alignment grid lines.
        if flags.grid {
            let grid_color = PackedRgba::rgba(60, 60, 80, 255);
            let step = 5.0;
            let mut gx = (bb.x / step).floor() * step;
            while gx <= bb.x + bb.width {
                let (cx, _) = to_cell(gx, 0.0);
                for y in 0..render_height {
                    if let Some(&cell) = buffer.get(cx, y)
                        && cell.content.as_char() == Some(' ')
                    {
                        buffer.set_fast(cx, y, Cell::from_char('|').with_fg(grid_color));
                    }
                }
                gx += step;
            }
            let mut gy = (bb.y / step).floor() * step;
            while gy <= bb.y + bb.height {
                let (_, cy) = to_cell(0.0, gy);
                for x in 0..render_width {
                    if let Some(&cell) = buffer.get(x, cy)
                        && cell.content.as_char() == Some(' ')
                    {
                        buffer.set_fast(x, cy, Cell::from_char('-').with_fg(grid_color));
                    }
                }
                gy += step;
            }
        }

        // Node bounds overlay: draw bounding box outlines.
        if flags.bounds {
            let bounds_color = PackedRgba::rgb(80, 200, 80);
            for node in &layout.nodes {
                let (x0, y0) = to_cell(node.rect.x, node.rect.y);
                let (x1, y1) = to_cell(
                    node.rect.x + node.rect.width,
                    node.rect.y + node.rect.height,
                );
                // Top/bottom edges.
                for x in x0..=x1.min(render_width.saturating_sub(1)) {
                    if let Some(&c) = buffer.get(x, y0)
                        && c.content.as_char() == Some(' ')
                    {
                        buffer.set_fast(x, y0, Cell::from_char('.').with_fg(bounds_color));
                    }
                    if let Some(&c) = buffer.get(x, y1)
                        && c.content.as_char() == Some(' ')
                    {
                        buffer.set_fast(x, y1, Cell::from_char('.').with_fg(bounds_color));
                    }
                }
                // Left/right edges.
                for y in y0..=y1.min(render_height.saturating_sub(1)) {
                    if let Some(&c) = buffer.get(x0, y)
                        && c.content.as_char() == Some(' ')
                    {
                        buffer.set_fast(x0, y, Cell::from_char(':').with_fg(bounds_color));
                    }
                    if let Some(&c) = buffer.get(x1, y)
                        && c.content.as_char() == Some(' ')
                    {
                        buffer.set_fast(x1, y, Cell::from_char(':').with_fg(bounds_color));
                    }
                }
            }
        }

        // Edge route overlay: show waypoints as colored dots.
        if flags.routes {
            let route_color = PackedRgba::rgb(255, 140, 40);
            for edge in &layout.edges {
                for wp in &edge.waypoints {
                    let (cx, cy) = to_cell(wp.x, wp.y);
                    if let Some(&cell) = buffer.get(cx, cy)
                        && cell.content.as_char() == Some(' ')
                    {
                        buffer.set_fast(cx, cy, Cell::from_char('*').with_fg(route_color));
                    }
                }
            }
        }

        // Port overlay: show node connection ports.
        if flags.ports {
            let port_color = PackedRgba::rgb(200, 80, 200);
            for node in &layout.nodes {
                // Draw markers at the midpoints of each edge of the node rect.
                let cx = node.rect.x + node.rect.width / 2.0;
                let cy = node.rect.y + node.rect.height / 2.0;
                let ports = [
                    (cx, node.rect.y),                    // top
                    (cx, node.rect.y + node.rect.height), // bottom
                    (node.rect.x, cy),                    // left
                    (node.rect.x + node.rect.width, cy),  // right
                ];
                for (px, py) in ports {
                    let (cell_x, cell_y) = to_cell(px, py);
                    if let Some(&c) = buffer.get(cell_x, cell_y) {
                        buffer.set_fast(cell_x, cell_y, Cell::from_char('+').with_fg(port_color));
                        let _ = c; // suppress unused warning
                    }
                }
            }
        }

        // Legend in top-right corner.
        if flags.any_active() {
            let legend_items: Vec<(&str, PackedRgba)> = [
                (flags.bounds, "B=bounds", PackedRgba::rgb(80, 200, 80)),
                (flags.routes, "R=routes", PackedRgba::rgb(255, 140, 40)),
                (flags.ports, "P=ports", PackedRgba::rgb(200, 80, 200)),
                (flags.grid, "G=grid", PackedRgba::rgba(60, 60, 80, 255)),
            ]
            .iter()
            .filter(|(active, _, _)| *active)
            .map(|(_, label, color)| (*label, *color))
            .collect();

            let legend_w: u16 = legend_items
                .iter()
                .map(|(l, _)| l.len() as u16 + 1)
                .sum::<u16>()
                + 1;
            let start_x = render_width.saturating_sub(legend_w);
            let mut x = start_x;
            let bg = PackedRgba::rgba(20, 20, 30, 255);
            for (label, color) in &legend_items {
                buffer.set_fast(x, 0, Cell::from_char(' ').with_bg(bg));
                x += 1;
                for ch in label.chars() {
                    if x < render_width {
                        buffer.set_fast(x, 0, Cell::from_char(ch).with_fg(*color).with_bg(bg));
                        x += 1;
                    }
                }
            }
        }
    }

    /// Render the search input bar at the bottom of the diagram area.
    fn render_search_bar(&self, frame: &mut Frame<'_>, area: Rect) {
        if self.state.mode != ShowcaseMode::Search {
            return;
        }
        let bar_y = area.y + area.height.saturating_sub(1);
        let bar_area = Rect::new(area.x, bar_y, area.width, 1);
        let match_info = if self.state.search_matches.is_empty() {
            if self.state.search_query.is_empty() {
                String::new()
            } else {
                " (no matches)".to_string()
            }
        } else {
            format!(
                " [{}/{}]",
                self.state.search_match_idx + 1,
                self.state.search_matches.len()
            )
        };
        let text = format!("/{}{}", self.state.search_query, match_info);
        let fg = PackedRgba::rgb(255, 255, 100);
        let bg = PackedRgba::rgb(30, 30, 50);
        for x in 0..bar_area.width {
            let ch = text.chars().nth(x as usize).unwrap_or(' ');
            let cell = Cell::from_char(ch).with_fg(fg).with_bg(bg);
            frame.buffer.set_fast(bar_area.x + x, bar_area.y, cell);
        }
    }

    /// Apply search dimming to non-matching nodes in the rendered buffer.
    fn apply_search_dimming(
        buffer: &mut Buffer,
        layout: &mermaid_layout::DiagramLayout,
        search_matches: &[usize],
        render_width: u16,
        render_height: u16,
    ) {
        if search_matches.is_empty() {
            return;
        }

        let bb = &layout.bounding_box;
        let margin = 1.0_f64;
        let avail_w = f64::from(render_width).max(1.0) - 2.0 * margin;
        let avail_h = f64::from(render_height).max(1.0) - 2.0 * margin;
        let bb_w = bb.width.max(1.0);
        let bb_h = bb.height.max(1.0);
        let scale = (avail_w / bb_w).min(avail_h / bb_h).max(0.1);
        let offset_x = margin + (avail_w - bb_w * scale) / 2.0 - bb.x * scale;
        let offset_y = margin + (avail_h - bb_h * scale) / 2.0 - bb.y * scale;

        // Identify cells belonging to matching nodes (we'll keep these bright).
        let mut bright_mask = vec![false; (render_width as usize) * (render_height as usize)];
        for &node_idx in search_matches {
            if let Some(node) = layout.nodes.iter().find(|n| n.node_idx == node_idx) {
                let x0 = ((node.rect.x * scale + offset_x).floor() as u16).min(render_width);
                let y0 = ((node.rect.y * scale + offset_y).floor() as u16).min(render_height);
                let x1 = (((node.rect.x + node.rect.width) * scale + offset_x).ceil() as u16)
                    .min(render_width);
                let y1 = (((node.rect.y + node.rect.height) * scale + offset_y).ceil() as u16)
                    .min(render_height);
                for y in y0..y1 {
                    for x in x0..x1 {
                        bright_mask[y as usize * render_width as usize + x as usize] = true;
                    }
                }
            }
        }

        // Dim all non-bright cells.
        for y in 0..render_height {
            for x in 0..render_width {
                if bright_mask[y as usize * render_width as usize + x as usize] {
                    continue;
                }
                if let Some(&cell) = buffer.get(x, y) {
                    let dim_fg = PackedRgba::rgba(
                        cell.fg.r() / 3,
                        cell.fg.g() / 3,
                        cell.fg.b() / 3,
                        cell.fg.a(),
                    );
                    let dim_bg = PackedRgba::rgba(
                        cell.bg.r() / 3,
                        cell.bg.g() / 3,
                        cell.bg.b() / 3,
                        cell.bg.a(),
                    );
                    buffer.set_fast(x, y, cell.with_fg(dim_fg).with_bg(dim_bg));
                }
            }
        }
    }

    /// Navigate to a connected node in the given direction using the cached
    /// adjacency list and layout positions.
    fn apply_navigate(&mut self, action: MermaidShowcaseAction) {
        let direction: u8 = match action {
            MermaidShowcaseAction::NavigateUp => 0,
            MermaidShowcaseAction::NavigateRight => 1,
            MermaidShowcaseAction::NavigateDown => 2,
            MermaidShowcaseAction::NavigateLeft => 3,
            _ => return,
        };
        let node_idx = match self.state.selected_node_idx {
            Some(idx) => idx,
            None => return,
        };
        let cache = self.cache.borrow();
        let layout = match cache.layout.as_ref() {
            Some(l) => l,
            None => return,
        };
        if cache.adjacency.is_empty() {
            return;
        }
        if let Some(target) =
            mermaid_render::navigate_direction(node_idx, direction, &cache.adjacency, layout)
        {
            drop(cache);
            self.state.selected_node_idx = Some(target);
            self.state.mode = ShowcaseMode::Inspect;
            self.state.bump_render();
            self.state.log_action("navigate", format!("node {target}"));
        }
    }

    /// Select next/previous node using the actual IR node count from cache.
    fn apply_select_node(&mut self, action: MermaidShowcaseAction) {
        let cache = self.cache.borrow();
        let node_count = cache.ir.as_ref().map_or(0, |ir| ir.nodes.len());
        drop(cache);

        if node_count == 0 {
            return;
        }

        let idx = match action {
            MermaidShowcaseAction::SelectNextNode => self
                .state
                .selected_node_idx
                .map_or(0, |i| (i + 1) % node_count),
            MermaidShowcaseAction::SelectPrevNode => self
                .state
                .selected_node_idx
                .map_or(node_count - 1, |i| (i + node_count - 1) % node_count),
            _ => return,
        };
        self.state.selected_node_idx = Some(idx);
        self.state.mode = ShowcaseMode::Inspect;
        self.state.bump_render();
        self.state.log_action("inspect", format!("node {idx}"));
    }

    /// Render a detail panel showing information about the selected node.
    fn render_node_detail(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Node Detail")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::accent::INFO).bg(theme::bg::DEEP));

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let cache = self.cache.borrow();
        let ir = match cache.ir.as_ref() {
            Some(ir) => ir,
            None => return,
        };
        let node_idx = match self.state.selected_node_idx {
            Some(idx) if idx < ir.nodes.len() => idx,
            _ => return,
        };

        let node = &ir.nodes[node_idx];
        let mut lines: Vec<String> = Vec::new();
        lines.push(format!("ID: {}", node.id));
        if let Some(label_id) = node.label
            && let Some(label) = ir.labels.get(label_id.0)
        {
            lines.push(format!("Label: {:?}", label));
        }
        lines.push(format!("Shape: {:?}", node.shape));

        // Count incoming/outgoing edges.
        use ftui_extras::mermaid::{IrEndpoint, IrNodeId};
        let mut incoming = 0usize;
        let mut outgoing = 0usize;
        for edge in &ir.edges {
            if edge.from == IrEndpoint::Node(IrNodeId(node_idx)) {
                outgoing += 1;
            }
            if edge.to == IrEndpoint::Node(IrNodeId(node_idx)) {
                incoming += 1;
            }
        }
        lines.push(format!("Edges: {} in, {} out", incoming, outgoing));

        // Cluster membership.
        for cluster in &ir.clusters {
            if cluster.members.iter().any(|m| m.0 == node_idx) {
                lines.push(format!("Cluster: #{}", cluster.id.0));
            }
        }

        let text = lines.join("\n");
        Paragraph::new(text)
            .style(Style::new().fg(theme::fg::PRIMARY))
            .render(inner, frame);
    }

    fn handle_key(&self, event: &KeyEvent) -> Option<MermaidShowcaseAction> {
        if event.kind != KeyEventKind::Press {
            return None;
        }

        // Mode-independent keys
        if let KeyCode::Char('?') = event.code {
            return Some(MermaidShowcaseAction::ToggleHelp);
        }
        if let KeyCode::Char('M') = event.code {
            return Some(MermaidShowcaseAction::ToggleMinimap);
        }

        // Mode-specific dispatch
        match self.state.mode {
            ShowcaseMode::Search => match event.code {
                KeyCode::Escape => Some(MermaidShowcaseAction::ExitMode),
                KeyCode::Enter | KeyCode::Char('n') if event.modifiers.is_empty() => {
                    Some(MermaidShowcaseAction::NextSearchMatch)
                }
                KeyCode::Char('N') => Some(MermaidShowcaseAction::PrevSearchMatch),
                KeyCode::Backspace => Some(MermaidShowcaseAction::SearchBackspace),
                KeyCode::Char(c) => Some(MermaidShowcaseAction::SearchInput(c)),
                _ => None,
            },
            ShowcaseMode::Inspect => match event.code {
                KeyCode::Escape => Some(MermaidShowcaseAction::ExitMode),
                KeyCode::Tab => Some(MermaidShowcaseAction::SelectNextNode),
                KeyCode::BackTab => Some(MermaidShowcaseAction::SelectPrevNode),
                KeyCode::Up => Some(MermaidShowcaseAction::NavigateUp),
                KeyCode::Down => Some(MermaidShowcaseAction::NavigateDown),
                KeyCode::Left => Some(MermaidShowcaseAction::NavigateLeft),
                KeyCode::Right => Some(MermaidShowcaseAction::NavigateRight),
                KeyCode::Char('+') | KeyCode::Char('=') => Some(MermaidShowcaseAction::ZoomIn),
                KeyCode::Char('-') => Some(MermaidShowcaseAction::ZoomOut),
                KeyCode::Char('0') => Some(MermaidShowcaseAction::ZoomReset),
                KeyCode::Char('f') => Some(MermaidShowcaseAction::FitToView),
                KeyCode::Char('m') => Some(MermaidShowcaseAction::ToggleMetrics),
                KeyCode::Char('c') => Some(MermaidShowcaseAction::ToggleControls),
                KeyCode::Char('i') => Some(MermaidShowcaseAction::ToggleStatusLog),
                KeyCode::Char('/') => Some(MermaidShowcaseAction::EnterSearchMode),
                _ => None,
            },
            ShowcaseMode::Normal => match event.code {
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
                KeyCode::Char('u') => Some(MermaidShowcaseAction::CycleLinkMode),
                KeyCode::Char('I') => Some(MermaidShowcaseAction::ToggleInitDirectives),
                KeyCode::Char('e') => Some(MermaidShowcaseAction::CycleErrorMode),
                KeyCode::Char('x') => Some(MermaidShowcaseAction::ToggleGuardProfile),
                KeyCode::Char('v') => Some(MermaidShowcaseAction::CycleViewportPreset),
                KeyCode::Char(']') => Some(MermaidShowcaseAction::IncreaseViewportWidth),
                KeyCode::Char('[') => Some(MermaidShowcaseAction::DecreaseViewportWidth),
                KeyCode::Char('}') => Some(MermaidShowcaseAction::IncreaseViewportHeight),
                KeyCode::Char('{') => Some(MermaidShowcaseAction::DecreaseViewportHeight),
                KeyCode::Char('o') => Some(MermaidShowcaseAction::ResetViewportOverride),
                KeyCode::Char('p') => Some(MermaidShowcaseAction::CyclePalette),
                KeyCode::Char('P') => Some(MermaidShowcaseAction::PrevPalette),
                KeyCode::Char('d') => Some(MermaidShowcaseAction::ToggleDebugOverlay),
                KeyCode::Char('1') => Some(MermaidShowcaseAction::ToggleDebugBounds),
                KeyCode::Char('2') => Some(MermaidShowcaseAction::ToggleDebugRoutes),
                KeyCode::Char('3') => Some(MermaidShowcaseAction::ToggleDebugPorts),
                KeyCode::Char('4') => Some(MermaidShowcaseAction::ToggleDebugGrid),
                KeyCode::Tab => Some(MermaidShowcaseAction::SelectNextNode),
                KeyCode::BackTab => Some(MermaidShowcaseAction::SelectPrevNode),
                KeyCode::Char('/') => Some(MermaidShowcaseAction::EnterSearchMode),
                KeyCode::Escape => Some(MermaidShowcaseAction::CollapsePanels),
                KeyCode::Char('i') => Some(MermaidShowcaseAction::ToggleStatusLog),
                _ => None,
            },
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

        let hint = if area.width >= 120 {
            "j/k sample  Enter render  l layout  t tier  u links  I init  e errors  x guard  v view  +/- zoom  m metrics  ? help"
        } else if area.width >= 80 {
            "j/k sample  Enter render  l layout  u links  m metrics  ? help"
        } else {
            "j/k sample  Enter render  ? help"
        };
        let metrics = if area.width >= 120 {
            if self.state.metrics_visible {
                format!(
                    "parse {}ms | layout {}ms | render {}ms",
                    self.state.metrics.parse_ms.unwrap_or(0.0),
                    self.state.metrics.layout_ms.unwrap_or(0.0),
                    self.state.metrics.render_ms.unwrap_or(0.0)
                )
            } else {
                "metrics hidden (m)".to_string()
            }
        } else {
            String::new()
        };
        let text = if metrics.is_empty() {
            hint.to_string()
        } else {
            format!("{hint} | {metrics}")
        };
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
            meta_parts.push(sample.family.as_str());
            meta_parts.push(sample.complexity.as_str());
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

        // Minimap overlay.
        if self.state.show_minimap
            && let Some(ref layout) = cache.layout
        {
            let minimap = ftui_extras::mermaid_minimap::Minimap::new(
                layout,
                ftui_extras::mermaid_minimap::MinimapConfig::default(),
            );
            if !minimap.is_trivial() {
                let viewport_rect = ftui_extras::mermaid_layout::LayoutRect {
                    x: f64::from(-self.state.viewport_pan.0),
                    y: f64::from(-self.state.viewport_pan.1),
                    width: f64::from(inner.width),
                    height: f64::from(inner.height),
                };
                minimap.render(
                    inner,
                    &mut frame.buffer,
                    Some(&viewport_rect),
                    self.state.selected_node_idx,
                );
            }
        }
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
            format!("Guard: {} (x)", self.state.guard_profile.as_str()),
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
            format!("Links: {} (u)", self.state.link_mode),
            format!(
                "Init: {} (I)",
                if self.state.init_directives_enabled {
                    "on"
                } else {
                    "off"
                }
            ),
            format!("Errors: {} (e)", self.state.error_mode),
            format!(
                "Viewport: {} (v)",
                self.state.viewport_size_override.map_or_else(
                    || "auto".to_string(),
                    |(cols, rows)| format!("{cols}x{rows}")
                )
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
            let cache = self.cache.borrow();

            // Render cache status.
            let last_label = if cache.last_cache_hit { "hit" } else { "miss" };
            let last_color = if cache.last_cache_hit {
                theme::accent::SUCCESS
            } else {
                theme::accent::WARNING
            };
            lines.push(Line::from_spans(vec![
                Span::styled("Cache: ", muted),
                Span::styled(
                    format!("hit {}  miss {}", cache.cache_hits, cache.cache_misses),
                    muted,
                ),
                Span::styled(format!("  last {last_label}"), Style::new().fg(last_color)),
            ]));

            // Warning count.
            let warn_val = metrics.warning_count.unwrap_or(0);
            let warn_style = if warn_val > 0 {
                Style::new().fg(theme::accent::WARNING)
            } else {
                muted
            };
            lines.push(Line::from_spans(vec![
                Span::styled("Warnings: ", muted),
                Span::styled(format!("{warn_val}"), warn_style),
            ]));

            // Link count (when enabled or present).
            if let Some(ir) = cache.ir.as_ref() {
                let total_links = ir.links.len();
                if total_links > 0 || self.state.link_mode != MermaidLinkMode::Off {
                    let allowed_links = ir
                        .links
                        .iter()
                        .filter(|l| l.sanitize_outcome == mermaid::LinkSanitizeOutcome::Allowed)
                        .count();
                    let link_style = if allowed_links == total_links && total_links > 0 {
                        Style::new().fg(theme::accent::SUCCESS)
                    } else if total_links > 0 {
                        Style::new().fg(theme::accent::WARNING)
                    } else {
                        muted
                    };
                    lines.push(Line::from_spans(vec![
                        Span::styled("Links: ", muted),
                        Span::styled(format!("{allowed_links}/{total_links}"), link_style),
                    ]));
                }
            }

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

            // Guard preview (complexity vs configured limits/budgets).
            if let Some(ir) = cache.ir.as_ref() {
                let guard = &ir.meta.guard;
                let config = self.build_config();

                lines.push(Line::from(Span::styled(
                    "Guard",
                    Style::new().fg(theme::accent::INFO),
                )));

                let node_style = if guard.node_limit_exceeded {
                    Style::new().fg(theme::accent::ERROR)
                } else {
                    muted
                };
                lines.push(Line::from_spans(vec![
                    Span::styled("Nodes: ", muted),
                    Span::styled(
                        format!("{}/{}", guard.complexity.nodes, config.max_nodes),
                        node_style,
                    ),
                ]));

                let edge_style = if guard.edge_limit_exceeded {
                    Style::new().fg(theme::accent::ERROR)
                } else {
                    muted
                };
                lines.push(Line::from_spans(vec![
                    Span::styled("Edges: ", muted),
                    Span::styled(
                        format!("{}/{}", guard.complexity.edges, config.max_edges),
                        edge_style,
                    ),
                ]));

                if guard.label_limit_exceeded
                    || guard.label_chars_over > 0
                    || guard.label_lines_over > 0
                {
                    let label_style = if guard.label_limit_exceeded {
                        Style::new().fg(theme::accent::ERROR)
                    } else {
                        Style::new().fg(theme::accent::WARNING)
                    };
                    lines.push(Line::from_spans(vec![
                        Span::styled("Labels: ", muted),
                        Span::styled(
                            format!(
                                "over chars {}  lines {}",
                                guard.label_chars_over, guard.label_lines_over
                            ),
                            label_style,
                        ),
                    ]));
                }

                let route_style = if guard.route_budget_exceeded {
                    Style::new().fg(theme::accent::ERROR)
                } else {
                    muted
                };
                lines.push(Line::from_spans(vec![
                    Span::styled("Route ops: ", muted),
                    Span::styled(
                        format!("{}/{}", guard.route_ops_estimate, config.route_budget),
                        route_style,
                    ),
                ]));

                let layout_budget_style = if guard.layout_budget_exceeded {
                    Style::new().fg(theme::accent::ERROR)
                } else {
                    muted
                };
                lines.push(Line::from_spans(vec![
                    Span::styled("Layout iters: ", muted),
                    Span::styled(
                        format!(
                            "{}/{}",
                            guard.layout_iterations_estimate, config.layout_iteration_budget
                        ),
                        layout_budget_style,
                    ),
                ]));

                let mut degrade_flags: Vec<&str> = Vec::new();
                if guard.degradation.hide_labels {
                    degrade_flags.push("hide_labels");
                }
                if guard.degradation.collapse_clusters {
                    degrade_flags.push("collapse_clusters");
                }
                if guard.degradation.simplify_routing {
                    degrade_flags.push("simplify_routing");
                }
                if guard.degradation.reduce_decoration {
                    degrade_flags.push("reduce_decoration");
                }
                let flags_str = if degrade_flags.is_empty() {
                    "none".to_string()
                } else {
                    degrade_flags.join(",")
                };
                let fidelity_style = if guard.limits_exceeded || guard.budget_exceeded {
                    Style::new().fg(theme::accent::WARNING)
                } else {
                    muted
                };
                lines.push(Line::from_spans(vec![
                    Span::styled("Degrade: ", muted),
                    Span::styled(guard.degradation.target_fidelity.as_str(), fidelity_style),
                    Span::styled(format!(" ({flags_str})"), muted),
                ]));

                if let Some(mode) = guard.degradation.force_glyph_mode {
                    lines.push(Line::from_spans(vec![
                        Span::styled("Force glyph: ", muted),
                        Span::styled(mode.to_string(), Style::new().fg(theme::accent::WARNING)),
                    ]));
                }
            }

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
                let errors = &cache.errors;
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

    /// Render a centered help overlay listing keybindings.
    fn render_help_overlay(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let compact = area.width < 80 || area.height < 20;
        let compact_sections: &[(&str, &[(&str, &str)])] = &[
            ("Nav", &[("j/k", "Sample"), ("Enter", "Re-render")]),
            (
                "Cfg",
                &[
                    ("u", "Links"),
                    ("I", "Init"),
                    ("e", "Errors"),
                    ("x", "Guard"),
                ],
            ),
            (
                "View",
                &[
                    ("l", "Layout"),
                    ("v", "Viewport"),
                    ("+/-", "Zoom"),
                    ("?", "Help"),
                ],
            ),
            ("Panels", &[("m", "Metrics"), ("Esc", "Collapse")]),
        ];
        let full_sections: &[(&str, &[(&str, &str)])] = &[
            (
                "Navigation",
                &[
                    ("j / Down", "Next sample"),
                    ("k / Up", "Previous sample"),
                    ("Home / End", "First / last sample"),
                    ("Enter", "Re-render sample"),
                    ("Tab", "Select next node"),
                    ("S-Tab", "Select previous node"),
                    ("/", "Enter search mode"),
                ],
            ),
            (
                "Render Config",
                &[
                    ("l", "Cycle layout mode"),
                    ("r", "Force re-layout"),
                    ("t", "Cycle tier"),
                    ("x", "Toggle guard profile"),
                    ("g", "Toggle glyph mode"),
                    ("b", "Cycle render mode"),
                    ("s", "Toggle styles"),
                    ("u", "Cycle link mode"),
                    ("I", "Toggle init directives"),
                    ("e", "Cycle error mode"),
                    ("w", "Cycle wrap mode"),
                    ("p / P", "Cycle palette"),
                ],
            ),
            (
                "Viewport",
                &[
                    ("+ / -", "Zoom in / out"),
                    ("0", "Reset zoom"),
                    ("f", "Fit to viewport"),
                    ("v", "Cycle viewport preset"),
                    ("] / [", "Viewport width +/-"),
                    ("} / {", "Viewport height +/-"),
                    ("o", "Reset viewport override"),
                ],
            ),
            (
                "Panels",
                &[
                    ("m", "Toggle metrics"),
                    ("c", "Toggle controls"),
                    ("i", "Toggle status log"),
                    ("d", "Toggle debug overlay"),
                    ("1-4", "Toggle bounds/routes/ports/grid"),
                    ("?", "Toggle this help"),
                    ("Esc", "Collapse panels"),
                ],
            ),
        ];
        let sections = if compact {
            compact_sections
        } else {
            full_sections
        };

        let mut content_lines: u16 = 2;
        for (_name, entries) in sections {
            content_lines += 1 + entries.len() as u16 + 1;
        }

        let overlay_w = if compact { 40u16 } else { 52u16 }.min(area.width.saturating_sub(2));
        let overlay_h = (content_lines + 2).min(area.height);
        if overlay_w < 10 || overlay_h < 5 {
            return;
        }

        let ox = area.x + area.width.saturating_sub(overlay_w) / 2;
        let oy = area.y + area.height.saturating_sub(overlay_h) / 2;
        let overlay = Rect::new(ox, oy, overlay_w, overlay_h);

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" Help (? to close) ")
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(theme::fg::PRIMARY).bg(theme::bg::DEEP));
        let inner = block.inner(overlay);
        block.render(overlay, frame);

        if inner.is_empty() {
            return;
        }

        let max_lines = inner.height as usize;
        let mut lines: Vec<Line> = Vec::new();

        let section_style = Style::new().fg(theme::accent::INFO);
        let key_style = Style::new().fg(theme::accent::WARNING);
        let desc_style = Style::new().fg(theme::fg::MUTED);
        let key_width = if compact { 8 } else { 12 };

        for (section_name, entries) in sections {
            if lines.len() >= max_lines {
                break;
            }
            lines.push(Line::from(Span::styled(
                format!("  {section_name}"),
                section_style,
            )));
            for (k, desc) in *entries {
                if lines.len() >= max_lines.saturating_sub(1) {
                    break;
                }
                lines.push(Line::from_spans(vec![
                    Span::styled(format!("    {k:>key_width$}"), key_style),
                    Span::styled(format!("  {desc}"), desc_style),
                ]));
            }
        }

        if lines.len() >= max_lines.saturating_sub(1) && content_lines as usize > max_lines {
            if lines.len() >= max_lines {
                lines.truncate(max_lines.saturating_sub(1));
            }
            lines.push(Line::from(Span::styled("    ... more below", desc_style)));
        }

        let text = Text::from_lines(lines);
        Paragraph::new(text).render(inner, frame);
    }

    /// Handle mouse events with hit-testing against cached layout areas.
    fn handle_mouse(&mut self, kind: MouseEventKind, x: u16, y: u16) -> Cmd<Event> {
        let samples = self.layout_samples.get();
        let viewport = self.layout_viewport.get();

        match kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if samples.contains(x, y) {
                    // Click on sample list — select sample by row offset
                    let row = (y.saturating_sub(samples.y + 1)) as usize; // +1 for border
                    let max = self.state.samples.len().saturating_sub(1);
                    let new_idx = row.min(max);
                    if new_idx != self.state.selected_index {
                        self.state.selected_index = new_idx;
                        self.state.bump_render();
                    }
                } else if viewport.contains(x, y) {
                    // Click on viewport — toggle inspect mode
                    if self.state.mode == ShowcaseMode::Inspect {
                        self.state.mode = ShowcaseMode::Normal;
                        self.state.selected_node_idx = None;
                    } else {
                        self.state.mode = ShowcaseMode::Inspect;
                    }
                    self.state.bump_render();
                }
            }
            MouseEventKind::ScrollUp => {
                if samples.contains(x, y) {
                    if self.state.selected_index > 0 {
                        self.state.selected_index -= 1;
                        self.state.bump_render();
                    }
                } else if viewport.contains(x, y) {
                    self.state.viewport_zoom = (self.state.viewport_zoom + ZOOM_STEP).min(ZOOM_MAX);
                    self.state.bump_render();
                }
            }
            MouseEventKind::ScrollDown => {
                if samples.contains(x, y) {
                    let max = self.state.samples.len().saturating_sub(1);
                    if self.state.selected_index < max {
                        self.state.selected_index += 1;
                        self.state.bump_render();
                    }
                } else if viewport.contains(x, y) {
                    self.state.viewport_zoom = (self.state.viewport_zoom - ZOOM_STEP).max(ZOOM_MIN);
                    self.state.bump_render();
                }
            }
            _ => {}
        }
        Cmd::None
    }
}

impl Screen for MermaidShowcaseScreen {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if let Event::Mouse(mouse) = event {
            return self.handle_mouse(mouse.kind, mouse.x, mouse.y);
        }
        if let Event::Key(key) = event
            && let Some(action) = self.handle_key(key)
        {
            match action {
                MermaidShowcaseAction::NavigateUp
                | MermaidShowcaseAction::NavigateDown
                | MermaidShowcaseAction::NavigateLeft
                | MermaidShowcaseAction::NavigateRight => {
                    self.apply_navigate(action);
                }
                MermaidShowcaseAction::SelectNextNode | MermaidShowcaseAction::SelectPrevNode => {
                    self.apply_select_node(action);
                }
                MermaidShowcaseAction::SearchInput(ch) => {
                    self.apply_search_input(ch);
                }
                MermaidShowcaseAction::SearchBackspace => {
                    self.apply_search_backspace();
                }
                MermaidShowcaseAction::NextSearchMatch | MermaidShowcaseAction::PrevSearchMatch
                    if self.state.mode == ShowcaseMode::Search =>
                {
                    self.state.apply_action(action);
                    // Update selection to current match.
                    if !self.state.search_matches.is_empty() {
                        self.state.selected_node_idx =
                            Some(self.state.search_matches[self.state.search_match_idx]);
                        self.state.bump_render();
                    }
                }
                _ => {
                    self.state.apply_action(action);
                }
            }
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
            self.layout_samples.set(columns[0]);
            self.render_samples(frame, columns[0]);
            self.layout_viewport.set(columns[1]);
            self.render_viewport(frame, columns[1]);
            let right = columns[2];
            self.layout_right.set(right);
            if right.is_empty() {
                return;
            }

            // Show node detail panel when inspecting a node.
            let show_node_detail =
                self.state.mode == ShowcaseMode::Inspect && self.state.selected_node_idx.is_some();

            // Collect which panels are visible.
            let show_controls = self.state.controls_visible;
            let show_metrics = self.state.metrics_visible;
            let show_log = self.state.status_log_visible;
            let panel_count = show_controls as u8 + show_metrics as u8 + show_log as u8;

            if show_node_detail {
                // In inspect mode: node detail on top, then other panels below.
                if right.height >= 16 && panel_count > 0 {
                    let rows = Flex::vertical()
                        .constraints([Constraint::Fixed(10), Constraint::Min(4)])
                        .split(right);
                    self.render_node_detail(frame, rows[0]);
                    if show_metrics {
                        self.render_metrics_panel(frame, rows[1]);
                    } else if show_controls {
                        self.render_controls_panel(frame, rows[1]);
                    } else if show_log {
                        self.render_status_log(frame, rows[1]);
                    }
                } else {
                    self.render_node_detail(frame, right);
                }
            } else {
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
            } // close else for show_node_detail
            // Search bar at the bottom.
            self.render_search_bar(frame, body);

            if self.state.help_visible {
                self.render_help_overlay(frame, area);
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
            if self.state.help_visible {
                self.render_help_overlay(frame, area);
            }
            return;
        }

        let rows = Flex::vertical()
            .constraints([Constraint::Fixed(6), Constraint::Min(1)])
            .split(body);
        self.render_samples(frame, rows[0]);
        self.render_viewport(frame, rows[1]);

        if self.state.help_visible {
            self.render_help_overlay(frame, area);
        }
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
                key: "x",
                action: "Toggle guard profile",
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
                key: "u",
                action: "Cycle link mode",
            },
            HelpEntry {
                key: "I",
                action: "Toggle init directives",
            },
            HelpEntry {
                key: "e",
                action: "Cycle error mode",
            },
            HelpEntry {
                key: "w",
                action: "Cycle wrap mode",
            },
            HelpEntry {
                key: "p / P",
                action: "Cycle palette",
            },
            HelpEntry {
                key: "v",
                action: "Cycle viewport preset",
            },
            HelpEntry {
                key: "i",
                action: "Toggle status log",
            },
            HelpEntry {
                key: "?",
                action: "Toggle help",
            },
            HelpEntry {
                key: "Esc",
                action: "Collapse panels",
            },
            HelpEntry {
                key: "Tab / S-Tab",
                action: "Next/prev node",
            },
            HelpEntry {
                key: "Arrows",
                action: "Follow edge (inspect)",
            },
            HelpEntry {
                key: "/",
                action: "Search nodes",
            },
            HelpEntry {
                key: "Click",
                action: "Select sample / toggle inspect",
            },
            HelpEntry {
                key: "Wheel",
                action: "Scroll samples / zoom viewport",
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
        assert_eq!(s.render_mode, MermaidRenderMode::Braille);
        s.apply_action(MermaidShowcaseAction::CycleRenderMode);
        assert_eq!(s.render_mode, MermaidRenderMode::Block);
        s.apply_action(MermaidShowcaseAction::CycleRenderMode);
        assert_eq!(s.render_mode, MermaidRenderMode::HalfBlock);
        s.apply_action(MermaidShowcaseAction::CycleRenderMode);
        assert_eq!(s.render_mode, MermaidRenderMode::CellOnly);
        s.apply_action(MermaidShowcaseAction::CycleRenderMode);
        assert_eq!(s.render_mode, MermaidRenderMode::Auto);
        s.apply_action(MermaidShowcaseAction::CycleRenderMode);
        assert_eq!(s.render_mode, MermaidRenderMode::Braille);
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
    fn key_u_maps_to_cycle_link_mode() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('u')));
        assert!(matches!(action, Some(MermaidShowcaseAction::CycleLinkMode)));
    }

    #[test]
    fn key_shift_i_maps_to_toggle_init_directives() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('I')));
        assert!(matches!(
            action,
            Some(MermaidShowcaseAction::ToggleInitDirectives)
        ));
    }

    #[test]
    fn key_e_maps_to_cycle_error_mode() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('e')));
        assert!(matches!(
            action,
            Some(MermaidShowcaseAction::CycleErrorMode)
        ));
    }

    #[test]
    fn key_x_maps_to_toggle_guard_profile() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('x')));
        assert!(matches!(
            action,
            Some(MermaidShowcaseAction::ToggleGuardProfile)
        ));
    }

    #[test]
    fn key_v_maps_to_cycle_viewport_preset() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('v')));
        assert!(matches!(
            action,
            Some(MermaidShowcaseAction::CycleViewportPreset)
        ));
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
        let action = screen.handle_key(&press(KeyCode::Char('y')));
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
    fn collapse_panels_does_not_block_nav_or_refresh() {
        let mut screen = new_screen();
        let initial_index = screen.state.selected_index;
        let initial_epoch = screen.state.render_epoch;

        screen.update(&Event::Key(press(KeyCode::Escape)));
        screen.update(&Event::Key(press(KeyCode::Down)));
        assert_eq!(screen.state.selected_index, initial_index + 1);

        screen.update(&Event::Key(press(KeyCode::Enter)));
        assert!(screen.state.render_epoch > initial_epoch);
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
    fn each_sample_has_id() {
        for sample in DEFAULT_SAMPLES {
            assert!(!sample.id.is_empty(), "sample {} has empty id", sample.name);
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
        let diagram_type = mermaid::parse(sample.source)
            .expect("parse sample")
            .diagram_type;
        let line = s.metrics_jsonl_line(sample, diagram_type, 7, Some("run-1"), 123, "alt");
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
        assert_eq!(value["sample_id"].as_str().unwrap_or_default(), sample.id);
        assert_eq!(
            value["sample_family"].as_str().unwrap_or_default(),
            sample.family.as_str()
        );
        assert_eq!(
            value["diagram_type"].as_str().unwrap_or_default(),
            diagram_type.as_str()
        );
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
        let diagram_type = mermaid::parse(sample.source)
            .expect("parse sample")
            .diagram_type;
        let line = s.metrics_jsonl_line(sample, diagram_type, 0, None, 0, "test");
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
        let diagram_type = mermaid::parse(sample.source)
            .expect("parse sample")
            .diagram_type;
        let line = s.metrics_jsonl_line(sample, diagram_type, 0, None, 0, "test");
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
        let diagram_type = mermaid::parse(sample.source)
            .expect("parse sample")
            .diagram_type;
        let line = s.metrics_jsonl_line(sample, diagram_type, 0, None, 0, "test");
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

    #[test]
    fn feature_matrix_all_sample_tags_known() {
        for sample in DEFAULT_SAMPLES {
            for tag in sample.features {
                assert!(
                    KNOWN_FEATURE_TAGS.contains(tag),
                    "Sample '{}' uses unknown feature tag '{}'",
                    sample.name,
                    tag
                );
            }
        }
    }

    #[test]
    fn feature_matrix_all_known_tags_exercised() {
        let exercised: std::collections::HashSet<&str> = DEFAULT_SAMPLES
            .iter()
            .flat_map(|s| s.features.iter().copied())
            .collect();
        let mut missing = Vec::new();
        for tag in KNOWN_FEATURE_TAGS {
            if !exercised.contains(tag) {
                missing.push(*tag);
            }
        }
        // All known tags should be exercised by at least one sample.
        assert!(
            missing.is_empty(),
            "Known feature tags without samples: {:?}",
            missing
        );
    }

    #[test]
    fn feature_gaps_are_documented() {
        for (tag, description) in FEATURE_GAPS {
            assert!(!tag.is_empty(), "Gap tag must not be empty");
            assert!(!description.is_empty(), "Gap description must not be empty");
        }
    }

    #[test]
    fn feature_gaps_not_in_known_tags() {
        for (gap_tag, _) in FEATURE_GAPS {
            assert!(
                !KNOWN_FEATURE_TAGS.contains(gap_tag),
                "Gap '{}' should not also be in KNOWN_FEATURE_TAGS (it\'s a gap)",
                gap_tag
            );
        }
    }

    // --- Sample Registry tests ---

    #[test]
    fn registry_all_returns_all_samples() {
        assert_eq!(SampleRegistry::all().len(), DEFAULT_SAMPLES.len());
    }

    #[test]
    fn registry_by_family_flow() {
        let flow = SampleRegistry::by_family(SampleFamily::Flow);
        assert!(
            flow.len() >= 5,
            "Expected at least 5 flow samples, got {}",
            flow.len()
        );
        for s in &flow {
            assert_eq!(s.family, SampleFamily::Flow);
        }
    }

    #[test]
    fn registry_by_family_unsupported() {
        let unsup = SampleRegistry::by_family(SampleFamily::Unsupported);
        assert!(
            unsup.is_empty(),
            "Unsupported sample bucket should be empty (no placeholders); got {} entries",
            unsup.len()
        );
    }

    #[test]
    fn registry_by_complexity_small() {
        let small = SampleRegistry::by_complexity(SampleComplexity::S);
        assert!(small.len() >= 3);
        for s in &small {
            assert_eq!(s.complexity, SampleComplexity::S);
        }
    }

    #[test]
    fn registry_by_min_complexity_medium() {
        let medium_plus = SampleRegistry::by_min_complexity(SampleComplexity::M);
        for s in &medium_plus {
            assert!(s.complexity >= SampleComplexity::M);
        }
    }

    #[test]
    fn registry_by_feature() {
        let edge_label = SampleRegistry::by_feature("edge-labels");
        assert!(
            edge_label.len() >= 2,
            "Expected at least 2 samples with edge-labels"
        );
        for s in &edge_label {
            assert!(s.features.contains(&"edge-labels"));
        }
    }

    #[test]
    fn registry_by_feature_links_and_init() {
        let links = SampleRegistry::by_feature("click-link");
        assert!(
            links.iter().any(|s| s.id == "flow-basic"),
            "Expected flow-basic to exercise click-link via links toggle"
        );

        let init = SampleRegistry::by_feature("init-directives");
        assert!(
            init.iter().any(|s| s.id == "flow-basic"),
            "Expected flow-basic to exercise init-directives via init toggle"
        );

        let link_style = SampleRegistry::by_feature("linkStyle");
        assert!(
            link_style.iter().any(|s| s.id == "flow-basic"),
            "Expected flow-basic to exercise linkStyle via links toggle"
        );
    }

    #[test]
    fn registry_by_feature_edge_styles_and_directions() {
        let dotted = SampleRegistry::by_feature("dotted-edges");
        assert!(
            dotted.iter().any(|s| s.id == "flow-dense"),
            "Expected flow-dense to exercise dotted-edges"
        );

        let thick = SampleRegistry::by_feature("thick-edges");
        assert!(
            thick.iter().any(|s| s.id == "flow-dense"),
            "Expected flow-dense to exercise thick-edges"
        );

        let bidir = SampleRegistry::by_feature("bidir-edges");
        assert!(
            bidir.iter().any(|s| s.id == "flow-dense"),
            "Expected flow-dense to exercise bidirectional edges"
        );

        let markers = SampleRegistry::by_feature("endpoint-markers");
        assert!(
            markers.iter().any(|s| s.id == "flow-dense"),
            "Expected flow-dense to exercise endpoint markers"
        );

        let rl = SampleRegistry::by_feature("direction-rl");
        assert!(
            rl.iter().any(|s| s.id == "flow-dense"),
            "Expected flow-dense to exercise RL direction"
        );

        let bt = SampleRegistry::by_feature("direction-bt");
        assert!(
            bt.iter().any(|s| s.id == "flow-subgraphs"),
            "Expected flow-subgraphs to exercise BT direction"
        );
    }

    #[test]
    fn registry_by_id() {
        let sample = SampleRegistry::by_id("flow-basic");
        assert!(sample.is_some(), "Should find flow-basic by id");
        assert_eq!(sample.unwrap().name, "Flow Basic");
    }

    #[test]
    fn registry_by_id_not_found() {
        assert!(SampleRegistry::by_id("nonexistent").is_none());
    }

    #[test]
    fn registry_by_max_size() {
        let small_vp = SampleRegistry::by_max_size(40, 12);
        assert!(!small_vp.is_empty(), "Some samples should fit in 40x12");
        for s in &small_vp {
            assert!(s.default_size.width <= 40);
            assert!(s.default_size.height <= 12);
        }
    }

    #[test]
    fn registry_select_combined() {
        let result = SampleRegistry::select(
            Some(SampleFamily::Flow),
            Some(SampleComplexity::S),
            None,
            None,
        );
        assert!(!result.is_empty());
        for s in &result {
            assert_eq!(s.family, SampleFamily::Flow);
            assert_eq!(s.complexity, SampleComplexity::S);
        }
    }

    #[test]
    fn registry_select_none_returns_all() {
        let result = SampleRegistry::select(None, None, None, None);
        assert_eq!(result.len(), DEFAULT_SAMPLES.len());
    }

    #[test]
    fn all_samples_have_unique_ids() {
        let mut ids: Vec<&str> = DEFAULT_SAMPLES.iter().map(|s| s.id).collect();
        ids.sort();
        let orig_len = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), orig_len, "Duplicate sample ids found");
    }

    #[test]
    fn all_families_have_samples() {
        for family in SampleFamily::ALL {
            if *family == SampleFamily::Unsupported {
                continue;
            }
            let samples = SampleRegistry::by_family(*family);
            assert!(!samples.is_empty(), "Family {:?} has no samples", family);
        }
    }

    #[test]
    fn sample_default_sizes_reasonable() {
        for sample in DEFAULT_SAMPLES {
            assert!(
                sample.default_size.width >= 20 && sample.default_size.width <= 200,
                "Sample {} has unreasonable width {}",
                sample.name,
                sample.default_size.width
            );
            assert!(
                sample.default_size.height >= 5 && sample.default_size.height <= 100,
                "Sample {} has unreasonable height {}",
                sample.name,
                sample.default_size.height
            );
        }
    }
    // ================================================================
    // bd-1yor8: keybinding coverage + viewport size-adjust tests
    // ================================================================

    // --- Normal mode keys: palette, debug, node selection, search ---

    #[test]
    fn key_p_maps_to_cycle_palette() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('p')));
        assert!(matches!(action, Some(MermaidShowcaseAction::CyclePalette)));
    }

    #[test]
    fn key_shift_p_maps_to_prev_palette() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('P')));
        assert!(matches!(action, Some(MermaidShowcaseAction::PrevPalette)));
    }

    #[test]
    fn key_d_maps_to_debug_overlay() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('d')));
        assert!(matches!(
            action,
            Some(MermaidShowcaseAction::ToggleDebugOverlay)
        ));
    }

    #[test]
    fn key_tab_maps_to_select_next_node() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Tab));
        assert!(matches!(
            action,
            Some(MermaidShowcaseAction::SelectNextNode)
        ));
    }

    #[test]
    fn key_backtab_maps_to_select_prev_node() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::BackTab));
        assert!(matches!(
            action,
            Some(MermaidShowcaseAction::SelectPrevNode)
        ));
    }

    #[test]
    fn key_slash_maps_to_enter_search() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('/')));
        assert!(matches!(
            action,
            Some(MermaidShowcaseAction::EnterSearchMode)
        ));
    }

    #[test]
    fn key_question_mark_maps_to_help_in_normal() {
        let screen = new_screen();
        let action = screen.handle_key(&press(KeyCode::Char('?')));
        assert!(matches!(action, Some(MermaidShowcaseAction::ToggleHelp)));
    }

    // --- Search mode keybindings ---

    #[test]
    fn search_mode_n_maps_to_next_match() {
        let mut screen = new_screen();
        screen.state.mode = ShowcaseMode::Search;
        let action = screen.handle_key(&press(KeyCode::Char('n')));
        assert!(matches!(
            action,
            Some(MermaidShowcaseAction::NextSearchMatch)
        ));
    }

    #[test]
    fn search_mode_shift_n_maps_to_prev_match() {
        let mut screen = new_screen();
        screen.state.mode = ShowcaseMode::Search;
        let action = screen.handle_key(&press(KeyCode::Char('N')));
        assert!(matches!(
            action,
            Some(MermaidShowcaseAction::PrevSearchMatch)
        ));
    }

    #[test]
    fn search_mode_escape_exits() {
        let mut screen = new_screen();
        screen.state.mode = ShowcaseMode::Search;
        let action = screen.handle_key(&press(KeyCode::Escape));
        assert!(matches!(action, Some(MermaidShowcaseAction::ExitMode)));
    }

    #[test]
    fn search_mode_captures_chars_as_input() {
        let mut screen = new_screen();
        screen.state.mode = ShowcaseMode::Search;
        // 'j' is NextSample in Normal mode but becomes SearchInput in Search mode.
        let action = screen.handle_key(&press(KeyCode::Char('j')));
        assert!(matches!(
            action,
            Some(MermaidShowcaseAction::SearchInput('j'))
        ));
        // Backspace maps to SearchBackspace.
        let action = screen.handle_key(&press(KeyCode::Backspace));
        assert!(matches!(
            action,
            Some(MermaidShowcaseAction::SearchBackspace)
        ));
    }

    #[test]
    fn search_mode_question_mark_toggles_help() {
        let mut screen = new_screen();
        screen.state.mode = ShowcaseMode::Search;
        let action = screen.handle_key(&press(KeyCode::Char('?')));
        assert!(matches!(action, Some(MermaidShowcaseAction::ToggleHelp)));
    }

    // --- Inspect mode keybindings ---

    #[test]
    fn inspect_mode_escape_exits() {
        let mut screen = new_screen();
        screen.state.mode = ShowcaseMode::Inspect;
        let action = screen.handle_key(&press(KeyCode::Escape));
        assert!(matches!(action, Some(MermaidShowcaseAction::ExitMode)));
    }

    #[test]
    fn inspect_mode_tab_selects_next() {
        let mut screen = new_screen();
        screen.state.mode = ShowcaseMode::Inspect;
        let action = screen.handle_key(&press(KeyCode::Tab));
        assert!(matches!(
            action,
            Some(MermaidShowcaseAction::SelectNextNode)
        ));
    }

    #[test]
    fn inspect_mode_backtab_selects_prev() {
        let mut screen = new_screen();
        screen.state.mode = ShowcaseMode::Inspect;
        let action = screen.handle_key(&press(KeyCode::BackTab));
        assert!(matches!(
            action,
            Some(MermaidShowcaseAction::SelectPrevNode)
        ));
    }

    #[test]
    fn inspect_mode_zoom_keys_work() {
        let mut screen = new_screen();
        screen.state.mode = ShowcaseMode::Inspect;
        assert!(matches!(
            screen.handle_key(&press(KeyCode::Char('+'))),
            Some(MermaidShowcaseAction::ZoomIn)
        ));
        assert!(matches!(
            screen.handle_key(&press(KeyCode::Char('='))),
            Some(MermaidShowcaseAction::ZoomIn)
        ));
        assert!(matches!(
            screen.handle_key(&press(KeyCode::Char('-'))),
            Some(MermaidShowcaseAction::ZoomOut)
        ));
        assert!(matches!(
            screen.handle_key(&press(KeyCode::Char('0'))),
            Some(MermaidShowcaseAction::ZoomReset)
        ));
        assert!(matches!(
            screen.handle_key(&press(KeyCode::Char('f'))),
            Some(MermaidShowcaseAction::FitToView)
        ));
    }

    #[test]
    fn inspect_mode_panel_keys_work() {
        let mut screen = new_screen();
        screen.state.mode = ShowcaseMode::Inspect;
        assert!(matches!(
            screen.handle_key(&press(KeyCode::Char('m'))),
            Some(MermaidShowcaseAction::ToggleMetrics)
        ));
        assert!(matches!(
            screen.handle_key(&press(KeyCode::Char('c'))),
            Some(MermaidShowcaseAction::ToggleControls)
        ));
        assert!(matches!(
            screen.handle_key(&press(KeyCode::Char('i'))),
            Some(MermaidShowcaseAction::ToggleStatusLog)
        ));
    }

    #[test]
    fn inspect_mode_slash_enters_search() {
        let mut screen = new_screen();
        screen.state.mode = ShowcaseMode::Inspect;
        let action = screen.handle_key(&press(KeyCode::Char('/')));
        assert!(matches!(
            action,
            Some(MermaidShowcaseAction::EnterSearchMode)
        ));
    }

    #[test]
    fn inspect_mode_ignores_sample_navigation() {
        let mut screen = new_screen();
        screen.state.mode = ShowcaseMode::Inspect;
        // 'j', 'k', Home, End are not available in inspect mode.
        assert!(screen.handle_key(&press(KeyCode::Char('j'))).is_none());
        assert!(screen.handle_key(&press(KeyCode::Char('k'))).is_none());
        assert!(screen.handle_key(&press(KeyCode::Home)).is_none());
        assert!(screen.handle_key(&press(KeyCode::End)).is_none());
    }

    #[test]
    fn inspect_mode_question_mark_toggles_help() {
        let mut screen = new_screen();
        screen.state.mode = ShowcaseMode::Inspect;
        let action = screen.handle_key(&press(KeyCode::Char('?')));
        assert!(matches!(action, Some(MermaidShowcaseAction::ToggleHelp)));
    }

    // --- State transitions: palette ---

    #[test]
    fn palette_cycles_through_all() {
        let mut s = new_state();
        assert_eq!(s.palette, DiagramPalettePreset::Default);
        s.apply_action(MermaidShowcaseAction::CyclePalette);
        assert_eq!(s.palette, DiagramPalettePreset::Corporate);
        s.apply_action(MermaidShowcaseAction::CyclePalette);
        assert_eq!(s.palette, DiagramPalettePreset::Neon);
        s.apply_action(MermaidShowcaseAction::CyclePalette);
        assert_eq!(s.palette, DiagramPalettePreset::Monochrome);
        s.apply_action(MermaidShowcaseAction::CyclePalette);
        assert_eq!(s.palette, DiagramPalettePreset::Pastel);
        s.apply_action(MermaidShowcaseAction::CyclePalette);
        assert_eq!(s.palette, DiagramPalettePreset::HighContrast);
        s.apply_action(MermaidShowcaseAction::CyclePalette);
        assert_eq!(s.palette, DiagramPalettePreset::Default);
    }

    #[test]
    fn prev_palette_cycles_backward() {
        let mut s = new_state();
        assert_eq!(s.palette, DiagramPalettePreset::Default);
        s.apply_action(MermaidShowcaseAction::PrevPalette);
        assert_eq!(s.palette, DiagramPalettePreset::HighContrast);
        s.apply_action(MermaidShowcaseAction::PrevPalette);
        assert_eq!(s.palette, DiagramPalettePreset::Pastel);
    }

    #[test]
    fn palette_bumps_render_epoch() {
        let mut s = new_state();
        let epoch = s.render_epoch;
        s.apply_action(MermaidShowcaseAction::CyclePalette);
        assert_eq!(s.render_epoch, epoch + 1);
    }

    // --- State transitions: debug overlay ---

    #[test]
    fn debug_overlay_toggles() {
        let mut s = new_state();
        assert!(!s.debug_overlay.any_active());
        s.apply_action(MermaidShowcaseAction::ToggleDebugOverlay);
        assert!(s.debug_overlay.any_active());
        s.apply_action(MermaidShowcaseAction::ToggleDebugOverlay);
        assert!(!s.debug_overlay.any_active());
    }

    #[test]
    fn debug_overlay_bumps_render_epoch() {
        let mut s = new_state();
        let epoch = s.render_epoch;
        s.apply_action(MermaidShowcaseAction::ToggleDebugOverlay);
        assert_eq!(s.render_epoch, epoch + 1);
    }

    // --- State transitions: help ---

    #[test]
    fn help_toggles() {
        let mut s = new_state();
        assert!(!s.help_visible);
        s.apply_action(MermaidShowcaseAction::ToggleHelp);
        assert!(s.help_visible);
        s.apply_action(MermaidShowcaseAction::ToggleHelp);
        assert!(!s.help_visible);
    }

    // --- State transitions: mode (search / inspect) ---

    #[test]
    fn enter_search_mode_sets_state() {
        let mut s = new_state();
        assert_eq!(s.mode, ShowcaseMode::Normal);
        s.apply_action(MermaidShowcaseAction::EnterSearchMode);
        assert_eq!(s.mode, ShowcaseMode::Search);
        assert!(s.search_query.is_empty());
        assert!(s.search_matches.is_empty());
        assert_eq!(s.search_match_idx, 0);
    }

    #[test]
    fn exit_search_mode_clears_state() {
        let mut s = new_state();
        s.mode = ShowcaseMode::Search;
        s.search_query = "test".to_string();
        s.search_matches = vec![0, 1];
        s.search_match_idx = 1;
        s.apply_action(MermaidShowcaseAction::ExitMode);
        assert_eq!(s.mode, ShowcaseMode::Normal);
        assert!(s.search_query.is_empty());
        assert!(s.search_matches.is_empty());
        assert_eq!(s.search_match_idx, 0);
    }

    #[test]
    fn exit_inspect_mode_clears_node() {
        let mut s = new_state();
        s.mode = ShowcaseMode::Inspect;
        s.selected_node_idx = Some(3);
        s.apply_action(MermaidShowcaseAction::ExitMode);
        assert_eq!(s.mode, ShowcaseMode::Normal);
        assert!(s.selected_node_idx.is_none());
    }

    #[test]
    fn exit_normal_mode_is_noop() {
        let mut s = new_state();
        assert_eq!(s.mode, ShowcaseMode::Normal);
        s.apply_action(MermaidShowcaseAction::ExitMode);
        assert_eq!(s.mode, ShowcaseMode::Normal);
    }

    #[test]
    fn next_search_match_wraps() {
        let mut s = new_state();
        s.mode = ShowcaseMode::Search;
        s.search_matches = vec![0, 3, 7];
        s.search_match_idx = 0;
        s.apply_action(MermaidShowcaseAction::NextSearchMatch);
        assert_eq!(s.search_match_idx, 1);
        s.apply_action(MermaidShowcaseAction::NextSearchMatch);
        assert_eq!(s.search_match_idx, 2);
        s.apply_action(MermaidShowcaseAction::NextSearchMatch);
        assert_eq!(s.search_match_idx, 0); // wraps
    }

    #[test]
    fn prev_search_match_wraps() {
        let mut s = new_state();
        s.mode = ShowcaseMode::Search;
        s.search_matches = vec![0, 3, 7];
        s.search_match_idx = 0;
        s.apply_action(MermaidShowcaseAction::PrevSearchMatch);
        assert_eq!(s.search_match_idx, 2); // wraps backward
        s.apply_action(MermaidShowcaseAction::PrevSearchMatch);
        assert_eq!(s.search_match_idx, 1);
    }

    #[test]
    fn search_match_noop_when_empty() {
        let mut s = new_state();
        s.mode = ShowcaseMode::Search;
        s.search_matches = vec![];
        s.search_match_idx = 0;
        s.apply_action(MermaidShowcaseAction::NextSearchMatch);
        assert_eq!(s.search_match_idx, 0);
        s.apply_action(MermaidShowcaseAction::PrevSearchMatch);
        assert_eq!(s.search_match_idx, 0);
    }

    // --- State transitions: node selection ---

    #[test]
    fn select_next_node_enters_inspect() {
        let mut s = new_state();
        assert_eq!(s.mode, ShowcaseMode::Normal);
        s.apply_action(MermaidShowcaseAction::SelectNextNode);
        // If cache_node_count() > 0, should enter inspect mode.
        // The heuristic may return 0 for some samples, so check conditionally.
        if s.selected_node_idx.is_some() {
            assert_eq!(s.mode, ShowcaseMode::Inspect);
        }
    }

    #[test]
    fn select_prev_node_enters_inspect() {
        let mut s = new_state();
        s.apply_action(MermaidShowcaseAction::SelectPrevNode);
        if s.selected_node_idx.is_some() {
            assert_eq!(s.mode, ShowcaseMode::Inspect);
        }
    }

    // --- Viewport size-adjust: comprehensive ---

    #[test]
    fn viewport_decrease_width() {
        let mut s = new_state();
        // First increase to establish an override.
        s.apply_action(MermaidShowcaseAction::IncreaseViewportWidth);
        let (cols1, rows1) = s.viewport_size_override.unwrap();
        s.apply_action(MermaidShowcaseAction::DecreaseViewportWidth);
        let (cols2, rows2) = s.viewport_size_override.unwrap();
        assert_eq!(cols2, cols1 - VIEWPORT_OVERRIDE_STEP_COLS as u16);
        assert_eq!(rows2, rows1); // height unchanged
    }

    #[test]
    fn viewport_decrease_height() {
        let mut s = new_state();
        s.apply_action(MermaidShowcaseAction::IncreaseViewportHeight);
        let (cols1, rows1) = s.viewport_size_override.unwrap();
        s.apply_action(MermaidShowcaseAction::DecreaseViewportHeight);
        let (cols2, rows2) = s.viewport_size_override.unwrap();
        assert_eq!(cols2, cols1); // width unchanged
        assert_eq!(rows2, rows1 - VIEWPORT_OVERRIDE_STEP_ROWS as u16);
    }

    #[test]
    fn viewport_size_accumulates() {
        let mut s = new_state();
        s.apply_action(MermaidShowcaseAction::IncreaseViewportWidth);
        s.apply_action(MermaidShowcaseAction::IncreaseViewportWidth);
        let (cols, _) = s.viewport_size_override.unwrap();
        let expected = VIEWPORT_OVERRIDE_DEFAULT_COLS + 2 * VIEWPORT_OVERRIDE_STEP_COLS as u16;
        assert_eq!(cols, expected);
    }

    #[test]
    fn viewport_width_clamps_at_minimum() {
        let mut s = new_state();
        s.viewport_size_override = Some((VIEWPORT_OVERRIDE_MIN_COLS, 24));
        s.apply_action(MermaidShowcaseAction::DecreaseViewportWidth);
        let (cols, _) = s.viewport_size_override.unwrap();
        assert!(cols >= VIEWPORT_OVERRIDE_MIN_COLS);
    }

    #[test]
    fn viewport_height_clamps_at_minimum() {
        let mut s = new_state();
        s.viewport_size_override = Some((80, VIEWPORT_OVERRIDE_MIN_ROWS));
        s.apply_action(MermaidShowcaseAction::DecreaseViewportHeight);
        let (_, rows) = s.viewport_size_override.unwrap();
        assert!(rows >= VIEWPORT_OVERRIDE_MIN_ROWS);
    }

    #[test]
    fn viewport_width_and_height_independent() {
        let mut s = new_state();
        s.apply_action(MermaidShowcaseAction::IncreaseViewportWidth);
        let (cols1, rows1) = s.viewport_size_override.unwrap();
        s.apply_action(MermaidShowcaseAction::IncreaseViewportHeight);
        let (cols2, rows2) = s.viewport_size_override.unwrap();
        assert_eq!(cols2, cols1); // width didn't change
        assert_eq!(rows2, rows1 + VIEWPORT_OVERRIDE_STEP_ROWS as u16);
    }

    #[test]
    fn viewport_reset_when_no_override_is_noop() {
        let mut s = new_state();
        assert!(s.viewport_size_override.is_none());
        let epoch = s.render_epoch;
        s.apply_action(MermaidShowcaseAction::ResetViewportOverride);
        assert!(s.viewport_size_override.is_none());
        // No epoch bump when already None.
        assert_eq!(s.render_epoch, epoch);
    }
    // --- Mouse interaction tests (bd-iuvb.17.10.3) ---

    #[test]
    fn mouse_click_samples_selects() {
        let mut screen = new_screen();
        screen.layout_samples.set(Rect::new(0, 0, 30, 20));
        assert_eq!(screen.state.selected_index, 0);
        let click = Event::Mouse(ftui_core::event::MouseEvent::new(
            MouseEventKind::Down(MouseButton::Left),
            10,
            4,
        ));
        screen.update(&click);
        assert_eq!(screen.state.selected_index, 3);
    }

    #[test]
    fn mouse_click_viewport_toggles_inspect() {
        let mut screen = new_screen();
        screen.layout_viewport.set(Rect::new(30, 0, 60, 20));
        assert_eq!(screen.state.mode, ShowcaseMode::Normal);
        let click = Event::Mouse(ftui_core::event::MouseEvent::new(
            MouseEventKind::Down(MouseButton::Left),
            50,
            10,
        ));
        screen.update(&click);
        assert_eq!(screen.state.mode, ShowcaseMode::Inspect);
        screen.update(&click);
        assert_eq!(screen.state.mode, ShowcaseMode::Normal);
    }

    #[test]
    fn mouse_scroll_samples_navigates() {
        let mut screen = new_screen();
        screen.layout_samples.set(Rect::new(0, 0, 30, 20));
        assert_eq!(screen.state.selected_index, 0);
        let scroll_down = Event::Mouse(ftui_core::event::MouseEvent::new(
            MouseEventKind::ScrollDown,
            10,
            10,
        ));
        screen.update(&scroll_down);
        assert_eq!(screen.state.selected_index, 1);
        let scroll_up = Event::Mouse(ftui_core::event::MouseEvent::new(
            MouseEventKind::ScrollUp,
            10,
            10,
        ));
        screen.update(&scroll_up);
        assert_eq!(screen.state.selected_index, 0);
        // Scroll up at 0 should stay at 0
        screen.update(&scroll_up);
        assert_eq!(screen.state.selected_index, 0);
    }

    #[test]
    fn mouse_scroll_viewport_zooms() {
        let mut screen = new_screen();
        screen.layout_viewport.set(Rect::new(30, 0, 60, 20));
        let initial_zoom = screen.state.viewport_zoom;
        let scroll_up = Event::Mouse(ftui_core::event::MouseEvent::new(
            MouseEventKind::ScrollUp,
            50,
            10,
        ));
        screen.update(&scroll_up);
        assert!(screen.state.viewport_zoom > initial_zoom);
        let scroll_down = Event::Mouse(ftui_core::event::MouseEvent::new(
            MouseEventKind::ScrollDown,
            50,
            10,
        ));
        screen.update(&scroll_down);
        assert!((screen.state.viewport_zoom - initial_zoom).abs() < 0.001);
    }

    #[test]
    fn keybindings_include_mouse_hints() {
        let screen = new_screen();
        let bindings = screen.keybindings();
        let keys: Vec<&str> = bindings.iter().map(|b| b.key).collect();
        assert!(keys.contains(&"Click"));
        assert!(keys.contains(&"Wheel"));
    }
}
