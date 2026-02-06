//! Mermaid Mega Showcase Screen — interactive layout lab.
//!
//! A comprehensive, over-the-top Mermaid diagram demo with:
//! - Full sample library with metadata and filtering
//! - Interactive node navigation and edge highlighting
//! - Split-panel layout with diagram, controls, metrics, and detail panels
//! - All configuration knobs exposed as keybindings
//! - Help overlay driven by the canonical keymap spec

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use ftui_core::geometry::Rect;
use ftui_extras::mermaid;
use ftui_extras::mermaid::{
    DiagramPalettePreset, MermaidCompatibilityMatrix, MermaidConfig, MermaidDiagramIr,
    MermaidError, MermaidFallbackPolicy, MermaidGlyphMode, MermaidRenderMode, MermaidTier,
    MermaidWrapMode, ShowcaseMode,
};
use ftui_extras::mermaid_layout;
use ftui_extras::mermaid_render;
use ftui_extras::mermaid_render::DiagramPalette;
use ftui_render::buffer::Buffer;
use ftui_render::cell::{Cell, PackedRgba};
use ftui_render::drawing::{BorderChars, Draw};

use crate::determinism;
use crate::screens::{Cmd, Event, Frame, HelpEntry, Screen};
use crate::test_logging::{escape_json, jsonl_enabled};

// ── Layout constants ────────────────────────────────────────────────

/// Minimum terminal width for full layout (below this, panels collapse).
const MIN_FULL_WIDTH: u16 = 100;
/// Minimum terminal height for full layout.
const MIN_FULL_HEIGHT: u16 = 20;
/// Side panel width when visible.
const SIDE_PANEL_WIDTH: u16 = 28;
/// Footer height (status + hints).
const FOOTER_HEIGHT: u16 = 2;
/// Controls panel height when visible.
const CONTROLS_PANEL_HEIGHT: u16 = 6;
/// Pan step in cells per keypress.
const PAN_STEP: i16 = 4;
/// Minimum milliseconds between layout recomputations (debounce window).
const LAYOUT_DEBOUNCE_MS: u128 = 50;
/// Layout computation budget in milliseconds.  Exceeding this triggers a warning.
const LAYOUT_BUDGET_MS: f32 = 16.0;
/// Default viewport override columns.
const VIEWPORT_OVERRIDE_DEFAULT_COLS: u16 = 80;
/// Default viewport override rows.
const VIEWPORT_OVERRIDE_DEFAULT_ROWS: u16 = 24;
/// Minimum viewport override columns.
const VIEWPORT_OVERRIDE_MIN_COLS: u16 = 10;
/// Minimum viewport override rows.
const VIEWPORT_OVERRIDE_MIN_ROWS: u16 = 4;
/// Viewport override step for width adjustments.
const VIEWPORT_OVERRIDE_STEP_COLS: i16 = 4;
/// Viewport override step for height adjustments.
const VIEWPORT_OVERRIDE_STEP_ROWS: i16 = 2;
const MEGA_JSONL_EVENT: &str = "mermaid_mega_recompute";
static MEGA_JSONL_SEQ: AtomicU64 = AtomicU64::new(0);

// ── Sample library ─────────────────────────────────────────────────

/// A curated Mermaid sample for the mega showcase.
struct MegaSample {
    name: &'static str,
    source: &'static str,
}

const GENERATED_SAMPLE_SOURCE: &str = "__generated__";

fn mega_sample_category(sample: &MegaSample) -> &'static str {
    if sample.source == GENERATED_SAMPLE_SOURCE {
        return "generated";
    }

    let src = sample.source.trim_start();
    if src.starts_with("graph") || src.starts_with("flowchart") {
        "flow"
    } else if src.starts_with("sequenceDiagram") {
        "sequence"
    } else if src.starts_with("classDiagram") {
        "class"
    } else if src.starts_with("stateDiagram") {
        "state"
    } else if src.starts_with("erDiagram") {
        "er"
    } else if src.starts_with("gantt") {
        "gantt"
    } else if src.starts_with("mindmap") {
        "mindmap"
    } else if src.starts_with("pie") {
        "pie"
    } else {
        "unknown"
    }
}

fn mega_sample_matches_tokens(sample: &MegaSample, tokens: &[&str]) -> bool {
    if tokens.is_empty() {
        return true;
    }

    let name = sample.name.to_ascii_lowercase();
    let category = mega_sample_category(sample);
    let source = sample.source.to_ascii_lowercase();

    tokens
        .iter()
        .all(|t| name.contains(t) || category.contains(t) || source.contains(t))
}

const MEGA_SAMPLES: &[MegaSample] = &[
    MegaSample {
        name: "Flow Basic",
        source: "graph TD\n    A[Start] --> B{Decision}\n    B -->|Yes| C[OK]\n    B -->|No| D[Error]\n    C --> E[End]\n    D --> E",
    },
    MegaSample {
        name: "Flow Subgraphs",
        source: "graph LR\n    subgraph Frontend\n        A[React] --> B[Redux]\n    end\n    subgraph Backend\n        C[API] --> D[DB]\n    end\n    B --> C",
    },
    MegaSample {
        name: "Flow Dense",
        source: "graph TD\n    A --> B --> C --> D\n    A --> C\n    B --> D\n    E --> F --> G\n    D --> G\n    A --> E",
    },
    MegaSample {
        name: "Flow RL Labels",
        source: "graph RL
    Z[Output] -->|result| Y[Transform]
    Y -->|data| X[Validate]
    X -->|raw| W[Input]
    X -->|error| V[Reject]
    V -->|retry| W",
    },
    MegaSample {
        name: "Flow Nested Subgraphs",
        source: "graph TB
    subgraph Cluster A
        subgraph Inner
            A1[Core] --> A2[Cache]
        end
        A3[Gateway] --> A1
    end
    subgraph Cluster B
        B1[Worker] --> B2[Queue]
    end
    A2 --> B2
    B1 --> A3",
    },
    MegaSample {
        name: "Flow BT Shapes",
        source: "graph BT
    D[(Database)] --> S{{Service}}
    S --> G>Gateway]
    G --> C([Client])
    S --> L[/Logger/]
    L --> M[[Monitor]]",
    },
    MegaSample {
        name: "Flow Complex",
        source: "graph LR
    A[Request] -->|HTTP| B{Auth?}
    B -->|valid| C[Router]
    B -->|invalid| D[401 Error]
    C -->|/api| E[REST Handler]
    C -->|/ws| F[WebSocket]
    E --> G[(PostgreSQL)]
    F --> H[(Redis)]
    G --> I[Response]
    H --> I
    I -->|cache| J{{CDN}}
    J --> K([User])",
    },
    MegaSample {
        name: "Sequence",
        source: "sequenceDiagram\n    Alice->>Bob: Hello Bob\n    Bob-->>Alice: Hi Alice\n    Alice->>Bob: How are you?\n    Bob-->>Alice: Great!",
    },
    MegaSample {
        name: "Class",
        source: "classDiagram\n    class Animal {\n        +String name\n        +makeSound()\n    }\n    class Dog {\n        +fetch()\n    }\n    Animal <|-- Dog",
    },
    MegaSample {
        name: "State",
        source: "stateDiagram-v2\n    [*] --> Idle\n    Idle --> Processing : start\n    Processing --> Done : finish\n    Processing --> Error : fail\n    Error --> Idle : retry\n    Done --> [*]",
    },
    MegaSample {
        name: "ER",
        source: "erDiagram\n    CUSTOMER ||--o{ ORDER : places\n    ORDER ||--|{ LINE-ITEM : contains\n    PRODUCT ||--o{ LINE-ITEM : \"ordered in\"",
    },
    MegaSample {
        name: "Pie",
        source: "pie title Browser Share\n    \"Chrome\" : 65\n    \"Firefox\" : 15\n    \"Safari\" : 12\n    \"Edge\" : 8",
    },
    MegaSample {
        name: "Gantt",
        source: "gantt\n    title Project Plan\n    section Design\n    Wireframes :a1, 2024-01-01, 7d\n    Mockups :a2, after a1, 5d\n    section Dev\n    Frontend :b1, after a2, 14d\n    Backend :b2, after a2, 14d",
    },
    MegaSample {
        name: "Mindmap",
        source: "mindmap\n  root((Project))\n    Planning\n      Goals\n      Timeline\n    Development\n      Frontend\n      Backend\n    Testing",
    },
    // -- Sequence samples (bd-3oaig.5.2) --
    MegaSample {
        name: "Sequence Loops",
        source: "sequenceDiagram\n    participant C as Client\n    participant S as Server\n    participant DB as Database\n    C->>S: Login\n    activate S\n    S->>DB: Query user\n    activate DB\n    DB-->>S: User data\n    deactivate DB\n    alt Valid credentials\n        S-->>C: 200 OK\n    else Invalid\n        S-->>C: 401 Unauthorized\n    end\n    deactivate S\n    loop Every 30s\n        C->>S: Heartbeat\n        S-->>C: ACK\n    end",
    },
    MegaSample {
        name: "Sequence Parallel",
        source: "sequenceDiagram\n    participant U as User\n    participant GW as Gateway\n    participant A as Auth Service\n    participant P as Product Service\n    participant O as Order Service\n    U->>GW: Place Order\n    par Auth Check\n        GW->>A: Validate token\n        A-->>GW: OK\n    and Inventory\n        GW->>P: Check stock\n        P-->>GW: Available\n    end\n    GW->>O: Create order\n    Note over O: Processing...\n    O-->>GW: Order ID\n    GW-->>U: Confirmation",
    },
    // -- Class diagram samples (bd-3oaig.5.3) --
    MegaSample {
        name: "Class Inheritance",
        source: "classDiagram\n    class Shape {\n        <<abstract>>\n        +area() float\n        +perimeter() float\n    }\n    class Circle {\n        -float radius\n        +area() float\n    }\n    class Rectangle {\n        -float width\n        -float height\n        +area() float\n    }\n    class Square {\n        +Square(float side)\n    }\n    Shape <|-- Circle\n    Shape <|-- Rectangle\n    Rectangle <|-- Square\n    Shape *-- Color",
    },
    MegaSample {
        name: "Class Large",
        source: "classDiagram\n    class Controller {\n        +handle()\n    }\n    class Service {\n        +execute()\n    }\n    class Repository {\n        +find()\n        +save()\n    }\n    class Model {\n        +validate()\n    }\n    class DTO {\n        +serialize()\n    }\n    class Mapper {\n        +toDTO()\n        +toModel()\n    }\n    class Config {\n        +load()\n    }\n    class Logger {\n        +info()\n        +error()\n    }\n    class Cache {\n        +get()\n        +set()\n    }\n    class Queue {\n        +push()\n        +pop()\n    }\n    Controller --> Service\n    Service --> Repository\n    Service --> Cache\n    Repository --> Model\n    Controller --> DTO\n    Mapper --> DTO\n    Mapper --> Model\n    Service --> Logger\n    Service --> Queue\n    Config <.. Service",
    },
    // -- State diagram samples (bd-3oaig.5.4) --
    MegaSample {
        name: "State Nested",
        source: "stateDiagram-v2\n    [*] --> Active\n    state Active {\n        [*] --> Running\n        state Running {\n            [*] --> Normal\n            Normal --> Degraded : high_load\n            Degraded --> Normal : load_reduced\n        }\n        Running --> Paused : pause\n        Paused --> Running : resume\n    }\n    Active --> Stopped : shutdown\n    Stopped --> Active : restart\n    Stopped --> [*]",
    },
    MegaSample {
        name: "State Parallel",
        source: "stateDiagram-v2\n    [*] --> Online\n    state Online {\n        state Network {\n            Connected --> Disconnected : timeout\n            Disconnected --> Connected : reconnect\n        }\n        --\n        state Auth {\n            LoggedIn --> LoggedOut : expire\n            LoggedOut --> LoggedIn : login\n        }\n    }\n    Online --> Offline : shutdown\n    Offline --> [*]",
    },
    // -- ER diagram samples (bd-3oaig.5.5) --
    MegaSample {
        name: "ER Complex",
        source: "erDiagram\n    CUSTOMER {\n        int id PK\n        string name\n        string email UK\n    }\n    ORDER {\n        int id PK\n        date created\n        string status\n    }\n    PRODUCT {\n        int id PK\n        string name\n        float price\n    }\n    LINE_ITEM {\n        int qty\n        float subtotal\n    }\n    ADDRESS {\n        string street\n        string city\n    }\n    CUSTOMER ||--o{ ORDER : places\n    CUSTOMER ||--o{ ADDRESS : has\n    ORDER ||--|{ LINE_ITEM : contains\n    PRODUCT ||--o{ LINE_ITEM : includes\n    ORDER }o--|| ADDRESS : ships_to",
    },
    MegaSample {
        name: "ER Minimal",
        source: "erDiagram\n    USER ||--o{ POST : writes\n    POST ||--o{ COMMENT : has\n    USER ||--o{ COMMENT : authors",
    },
    // -- Journey + Gantt samples (bd-3oaig.5.6) --
    MegaSample {
        name: "Gantt Multi-Section",
        source: "gantt\n    title Release Cycle\n    dateFormat YYYY-MM-DD\n    section Planning\n    Requirements :a1, 2024-01-01, 10d\n    Architecture :a2, after a1, 7d\n    section Development\n    Core Module :b1, after a2, 21d\n    API Layer   :b2, after a2, 14d\n    UI Components :b3, after b2, 14d\n    section QA\n    Unit Tests  :c1, after b1, 7d\n    Integration :c2, after b3, 10d\n    section Release\n    Staging     :d1, after c2, 5d\n    Production  :d2, after d1, 3d",
    },
    MegaSample {
        name: "Journey",
        source: "journey\n    title User Onboarding\n    section Discovery\n      Visit landing page: 5: User\n      Read features: 3: User\n      Watch demo video: 4: User\n    section Signup\n      Create account: 5: User\n      Verify email: 2: User, System\n      Set preferences: 3: User\n    section First Use\n      Complete tutorial: 4: User\n      Create first project: 5: User\n      Invite team: 3: User",
    },
    // -- Mindmap samples (bd-3oaig.5.7) --
    MegaSample {
        name: "Mindmap Deep",
        source: "mindmap\n  root((Architecture))\n    Frontend\n      Components\n        Atoms\n        Molecules\n        Organisms\n      State\n        Redux\n        Context\n      Routing\n    Backend\n      API\n        REST\n        GraphQL\n      Database\n        SQL\n        NoSQL\n      Auth\n        OAuth\n        JWT\n    DevOps\n      CI/CD\n      Monitoring\n      Containers",
    },
    MegaSample {
        name: "Mindmap Wide",
        source: "mindmap\n  root((Product))\n    Design\n    Engineering\n    Marketing\n    Sales\n    Support\n    Legal\n    Finance\n    Operations",
    },
    // -- Pie chart samples (bd-3oaig.5.8) --
    MegaSample {
        name: "Pie Many Slices",
        source: "pie title Language Popularity\n    \"JavaScript\" : 28\n    \"Python\" : 22\n    \"Java\" : 15\n    \"TypeScript\" : 12\n    \"C#\" : 8\n    \"Rust\" : 5\n    \"Go\" : 5\n    \"Other\" : 5",
    },
    MegaSample {
        name: "Pie Dominant",
        source: "pie title Market Share\n    \"Leader\" : 72\n    \"Runner-up\" : 15\n    \"Others\" : 13",
    },
    // -- Mixed/stress samples (bd-3oaig.5.9) --
    MegaSample {
        name: "Flow Stress 20",
        source: "graph TD\n    N1 --> N2 --> N3 --> N4 --> N5\n    N1 --> N3\n    N2 --> N5\n    N6 --> N7 --> N8 --> N9 --> N10\n    N5 --> N6\n    N3 --> N8\n    N11 --> N12 --> N13 --> N14 --> N15\n    N10 --> N11\n    N7 --> N13\n    N16 --> N17 --> N18 --> N19 --> N20\n    N15 --> N16\n    N12 --> N18\n    N1 --> N11\n    N6 --> N16\n    N5 --> N20",
    },
    MegaSample {
        name: "Sequence Dense",
        source: "sequenceDiagram\n    participant A\n    participant B\n    participant C\n    participant D\n    participant E\n    A->>B: msg1\n    B->>C: msg2\n    C->>D: msg3\n    D->>E: msg4\n    E-->>D: reply4\n    D-->>C: reply3\n    C-->>B: reply2\n    B-->>A: reply1\n    A->>C: direct\n    C->>E: skip\n    B->>D: cross\n    D->>A: back",
    },
    // Intentionally invalid sample (bd-3oaig.15): exercise error overlay + diagnostics view.
    MegaSample {
        name: "Flow Invalid (Parse Error)",
        source: "graph TD\n    A[Start] --> B[Ok]\n    classDef",
    },
    MegaSample {
        name: "Generated",
        source: GENERATED_SAMPLE_SOURCE,
    },
];

// ── Parametric generator ────────────────────────────────────────────

const GENERATOR_MIN_NODES: u16 = 6;
const GENERATOR_MAX_NODES: u16 = 200;
const GENERATOR_MIN_BRANCHING: u8 = 1;
const GENERATOR_MAX_BRANCHING: u8 = 6;
const GENERATOR_MIN_LABEL_LEN: u8 = 3;
const GENERATOR_MAX_LABEL_LEN: u8 = 20;
const GENERATOR_MIN_DENSITY: u8 = 0;
const GENERATOR_MAX_DENSITY: u8 = 100;

#[derive(Debug, Clone, Copy)]
struct GeneratorParams {
    nodes: u16,
    branching: u8,
    density: u8,
    label_len: u8,
    seed: u64,
}

impl GeneratorParams {
    fn clamp(self) -> Self {
        Self {
            nodes: self.nodes.clamp(GENERATOR_MIN_NODES, GENERATOR_MAX_NODES),
            branching: self
                .branching
                .clamp(GENERATOR_MIN_BRANCHING, GENERATOR_MAX_BRANCHING),
            density: self
                .density
                .clamp(GENERATOR_MIN_DENSITY, GENERATOR_MAX_DENSITY),
            label_len: self
                .label_len
                .clamp(GENERATOR_MIN_LABEL_LEN, GENERATOR_MAX_LABEL_LEN),
            seed: self.seed,
        }
    }
}

impl Default for GeneratorParams {
    fn default() -> Self {
        Self {
            nodes: 24,
            branching: 2,
            density: 25,
            label_len: 6,
            seed: 1,
        }
    }
}

struct GeneratorRng {
    state: u64,
}

impl GeneratorRng {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u32(&mut self) -> u32 {
        self.state = self.state.wrapping_mul(6364136223846793005).wrapping_add(1);
        (self.state >> 32) as u32
    }

    fn next_range(&mut self, max: u32) -> u32 {
        if max == 0 { 0 } else { self.next_u32() % max }
    }
}

fn make_generator_label(index: usize, len: usize) -> String {
    let mut label = format!("N{index}");
    let mut cursor = 0usize;
    while label.len() < len {
        let ch = (b'A' + (cursor as u8 % 26)) as char;
        label.push(ch);
        cursor = cursor.saturating_add(1);
    }
    label.truncate(len.max(1));
    label
}

fn generate_parametric_flowchart(params: GeneratorParams) -> String {
    let params = params.clamp();
    let nodes = params.nodes as usize;
    let branching = params.branching as usize;
    let label_len = params.label_len as usize;
    let density = params.density as usize;

    let mut out = String::new();
    out.push_str("graph TD\n");

    for i in 1..=nodes {
        let label = make_generator_label(i, label_len);
        out.push_str(&format!("    N{i}[{label}]\n"));
    }

    let mut edges: BTreeSet<(usize, usize)> = BTreeSet::new();
    for i in 1..nodes {
        edges.insert((i, i + 1));
        for b in 0..branching {
            let target = i + b + 2;
            if target <= nodes {
                edges.insert((i, target));
            }
        }
    }

    let base_edges = edges.len();
    let extra_edges = density.saturating_mul(nodes) / 100;
    let mut rng = GeneratorRng::new(params.seed ^ ((nodes as u64) << 32));
    let mut attempts = 0usize;
    let max_attempts = extra_edges.saturating_mul(20).saturating_add(10);

    while edges.len() < base_edges + extra_edges && attempts < max_attempts {
        let src = rng.next_range(nodes as u32) as usize + 1;
        let dst = rng.next_range(nodes as u32) as usize + 1;
        if src != dst {
            edges.insert((src, dst));
        }
        attempts = attempts.saturating_add(1);
    }

    for (src, dst) in edges {
        out.push_str(&format!("    N{src} --> N{dst}\n"));
    }
    out
}

// ── Panel visibility ────────────────────────────────────────────────

/// Which panels are currently visible.
#[derive(Debug, Clone, Copy)]
struct PanelVisibility {
    controls: bool,
    metrics: bool,
    detail: bool,
    status_log: bool,
    help_overlay: bool,
    diagnostics: bool,
}

impl Default for PanelVisibility {
    fn default() -> Self {
        Self {
            controls: true,
            metrics: true,
            detail: false,
            status_log: false,
            help_overlay: false,
            diagnostics: false,
        }
    }
}

// ── Debug overlays ─────────────────────────────────────────────────

/// Debug overlay toggles for visualizing internal layout structures.
#[derive(Debug, Clone, Copy, Default)]
struct DebugOverlays {
    /// Show rectangles around node bounds.
    node_bounds: bool,
    /// Show markers at edge route waypoints.
    edge_routes: bool,
    /// Show rectangles around cluster (subgraph) bounds.
    cluster_bounds: bool,
    /// Show alignment grid at rank boundaries.
    alignment_grid: bool,
}

impl DebugOverlays {
    fn any_active(self) -> bool {
        self.node_bounds || self.edge_routes || self.cluster_bounds || self.alignment_grid
    }
}

// ── Layout mode ─────────────────────────────────────────────────────

/// Layout density preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LayoutMode {
    Dense,
    Normal,
    Spacious,
    Auto,
}

impl LayoutMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Dense => "dense",
            Self::Normal => "normal",
            Self::Spacious => "spacious",
            Self::Auto => "auto",
        }
    }

    const fn next(self) -> Self {
        match self {
            Self::Dense => Self::Normal,
            Self::Normal => Self::Spacious,
            Self::Spacious => Self::Auto,
            Self::Auto => Self::Dense,
        }
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

fn push_opt_f64(json: &mut String, key: &str, value: Option<f64>) {
    json.push_str(&format!(",\"{key}\":"));
    if let Some(v) = value
        && v.is_finite()
    {
        json.push_str(&format!("{v:.6}"));
    } else {
        json.push_str("null");
    }
}

fn push_opt_u64(json: &mut String, key: &str, value: Option<u64>) {
    json.push_str(&format!(",\"{key}\":"));
    if let Some(v) = value {
        json.push_str(&v.to_string());
    } else {
        json.push_str("null");
    }
}

// ── Computed layout regions ─────────────────────────────────────────

/// Regions computed from the terminal area and panel visibility.
#[derive(Debug, Clone, Copy, Default)]
struct LayoutRegions {
    /// Main diagram rendering area.
    diagram: Rect,
    /// Right-side panel area (metrics + detail).
    side_panel: Rect,
    /// Top controls strip.
    controls: Rect,
    /// Bottom footer (status line + key hints).
    footer: Rect,
}

impl LayoutRegions {
    /// Compute layout regions from available area and panel state.
    fn compute(area: Rect, panels: &PanelVisibility) -> Self {
        if area.width < 10 || area.height < 5 {
            return Self {
                diagram: area,
                ..Default::default()
            };
        }

        let x = area.x;
        let mut y = area.y;
        let mut w = area.width;
        let mut h = area.height;

        // Footer always present.
        let footer_h = FOOTER_HEIGHT.min(h.saturating_sub(3));
        h = h.saturating_sub(footer_h);
        let footer = Rect::new(x, y + h, w, footer_h);

        // Controls strip at top.
        let controls_h = if panels.controls && h > 8 {
            CONTROLS_PANEL_HEIGHT.min(h / 3)
        } else {
            0
        };
        let controls = Rect::new(x, y, w, controls_h);
        y += controls_h;
        h = h.saturating_sub(controls_h);

        // Side panel on right.
        let side_w = if (panels.metrics || panels.detail) && w >= MIN_FULL_WIDTH {
            SIDE_PANEL_WIDTH.min(w / 3)
        } else {
            0
        };
        let side_panel = if side_w > 0 {
            w -= side_w;
            Rect::new(x + w, y, side_w, h)
        } else {
            Rect::default()
        };

        let diagram = Rect::new(x, y, w, h);

        Self {
            diagram,
            side_panel,
            controls,
            footer,
        }
    }
}

// ── Render cache ───────────────────────────────────────────────────

/// Cached render pipeline output for the mega showcase.
#[derive(Debug)]
struct MegaRenderCache {
    analysis_epoch: u64,
    layout_epoch: u64,
    render_epoch: u64,
    viewport: (u16, u16),
    zoom: f32,
    ir: Option<MermaidDiagramIr>,
    layout: Option<mermaid_layout::DiagramLayout>,
    buffer: Buffer,
    errors: Vec<MermaidError>,
    cache_hits: u64,
    cache_misses: u64,
    last_cache_hit: bool,
    parse_ms: Option<f32>,
    layout_ms: Option<f32>,
    render_ms: Option<f32>,
    /// Timestamp of the most recent layout computation (for debounce).
    last_layout_instant: Option<Instant>,
    /// Whether the last layout exceeded the budget threshold.
    layout_budget_exceeded: bool,
    /// Whether the budget warning has been logged (prevents duplicate logs).
    budget_warning_logged: bool,
    /// Number of layout passes deferred due to debounce.
    debounce_skips: u64,
    /// Monotonic sequence number for telemetry events.
    telemetry_seq: u64,
}

impl MegaRenderCache {
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
            errors: Vec::new(),
            cache_hits: 0,
            cache_misses: 0,
            last_cache_hit: false,
            parse_ms: None,
            layout_ms: None,
            render_ms: None,
            last_layout_instant: None,
            layout_budget_exceeded: false,
            budget_warning_logged: true,
            debounce_skips: 0,
            telemetry_seq: 0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct MegaRecomputeFlags {
    analysis_ran: bool,
    layout_ran: bool,
    render_ran: bool,
}

// ── State ───────────────────────────────────────────────────────────

/// Maximum status log entries before oldest are evicted.
const STATUS_LOG_CAP: usize = 64;

/// A single entry in the status log.
#[derive(Debug, Clone)]
struct StatusLogEntry {
    action: &'static str,
    detail: String,
}

// ── Structured telemetry (bd-3oaig.20) ────────────────────────────

/// Schema version for mega showcase telemetry JSONL events.
pub const MEGA_TELEMETRY_SCHEMA: &str = "ftui-mega-telemetry-v1";

/// Structured telemetry event emitted after each cache-miss recompute pass.
///
/// Contains the full decision context, phase breakdown, diagram dimensions,
/// cache statistics, and layout quality metrics.  Serialised to a single
/// JSONL line via [`MegaRecomputeEvent::to_jsonl`].
#[derive(Debug, Clone)]
pub struct MegaRecomputeEvent {
    // ── Identity ───────────────────────────────────────────────────
    /// Schema version for forward compatibility.
    pub schema_version: &'static str,
    /// Event type identifier (always `"mermaid_mega_recompute"`).
    pub event: &'static str,
    /// Monotonic sequence number within this session.
    pub seq: u64,

    // ── Sample ─────────────────────────────────────────────────────
    /// Sample name from the mega sample library.
    pub sample: String,
    /// Sample index in the library.
    pub sample_idx: usize,
    /// Detected diagram type from the parsed IR.
    pub diagram_type: String,

    // ── Trigger ────────────────────────────────────────────────────
    /// What caused this recompute: "initial", "sample_change",
    /// "config_change", "resize", "zoom".
    pub trigger: String,

    // ── Configuration snapshot ─────────────────────────────────────
    pub layout_mode: String,
    pub tier: String,
    pub glyph_mode: String,
    pub render_mode: String,
    pub wrap_mode: String,
    pub palette: String,
    pub viewport_cols: u16,
    pub viewport_rows: u16,
    pub zoom: f32,

    // ── Phase timings (milliseconds; None if phase was skipped) ────
    pub parse_ms: Option<f32>,
    pub layout_ms: Option<f32>,
    pub render_ms: Option<f32>,
    /// Sum of non-None phase timings.
    pub total_ms: f32,

    // ── Which phases actually ran ──────────────────────────────────
    pub analysis_ran: bool,
    pub layout_ran: bool,
    pub render_ran: bool,

    // ── Diagram dimensions (from IR) ───────────────────────────────
    pub node_count: usize,
    pub edge_count: usize,
    pub cluster_count: usize,
    pub label_count: usize,

    // ── Cache statistics ───────────────────────────────────────────
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub cache_hit_ratio: f32,
    pub debounce_skips: u64,

    // ── Layout quality metrics (None if layout was not recomputed) ─
    pub layout_iterations: Option<usize>,
    pub layout_budget_exceeded: bool,
    pub crossings: Option<usize>,
    pub total_bends: Option<usize>,
    pub position_variance: Option<f64>,
    pub ranks: Option<usize>,
    pub max_rank_width: Option<usize>,

    // ── Errors ─────────────────────────────────────────────────────
    pub error_count: usize,
}

impl MegaRecomputeEvent {
    /// Serialise this event to a single JSON line (no trailing newline).
    #[must_use]
    pub fn to_jsonl(&self) -> String {
        serde_json::json!({
            "schema_version": self.schema_version,
            "event": self.event,
            "seq": self.seq,
            "sample": self.sample,
            "sample_idx": self.sample_idx,
            "diagram_type": self.diagram_type,
            "trigger": self.trigger,
            "layout_mode": self.layout_mode,
            "tier": self.tier,
            "glyph_mode": self.glyph_mode,
            "render_mode": self.render_mode,
            "wrap_mode": self.wrap_mode,
            "palette": self.palette,
            "viewport_cols": self.viewport_cols,
            "viewport_rows": self.viewport_rows,
            "zoom": self.zoom,
            "parse_ms": self.parse_ms,
            "layout_ms": self.layout_ms,
            "render_ms": self.render_ms,
            "total_ms": self.total_ms,
            "analysis_ran": self.analysis_ran,
            "layout_ran": self.layout_ran,
            "render_ran": self.render_ran,
            "node_count": self.node_count,
            "edge_count": self.edge_count,
            "cluster_count": self.cluster_count,
            "label_count": self.label_count,
            "cache_hits": self.cache_hits,
            "cache_misses": self.cache_misses,
            "cache_hit_ratio": self.cache_hit_ratio,
            "debounce_skips": self.debounce_skips,
            "layout_iterations": self.layout_iterations,
            "layout_budget_exceeded": self.layout_budget_exceeded,
            "crossings": self.crossings,
            "total_bends": self.total_bends,
            "position_variance": self.position_variance,
            "ranks": self.ranks,
            "max_rank_width": self.max_rank_width,
            "error_count": self.error_count,
        })
        .to_string()
    }
}

/// Required top-level fields in every `mega_recompute` JSONL event.
///
/// These fields must always be present and non-null.
pub const MEGA_TELEMETRY_REQUIRED_FIELDS: &[&str] = &[
    // Identity
    "schema_version",
    "event",
    "seq",
    "timestamp",
    "seed",
    "screen_mode",
    // Sample
    "sample",
    "diagram_type",
    // Configuration
    "layout_mode",
    "tier",
    "glyph_mode",
    "render_mode",
    "wrap_mode",
    "palette",
    "styles_enabled",
    "comparison_enabled",
    "comparison_layout_mode",
    // Viewport
    "viewport_cols",
    "viewport_rows",
    "render_cols",
    "render_rows",
    "zoom",
    "pan_x",
    "pan_y",
    // Epochs
    "analysis_epoch",
    "layout_epoch",
    "render_epoch",
    // Phase flags
    "analysis_ran",
    "layout_ran",
    "render_ran",
    // Cache
    "cache_hits",
    "cache_misses",
    "cache_hit",
    "debounce_skips",
    "layout_budget_exceeded",
];

/// Fields that may be `null` (phase was skipped or data unavailable).
///
/// These fields must always be present but may have a `null` value.
pub const MEGA_TELEMETRY_NULLABLE_FIELDS: &[&str] = &[
    // Optional identity
    "run_id",
    // Phase timings (null when phase was skipped)
    "parse_ms",
    "layout_ms",
    "render_ms",
    // Diagram dimensions (null when IR unavailable)
    "node_count",
    "edge_count",
    "error_count",
    // Layout quality metrics (null when layout was not recomputed)
    "layout_iterations",
    "layout_iterations_max",
    "layout_budget_exceeded_layout",
    "layout_crossings",
    "layout_ranks",
    "layout_max_rank_width",
    "layout_total_bends",
    "layout_position_variance",
];

/// Valid values for the `trigger` field in `MegaRecomputeEvent`.
pub const MEGA_TELEMETRY_VALID_TRIGGERS: &[&str] = &[
    "initial",
    "sample_change",
    "config_change",
    "resize",
    "zoom",
];

/// Fields that must be non-negative numbers (integer or float).
const MEGA_TELEMETRY_NONNEG_FIELDS: &[&str] = &[
    "seq",
    "seed",
    "viewport_cols",
    "viewport_rows",
    "render_cols",
    "render_rows",
    "cache_hits",
    "cache_misses",
    "debounce_skips",
    "analysis_epoch",
    "layout_epoch",
    "render_epoch",
];

/// Fields that must be boolean values.
const MEGA_TELEMETRY_BOOL_FIELDS: &[&str] = &[
    "analysis_ran",
    "layout_ran",
    "render_ran",
    "cache_hit",
    "layout_budget_exceeded",
    "styles_enabled",
    "comparison_enabled",
];

/// Validate a JSONL line against the mega telemetry schema.
///
/// Returns `Ok(())` if the line is valid, or `Err(reason)` with a
/// human-readable explanation of the first validation failure.
///
/// Checks:
/// - Valid JSON object
/// - Schema version matches `MEGA_TELEMETRY_SCHEMA`
/// - Event type is `"mermaid_mega_recompute"`
/// - All required fields present and non-null
/// - All nullable fields present (may be null)
/// - Boolean fields are actually booleans
/// - Non-negative numeric fields are numbers >= 0
/// - Zoom is positive
/// - Timing fields (when non-null) are non-negative
pub fn validate_mega_telemetry_line(line: &str) -> Result<(), String> {
    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|e| format!("invalid JSON: {e}"))?;

    let obj = value
        .as_object()
        .ok_or_else(|| "top-level value must be a JSON object".to_string())?;

    // Check schema version.
    match obj.get("schema_version").and_then(|v| v.as_str()) {
        Some(v) if v == MEGA_TELEMETRY_SCHEMA => {}
        Some(v) => {
            return Err(format!(
                "schema_version mismatch: expected \"{MEGA_TELEMETRY_SCHEMA}\", got \"{v}\""
            ));
        }
        None => return Err("missing field: schema_version".to_string()),
    }

    // Check event type.
    match obj.get("event").and_then(|v| v.as_str()) {
        Some("mermaid_mega_recompute") => {}
        Some(v) => {
            return Err(format!(
                "event type mismatch: expected \"mermaid_mega_recompute\", got \"{v}\""
            ));
        }
        None => return Err("missing field: event".to_string()),
    }

    // Check all required fields are present and non-null.
    for &field in MEGA_TELEMETRY_REQUIRED_FIELDS {
        match obj.get(field) {
            None => return Err(format!("missing required field: {field}")),
            Some(v) if v.is_null() => {
                return Err(format!("required field is null: {field}"));
            }
            _ => {}
        }
    }

    // Check nullable fields exist (they must be present, but may be null).
    for &field in MEGA_TELEMETRY_NULLABLE_FIELDS {
        if !obj.contains_key(field) {
            return Err(format!("missing nullable field: {field}"));
        }
    }

    // Validate boolean fields are actually booleans.
    for &field in MEGA_TELEMETRY_BOOL_FIELDS {
        if let Some(v) = obj.get(field)
            && !v.is_boolean()
        {
            return Err(format!("field \"{field}\" must be boolean, got {v}"));
        }
    }

    // Validate non-negative numeric fields.
    for &field in MEGA_TELEMETRY_NONNEG_FIELDS {
        if let Some(v) = obj.get(field)
            && !v.is_null()
        {
            let n = v
                .as_f64()
                .ok_or_else(|| format!("field \"{field}\" must be a number, got {v}"))?;
            if n < 0.0 {
                return Err(format!("field \"{field}\" must be non-negative, got {n}"));
            }
        }
    }

    // Validate zoom is positive.
    if let Some(zoom) = obj.get("zoom").and_then(|v| v.as_f64())
        && zoom <= 0.0
    {
        return Err(format!("zoom must be positive, got {zoom}"));
    }

    // Validate timing fields are non-negative when present.
    for &field in &["parse_ms", "layout_ms", "render_ms"] {
        if let Some(v) = obj.get(field)
            && let Some(n) = v.as_f64()
            && n < 0.0
        {
            return Err(format!("{field} must be non-negative, got {n}"));
        }
    }

    // Validate string fields are actually strings.
    for &field in &[
        "schema_version",
        "event",
        "timestamp",
        "screen_mode",
        "sample",
        "diagram_type",
        "layout_mode",
        "tier",
        "glyph_mode",
        "render_mode",
        "wrap_mode",
        "palette",
        "comparison_layout_mode",
    ] {
        if let Some(v) = obj.get(field)
            && !v.is_string()
        {
            return Err(format!("field \"{field}\" must be a string, got {v}"));
        }
    }

    Ok(())
}

/// Validate all lines in a multi-line JSONL string.
///
/// Returns a vec of `(line_number, error)` for each invalid line.
/// Empty lines are skipped.
#[must_use]
pub fn validate_mega_telemetry_jsonl(content: &str) -> Vec<(usize, String)> {
    content
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .filter_map(|(i, line)| {
            validate_mega_telemetry_line(line.trim())
                .err()
                .map(|e| (i + 1, e))
        })
        .collect()
}

/// State for the Mermaid Mega Showcase screen.
#[derive(Debug)]
pub struct MermaidMegaState {
    /// Current interaction mode.
    mode: ShowcaseMode,
    /// Panel visibility flags.
    panels: PanelVisibility,
    /// Layout density mode.
    layout_mode: LayoutMode,
    /// Fidelity tier.
    tier: MermaidTier,
    /// Glyph mode (Unicode / ASCII).
    glyph_mode: MermaidGlyphMode,
    /// Render mode (Cell / Braille / Block / etc).
    render_mode: MermaidRenderMode,
    /// Wrap mode for labels.
    wrap_mode: MermaidWrapMode,
    /// Color palette preset.
    palette: DiagramPalettePreset,
    /// Whether classDef/style rendering is enabled.
    styles_enabled: bool,
    /// Viewport zoom level (1.0 = 100%).
    viewport_zoom: f32,
    /// Viewport pan offset (x, y) in cells.
    viewport_pan: (i16, i16),
    /// Optional explicit viewport size override (cols, rows).
    viewport_size_override: Option<(u16, u16)>,
    /// Selected sample index.
    selected_sample: usize,
    /// Parametric generator settings for the generated sample.
    generator: GeneratorParams,
    /// Cached source for the generated sample.
    generated_source: String,
    /// Selected node index for inspect mode.
    selected_node: Option<usize>,
    /// Search query (when in search mode).
    search_query: Option<String>,
    /// Matching sample indices for the current search query.
    search_matches: Vec<usize>,
    /// Cursor within search_matches.
    search_match_idx: usize,
    /// Epoch counters for cache invalidation.
    analysis_epoch: u64,
    layout_epoch: u64,
    render_epoch: u64,
    /// Status log for debugging state changes.
    status_log: Vec<StatusLogEntry>,
    /// Debug overlay visibility flags.
    debug_overlays: DebugOverlays,
    /// Comparison mode: render a second layout side-by-side.
    comparison_enabled: bool,
    /// Layout mode used for the comparison (right-side) pane.
    comparison_layout_mode: LayoutMode,
}

impl Default for MermaidMegaState {
    fn default() -> Self {
        let generator = GeneratorParams::default();
        let generated_source = generate_parametric_flowchart(generator);
        Self {
            mode: ShowcaseMode::Normal,
            panels: PanelVisibility::default(),
            layout_mode: LayoutMode::Auto,
            tier: MermaidTier::Auto,
            glyph_mode: MermaidGlyphMode::Unicode,
            render_mode: MermaidRenderMode::Braille,
            wrap_mode: MermaidWrapMode::WordChar,
            palette: DiagramPalettePreset::Default,
            styles_enabled: true,
            viewport_zoom: 1.0,
            viewport_pan: (0, 0),
            viewport_size_override: None,
            selected_sample: 0,
            generator,
            generated_source,
            selected_node: None,
            search_query: None,
            search_matches: Vec::new(),
            search_match_idx: 0,
            analysis_epoch: 0,
            layout_epoch: 0,
            render_epoch: 0,
            status_log: Vec::new(),
            debug_overlays: DebugOverlays::default(),
            comparison_enabled: false,
            comparison_layout_mode: LayoutMode::Dense,
        }
    }
}

impl MermaidMegaState {
    /// Record an action in the status log.
    fn log_action(&mut self, action: &'static str, detail: String) {
        if self.status_log.len() >= STATUS_LOG_CAP {
            self.status_log.remove(0);
        }
        self.status_log.push(StatusLogEntry { action, detail });
    }

    /// Build a MermaidConfig from the current state.
    fn to_config(&self) -> MermaidConfig {
        let mut config = MermaidConfig {
            glyph_mode: self.glyph_mode,
            render_mode: self.render_mode,
            tier_override: self.tier,
            wrap_mode: self.wrap_mode,
            enable_styles: self.styles_enabled,
            palette: self.palette,
            ..Default::default()
        };
        match self.layout_mode {
            LayoutMode::Dense => {
                config.layout_iteration_budget = 400;
                config.route_budget = 8_000;
            }
            LayoutMode::Spacious => {
                config.layout_iteration_budget = 140;
                config.route_budget = 3_000;
            }
            LayoutMode::Normal | LayoutMode::Auto => {}
        }
        config
    }

    /// Get layout spacing based on the current layout mode.
    fn layout_spacing(&self) -> mermaid_layout::LayoutSpacing {
        match self.layout_mode {
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
            LayoutMode::Normal | LayoutMode::Auto => mermaid_layout::LayoutSpacing::default(),
        }
    }

    fn selected_sample_entry(&self) -> Option<&'static MegaSample> {
        if MEGA_SAMPLES.is_empty() {
            return None;
        }
        let idx = self.selected_sample % MEGA_SAMPLES.len();
        Some(&MEGA_SAMPLES[idx])
    }

    fn selected_is_generated(&self) -> bool {
        self.selected_sample_entry()
            .is_some_and(|sample| sample.source == GENERATED_SAMPLE_SOURCE)
    }

    fn regenerate_generated_source(&mut self) {
        self.generator = self.generator.clamp();
        self.generated_source = generate_parametric_flowchart(self.generator);
    }

    fn on_generator_change(&mut self) {
        self.regenerate_generated_source();
        if self.selected_is_generated() {
            self.bump_analysis();
        }
    }

    /// Get the currently selected sample source, wrapping around.
    fn selected_source(&self) -> Option<&str> {
        let sample = self.selected_sample_entry()?;
        if sample.source == GENERATED_SAMPLE_SOURCE {
            Some(self.generated_source.as_str())
        } else {
            Some(sample.source)
        }
    }

    /// Get the currently selected sample name, wrapping around.
    fn selected_name(&self) -> &'static str {
        self.selected_sample_entry()
            .map(|sample| sample.name)
            .unwrap_or("(none)")
    }

    /// Adjust the viewport size override by a delta, creating one if needed.
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

    /// Bump render epoch (triggers re-render without re-layout).
    fn bump_render(&mut self) {
        self.render_epoch = self.render_epoch.wrapping_add(1);
    }

    /// Bump layout epoch (triggers re-layout + re-render).
    fn bump_layout(&mut self) {
        self.layout_epoch = self.layout_epoch.wrapping_add(1);
        self.bump_render();
    }

    /// Bump analysis epoch (triggers full re-parse + re-layout + re-render).
    fn bump_analysis(&mut self) {
        self.analysis_epoch = self.analysis_epoch.wrapping_add(1);
        self.bump_layout();
    }

    fn filter_active(&self) -> bool {
        self.search_query
            .as_ref()
            .is_some_and(|q| !q.trim().is_empty())
    }

    fn rebuild_search_matches(&mut self) {
        self.search_matches.clear();
        self.search_match_idx = 0;

        let Some(query) = self.search_query.as_deref() else {
            return;
        };
        let query = query.trim();
        if query.is_empty() {
            return;
        }

        let lowered = query.to_ascii_lowercase();
        let tokens: Vec<&str> = lowered
            .split_whitespace()
            .filter(|t| !t.is_empty())
            .collect();
        if tokens.is_empty() {
            return;
        }

        self.search_matches = MEGA_SAMPLES
            .iter()
            .enumerate()
            .filter(|(_, s)| mega_sample_matches_tokens(s, &tokens))
            .map(|(idx, _)| idx)
            .collect();

        if self.search_matches.is_empty() {
            self.search_match_idx = 0;
            return;
        }

        // Keep match cursor stable when possible.
        if let Some(pos) = self
            .search_matches
            .iter()
            .position(|idx| *idx == self.selected_sample)
        {
            self.search_match_idx = pos;
        }
    }
}

// ── Actions ─────────────────────────────────────────────────────────

/// Actions the mega showcase screen can perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MegaAction {
    NextSample,
    PrevSample,
    IncreaseGenNodes,
    DecreaseGenNodes,
    IncreaseGenBranching,
    DecreaseGenBranching,
    IncreaseGenDensity,
    DecreaseGenDensity,
    IncreaseGenLabelLen,
    DecreaseGenLabelLen,
    NextGenSeed,
    CycleTier,
    ToggleGlyphMode,
    CycleRenderMode,
    CycleWrapMode,
    ToggleStyles,
    CycleLayoutMode,
    ForceRelayout,
    CyclePalette,
    PrevPalette,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    FitToView,
    PanLeft,
    PanRight,
    PanUp,
    PanDown,
    IncreaseViewportWidth,
    DecreaseViewportWidth,
    IncreaseViewportHeight,
    DecreaseViewportHeight,
    ResetViewportOverride,
    ToggleMetrics,
    ToggleControls,
    ToggleDetail,
    ToggleStatusLog,
    ToggleHelp,
    SelectNextNode,
    SelectPrevNode,
    DeselectNode,
    EnterSearch,
    ExitSearch,
    SearchAppendChar(char),
    SearchBackspace,
    SearchAccept,
    ClearFilter,
    NextSearchMatch,
    PrevSearchMatch,
    ToggleDiagnostics,
    ToggleDebugNodeBounds,
    ToggleDebugEdgeRoutes,
    ToggleDebugClusterBounds,
    ToggleDebugGrid,
    CollapsePanels,
    ToggleComparison,
    CycleComparisonLayout,
}

impl MermaidMegaState {
    /// Apply an action to the state.
    fn apply(&mut self, action: MegaAction) {
        match action {
            MegaAction::NextSample => {
                if self.filter_active() && !self.search_matches.is_empty() {
                    let pos = self
                        .search_matches
                        .iter()
                        .position(|idx| *idx == self.selected_sample)
                        .unwrap_or(0);
                    let next = (pos + 1) % self.search_matches.len();
                    self.search_match_idx = next;
                    self.selected_sample = self.search_matches[next];
                } else {
                    self.selected_sample = self.selected_sample.wrapping_add(1);
                }
                self.selected_node = None;
                self.bump_analysis();
            }
            MegaAction::PrevSample => {
                if self.filter_active() && !self.search_matches.is_empty() {
                    let pos = self
                        .search_matches
                        .iter()
                        .position(|idx| *idx == self.selected_sample)
                        .unwrap_or(0);
                    let prev = (pos + self.search_matches.len() - 1) % self.search_matches.len();
                    self.search_match_idx = prev;
                    self.selected_sample = self.search_matches[prev];
                } else {
                    self.selected_sample = self.selected_sample.wrapping_sub(1);
                }
                self.selected_node = None;
                self.bump_analysis();
            }
            MegaAction::IncreaseGenNodes => {
                let next = self
                    .generator
                    .nodes
                    .saturating_add(4)
                    .clamp(GENERATOR_MIN_NODES, GENERATOR_MAX_NODES);
                if next != self.generator.nodes {
                    self.generator.nodes = next;
                    self.on_generator_change();
                }
            }
            MegaAction::DecreaseGenNodes => {
                let next = self
                    .generator
                    .nodes
                    .saturating_sub(4)
                    .clamp(GENERATOR_MIN_NODES, GENERATOR_MAX_NODES);
                if next != self.generator.nodes {
                    self.generator.nodes = next;
                    self.on_generator_change();
                }
            }
            MegaAction::IncreaseGenBranching => {
                let next = self
                    .generator
                    .branching
                    .saturating_add(1)
                    .clamp(GENERATOR_MIN_BRANCHING, GENERATOR_MAX_BRANCHING);
                if next != self.generator.branching {
                    self.generator.branching = next;
                    self.on_generator_change();
                }
            }
            MegaAction::DecreaseGenBranching => {
                let next = self
                    .generator
                    .branching
                    .saturating_sub(1)
                    .clamp(GENERATOR_MIN_BRANCHING, GENERATOR_MAX_BRANCHING);
                if next != self.generator.branching {
                    self.generator.branching = next;
                    self.on_generator_change();
                }
            }
            MegaAction::IncreaseGenDensity => {
                let next = self
                    .generator
                    .density
                    .saturating_add(5)
                    .clamp(GENERATOR_MIN_DENSITY, GENERATOR_MAX_DENSITY);
                if next != self.generator.density {
                    self.generator.density = next;
                    self.on_generator_change();
                }
            }
            MegaAction::DecreaseGenDensity => {
                let next = self
                    .generator
                    .density
                    .saturating_sub(5)
                    .clamp(GENERATOR_MIN_DENSITY, GENERATOR_MAX_DENSITY);
                if next != self.generator.density {
                    self.generator.density = next;
                    self.on_generator_change();
                }
            }
            MegaAction::IncreaseGenLabelLen => {
                let next = self
                    .generator
                    .label_len
                    .saturating_add(1)
                    .clamp(GENERATOR_MIN_LABEL_LEN, GENERATOR_MAX_LABEL_LEN);
                if next != self.generator.label_len {
                    self.generator.label_len = next;
                    self.on_generator_change();
                }
            }
            MegaAction::DecreaseGenLabelLen => {
                let next = self
                    .generator
                    .label_len
                    .saturating_sub(1)
                    .clamp(GENERATOR_MIN_LABEL_LEN, GENERATOR_MAX_LABEL_LEN);
                if next != self.generator.label_len {
                    self.generator.label_len = next;
                    self.on_generator_change();
                }
            }
            MegaAction::NextGenSeed => {
                self.generator.seed = self.generator.seed.wrapping_add(1);
                self.on_generator_change();
            }
            MegaAction::CycleTier => {
                self.tier = match self.tier {
                    MermaidTier::Auto => MermaidTier::Compact,
                    MermaidTier::Compact => MermaidTier::Normal,
                    MermaidTier::Normal => MermaidTier::Rich,
                    MermaidTier::Rich => MermaidTier::Auto,
                };
                self.bump_layout();
            }
            MegaAction::ToggleGlyphMode => {
                self.glyph_mode = match self.glyph_mode {
                    MermaidGlyphMode::Unicode => MermaidGlyphMode::Ascii,
                    MermaidGlyphMode::Ascii => MermaidGlyphMode::Unicode,
                };
                self.bump_render();
            }
            MegaAction::CycleRenderMode => {
                self.render_mode = match self.render_mode {
                    MermaidRenderMode::Auto => MermaidRenderMode::CellOnly,
                    MermaidRenderMode::CellOnly => MermaidRenderMode::Braille,
                    MermaidRenderMode::Braille => MermaidRenderMode::Block,
                    MermaidRenderMode::Block => MermaidRenderMode::HalfBlock,
                    MermaidRenderMode::HalfBlock => MermaidRenderMode::Auto,
                };
                self.bump_render();
            }
            MegaAction::CycleWrapMode => {
                self.wrap_mode = match self.wrap_mode {
                    MermaidWrapMode::None => MermaidWrapMode::Word,
                    MermaidWrapMode::Word => MermaidWrapMode::Char,
                    MermaidWrapMode::Char => MermaidWrapMode::WordChar,
                    MermaidWrapMode::WordChar => MermaidWrapMode::None,
                };
                self.bump_layout();
            }
            MegaAction::ToggleStyles => {
                self.styles_enabled = !self.styles_enabled;
                self.bump_render();
            }
            MegaAction::CycleLayoutMode => {
                self.layout_mode = self.layout_mode.next();
                self.bump_layout();
            }
            MegaAction::ForceRelayout => {
                self.bump_layout();
            }
            MegaAction::CyclePalette => {
                self.palette = self.palette.next();
                self.bump_render();
            }
            MegaAction::PrevPalette => {
                self.palette = self.palette.prev();
                self.bump_render();
            }
            MegaAction::ZoomIn => {
                self.viewport_zoom = (self.viewport_zoom * 1.25).min(4.0);
                self.bump_render();
            }
            MegaAction::ZoomOut => {
                self.viewport_zoom = (self.viewport_zoom / 1.25).max(0.25);
                self.bump_render();
            }
            MegaAction::ZoomReset => {
                self.viewport_zoom = 1.0;
                self.viewport_pan = (0, 0);
                self.bump_render();
            }
            MegaAction::FitToView => {
                self.viewport_zoom = 1.0;
                self.viewport_pan = (0, 0);
                self.bump_render();
            }
            MegaAction::PanLeft => {
                self.viewport_pan.0 = self.viewport_pan.0.saturating_sub(PAN_STEP);
            }
            MegaAction::PanRight => {
                self.viewport_pan.0 = self.viewport_pan.0.saturating_add(PAN_STEP);
            }
            MegaAction::PanUp => {
                self.viewport_pan.1 = self.viewport_pan.1.saturating_sub(PAN_STEP);
            }
            MegaAction::PanDown => {
                self.viewport_pan.1 = self.viewport_pan.1.saturating_add(PAN_STEP);
            }
            MegaAction::IncreaseViewportWidth => {
                self.adjust_viewport_override(VIEWPORT_OVERRIDE_STEP_COLS, 0);
            }
            MegaAction::DecreaseViewportWidth => {
                self.adjust_viewport_override(-VIEWPORT_OVERRIDE_STEP_COLS, 0);
            }
            MegaAction::IncreaseViewportHeight => {
                self.adjust_viewport_override(0, VIEWPORT_OVERRIDE_STEP_ROWS);
            }
            MegaAction::DecreaseViewportHeight => {
                self.adjust_viewport_override(0, -VIEWPORT_OVERRIDE_STEP_ROWS);
            }
            MegaAction::ResetViewportOverride => {
                if self.viewport_size_override.is_some() {
                    self.viewport_size_override = None;
                    self.bump_render();
                }
            }
            MegaAction::ToggleMetrics => {
                self.panels.metrics = !self.panels.metrics;
            }
            MegaAction::ToggleControls => {
                self.panels.controls = !self.panels.controls;
            }
            MegaAction::ToggleDetail => {
                self.panels.detail = !self.panels.detail;
            }
            MegaAction::ToggleStatusLog => {
                self.panels.status_log = !self.panels.status_log;
            }
            MegaAction::ToggleHelp => {
                self.panels.help_overlay = !self.panels.help_overlay;
            }
            MegaAction::SelectNextNode => {
                self.selected_node = Some(self.selected_node.map_or(0, |n| n + 1));
                self.mode = ShowcaseMode::Inspect;
                self.bump_render();
            }
            MegaAction::SelectPrevNode => {
                self.selected_node = Some(self.selected_node.map_or(0, |n| n.saturating_sub(1)));
                self.mode = ShowcaseMode::Inspect;
                self.bump_render();
            }
            MegaAction::DeselectNode => {
                self.selected_node = None;
                self.mode = ShowcaseMode::Normal;
                self.bump_render();
            }
            MegaAction::EnterSearch => {
                self.mode = ShowcaseMode::Search;
                if self.search_query.is_none() {
                    self.search_query = Some(String::new());
                }
                self.rebuild_search_matches();
            }
            MegaAction::ExitSearch => {
                self.mode = ShowcaseMode::Normal;
                // If the query is empty, drop the filter entirely.
                if self
                    .search_query
                    .as_ref()
                    .is_some_and(|q| q.trim().is_empty())
                {
                    self.search_query = None;
                }
                self.bump_render();
            }
            MegaAction::SearchAppendChar(ch) => {
                if self.mode != ShowcaseMode::Search {
                    return;
                }
                let query = self.search_query.get_or_insert_with(String::new);
                query.push(ch);
                self.rebuild_search_matches();
                if let Some(sel) = self.search_matches.first().copied() {
                    self.selected_sample = sel;
                    self.selected_node = None;
                    self.bump_analysis();
                } else {
                    self.bump_render();
                }
            }
            MegaAction::SearchBackspace => {
                if self.mode != ShowcaseMode::Search {
                    return;
                }
                if let Some(query) = self.search_query.as_mut() {
                    let _ = query.pop();
                }
                self.rebuild_search_matches();
                if let Some(sel) = self.search_matches.first().copied() {
                    self.selected_sample = sel;
                    self.selected_node = None;
                    self.bump_analysis();
                } else {
                    self.bump_render();
                }
            }
            MegaAction::SearchAccept => {
                if self.mode != ShowcaseMode::Search {
                    return;
                }
                self.mode = ShowcaseMode::Normal;
                if let Some(sel) = self.search_matches.get(self.search_match_idx).copied() {
                    self.selected_sample = sel;
                    self.selected_node = None;
                    self.bump_analysis();
                } else {
                    self.bump_render();
                }
                // Drop empty queries to avoid "active but empty" filter state.
                if self
                    .search_query
                    .as_ref()
                    .is_some_and(|q| q.trim().is_empty())
                {
                    self.search_query = None;
                    self.search_matches.clear();
                    self.search_match_idx = 0;
                }
            }
            MegaAction::ClearFilter => {
                if self.mode == ShowcaseMode::Search {
                    self.search_query = Some(String::new());
                } else {
                    self.search_query = None;
                }
                self.search_matches.clear();
                self.search_match_idx = 0;
                self.bump_render();
            }
            MegaAction::NextSearchMatch => {
                if self.mode != ShowcaseMode::Search {
                    return;
                }
                if self.search_matches.is_empty() {
                    return;
                }
                self.search_match_idx = (self.search_match_idx + 1) % self.search_matches.len();
                self.selected_sample = self.search_matches[self.search_match_idx];
                self.selected_node = None;
                self.bump_analysis();
            }
            MegaAction::PrevSearchMatch => {
                if self.mode != ShowcaseMode::Search {
                    return;
                }
                if self.search_matches.is_empty() {
                    return;
                }
                self.search_match_idx = (self.search_match_idx + self.search_matches.len() - 1)
                    % self.search_matches.len();
                self.selected_sample = self.search_matches[self.search_match_idx];
                self.selected_node = None;
                self.bump_analysis();
            }
            MegaAction::ToggleDiagnostics => {
                self.panels.diagnostics = !self.panels.diagnostics;
            }
            MegaAction::ToggleDebugNodeBounds => {
                self.debug_overlays.node_bounds = !self.debug_overlays.node_bounds;
            }
            MegaAction::ToggleDebugEdgeRoutes => {
                self.debug_overlays.edge_routes = !self.debug_overlays.edge_routes;
            }
            MegaAction::ToggleDebugClusterBounds => {
                self.debug_overlays.cluster_bounds = !self.debug_overlays.cluster_bounds;
            }
            MegaAction::ToggleDebugGrid => {
                self.debug_overlays.alignment_grid = !self.debug_overlays.alignment_grid;
            }
            MegaAction::CollapsePanels => {
                if self.mode == ShowcaseMode::Inspect {
                    self.selected_node = None;
                    self.mode = ShowcaseMode::Normal;
                    self.bump_render();
                } else if self.mode == ShowcaseMode::Search {
                    self.search_query = None;
                    self.search_matches.clear();
                    self.search_match_idx = 0;
                    self.mode = ShowcaseMode::Normal;
                    self.bump_render();
                } else {
                    self.panels.controls = false;
                    self.panels.metrics = false;
                    self.panels.detail = false;
                    self.panels.status_log = false;
                    self.panels.help_overlay = false;
                    self.panels.diagnostics = false;
                }
            }
            MegaAction::ToggleComparison => {
                self.comparison_enabled = !self.comparison_enabled;
                self.bump_render();
            }
            MegaAction::CycleComparisonLayout => {
                self.comparison_layout_mode = match self.comparison_layout_mode {
                    LayoutMode::Dense => LayoutMode::Normal,
                    LayoutMode::Normal => LayoutMode::Spacious,
                    LayoutMode::Spacious => LayoutMode::Auto,
                    LayoutMode::Auto => LayoutMode::Dense,
                };
                self.bump_render();
            }
        }
        // Log every action for debugging.
        self.log_action("action", format!("{action:?}"));
    }
}

// ── Screen ──────────────────────────────────────────────────────────

/// Mermaid Mega Showcase — the over-the-top interactive diagram lab.
pub struct MermaidMegaShowcaseScreen {
    state: MermaidMegaState,
    cache: RefCell<MegaRenderCache>,
    /// Separate cache for the comparison (right-side) layout.
    comparison_cache: RefCell<MegaRenderCache>,
}

impl Default for MermaidMegaShowcaseScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl MermaidMegaShowcaseScreen {
    /// Create a new mega showcase screen.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: MermaidMegaState::default(),
            cache: RefCell::new(MegaRenderCache::empty()),
            comparison_cache: RefCell::new(MegaRenderCache::empty()),
        }
    }

    /// Map a key event to an action.
    fn handle_key(&self, event: &ftui_core::event::KeyEvent) -> Option<MegaAction> {
        use ftui_core::event::{KeyCode, KeyEventKind, Modifiers};

        if event.kind != KeyEventKind::Press {
            return None;
        }

        // Ctrl+U clears the current filter.
        if event.modifiers.contains(Modifiers::CTRL)
            && let KeyCode::Char('u') | KeyCode::Char('U') = event.code
        {
            return Some(MegaAction::ClearFilter);
        }

        if self.state.mode == ShowcaseMode::Search {
            return match event.code {
                KeyCode::Escape => Some(MegaAction::CollapsePanels),
                KeyCode::Enter => Some(MegaAction::SearchAccept),
                KeyCode::Backspace => Some(MegaAction::SearchBackspace),
                KeyCode::Up => Some(MegaAction::PrevSearchMatch),
                KeyCode::Down => Some(MegaAction::NextSearchMatch),
                KeyCode::Char(ch)
                    if !event.modifiers.contains(Modifiers::CTRL)
                        && !event.modifiers.contains(Modifiers::ALT) =>
                {
                    Some(MegaAction::SearchAppendChar(ch))
                }
                _ => None,
            };
        }

        match event.code {
            // Sample navigation
            KeyCode::Down | KeyCode::Char('j') => Some(MegaAction::NextSample),
            KeyCode::Up | KeyCode::Char('k') => Some(MegaAction::PrevSample),
            // Generator controls (parametric sample)
            KeyCode::Char('U') => Some(MegaAction::IncreaseGenNodes),
            KeyCode::Char('u') => Some(MegaAction::DecreaseGenNodes),
            KeyCode::Char('N') => Some(MegaAction::IncreaseGenBranching),
            KeyCode::Char('n') => Some(MegaAction::DecreaseGenBranching),
            KeyCode::Char('X') => Some(MegaAction::IncreaseGenDensity),
            KeyCode::Char('x') => Some(MegaAction::DecreaseGenDensity),
            KeyCode::Char('Y') => Some(MegaAction::IncreaseGenLabelLen),
            KeyCode::Char('y') => Some(MegaAction::DecreaseGenLabelLen),
            KeyCode::Char('R') => Some(MegaAction::NextGenSeed),
            // Render config
            KeyCode::Char('t') => Some(MegaAction::CycleTier),
            KeyCode::Char('g') => Some(MegaAction::ToggleGlyphMode),
            KeyCode::Char('b') => Some(MegaAction::CycleRenderMode),
            KeyCode::Char('s') => Some(MegaAction::ToggleStyles),
            KeyCode::Char('w') => Some(MegaAction::CycleWrapMode),
            KeyCode::Char('l') => Some(MegaAction::CycleLayoutMode),
            KeyCode::Char('r') => Some(MegaAction::ForceRelayout),
            // Theme
            KeyCode::Char('p') => Some(MegaAction::CyclePalette),
            KeyCode::Char('P') => Some(MegaAction::PrevPalette),
            // Viewport zoom
            KeyCode::Char('+') | KeyCode::Char('=') => Some(MegaAction::ZoomIn),
            KeyCode::Char('-') => Some(MegaAction::ZoomOut),
            KeyCode::Char('0') => Some(MegaAction::ZoomReset),
            KeyCode::Char('f') => Some(MegaAction::FitToView),
            // Viewport pan (Shift+H/J/K/L)
            KeyCode::Char('H') => Some(MegaAction::PanLeft),
            KeyCode::Char('J') => Some(MegaAction::PanDown),
            KeyCode::Char('K') => Some(MegaAction::PanUp),
            KeyCode::Char('L') => Some(MegaAction::PanRight),
            // Viewport size override
            KeyCode::Char(']') => Some(MegaAction::IncreaseViewportWidth),
            KeyCode::Char('[') => Some(MegaAction::DecreaseViewportWidth),
            KeyCode::Char('}') => Some(MegaAction::IncreaseViewportHeight),
            KeyCode::Char('{') => Some(MegaAction::DecreaseViewportHeight),
            KeyCode::Char('o') => Some(MegaAction::ResetViewportOverride),
            // Panels
            KeyCode::Char('m') => Some(MegaAction::ToggleMetrics),
            KeyCode::Char('c') => Some(MegaAction::ToggleControls),
            KeyCode::Char('d') => Some(MegaAction::ToggleDetail),
            KeyCode::Char('i') => Some(MegaAction::ToggleStatusLog),
            KeyCode::Char('e') => Some(MegaAction::ToggleDiagnostics),
            KeyCode::Char('?') => Some(MegaAction::ToggleHelp),
            // Node inspection
            KeyCode::Tab => Some(MegaAction::SelectNextNode),
            KeyCode::BackTab => Some(MegaAction::SelectPrevNode),
            // Comparison mode
            KeyCode::Char('v') => Some(MegaAction::ToggleComparison),
            KeyCode::Char('V') => Some(MegaAction::CycleComparisonLayout),
            // Search
            KeyCode::Char('/') => Some(MegaAction::EnterSearch),
            // Escape is context-dependent
            KeyCode::Escape => Some(MegaAction::CollapsePanels),
            _ => None,
        }
    }

    // ── Render pipeline ────────────────────────────────────────────

    /// Compute the target viewport size, respecting overrides.
    fn target_viewport_size(&self, inner: Rect) -> (u16, u16) {
        if let Some((cols, rows)) = self.state.viewport_size_override {
            (cols.max(1), rows.max(1))
        } else {
            (inner.width.max(1), inner.height.max(1))
        }
    }

    /// Ensure the render cache is up-to-date for the given diagram area.
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
        let mut ran_analysis = false;
        let mut ran_layout = false;
        let mut ran_render = false;
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

        // Debounce: when the previous layout was expensive (exceeded the budget)
        // and was computed very recently, defer this pass to coalesce rapid
        // input and prevent CPU spikes on complex diagrams.
        if (analysis_needed || layout_needed)
            && cache.layout.is_some()
            && cache.layout_budget_exceeded
            && cache
                .last_layout_instant
                .is_some_and(|last| last.elapsed().as_millis() < LAYOUT_DEBOUNCE_MS)
        {
            cache.debounce_skips = cache.debounce_skips.saturating_add(1);
            cache.cache_hits = cache.cache_hits.saturating_add(1);
            cache.last_cache_hit = true;
            return;
        }

        cache.cache_misses = cache.cache_misses.saturating_add(1);
        cache.last_cache_hit = false;

        let source = match self.state.selected_source() {
            Some(s) => s,
            None => {
                cache.analysis_epoch = self.state.analysis_epoch;
                cache.layout_epoch = self.state.layout_epoch;
                cache.render_epoch = self.state.render_epoch;
                cache.ir = None;
                cache.layout = None;
                cache.errors.clear();
                cache.parse_ms = None;
                cache.layout_ms = None;
                cache.render_ms = None;
                return;
            }
        };

        let config = self.state.to_config();
        let matrix = MermaidCompatibilityMatrix::default();
        let policy = MermaidFallbackPolicy::default();

        if analysis_needed {
            ran_analysis = true;
            let parse_start = Instant::now();
            let parsed = mermaid::parse_with_diagnostics(source);
            cache.parse_ms = Some(parse_start.elapsed().as_secs_f32() * 1000.0);

            let ir_parse = mermaid::normalize_ast_to_ir(&parsed.ast, &config, &matrix, &policy);
            let mut errors = Vec::new();
            errors.extend(parsed.errors);
            errors.extend(ir_parse.errors);
            cache.errors = errors;
            cache.ir = Some(ir_parse.ir);
            cache.analysis_epoch = self.state.analysis_epoch;
            layout_needed = true;
            render_needed = true;
        }

        if layout_needed {
            if let Some(ir) = cache.ir.as_ref() {
                let spacing = self.state.layout_spacing();
                let layout_start = Instant::now();
                let layout = mermaid_layout::layout_diagram_with_spacing(ir, &config, &spacing);
                let elapsed_ms = layout_start.elapsed().as_secs_f32() * 1000.0;
                cache.layout_ms = Some(elapsed_ms);
                cache.layout = Some(layout);
                ran_layout = true;

                // Record timestamp for debounce and check budget.
                cache.last_layout_instant = Some(Instant::now());
                if elapsed_ms > LAYOUT_BUDGET_MS {
                    cache.layout_budget_exceeded = true;
                    cache.budget_warning_logged = false;
                } else {
                    cache.layout_budget_exceeded = false;
                }
            }
            cache.layout_epoch = self.state.layout_epoch;
            render_needed = true;
        }

        if render_needed {
            if let (Some(ir), Some(layout)) = (cache.ir.as_ref(), cache.layout.as_ref()) {
                let mut buffer = Buffer::new(render_width, render_height);
                let area = Rect::new(0, 0, render_width, render_height);
                let render_start = Instant::now();
                let _plan =
                    mermaid_render::render_diagram_adaptive(layout, ir, &config, area, &mut buffer);
                cache.render_ms = Some(render_start.elapsed().as_secs_f32() * 1000.0);
                cache.buffer = buffer;
                ran_render = true;
            }
            cache.viewport = (width, height);
            cache.zoom = zoom;
            cache.render_epoch = self.state.render_epoch;
        }

        if ran_analysis || ran_layout || ran_render {
            let flags = MegaRecomputeFlags {
                analysis_ran: ran_analysis,
                layout_ran: ran_layout,
                render_ran: ran_render,
            };
            self.emit_recompute_jsonl(
                &cache,
                flags,
                (width, height),
                (render_width, render_height),
            );
        }
    }

    fn emit_recompute_jsonl(
        &self,
        cache: &MegaRenderCache,
        flags: MegaRecomputeFlags,
        viewport: (u16, u16),
        render_size: (u16, u16),
    ) {
        if !jsonl_enabled() {
            return;
        }
        let seq = MEGA_JSONL_SEQ.fetch_add(1, Ordering::Relaxed);
        let run_id = determinism::demo_run_id();
        let seed = determinism::demo_seed(0);
        let screen_mode = determinism::demo_screen_mode();
        let line = self.recompute_jsonl_line(
            cache,
            seq,
            run_id.as_deref(),
            seed,
            &screen_mode,
            flags,
            viewport,
            render_size,
        );
        let _ = writeln!(std::io::stderr(), "{line}");
    }

    #[allow(clippy::too_many_arguments)]
    fn recompute_jsonl_line(
        &self,
        cache: &MegaRenderCache,
        seq: u64,
        run_id: Option<&str>,
        seed: u64,
        screen_mode: &str,
        flags: MegaRecomputeFlags,
        viewport: (u16, u16),
        render_size: (u16, u16),
    ) -> String {
        let timestamp = determinism::chrono_like_timestamp();
        let sample = self.state.selected_name();
        let diagram_type = cache
            .ir
            .as_ref()
            .map(|ir| ir.diagram_type.as_str())
            .unwrap_or("unknown");
        let node_count = cache.ir.as_ref().map(|ir| ir.nodes.len() as u64);
        let edge_count = cache.ir.as_ref().map(|ir| ir.edges.len() as u64);
        let layout_stats = cache.layout.as_ref().map(|layout| &layout.stats);

        let mut json = String::new();
        json.push('{');
        json.push_str(&format!("\"schema_version\":\"{}\"", MEGA_TELEMETRY_SCHEMA));
        json.push_str(&format!(",\"event\":\"{}\"", MEGA_JSONL_EVENT));
        json.push_str(&format!(",\"seq\":{seq}"));
        json.push_str(&format!(",\"timestamp\":\"{}\"", escape_json(&timestamp)));
        match run_id {
            Some(id) => json.push_str(&format!(",\"run_id\":\"{}\"", escape_json(id))),
            None => json.push_str(",\"run_id\":null"),
        }
        json.push_str(&format!(",\"seed\":{seed}"));
        json.push_str(&format!(
            ",\"screen_mode\":\"{}\"",
            escape_json(screen_mode)
        ));
        json.push_str(&format!(",\"sample\":\"{}\"", escape_json(sample)));
        json.push_str(&format!(",\"diagram_type\":\"{diagram_type}\""));
        json.push_str(&format!(
            ",\"layout_mode\":\"{}\"",
            self.state.layout_mode.as_str()
        ));
        json.push_str(&format!(
            ",\"tier\":\"{}\"",
            escape_json(&self.state.tier.to_string())
        ));
        json.push_str(&format!(
            ",\"glyph_mode\":\"{}\"",
            escape_json(&self.state.glyph_mode.to_string())
        ));
        json.push_str(&format!(
            ",\"wrap_mode\":\"{}\"",
            escape_json(&self.state.wrap_mode.to_string())
        ));
        json.push_str(&format!(
            ",\"render_mode\":\"{}\"",
            escape_json(&self.state.render_mode.to_string())
        ));
        json.push_str(&format!(
            ",\"palette\":\"{}\"",
            escape_json(&self.state.palette.to_string())
        ));
        json.push_str(&format!(
            ",\"styles_enabled\":{}",
            self.state.styles_enabled
        ));
        json.push_str(&format!(
            ",\"comparison_enabled\":{}",
            self.state.comparison_enabled
        ));
        json.push_str(&format!(
            ",\"comparison_layout_mode\":\"{}\"",
            self.state.comparison_layout_mode.as_str()
        ));
        json.push_str(&format!(",\"viewport_cols\":{}", viewport.0));
        json.push_str(&format!(",\"viewport_rows\":{}", viewport.1));
        json.push_str(&format!(",\"render_cols\":{}", render_size.0));
        json.push_str(&format!(",\"render_rows\":{}", render_size.1));
        json.push_str(&format!(",\"zoom\":{:.3}", self.state.viewport_zoom));
        json.push_str(&format!(",\"pan_x\":{}", self.state.viewport_pan.0));
        json.push_str(&format!(",\"pan_y\":{}", self.state.viewport_pan.1));
        json.push_str(&format!(
            ",\"analysis_epoch\":{}",
            self.state.analysis_epoch
        ));
        json.push_str(&format!(",\"layout_epoch\":{}", self.state.layout_epoch));
        json.push_str(&format!(",\"render_epoch\":{}", self.state.render_epoch));
        json.push_str(&format!(",\"analysis_ran\":{}", flags.analysis_ran));
        json.push_str(&format!(",\"layout_ran\":{}", flags.layout_ran));
        json.push_str(&format!(",\"render_ran\":{}", flags.render_ran));
        json.push_str(&format!(",\"cache_hits\":{}", cache.cache_hits));
        json.push_str(&format!(",\"cache_misses\":{}", cache.cache_misses));
        json.push_str(&format!(",\"cache_hit\":{}", cache.last_cache_hit));
        json.push_str(&format!(",\"debounce_skips\":{}", cache.debounce_skips));
        json.push_str(&format!(
            ",\"layout_budget_exceeded\":{}",
            cache.layout_budget_exceeded
        ));
        push_opt_f32(&mut json, "parse_ms", cache.parse_ms);
        push_opt_f32(&mut json, "layout_ms", cache.layout_ms);
        push_opt_f32(&mut json, "render_ms", cache.render_ms);
        push_opt_u64(&mut json, "node_count", node_count);
        push_opt_u64(&mut json, "edge_count", edge_count);
        push_opt_u64(&mut json, "error_count", Some(cache.errors.len() as u64));

        if let Some(stats) = layout_stats {
            push_opt_u64(
                &mut json,
                "layout_iterations",
                Some(stats.iterations_used as u64),
            );
            push_opt_u64(
                &mut json,
                "layout_iterations_max",
                Some(stats.max_iterations as u64),
            );
            json.push_str(&format!(
                ",\"layout_budget_exceeded_layout\":{}",
                stats.budget_exceeded
            ));
            push_opt_u64(&mut json, "layout_crossings", Some(stats.crossings as u64));
            push_opt_u64(&mut json, "layout_ranks", Some(stats.ranks as u64));
            push_opt_u64(
                &mut json,
                "layout_max_rank_width",
                Some(stats.max_rank_width as u64),
            );
            push_opt_u64(
                &mut json,
                "layout_total_bends",
                Some(stats.total_bends as u64),
            );
            push_opt_f64(
                &mut json,
                "layout_position_variance",
                Some(stats.position_variance),
            );
        } else {
            push_opt_u64(&mut json, "layout_iterations", None);
            push_opt_u64(&mut json, "layout_iterations_max", None);
            json.push_str(",\"layout_budget_exceeded_layout\":null");
            push_opt_u64(&mut json, "layout_crossings", None);
            push_opt_u64(&mut json, "layout_ranks", None);
            push_opt_u64(&mut json, "layout_max_rank_width", None);
            push_opt_u64(&mut json, "layout_total_bends", None);
            push_opt_f64(&mut json, "layout_position_variance", None);
        }
        json.push('}');
        json
    }

    /// Ensure the comparison cache is up-to-date for side-by-side mode.
    ///
    /// Shares the parsed IR from the main cache but applies an independent
    /// layout using `comparison_layout_mode`.
    fn ensure_comparison_cache(&self, inner: Rect) {
        let (width, height) = self.target_viewport_size(inner);
        let zoom = self.state.viewport_zoom;
        let render_width = (f32::from(width) * zoom)
            .round()
            .clamp(1.0, f32::from(u16::MAX)) as u16;
        let render_height = (f32::from(height) * zoom)
            .round()
            .clamp(1.0, f32::from(u16::MAX)) as u16;

        let main_cache = self.cache.borrow();
        let main_ir = main_cache.ir.clone();
        let main_analysis_epoch = main_cache.analysis_epoch;
        drop(main_cache);

        let mut cache = self.comparison_cache.borrow_mut();
        let zoom_matches = (cache.zoom - zoom).abs() <= f32::EPSILON;
        let analysis_changed = cache.analysis_epoch != main_analysis_epoch;
        let layout_changed = cache.layout_epoch != self.state.layout_epoch;
        let render_changed = cache.render_epoch != self.state.render_epoch
            || cache.viewport != (width, height)
            || !zoom_matches;

        if !analysis_changed && !layout_changed && !render_changed && cache.layout.is_some() {
            cache.cache_hits = cache.cache_hits.saturating_add(1);
            cache.last_cache_hit = true;
            return;
        }

        cache.cache_misses = cache.cache_misses.saturating_add(1);
        cache.last_cache_hit = false;

        let ir = match main_ir {
            Some(ir) => ir,
            None => return,
        };

        let config = self.state.to_config();
        let spacing = match self.state.comparison_layout_mode {
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
            LayoutMode::Normal | LayoutMode::Auto => mermaid_layout::LayoutSpacing::default(),
        };

        if analysis_changed || layout_changed || cache.layout.is_none() {
            let layout = mermaid_layout::layout_diagram_with_spacing(&ir, &config, &spacing);
            cache.layout_ms = Some(0.0);
            cache.layout = Some(layout);
            cache.analysis_epoch = main_analysis_epoch;
            cache.layout_epoch = self.state.layout_epoch;
        }

        if let Some(layout) = cache.layout.as_ref() {
            let mut buffer = Buffer::new(render_width, render_height);
            let area = Rect::new(0, 0, render_width, render_height);
            let _plan =
                mermaid_render::render_diagram_adaptive(layout, &ir, &config, area, &mut buffer);
            cache.buffer = buffer;
        }
        cache.ir = Some(ir);
        cache.viewport = (width, height);
        cache.zoom = zoom;
        cache.render_epoch = self.state.render_epoch;
    }

    /// Blit a cached buffer onto the frame with centering and pan offset.
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

    // ── Panel renderers ────────────────────────────────────────────

    /// Render the controls strip at the top.
    fn render_controls(&self, area: Rect, frame: &mut Frame) {
        if area.is_empty() {
            return;
        }
        let border = Cell::from_char(' ').with_fg(PackedRgba::rgb(80, 80, 100));
        frame.draw_border(area, BorderChars::SQUARE, border);

        let s = &self.state;
        let viewport_info = if let Some((cols, rows)) = s.viewport_size_override {
            format!("VP:{cols}x{rows}")
        } else {
            "VP:auto".to_string()
        };
        let pan_info = if s.viewport_pan != (0, 0) {
            format!(" Pan:{},{}", s.viewport_pan.0, s.viewport_pan.1)
        } else {
            String::new()
        };
        let err_count = self.cache.borrow().errors.len();
        let err_indicator = if err_count > 0 {
            format!(" ERR:{err_count}")
        } else {
            String::new()
        };
        let gen_info = format!(
            " Gen:n{} b{} d{}% l{} seed{}",
            s.generator.nodes,
            s.generator.branching,
            s.generator.density,
            s.generator.label_len,
            s.generator.seed
        );
        let filter_info = if let Some(q) = s.search_query.as_deref() {
            let q = q.trim();
            if q.is_empty() {
                " Filter:/".to_string()
            } else if !s.search_matches.is_empty() {
                format!(
                    " Filter:/{q} ({}/{})",
                    s.search_match_idx.saturating_add(1),
                    s.search_matches.len()
                )
            } else {
                format!(" Filter:/{q} (0)")
            }
        } else {
            String::new()
        };
        let status = format!(
            " Tier:{} Glyph:{} Render:{} Wrap:{} Layout:{} Palette:{} Zoom:{:.0}% {}{}{}{}{} ",
            s.tier,
            s.glyph_mode,
            s.render_mode,
            s.wrap_mode,
            s.layout_mode.as_str(),
            s.palette,
            (s.viewport_zoom * 100.0),
            viewport_info,
            pan_info,
            err_indicator,
            filter_info,
            gen_info,
        );
        let text_cell = Cell::from_char(' ').with_fg(PackedRgba::rgb(180, 200, 220));
        frame.print_text_clipped(
            area.x + 1,
            area.y + 1,
            &status,
            text_cell,
            area.x + area.width - 1,
        );
    }

    /// Render the side panel (metrics / detail) with cache stats.
    fn render_side_panel(&self, area: Rect, frame: &mut Frame) {
        if area.is_empty() {
            return;
        }
        let border = Cell::from_char(' ').with_fg(PackedRgba::rgb(80, 80, 100));
        frame.draw_border(area, BorderChars::SQUARE, border);

        let title = if self.state.panels.detail {
            " Detail "
        } else {
            " Metrics "
        };
        let title_cell = Cell::from_char(' ').with_fg(PackedRgba::rgb(140, 180, 220));
        frame.print_text_clipped(
            area.x + 1,
            area.y,
            title,
            title_cell,
            area.x + area.width - 1,
        );

        let cache = self.cache.borrow();
        let mut lines: Vec<String> = Vec::new();

        lines.push(format!("Mode: {}", self.state.mode.as_str()));
        lines.push(format!("Sample: {}", self.state.selected_name()));
        if let Some(q) = self.state.search_query.as_deref() {
            let q = q.trim();
            if q.is_empty() {
                lines.push("Filter: /".to_string());
            } else {
                lines.push(format!("Filter: /{q}"));
            }
            lines.push(format!(
                "Matches: {}/{}",
                self.state
                    .search_match_idx
                    .saturating_add(1)
                    .min(self.state.search_matches.len()),
                self.state.search_matches.len()
            ));
        }
        if self.state.selected_is_generated() {
            lines.push(format!(
                "Gen: n{} b{} d{}% l{} seed{}",
                self.state.generator.nodes,
                self.state.generator.branching,
                self.state.generator.density,
                self.state.generator.label_len,
                self.state.generator.seed
            ));
        }
        lines.push(format!("Palette: {}", self.state.palette));
        lines.push(format!(
            "Node: {}",
            self.state
                .selected_node
                .map_or("-".to_string(), |n| format!("#{n}"))
        ));
        lines.push(format!(
            "Epoch: a{}/l{}/r{}",
            self.state.analysis_epoch, self.state.layout_epoch, self.state.render_epoch
        ));
        if let Some((cols, rows)) = self.state.viewport_size_override {
            lines.push(format!("VP: {cols}x{rows} (override)"));
        } else {
            lines.push("VP: auto".to_string());
        }
        if self.state.viewport_pan != (0, 0) {
            lines.push(format!(
                "Pan: {},{}",
                self.state.viewport_pan.0, self.state.viewport_pan.1
            ));
        }
        lines.push(String::new());

        // Cache performance metrics
        lines.push(format!(
            "Parse: {}",
            cache
                .parse_ms
                .map_or("-".to_string(), |ms| format!("{ms:.1}ms"))
        ));
        lines.push(format!(
            "Layout: {}",
            cache
                .layout_ms
                .map_or("-".to_string(), |ms| format!("{ms:.1}ms"))
        ));
        lines.push(format!(
            "Render: {}",
            cache
                .render_ms
                .map_or("-".to_string(), |ms| format!("{ms:.1}ms"))
        ));
        lines.push(format!(
            "Cache: {}/{}",
            cache.cache_hits, cache.cache_misses
        ));
        lines.push(format!(
            "Last: {}",
            if cache.last_cache_hit { "HIT" } else { "MISS" }
        ));

        if cache.debounce_skips > 0 {
            lines.push(format!("Debounce: {}", cache.debounce_skips));
        }
        if cache.layout_budget_exceeded {
            lines.push(format!("Budget: OVER ({LAYOUT_BUDGET_MS:.0}ms)"));
        }
        if !cache.errors.is_empty() {
            lines.push(format!("Errors: {} (e=diag)", cache.errors.len()));
        }

        let info_cell = Cell::from_char(' ').with_fg(PackedRgba::rgb(160, 160, 180));
        let max_x = area.x + area.width - 1;
        for (row, line) in lines.iter().enumerate() {
            let y = area.y + 2 + row as u16;
            if y >= area.y + area.height - 1 {
                break;
            }
            frame.print_text_clipped(area.x + 1, y, line, info_cell, max_x);
        }
    }

    /// Render the footer with mode indicator and key hints.
    fn render_footer(&self, area: Rect, frame: &mut Frame) {
        if area.is_empty() {
            return;
        }
        let mode_str = match self.state.mode {
            ShowcaseMode::Normal => "NORMAL",
            ShowcaseMode::Inspect => "INSPECT",
            ShowcaseMode::Search => "SEARCH",
        };
        let mode_color = match self.state.mode {
            ShowcaseMode::Normal => PackedRgba::rgb(80, 200, 120),
            ShowcaseMode::Inspect => PackedRgba::rgb(80, 180, 255),
            ShowcaseMode::Search => PackedRgba::rgb(255, 200, 80),
        };
        let mode_cell = Cell::from_char(' ').with_fg(mode_color);
        let end =
            frame.print_text_clipped(area.x, area.y, mode_str, mode_cell, area.x + area.width);

        let hints = " j/k:sample u/U:nodes x/X:density t:tier p:palette H/J/K/L:pan []/{}:size o:reset /:filter Ctrl+U:clear ?:help";
        let hint_cell = Cell::from_char(' ').with_fg(PackedRgba::rgb(100, 100, 120));
        frame.print_text_clipped(end + 1, area.y, hints, hint_cell, area.x + area.width);
    }

    /// Render the help/legend overlay centered on the given area.
    fn render_help_overlay(&self, area: Rect, frame: &mut Frame) {
        if area.is_empty() {
            return;
        }

        // Build help content: category → [(key, description)]
        let sections: &[(&str, &[(&str, &str)])] = &[
            (
                "Navigation",
                &[
                    ("j / Down", "Next sample"),
                    ("k / Up", "Previous sample"),
                    ("Tab", "Select next node"),
                    ("S-Tab", "Select previous node"),
                    ("/", "Sample filter/search"),
                    ("Enter", "Accept match (search)"),
                    ("Up/Down", "Cycle matches (search)"),
                    ("Ctrl+U", "Clear filter"),
                ],
            ),
            (
                "Render Config",
                &[
                    ("t", "Cycle tier (Auto/Compact/Normal/Rich)"),
                    ("g", "Toggle glyph mode (Unicode/ASCII)"),
                    ("b", "Cycle render mode (Auto/Cell/Braille/...)"),
                    ("s", "Toggle styles (classDef/style)"),
                    ("w", "Cycle wrap mode (None/Word/Char/WordChar)"),
                    ("l", "Cycle layout mode (Dense/Normal/Spacious/Auto)"),
                    ("r", "Force relayout"),
                    ("p / P", "Cycle palette forward / backward"),
                ],
            ),
            (
                "Viewport",
                &[
                    ("+ / =", "Zoom in"),
                    ("-", "Zoom out"),
                    ("0", "Reset zoom (+ clear pan)"),
                    ("f", "Fit to view (+ clear pan)"),
                    ("H / J / K / L", "Pan left / down / up / right"),
                    ("] / [", "Increase / decrease viewport width"),
                    ("} / {", "Increase / decrease viewport height"),
                    ("o", "Reset viewport override"),
                ],
            ),
            (
                "Panels",
                &[
                    ("m", "Toggle metrics panel"),
                    ("c", "Toggle controls strip"),
                    ("d", "Toggle detail panel"),
                    ("i", "Toggle status log"),
                    ("e", "Toggle diagnostics (errors)"),
                    ("?", "Toggle this help overlay"),
                    ("Esc", "Deselect node / exit search / collapse"),
                ],
            ),
        ];

        // Compute overlay dimensions.
        let content_width: u16 = 50;
        let mut content_lines: u16 = 2; // title + blank
        for (name, entries) in sections {
            content_lines += 1; // section header
            content_lines += entries.len() as u16;
            content_lines += 1; // blank separator
            let _ = name;
        }
        let overlay_w = (content_width + 4).min(area.width);
        let overlay_h = (content_lines + 3).min(area.height);

        // Center the overlay.
        let ox = area.x + area.width.saturating_sub(overlay_w) / 2;
        let oy = area.y + area.height.saturating_sub(overlay_h) / 2;
        let overlay = Rect::new(ox, oy, overlay_w, overlay_h);

        // Clear the overlay area with a dark background.
        let bg = Cell::from_char(' ').with_fg(PackedRgba::rgb(40, 40, 60));
        for row in overlay.y..overlay.y + overlay.height {
            for col in overlay.x..overlay.x + overlay.width {
                frame.buffer.set(col, row, bg);
            }
        }

        // Draw border.
        let border_cell = Cell::from_char(' ').with_fg(PackedRgba::rgb(100, 140, 200));
        frame.draw_border(overlay, BorderChars::ROUNDED, border_cell);

        // Title.
        let title = " Help (? to close) ";
        let title_cell = Cell::from_char(' ').with_fg(PackedRgba::rgb(200, 220, 255));
        frame.print_text_clipped(
            overlay.x + 2,
            overlay.y,
            title,
            title_cell,
            overlay.x + overlay.width - 1,
        );

        let max_x = overlay.x + overlay.width - 2;
        let mut y = overlay.y + 2;
        let max_y = overlay.y + overlay.height - 1;

        let section_cell = Cell::from_char(' ').with_fg(PackedRgba::rgb(140, 200, 255));
        let key_cell = Cell::from_char(' ').with_fg(PackedRgba::rgb(255, 220, 140));
        let desc_cell = Cell::from_char(' ').with_fg(PackedRgba::rgb(180, 180, 200));

        for (section_name, entries) in sections {
            if y >= max_y {
                break;
            }
            // Section header.
            frame.print_text_clipped(overlay.x + 2, y, section_name, section_cell, max_x);
            y += 1;

            for (k, desc) in *entries {
                if y >= max_y {
                    break;
                }
                let end = frame.print_text_clipped(overlay.x + 3, y, k, key_cell, max_x);
                // Pad key column to fixed width for alignment.
                let desc_x = (overlay.x + 19).max(end + 1);
                frame.print_text_clipped(desc_x, y, desc, desc_cell, max_x);
                y += 1;
            }
            y += 1; // blank line between sections
        }

        // Current state legend at bottom if space allows.
        if y + 2 < max_y {
            let s = &self.state;
            let legend = format!(
                "Mode:{} Tier:{} Palette:{} Zoom:{:.0}%",
                s.mode.as_str(),
                s.tier,
                s.palette,
                s.viewport_zoom * 100.0,
            );
            let legend_cell = Cell::from_char(' ').with_fg(PackedRgba::rgb(120, 160, 140));
            frame.print_text_clipped(overlay.x + 2, y, &legend, legend_cell, max_x);
        }
    }

    /// Render the diagram area using the full Mermaid pipeline with caching.
    fn render_diagram(&self, area: Rect, frame: &mut Frame) {
        if area.is_empty() {
            return;
        }

        if self.state.comparison_enabled && area.width >= 20 {
            self.render_diagram_comparison(area, frame);
            return;
        }

        let border = Cell::from_char(' ').with_fg(PackedRgba::rgb(60, 60, 80));
        frame.draw_border(area, BorderChars::SQUARE, border);

        let palette = DiagramPalette::from_preset(self.state.palette);
        let title = format!(" {} [{}] ", self.state.selected_name(), self.state.palette);
        let title_cell = Cell::from_char(' ').with_fg(palette.node_border);
        frame.print_text_clipped(
            area.x + 1,
            area.y,
            &title,
            title_cell,
            area.x + area.width - 1,
        );

        // Inner area (inside the border).
        let inner = Rect::new(
            area.x + 1,
            area.y + 1,
            area.width.saturating_sub(2),
            area.height.saturating_sub(2),
        );
        if inner.is_empty() {
            return;
        }

        // Run the pipeline and blit.
        self.ensure_render_cache(inner);
        let cache = self.cache.borrow();

        if self.state.panels.diagnostics && !cache.errors.is_empty() {
            // When diagnostics panel is active, render a dedicated error display.
            drop(cache);
            self.render_diagnostics_view(inner, frame);
        } else if !cache.errors.is_empty() {
            // Errors exist but diagnostics panel is off: blit diagram then overlay.
            let has_content = cache.ir.as_ref().is_some_and(|ir| {
                !ir.nodes.is_empty()
                    || !ir.edges.is_empty()
                    || !ir.labels.is_empty()
                    || !ir.clusters.is_empty()
            });
            // Blit the (possibly partial) diagram first.
            self.blit_buffer(frame, inner, &cache.buffer, self.state.viewport_pan);
            // Grab error data while the borrow is still live.
            let errors: Vec<_> = cache.errors.clone();
            let source = self.state.selected_source().unwrap_or("").to_string();
            drop(cache);
            let config = self.state.to_config();
            // Build a scratch buffer the size of the inner area and render the
            // error panel/overlay into it, then blit onto the frame.
            let mut err_buf = Buffer::new(inner.width, inner.height);
            let err_area = Rect::new(0, 0, inner.width, inner.height);
            if has_content {
                mermaid_render::render_mermaid_error_overlay(
                    &errors,
                    &source,
                    &config,
                    err_area,
                    &mut err_buf,
                );
            } else {
                mermaid_render::render_mermaid_error_panel(
                    &errors,
                    &source,
                    &config,
                    err_area,
                    &mut err_buf,
                );
            }
            // Overlay non-empty cells from the error buffer onto the frame.
            self.overlay_buffer(frame, inner, &err_buf);
        } else {
            self.blit_buffer(frame, inner, &cache.buffer, self.state.viewport_pan);
        }
    }

    /// Render comparison mode: two diagrams side-by-side with different layout modes.
    fn render_diagram_comparison(&self, area: Rect, frame: &mut Frame) {
        // Split the area into left (primary) and right (comparison) halves.
        let half_w = area.width / 2;
        let left_area = Rect::new(area.x, area.y, half_w, area.height);
        let right_area = Rect::new(
            area.x + half_w,
            area.y,
            area.width.saturating_sub(half_w),
            area.height,
        );

        let border = Cell::from_char(' ').with_fg(PackedRgba::rgb(60, 60, 80));
        let palette = DiagramPalette::from_preset(self.state.palette);
        let sample_name = self.state.selected_name();

        // Draw left pane (primary layout mode).
        frame.draw_border(left_area, BorderChars::SQUARE, border);
        let left_title = format!(" {} [{}] ", sample_name, self.state.layout_mode.as_str());
        let title_cell = Cell::from_char(' ').with_fg(palette.node_border);
        frame.print_text_clipped(
            left_area.x + 1,
            left_area.y,
            &left_title,
            title_cell,
            left_area.x + left_area.width - 1,
        );

        let left_inner = Rect::new(
            left_area.x + 1,
            left_area.y + 1,
            left_area.width.saturating_sub(2),
            left_area.height.saturating_sub(2),
        );
        if !left_inner.is_empty() {
            self.ensure_render_cache(left_inner);
            let cache = self.cache.borrow();
            self.blit_buffer(frame, left_inner, &cache.buffer, self.state.viewport_pan);
        }

        // Draw right pane (comparison layout mode).
        frame.draw_border(right_area, BorderChars::SQUARE, border);
        let right_title = format!(
            " {} [{}] ",
            sample_name,
            self.state.comparison_layout_mode.as_str()
        );
        let cmp_cell = Cell::from_char(' ').with_fg(PackedRgba::rgb(200, 160, 100));
        frame.print_text_clipped(
            right_area.x + 1,
            right_area.y,
            &right_title,
            cmp_cell,
            right_area.x + right_area.width - 1,
        );

        let right_inner = Rect::new(
            right_area.x + 1,
            right_area.y + 1,
            right_area.width.saturating_sub(2),
            right_area.height.saturating_sub(2),
        );
        if !right_inner.is_empty() {
            self.ensure_comparison_cache(right_inner);
            let cache = self.comparison_cache.borrow();
            self.blit_buffer(frame, right_inner, &cache.buffer, self.state.viewport_pan);
        }
    }

    /// Overlay non-empty cells from a buffer onto the frame (for error overlays).
    fn overlay_buffer(&self, frame: &mut Frame, area: Rect, buf: &Buffer) {
        let empty = Cell::default();
        for row in 0..buf.height().min(area.height) {
            for col in 0..buf.width().min(area.width) {
                if let Some(cell) = buf.get(col, row)
                    && !cell.bits_eq(&empty)
                {
                    frame.buffer.set(area.x + col, area.y + row, *cell);
                }
            }
        }
    }

    /// Render a dedicated diagnostics view showing detailed error information.
    fn render_diagnostics_view(&self, area: Rect, frame: &mut Frame) {
        if area.is_empty() {
            return;
        }

        let cache = self.cache.borrow();
        let errors = &cache.errors;

        // Dark background for diagnostics.
        let bg = Cell::from_char(' ').with_fg(PackedRgba::rgb(40, 30, 30));
        for row in area.y..area.y + area.height {
            for col in area.x..area.x + area.width {
                frame.buffer.set(col, row, bg);
            }
        }

        let max_x = area.x + area.width.saturating_sub(1);
        let mut y = area.y;

        // Title
        let title = format!(
            " Diagnostics ({} error{}) ",
            errors.len(),
            if errors.len() == 1 { "" } else { "s" }
        );
        let title_cell = Cell::from_char(' ').with_fg(PackedRgba::rgb(255, 100, 100));
        frame.print_text_clipped(area.x, y, &title, title_cell, max_x);
        y += 1;

        // Separator
        let sep_cell = Cell::from_char(' ').with_fg(PackedRgba::rgb(80, 40, 40));
        let sep: String = "─".repeat(area.width as usize);
        frame.print_text_clipped(area.x, y, &sep, sep_cell, max_x);
        y += 1;

        let err_cell = Cell::from_char(' ').with_fg(PackedRgba::rgb(255, 160, 120));
        let loc_cell = Cell::from_char(' ').with_fg(PackedRgba::rgb(180, 180, 200));
        let hint_cell = Cell::from_char(' ').with_fg(PackedRgba::rgb(140, 200, 140));

        for (i, error) in errors.iter().enumerate() {
            if y >= area.y + area.height.saturating_sub(1) {
                // Show truncation indicator.
                let remaining = errors.len() - i;
                let trunc = format!(
                    "... and {} more error{}",
                    remaining,
                    if remaining == 1 { "" } else { "s" }
                );
                frame.print_text_clipped(area.x + 1, y, &trunc, loc_cell, max_x);
                break;
            }

            // Error number and message
            let msg = format!("{}. {}", i + 1, error.message);
            frame.print_text_clipped(area.x + 1, y, &msg, err_cell, max_x);
            y += 1;

            if y >= area.y + area.height.saturating_sub(1) {
                break;
            }

            // Location info
            let loc = format!(
                "   line {}, col {}",
                error.span.start.line, error.span.start.col
            );
            frame.print_text_clipped(area.x + 1, y, &loc, loc_cell, max_x);
            y += 1;

            // Expected tokens (if any)
            if let Some(expected) = &error.expected
                && y < area.y + area.height.saturating_sub(1)
                && !expected.is_empty()
            {
                let exp = format!("   expected: {}", expected.join(", "));
                frame.print_text_clipped(area.x + 1, y, &exp, hint_cell, max_x);
                y += 1;
            }

            // Blank separator between errors
            if y < area.y + area.height.saturating_sub(1) {
                y += 1;
            }
        }

        // Footer hint at bottom
        let footer_y = area.y + area.height.saturating_sub(1);
        let hint = " Press 'e' to close diagnostics ";
        let footer_cell = Cell::from_char(' ').with_fg(PackedRgba::rgb(120, 120, 140));
        frame.print_text_clipped(area.x, footer_y, hint, footer_cell, max_x);
    }
}

impl Screen for MermaidMegaShowcaseScreen {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if let Event::Key(key) = event
            && let Some(action) = self.handle_key(key)
        {
            self.state.apply(action);
        }
        Cmd::None
    }

    fn tick(&mut self, _tick_count: u64) {
        let cache = self.cache.borrow();
        if cache.layout_budget_exceeded && !cache.budget_warning_logged {
            let ms = cache.layout_ms.unwrap_or(0.0);
            let sample = self.state.selected_name();
            let mode = self.state.layout_mode.as_str();
            drop(cache);
            self.state.log_action(
                "budget",
                format!("layout {ms:.1}ms > {LAYOUT_BUDGET_MS:.0}ms budget ({sample}, {mode})"),
            );
            self.cache.borrow_mut().budget_warning_logged = true;
        }

        // Log parse/IR errors to status log once (when errors first appear).
        let cache = self.cache.borrow();
        let err_count = cache.errors.len();
        if err_count > 0 {
            // Check if last status log entry already covers this error set.
            let already_logged = self.state.status_log.last().is_some_and(|e| {
                e.action == "errors" && e.detail.contains(&format!("{err_count} error"))
            });
            if !already_logged {
                let sample = self.state.selected_name().to_string();
                let first_msg = cache.errors[0].message.clone();
                drop(cache);
                self.state.log_action(
                    "errors",
                    format!("{err_count} error(s) in {sample}: {first_msg}"),
                );
            }
        }
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        let regions = LayoutRegions::compute(area, &self.state.panels);

        // Render in layer order: background panels first, then diagram, then overlay.
        if !regions.controls.is_empty() {
            self.render_controls(regions.controls, frame);
        }
        if !regions.side_panel.is_empty() {
            self.render_side_panel(regions.side_panel, frame);
        }
        self.render_diagram(regions.diagram, frame);
        self.render_footer(regions.footer, frame);

        // Help overlay renders last (on top of everything).
        if self.state.panels.help_overlay {
            self.render_help_overlay(area, frame);
        }
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "j/↓",
                action: "Next sample",
            },
            HelpEntry {
                key: "k/↑",
                action: "Previous sample",
            },
            HelpEntry {
                key: "u/U",
                action: "Gen nodes -/+",
            },
            HelpEntry {
                key: "n/N",
                action: "Gen branch -/+",
            },
            HelpEntry {
                key: "x/X",
                action: "Gen density -/+",
            },
            HelpEntry {
                key: "y/Y",
                action: "Gen label -/+",
            },
            HelpEntry {
                key: "R",
                action: "Gen reseed",
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
                key: "l",
                action: "Cycle layout mode",
            },
            HelpEntry {
                key: "r",
                action: "Force relayout",
            },
            HelpEntry {
                key: "p/P",
                action: "Cycle palette",
            },
            HelpEntry {
                key: "Tab",
                action: "Select next node",
            },
            HelpEntry {
                key: "S-Tab",
                action: "Select previous node",
            },
            HelpEntry {
                key: "/",
                action: "Search",
            },
            HelpEntry {
                key: "+/-",
                action: "Zoom in/out",
            },
            HelpEntry {
                key: "0",
                action: "Reset zoom",
            },
            HelpEntry {
                key: "f",
                action: "Fit to view",
            },
            HelpEntry {
                key: "H/J/K/L",
                action: "Pan viewport",
            },
            HelpEntry {
                key: "]/[",
                action: "Viewport width +/-",
            },
            HelpEntry {
                key: "}/{",
                action: "Viewport height +/-",
            },
            HelpEntry {
                key: "o",
                action: "Reset viewport override",
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
                key: "d",
                action: "Toggle detail",
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
                key: "e",
                action: "Toggle diagnostics",
            },
            HelpEntry {
                key: "v",
                action: "Toggle comparison",
            },
            HelpEntry {
                key: "V",
                action: "Cycle comparison layout",
            },
            HelpEntry {
                key: "Tab",
                action: "Select next node",
            },
            HelpEntry {
                key: "/",
                action: "Enter search",
            },
            HelpEntry {
                key: "Esc",
                action: "Deselect / collapse",
            },
        ]
    }

    fn title(&self) -> &'static str {
        "Mermaid Mega Showcase"
    }

    fn tab_label(&self) -> &'static str {
        "MermaidMega"
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_regions_full_size() {
        let area = Rect::new(0, 0, 120, 40);
        let panels = PanelVisibility::default();
        let regions = LayoutRegions::compute(area, &panels);

        assert!(regions.diagram.width > 0);
        assert!(regions.diagram.height > 0);
        assert!(regions.footer.height > 0);
        assert!(regions.controls.height > 0);
        assert!(
            regions.side_panel.width > 0,
            "metrics panel should be visible at 120 cols"
        );
    }

    #[test]
    fn layout_regions_narrow_collapses_side() {
        let area = Rect::new(0, 0, 80, 24);
        let panels = PanelVisibility::default();
        let regions = LayoutRegions::compute(area, &panels);

        assert_eq!(
            regions.side_panel.width, 0,
            "side panel should collapse at 80 cols"
        );
        assert!(regions.diagram.width > 60);
    }

    #[test]
    fn layout_regions_tiny_gives_all_to_diagram() {
        let area = Rect::new(0, 0, 8, 4);
        let panels = PanelVisibility::default();
        let regions = LayoutRegions::compute(area, &panels);

        assert_eq!(regions.diagram, area);
    }

    #[test]
    fn layout_regions_no_panels() {
        let area = Rect::new(0, 0, 120, 40);
        let panels = PanelVisibility {
            controls: false,
            metrics: false,
            detail: false,
            status_log: false,
            help_overlay: false,
            diagnostics: false,
        };
        let regions = LayoutRegions::compute(area, &panels);

        assert_eq!(regions.controls.height, 0);
        assert_eq!(regions.side_panel.width, 0);
        assert!(regions.diagram.height > 30);
    }

    #[test]
    fn state_default_is_normal_mode() {
        let state = MermaidMegaState::default();
        assert_eq!(state.mode, ShowcaseMode::Normal);
        assert_eq!(state.palette, DiagramPalettePreset::Default);
        assert_eq!(state.selected_node, None);
    }

    #[test]
    fn state_apply_cycle_palette() {
        let mut state = MermaidMegaState::default();
        let epoch_before = state.render_epoch;
        state.apply(MegaAction::CyclePalette);
        assert_eq!(state.palette, DiagramPalettePreset::Corporate);
        assert!(state.render_epoch > epoch_before);
    }

    #[test]
    fn state_apply_select_node_enters_inspect() {
        let mut state = MermaidMegaState::default();
        state.apply(MegaAction::SelectNextNode);
        assert_eq!(state.mode, ShowcaseMode::Inspect);
        assert_eq!(state.selected_node, Some(0));
    }

    #[test]
    fn state_apply_deselect_returns_normal() {
        let mut state = MermaidMegaState::default();
        state.apply(MegaAction::SelectNextNode);
        state.apply(MegaAction::DeselectNode);
        assert_eq!(state.mode, ShowcaseMode::Normal);
        assert_eq!(state.selected_node, None);
    }

    #[test]
    fn state_apply_enter_search_mode() {
        let mut state = MermaidMegaState::default();
        state.apply(MegaAction::EnterSearch);
        assert_eq!(state.mode, ShowcaseMode::Search);
        assert!(state.search_query.is_some());
    }

    #[test]
    fn state_apply_escape_from_search() {
        let mut state = MermaidMegaState::default();
        state.apply(MegaAction::EnterSearch);
        state.apply(MegaAction::CollapsePanels);
        assert_eq!(state.mode, ShowcaseMode::Normal);
        assert!(state.search_query.is_none());
        assert!(state.search_matches.is_empty());
    }

    #[test]
    fn state_search_append_char_builds_matches() {
        let mut state = MermaidMegaState::default();
        state.apply(MegaAction::EnterSearch);
        for ch in ['f', 'l', 'o', 'w'] {
            state.apply(MegaAction::SearchAppendChar(ch));
        }
        assert_eq!(state.search_query.as_deref(), Some("flow"));
        assert!(
            !state.search_matches.is_empty(),
            "expected flow search to match at least one sample"
        );
        let first = state.search_matches[0];
        assert!(
            MEGA_SAMPLES[first]
                .name
                .to_ascii_lowercase()
                .contains("flow"),
            "expected first match name to include 'flow'"
        );
    }

    #[test]
    fn state_search_accept_keeps_filter_and_selects_match() {
        let mut state = MermaidMegaState::default();
        state.apply(MegaAction::EnterSearch);
        for ch in ['g', 'a', 'n', 't', 't'] {
            state.apply(MegaAction::SearchAppendChar(ch));
        }
        assert!(
            !state.search_matches.is_empty(),
            "expected gantt search to match samples"
        );
        state.apply(MegaAction::SearchAccept);
        assert_eq!(state.mode, ShowcaseMode::Normal);
        assert!(
            state.search_query.is_some(),
            "filter should remain active after accept"
        );
        assert!(
            state.selected_name().to_ascii_lowercase().contains("gantt")
                || mega_sample_category(
                    MEGA_SAMPLES
                        .get(state.selected_sample % MEGA_SAMPLES.len())
                        .expect("sample")
                )
                .contains("gantt"),
            "expected selected sample to be a gantt match"
        );
    }

    #[test]
    fn state_clear_filter_drops_query_and_matches() {
        let mut state = MermaidMegaState::default();
        state.apply(MegaAction::EnterSearch);
        state.apply(MegaAction::SearchAppendChar('f'));
        assert!(state.search_query.is_some());
        assert!(!state.search_matches.is_empty());
        state.apply(MegaAction::ClearFilter);
        assert!(
            state.search_query.is_some(),
            "in search mode, clear keeps empty input"
        );
        assert!(state.search_matches.is_empty());
        // Exit search: empty input should drop filter.
        state.apply(MegaAction::ExitSearch);
        assert!(state.search_query.is_none());
    }

    #[test]
    fn state_to_config_applies_palette() {
        let state = MermaidMegaState {
            palette: DiagramPalettePreset::Neon,
            ..MermaidMegaState::default()
        };
        let config = state.to_config();
        assert_eq!(config.palette, DiagramPalettePreset::Neon);
    }

    #[test]
    fn screen_new_does_not_panic() {
        let _screen = MermaidMegaShowcaseScreen::new();
    }

    #[test]
    fn layout_mode_cycles_through_all() {
        let mut mode = LayoutMode::Dense;
        let start = mode;
        for _ in 0..4 {
            mode = mode.next();
        }
        assert_eq!(mode, start);
    }

    #[test]
    fn layout_regions_deterministic() {
        let area = Rect::new(0, 0, 120, 40);
        let panels = PanelVisibility::default();
        let r1 = LayoutRegions::compute(area, &panels);
        let r2 = LayoutRegions::compute(area, &panels);
        assert_eq!(r1.diagram, r2.diagram);
        assert_eq!(r1.footer, r2.footer);
        assert_eq!(r1.controls, r2.controls);
        assert_eq!(r1.side_panel, r2.side_panel);
    }

    #[test]
    fn status_log_records_actions() {
        let mut state = MermaidMegaState::default();
        assert!(state.status_log.is_empty());
        state.apply(MegaAction::CycleTier);
        assert_eq!(state.status_log.len(), 1);
        assert_eq!(state.status_log[0].action, "action");
        assert!(state.status_log[0].detail.contains("CycleTier"));
    }

    #[test]
    fn status_log_caps_at_limit() {
        let mut state = MermaidMegaState::default();
        for _ in 0..STATUS_LOG_CAP + 10 {
            state.apply(MegaAction::CycleTier);
        }
        assert_eq!(state.status_log.len(), STATUS_LOG_CAP);
    }

    #[test]
    fn mega_samples_non_empty() {
        assert!(!MEGA_SAMPLES.is_empty());
        for sample in MEGA_SAMPLES {
            assert!(!sample.name.is_empty());
            assert!(!sample.source.is_empty());
        }
    }

    #[test]
    fn selected_source_wraps_around() {
        let state = MermaidMegaState {
            selected_sample: MEGA_SAMPLES.len() + 2,
            ..MermaidMegaState::default()
        };
        assert!(state.selected_source().is_some());
        assert_eq!(state.selected_name(), MEGA_SAMPLES[2].name);
    }

    #[test]
    fn render_cache_starts_empty() {
        let cache = MegaRenderCache::empty();
        assert!(cache.ir.is_none());
        assert!(cache.layout.is_none());
        assert_eq!(cache.cache_hits, 0);
        assert_eq!(cache.cache_misses, 0);
        assert!(cache.parse_ms.is_none());
    }

    #[test]
    fn ensure_cache_populates_on_first_call() {
        let screen = MermaidMegaShowcaseScreen::new();
        let area = Rect::new(0, 0, 80, 24);
        screen.ensure_render_cache(area);

        let cache = screen.cache.borrow();
        assert!(
            cache.ir.is_some(),
            "IR should be populated after first render"
        );
        assert!(
            cache.layout.is_some(),
            "layout should be populated after first render"
        );
        assert!(cache.parse_ms.is_some(), "parse timing should be recorded");
        assert!(
            cache.layout_ms.is_some(),
            "layout timing should be recorded"
        );
        assert!(
            cache.render_ms.is_some(),
            "render timing should be recorded"
        );
        assert_eq!(cache.cache_misses, 1);
        assert_eq!(cache.cache_hits, 0);
    }

    #[test]
    fn ensure_cache_hits_on_repeat() {
        let screen = MermaidMegaShowcaseScreen::new();
        let area = Rect::new(0, 0, 80, 24);
        screen.ensure_render_cache(area);
        screen.ensure_render_cache(area);

        let cache = screen.cache.borrow();
        assert_eq!(cache.cache_misses, 1);
        assert_eq!(cache.cache_hits, 1);
        assert!(cache.last_cache_hit);
    }

    #[test]
    fn config_dense_has_higher_budget() {
        let state = MermaidMegaState {
            layout_mode: LayoutMode::Dense,
            ..MermaidMegaState::default()
        };
        let config = state.to_config();
        assert_eq!(config.layout_iteration_budget, 400);
    }

    #[test]
    fn layout_spacing_dense_tighter() {
        let state = MermaidMegaState {
            layout_mode: LayoutMode::Dense,
            ..MermaidMegaState::default()
        };
        let spacing = state.layout_spacing();
        assert!(spacing.rank_gap < mermaid_layout::LayoutSpacing::default().rank_gap);
    }

    #[test]
    fn pan_left_decreases_x() {
        let mut state = MermaidMegaState::default();
        state.apply(MegaAction::PanLeft);
        assert!(state.viewport_pan.0 < 0);
        assert_eq!(state.viewport_pan.1, 0);
    }

    #[test]
    fn pan_right_increases_x() {
        let mut state = MermaidMegaState::default();
        state.apply(MegaAction::PanRight);
        assert!(state.viewport_pan.0 > 0);
        assert_eq!(state.viewport_pan.1, 0);
    }

    #[test]
    fn pan_up_decreases_y() {
        let mut state = MermaidMegaState::default();
        state.apply(MegaAction::PanUp);
        assert_eq!(state.viewport_pan.0, 0);
        assert!(state.viewport_pan.1 < 0);
    }

    #[test]
    fn pan_down_increases_y() {
        let mut state = MermaidMegaState::default();
        state.apply(MegaAction::PanDown);
        assert_eq!(state.viewport_pan.0, 0);
        assert!(state.viewport_pan.1 > 0);
    }

    #[test]
    fn zoom_reset_clears_pan() {
        let mut state = MermaidMegaState {
            viewport_pan: (10, 20),
            ..MermaidMegaState::default()
        };
        state.apply(MegaAction::ZoomReset);
        assert_eq!(state.viewport_pan, (0, 0));
    }

    #[test]
    fn fit_to_view_clears_pan() {
        let mut state = MermaidMegaState {
            viewport_pan: (5, 5),
            ..MermaidMegaState::default()
        };
        state.apply(MegaAction::FitToView);
        assert_eq!(state.viewport_pan, (0, 0));
    }

    #[test]
    fn viewport_override_increase_sets_default() {
        let mut state = MermaidMegaState::default();
        assert!(state.viewport_size_override.is_none());
        let epoch = state.render_epoch;
        state.apply(MegaAction::IncreaseViewportWidth);
        let expected_cols =
            (VIEWPORT_OVERRIDE_DEFAULT_COLS as i32 + VIEWPORT_OVERRIDE_STEP_COLS as i32) as u16;
        let expected_rows = VIEWPORT_OVERRIDE_DEFAULT_ROWS;
        assert_eq!(
            state.viewport_size_override,
            Some((expected_cols, expected_rows))
        );
        assert!(state.render_epoch > epoch);
    }

    #[test]
    fn viewport_override_reset_clears() {
        let mut state = MermaidMegaState::default();
        state.apply(MegaAction::IncreaseViewportHeight);
        assert!(state.viewport_size_override.is_some());
        let epoch = state.render_epoch;
        state.apply(MegaAction::ResetViewportOverride);
        assert!(state.viewport_size_override.is_none());
        assert!(state.render_epoch > epoch);
    }

    #[test]
    fn viewport_override_min_clamped() {
        let mut state = MermaidMegaState {
            viewport_size_override: Some((VIEWPORT_OVERRIDE_MIN_COLS, VIEWPORT_OVERRIDE_MIN_ROWS)),
            ..MermaidMegaState::default()
        };
        // Try to decrease below minimum — should clamp
        state.adjust_viewport_override(-100, -100);
        let (cols, rows) = state.viewport_size_override.unwrap();
        assert!(cols >= VIEWPORT_OVERRIDE_MIN_COLS);
        assert!(rows >= VIEWPORT_OVERRIDE_MIN_ROWS);
    }

    #[test]
    fn key_shift_h_maps_to_pan_left() {
        use ftui_core::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};
        let screen = MermaidMegaShowcaseScreen::new();
        let event = KeyEvent {
            code: KeyCode::Char('H'),
            modifiers: Modifiers::SHIFT,
            kind: KeyEventKind::Press,
        };
        assert_eq!(screen.handle_key(&event), Some(MegaAction::PanLeft));
    }

    #[test]
    fn key_bracket_maps_to_viewport_width() {
        use ftui_core::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};
        let screen = MermaidMegaShowcaseScreen::new();
        let event = KeyEvent {
            code: KeyCode::Char(']'),
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Press,
        };
        assert_eq!(
            screen.handle_key(&event),
            Some(MegaAction::IncreaseViewportWidth)
        );
    }

    #[test]
    fn key_o_maps_to_reset_viewport() {
        use ftui_core::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};
        let screen = MermaidMegaShowcaseScreen::new();
        let event = KeyEvent {
            code: KeyCode::Char('o'),
            modifiers: Modifiers::NONE,
            kind: KeyEventKind::Press,
        };
        assert_eq!(
            screen.handle_key(&event),
            Some(MegaAction::ResetViewportOverride)
        );
    }

    // ── Exhaustive keymap coverage ───────────────────────────────────

    /// Helper: create a key press event.
    fn key(code: ftui_core::event::KeyCode) -> ftui_core::event::KeyEvent {
        ftui_core::event::KeyEvent {
            code,
            modifiers: ftui_core::event::Modifiers::NONE,
            kind: ftui_core::event::KeyEventKind::Press,
        }
    }

    fn shift_key(ch: char) -> ftui_core::event::KeyEvent {
        ftui_core::event::KeyEvent {
            code: ftui_core::event::KeyCode::Char(ch),
            modifiers: ftui_core::event::Modifiers::SHIFT,
            kind: ftui_core::event::KeyEventKind::Press,
        }
    }

    #[test]
    fn keymap_sample_navigation() {
        use ftui_core::event::KeyCode;
        let s = MermaidMegaShowcaseScreen::new();
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('j'))),
            Some(MegaAction::NextSample)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Down)),
            Some(MegaAction::NextSample)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('k'))),
            Some(MegaAction::PrevSample)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Up)),
            Some(MegaAction::PrevSample)
        );
    }

    #[test]
    fn keymap_generator_controls() {
        use ftui_core::event::KeyCode;
        let s = MermaidMegaShowcaseScreen::new();
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('u'))),
            Some(MegaAction::DecreaseGenNodes)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('U'))),
            Some(MegaAction::IncreaseGenNodes)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('n'))),
            Some(MegaAction::DecreaseGenBranching)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('N'))),
            Some(MegaAction::IncreaseGenBranching)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('x'))),
            Some(MegaAction::DecreaseGenDensity)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('X'))),
            Some(MegaAction::IncreaseGenDensity)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('y'))),
            Some(MegaAction::DecreaseGenLabelLen)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('Y'))),
            Some(MegaAction::IncreaseGenLabelLen)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('R'))),
            Some(MegaAction::NextGenSeed)
        );
    }

    #[test]
    fn keymap_render_config() {
        use ftui_core::event::KeyCode;
        let s = MermaidMegaShowcaseScreen::new();
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('t'))),
            Some(MegaAction::CycleTier)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('g'))),
            Some(MegaAction::ToggleGlyphMode)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('b'))),
            Some(MegaAction::CycleRenderMode)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('s'))),
            Some(MegaAction::ToggleStyles)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('w'))),
            Some(MegaAction::CycleWrapMode)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('l'))),
            Some(MegaAction::CycleLayoutMode)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('r'))),
            Some(MegaAction::ForceRelayout)
        );
    }

    #[test]
    fn keymap_palette() {
        use ftui_core::event::KeyCode;
        let s = MermaidMegaShowcaseScreen::new();
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('p'))),
            Some(MegaAction::CyclePalette)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('P'))),
            Some(MegaAction::PrevPalette)
        );
    }

    #[test]
    fn keymap_zoom() {
        use ftui_core::event::KeyCode;
        let s = MermaidMegaShowcaseScreen::new();
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('+'))),
            Some(MegaAction::ZoomIn)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('='))),
            Some(MegaAction::ZoomIn)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('-'))),
            Some(MegaAction::ZoomOut)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('0'))),
            Some(MegaAction::ZoomReset)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('f'))),
            Some(MegaAction::FitToView)
        );
    }

    #[test]
    fn keymap_pan_shift_hjkl() {
        use ftui_core::event::KeyCode;
        let s = MermaidMegaShowcaseScreen::new();
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('H'))),
            Some(MegaAction::PanLeft)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('J'))),
            Some(MegaAction::PanDown)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('K'))),
            Some(MegaAction::PanUp)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('L'))),
            Some(MegaAction::PanRight)
        );
    }

    #[test]
    fn keymap_viewport_size() {
        use ftui_core::event::KeyCode;
        let s = MermaidMegaShowcaseScreen::new();
        assert_eq!(
            s.handle_key(&key(KeyCode::Char(']'))),
            Some(MegaAction::IncreaseViewportWidth)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('['))),
            Some(MegaAction::DecreaseViewportWidth)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('}'))),
            Some(MegaAction::IncreaseViewportHeight)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('{'))),
            Some(MegaAction::DecreaseViewportHeight)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('o'))),
            Some(MegaAction::ResetViewportOverride)
        );
    }

    #[test]
    fn keymap_panels() {
        use ftui_core::event::KeyCode;
        let s = MermaidMegaShowcaseScreen::new();
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('m'))),
            Some(MegaAction::ToggleMetrics)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('c'))),
            Some(MegaAction::ToggleControls)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('d'))),
            Some(MegaAction::ToggleDetail)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('i'))),
            Some(MegaAction::ToggleStatusLog)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('?'))),
            Some(MegaAction::ToggleHelp)
        );
    }

    #[test]
    fn keymap_node_search_escape() {
        use ftui_core::event::KeyCode;
        let s = MermaidMegaShowcaseScreen::new();
        assert_eq!(
            s.handle_key(&key(KeyCode::Tab)),
            Some(MegaAction::SelectNextNode)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::BackTab)),
            Some(MegaAction::SelectPrevNode)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Char('/'))),
            Some(MegaAction::EnterSearch)
        );
        assert_eq!(
            s.handle_key(&key(KeyCode::Escape)),
            Some(MegaAction::CollapsePanels)
        );
    }

    #[test]
    fn keymap_unbound_returns_none() {
        use ftui_core::event::KeyCode;
        let s = MermaidMegaShowcaseScreen::new();
        assert_eq!(s.handle_key(&key(KeyCode::Char('q'))), None);
        assert_eq!(s.handle_key(&key(KeyCode::Char('z'))), None);
        assert_eq!(s.handle_key(&key(KeyCode::F(1))), None);
        assert_eq!(s.handle_key(&key(KeyCode::Enter)), None);
    }

    // ── State transition: tier cycling ───────────────────────────────

    #[test]
    fn cycle_tier_full_loop() {
        let mut state = MermaidMegaState::default();
        assert_eq!(state.tier, MermaidTier::Auto);
        state.apply(MegaAction::CycleTier);
        assert_eq!(state.tier, MermaidTier::Compact);
        state.apply(MegaAction::CycleTier);
        assert_eq!(state.tier, MermaidTier::Normal);
        state.apply(MegaAction::CycleTier);
        assert_eq!(state.tier, MermaidTier::Rich);
        state.apply(MegaAction::CycleTier);
        assert_eq!(state.tier, MermaidTier::Auto);
    }

    #[test]
    fn cycle_tier_bumps_layout_and_render_epochs() {
        let mut state = MermaidMegaState::default();
        let le = state.layout_epoch;
        let re = state.render_epoch;
        state.apply(MegaAction::CycleTier);
        assert!(state.layout_epoch > le);
        assert!(state.render_epoch > re);
    }

    // ── State transition: glyph mode ─────────────────────────────────

    #[test]
    fn toggle_glyph_mode_oscillates() {
        let mut state = MermaidMegaState::default();
        assert_eq!(state.glyph_mode, MermaidGlyphMode::Unicode);
        state.apply(MegaAction::ToggleGlyphMode);
        assert_eq!(state.glyph_mode, MermaidGlyphMode::Ascii);
        state.apply(MegaAction::ToggleGlyphMode);
        assert_eq!(state.glyph_mode, MermaidGlyphMode::Unicode);
    }

    #[test]
    fn toggle_glyph_bumps_render_not_layout() {
        let mut state = MermaidMegaState::default();
        let le = state.layout_epoch;
        let re = state.render_epoch;
        state.apply(MegaAction::ToggleGlyphMode);
        assert_eq!(state.layout_epoch, le, "glyph should not bump layout");
        assert!(state.render_epoch > re);
    }

    // ── State transition: render mode cycling ────────────────────────

    #[test]
    fn cycle_render_mode_full_loop() {
        let mut state = MermaidMegaState::default();
        assert_eq!(state.render_mode, MermaidRenderMode::Braille);
        state.apply(MegaAction::CycleRenderMode);
        assert_eq!(state.render_mode, MermaidRenderMode::Block);
        state.apply(MegaAction::CycleRenderMode);
        assert_eq!(state.render_mode, MermaidRenderMode::HalfBlock);
        state.apply(MegaAction::CycleRenderMode);
        assert_eq!(state.render_mode, MermaidRenderMode::Auto);
        state.apply(MegaAction::CycleRenderMode);
        assert_eq!(state.render_mode, MermaidRenderMode::CellOnly);
        state.apply(MegaAction::CycleRenderMode);
        assert_eq!(state.render_mode, MermaidRenderMode::Braille);
    }

    // ── State transition: wrap mode cycling ──────────────────────────

    #[test]
    fn cycle_wrap_mode_full_loop() {
        let mut state = MermaidMegaState::default();
        assert_eq!(state.wrap_mode, MermaidWrapMode::WordChar);
        // WordChar → None → Word → Char → WordChar
        state.apply(MegaAction::CycleWrapMode);
        state.apply(MegaAction::CycleWrapMode);
        state.apply(MegaAction::CycleWrapMode);
        state.apply(MegaAction::CycleWrapMode);
        assert_eq!(state.wrap_mode, MermaidWrapMode::WordChar);
    }

    #[test]
    fn cycle_wrap_bumps_layout() {
        let mut state = MermaidMegaState::default();
        let le = state.layout_epoch;
        state.apply(MegaAction::CycleWrapMode);
        assert!(state.layout_epoch > le);
    }

    // ── State transition: styles toggle ──────────────────────────────

    #[test]
    fn toggle_styles_flips() {
        let mut state = MermaidMegaState::default();
        assert!(state.styles_enabled);
        state.apply(MegaAction::ToggleStyles);
        assert!(!state.styles_enabled);
        state.apply(MegaAction::ToggleStyles);
        assert!(state.styles_enabled);
    }

    #[test]
    fn toggle_styles_bumps_render_not_layout() {
        let mut state = MermaidMegaState::default();
        let le = state.layout_epoch;
        state.apply(MegaAction::ToggleStyles);
        assert_eq!(state.layout_epoch, le);
    }

    // ── State transition: palette ────────────────────────────────────

    #[test]
    fn cycle_palette_changes_preset() {
        let mut state = MermaidMegaState::default();
        let orig = state.palette;
        state.apply(MegaAction::CyclePalette);
        assert_ne!(state.palette, orig);
    }

    #[test]
    fn prev_palette_reverses_cycle() {
        let mut state = MermaidMegaState::default();
        state.apply(MegaAction::CyclePalette);
        let after_next = state.palette;
        state.apply(MegaAction::PrevPalette);
        assert_eq!(state.palette, DiagramPalettePreset::Default);
        let _ = after_next; // suppress unused
    }

    // ── State transition: zoom ───────────────────────────────────────

    #[test]
    fn zoom_in_increases() {
        let mut state = MermaidMegaState::default();
        state.apply(MegaAction::ZoomIn);
        assert!(state.viewport_zoom > 1.0);
    }

    #[test]
    fn zoom_out_decreases() {
        let mut state = MermaidMegaState::default();
        state.apply(MegaAction::ZoomOut);
        assert!(state.viewport_zoom < 1.0);
    }

    #[test]
    fn zoom_in_clamped_at_max() {
        let mut state = MermaidMegaState::default();
        for _ in 0..50 {
            state.apply(MegaAction::ZoomIn);
        }
        assert!(state.viewport_zoom <= 4.0);
    }

    #[test]
    fn zoom_out_clamped_at_min() {
        let mut state = MermaidMegaState::default();
        for _ in 0..50 {
            state.apply(MegaAction::ZoomOut);
        }
        assert!(state.viewport_zoom >= 0.25);
    }

    // ── State transition: force relayout ─────────────────────────────

    #[test]
    fn force_relayout_bumps_layout_epoch() {
        let mut state = MermaidMegaState::default();
        let le = state.layout_epoch;
        state.apply(MegaAction::ForceRelayout);
        assert!(state.layout_epoch > le);
    }

    // ── State transition: sample navigation ──────────────────────────

    #[test]
    fn next_sample_increments_and_bumps_analysis() {
        let mut state = MermaidMegaState::default();
        let ae = state.analysis_epoch;
        state.apply(MegaAction::NextSample);
        assert_eq!(state.selected_sample, 1);
        assert!(state.analysis_epoch > ae);
    }

    #[test]
    fn prev_sample_decrements_and_bumps_analysis() {
        let mut state = MermaidMegaState {
            selected_sample: 3,
            ..MermaidMegaState::default()
        };
        let ae = state.analysis_epoch;
        state.apply(MegaAction::PrevSample);
        assert_eq!(state.selected_sample, 2);
        assert!(state.analysis_epoch > ae);
    }

    #[test]
    fn sample_navigation_clears_selected_node() {
        let mut state = MermaidMegaState {
            selected_node: Some(3),
            ..MermaidMegaState::default()
        };
        state.apply(MegaAction::NextSample);
        assert_eq!(state.selected_node, None);
    }

    #[test]
    fn next_sample_wraps_via_modulo() {
        let mut state = MermaidMegaState {
            selected_sample: MEGA_SAMPLES.len() - 1,
            ..MermaidMegaState::default()
        };
        state.apply(MegaAction::NextSample);
        // wrapping_add — the selected_source() handles modulo
        assert_eq!(state.selected_name(), MEGA_SAMPLES[0].name);
    }

    // ── State transition: node selection ──────────────────────────────

    #[test]
    fn select_next_node_from_none_starts_at_zero() {
        let mut state = MermaidMegaState::default();
        assert_eq!(state.selected_node, None);
        state.apply(MegaAction::SelectNextNode);
        assert_eq!(state.selected_node, Some(0));
        assert_eq!(state.mode, ShowcaseMode::Inspect);
    }

    #[test]
    fn select_next_node_increments() {
        let mut state = MermaidMegaState {
            selected_node: Some(2),
            ..MermaidMegaState::default()
        };
        state.apply(MegaAction::SelectNextNode);
        assert_eq!(state.selected_node, Some(3));
    }

    #[test]
    fn select_prev_node_from_none_starts_at_zero() {
        let mut state = MermaidMegaState::default();
        state.apply(MegaAction::SelectPrevNode);
        assert_eq!(state.selected_node, Some(0));
        assert_eq!(state.mode, ShowcaseMode::Inspect);
    }

    #[test]
    fn select_prev_node_saturates_at_zero() {
        let mut state = MermaidMegaState {
            selected_node: Some(0),
            ..MermaidMegaState::default()
        };
        state.apply(MegaAction::SelectPrevNode);
        assert_eq!(state.selected_node, Some(0));
    }

    #[test]
    fn deselect_node_clears_and_returns_normal() {
        let mut state = MermaidMegaState {
            selected_node: Some(5),
            mode: ShowcaseMode::Inspect,
            ..MermaidMegaState::default()
        };
        state.apply(MegaAction::DeselectNode);
        assert_eq!(state.selected_node, None);
        assert_eq!(state.mode, ShowcaseMode::Normal);
    }

    // ── State transition: search mode ────────────────────────────────

    #[test]
    fn enter_search_sets_mode_and_query() {
        let mut state = MermaidMegaState::default();
        state.apply(MegaAction::EnterSearch);
        assert_eq!(state.mode, ShowcaseMode::Search);
        assert_eq!(state.search_query, Some(String::new()));
    }

    #[test]
    fn exit_search_clears_and_returns_normal() {
        let mut state = MermaidMegaState::default();
        state.apply(MegaAction::EnterSearch);
        state.apply(MegaAction::ExitSearch);
        assert_eq!(state.mode, ShowcaseMode::Normal);
        assert_eq!(state.search_query, None);
    }

    // ── State transition: panel toggles ──────────────────────────────

    #[test]
    fn toggle_metrics_panel() {
        let mut state = MermaidMegaState::default();
        let before = state.panels.metrics;
        state.apply(MegaAction::ToggleMetrics);
        assert_ne!(state.panels.metrics, before);
        state.apply(MegaAction::ToggleMetrics);
        assert_eq!(state.panels.metrics, before);
    }

    #[test]
    fn toggle_controls_panel() {
        let mut state = MermaidMegaState::default();
        let before = state.panels.controls;
        state.apply(MegaAction::ToggleControls);
        assert_ne!(state.panels.controls, before);
    }

    #[test]
    fn toggle_detail_panel() {
        let mut state = MermaidMegaState::default();
        assert!(!state.panels.detail);
        state.apply(MegaAction::ToggleDetail);
        assert!(state.panels.detail);
    }

    #[test]
    fn toggle_status_log_panel() {
        let mut state = MermaidMegaState::default();
        assert!(!state.panels.status_log);
        state.apply(MegaAction::ToggleStatusLog);
        assert!(state.panels.status_log);
    }

    #[test]
    fn toggle_help_overlay() {
        let mut state = MermaidMegaState::default();
        assert!(!state.panels.help_overlay);
        state.apply(MegaAction::ToggleHelp);
        assert!(state.panels.help_overlay);
    }

    // ── State transition: collapse panels (context-dependent) ────────

    #[test]
    fn collapse_from_inspect_deselects_node() {
        let mut state = MermaidMegaState {
            mode: ShowcaseMode::Inspect,
            selected_node: Some(3),
            ..MermaidMegaState::default()
        };
        state.apply(MegaAction::CollapsePanels);
        assert_eq!(state.mode, ShowcaseMode::Normal);
        assert_eq!(state.selected_node, None);
    }

    #[test]
    fn collapse_from_search_clears_query() {
        let mut state = MermaidMegaState {
            mode: ShowcaseMode::Search,
            search_query: Some("test".into()),
            ..MermaidMegaState::default()
        };
        state.apply(MegaAction::CollapsePanels);
        assert_eq!(state.mode, ShowcaseMode::Normal);
        assert_eq!(state.search_query, None);
    }

    #[test]
    fn collapse_from_normal_hides_all_panels() {
        let mut state = MermaidMegaState::default();
        state.panels.controls = true;
        state.panels.metrics = true;
        state.panels.detail = true;
        state.panels.status_log = true;
        state.panels.help_overlay = true;
        state.apply(MegaAction::CollapsePanels);
        assert!(!state.panels.controls);
        assert!(!state.panels.metrics);
        assert!(!state.panels.detail);
        assert!(!state.panels.status_log);
        assert!(!state.panels.help_overlay);
    }

    // ── Epoch cascade tests ──────────────────────────────────────────

    #[test]
    fn bump_analysis_cascades_to_layout_and_render() {
        let mut state = MermaidMegaState::default();
        let ae = state.analysis_epoch;
        let le = state.layout_epoch;
        let re = state.render_epoch;
        state.apply(MegaAction::NextSample); // bumps analysis
        assert!(state.analysis_epoch > ae);
        assert!(state.layout_epoch > le);
        assert!(state.render_epoch > re);
    }

    #[test]
    fn bump_layout_cascades_to_render_not_analysis() {
        let mut state = MermaidMegaState::default();
        let ae = state.analysis_epoch;
        let le = state.layout_epoch;
        let re = state.render_epoch;
        state.apply(MegaAction::CycleLayoutMode); // bumps layout
        assert_eq!(state.analysis_epoch, ae);
        assert!(state.layout_epoch > le);
        assert!(state.render_epoch > re);
    }

    #[test]
    fn bump_render_only_touches_render() {
        let mut state = MermaidMegaState::default();
        let ae = state.analysis_epoch;
        let le = state.layout_epoch;
        let re = state.render_epoch;
        state.apply(MegaAction::ToggleGlyphMode); // bumps render only
        assert_eq!(state.analysis_epoch, ae);
        assert_eq!(state.layout_epoch, le);
        assert!(state.render_epoch > re);
    }

    // ── Sample registry tests ────────────────────────────────────────

    #[test]
    fn mega_samples_all_unique_names() {
        let names: Vec<&str> = MEGA_SAMPLES.iter().map(|s| s.name).collect();
        for (i, name) in names.iter().enumerate() {
            for (j, other) in names.iter().enumerate() {
                if i != j {
                    assert_ne!(name, other, "duplicate sample name at indices {i} and {j}");
                }
            }
        }
    }

    #[test]
    fn mega_samples_cover_expected_diagram_types() {
        let names: Vec<&str> = MEGA_SAMPLES.iter().map(|s| s.name).collect();
        // Each of these keywords should appear in at least one sample name
        for keyword in &[
            "Flow", "Sequence", "Class", "State", "ER", "Pie", "Gantt", "Mindmap",
        ] {
            assert!(
                names.iter().any(|n| n.contains(keyword)),
                "no sample name contains {keyword:?}"
            );
        }
    }

    #[test]
    fn selected_source_at_zero_is_first() {
        let state = MermaidMegaState::default();
        assert_eq!(state.selected_source(), Some(MEGA_SAMPLES[0].source));
        assert_eq!(state.selected_name(), MEGA_SAMPLES[0].name);
    }

    #[test]
    fn selected_source_large_index_wraps() {
        let state = MermaidMegaState {
            selected_sample: usize::MAX,
            ..MermaidMegaState::default()
        };
        // Should not panic; modulo handles it
        let _ = state.selected_source();
        let _ = state.selected_name();
    }

    #[test]
    fn generated_sample_uses_generator_source() {
        let mut state = MermaidMegaState::default();
        let idx = MEGA_SAMPLES
            .iter()
            .position(|sample| sample.source == GENERATED_SAMPLE_SOURCE)
            .expect("generated sample missing from MEGA_SAMPLES");
        state.selected_sample = idx;
        let source = state.selected_source().unwrap_or("");
        assert!(source.contains("graph TD"));
        assert!(source.contains("N1"));
    }

    #[test]
    fn generator_is_deterministic_for_seed() {
        let params = GeneratorParams {
            nodes: 18,
            branching: 2,
            density: 40,
            label_len: 8,
            seed: 42,
        };
        let a = generate_parametric_flowchart(params);
        let b = generate_parametric_flowchart(params);
        assert_eq!(a, b);

        let mut bumped = params;
        bumped.seed = 43;
        let c = generate_parametric_flowchart(bumped);
        assert_ne!(a, c);
    }

    // ── Config generation tests ──────────────────────────────────────

    #[test]
    fn config_reflects_glyph_mode() {
        let state = MermaidMegaState {
            glyph_mode: MermaidGlyphMode::Ascii,
            ..MermaidMegaState::default()
        };
        let config = state.to_config();
        assert_eq!(config.glyph_mode, MermaidGlyphMode::Ascii);
    }

    #[test]
    fn config_reflects_render_mode() {
        let state = MermaidMegaState {
            render_mode: MermaidRenderMode::Braille,
            ..MermaidMegaState::default()
        };
        let config = state.to_config();
        assert_eq!(config.render_mode, MermaidRenderMode::Braille);
    }

    #[test]
    fn config_reflects_styles_enabled() {
        let state = MermaidMegaState {
            styles_enabled: false,
            ..MermaidMegaState::default()
        };
        let config = state.to_config();
        assert!(!config.enable_styles);
    }

    #[test]
    fn config_spacious_has_lower_budget() {
        let state = MermaidMegaState {
            layout_mode: LayoutMode::Spacious,
            ..MermaidMegaState::default()
        };
        let config = state.to_config();
        assert_eq!(config.layout_iteration_budget, 140);
        assert_eq!(config.route_budget, 3_000);
    }

    #[test]
    fn config_normal_uses_defaults() {
        let state = MermaidMegaState {
            layout_mode: LayoutMode::Normal,
            ..MermaidMegaState::default()
        };
        let config = state.to_config();
        let default_config = MermaidConfig::default();
        assert_eq!(
            config.layout_iteration_budget,
            default_config.layout_iteration_budget
        );
    }

    // ── Layout spacing tests ─────────────────────────────────────────

    #[test]
    fn layout_spacing_spacious_wider() {
        let state = MermaidMegaState {
            layout_mode: LayoutMode::Spacious,
            ..MermaidMegaState::default()
        };
        let spacing = state.layout_spacing();
        let default_spacing = mermaid_layout::LayoutSpacing::default();
        assert!(spacing.rank_gap > default_spacing.rank_gap);
        assert!(spacing.node_gap > default_spacing.node_gap);
    }

    #[test]
    fn layout_spacing_normal_matches_default() {
        let state = MermaidMegaState {
            layout_mode: LayoutMode::Normal,
            ..MermaidMegaState::default()
        };
        let spacing = state.layout_spacing();
        let default_spacing = mermaid_layout::LayoutSpacing::default();
        assert!((spacing.rank_gap - default_spacing.rank_gap).abs() < f64::EPSILON);
    }

    // ── Layout mode cycling ──────────────────────────────────────────

    #[test]
    fn layout_mode_as_str_matches() {
        assert_eq!(LayoutMode::Dense.as_str(), "dense");
        assert_eq!(LayoutMode::Normal.as_str(), "normal");
        assert_eq!(LayoutMode::Spacious.as_str(), "spacious");
        assert_eq!(LayoutMode::Auto.as_str(), "auto");
    }

    // ── Cache invalidation: viewport resize triggers miss ────────────

    #[test]
    fn cache_miss_on_viewport_resize() {
        let screen = MermaidMegaShowcaseScreen::new();
        let area1 = Rect::new(0, 0, 80, 24);
        let area2 = Rect::new(0, 0, 120, 40);
        screen.ensure_render_cache(area1);
        screen.ensure_render_cache(area2);
        let cache = screen.cache.borrow();
        // Two different viewports → two misses
        assert_eq!(cache.cache_misses, 2);
    }

    #[test]
    fn cache_miss_on_sample_change() {
        let mut screen = MermaidMegaShowcaseScreen::new();
        let area = Rect::new(0, 0, 80, 24);
        screen.ensure_render_cache(area);
        screen.state.apply(MegaAction::NextSample);
        screen.ensure_render_cache(area);
        let cache = screen.cache.borrow();
        assert_eq!(cache.cache_misses, 2);
        assert!(!cache.last_cache_hit);
    }

    #[test]
    fn cache_hit_when_only_pan_changes() {
        let mut screen = MermaidMegaShowcaseScreen::new();
        let area = Rect::new(0, 0, 80, 24);
        screen.ensure_render_cache(area);
        // Pan does NOT bump any epoch — cache should still hit
        screen.state.apply(MegaAction::PanLeft);
        screen.ensure_render_cache(area);
        let cache = screen.cache.borrow();
        assert_eq!(cache.cache_hits, 1, "pan should not invalidate cache");
    }

    #[test]
    fn cache_miss_on_zoom_change() {
        let mut screen = MermaidMegaShowcaseScreen::new();
        let area = Rect::new(0, 0, 80, 24);
        screen.ensure_render_cache(area);
        screen.state.apply(MegaAction::ZoomIn);
        screen.ensure_render_cache(area);
        let cache = screen.cache.borrow();
        assert_eq!(cache.cache_misses, 2, "zoom change should invalidate");
    }

    #[test]
    fn cache_miss_on_render_epoch_bump() {
        let mut screen = MermaidMegaShowcaseScreen::new();
        let area = Rect::new(0, 0, 80, 24);
        screen.ensure_render_cache(area);
        screen.state.apply(MegaAction::ToggleStyles);
        screen.ensure_render_cache(area);
        let cache = screen.cache.borrow();
        assert_eq!(cache.cache_misses, 2);
    }

    #[test]
    fn cache_miss_on_layout_epoch_bump() {
        let mut screen = MermaidMegaShowcaseScreen::new();
        let area = Rect::new(0, 0, 80, 24);
        screen.ensure_render_cache(area);
        screen.state.apply(MegaAction::CycleLayoutMode);
        screen.ensure_render_cache(area);
        let cache = screen.cache.borrow();
        assert_eq!(cache.cache_misses, 2);
    }

    #[test]
    fn cache_records_timing() {
        let screen = MermaidMegaShowcaseScreen::new();
        let area = Rect::new(0, 0, 80, 24);
        screen.ensure_render_cache(area);
        let cache = screen.cache.borrow();
        assert!(cache.parse_ms.unwrap() >= 0.0);
        assert!(cache.layout_ms.unwrap() >= 0.0);
        assert!(cache.render_ms.unwrap() >= 0.0);
    }

    // ── Viewport override edge cases ─────────────────────────────────

    #[test]
    fn viewport_override_decrease_creates_with_default() {
        let mut state = MermaidMegaState::default();
        assert!(state.viewport_size_override.is_none());
        state.apply(MegaAction::DecreaseViewportWidth);
        let (cols, rows) = state.viewport_size_override.unwrap();
        assert_eq!(
            cols,
            (VIEWPORT_OVERRIDE_DEFAULT_COLS as i32 - VIEWPORT_OVERRIDE_STEP_COLS as i32) as u16
        );
        assert_eq!(rows, VIEWPORT_OVERRIDE_DEFAULT_ROWS);
    }

    #[test]
    fn viewport_override_height_increase() {
        let mut state = MermaidMegaState::default();
        state.apply(MegaAction::IncreaseViewportHeight);
        let (cols, rows) = state.viewport_size_override.unwrap();
        assert_eq!(cols, VIEWPORT_OVERRIDE_DEFAULT_COLS);
        assert_eq!(
            rows,
            (VIEWPORT_OVERRIDE_DEFAULT_ROWS as i32 + VIEWPORT_OVERRIDE_STEP_ROWS as i32) as u16
        );
    }

    #[test]
    fn viewport_override_reset_when_none_is_noop() {
        let mut state = MermaidMegaState::default();
        let re = state.render_epoch;
        state.apply(MegaAction::ResetViewportOverride);
        // No override existed, so render epoch should NOT be bumped
        assert_eq!(state.render_epoch, re);
    }

    #[test]
    fn target_viewport_uses_override_when_set() {
        let mut screen = MermaidMegaShowcaseScreen::new();
        screen.state.viewport_size_override = Some((50, 20));
        let area = Rect::new(0, 0, 100, 40);
        let (w, h) = screen.target_viewport_size(area);
        assert_eq!(w, 50);
        assert_eq!(h, 20);
    }

    #[test]
    fn target_viewport_uses_area_when_no_override() {
        let screen = MermaidMegaShowcaseScreen::new();
        let area = Rect::new(0, 0, 100, 40);
        let (w, h) = screen.target_viewport_size(area);
        assert_eq!(w, 100);
        assert_eq!(h, 40);
    }

    // ── Layout region edge cases ─────────────────────────────────────

    #[test]
    fn layout_regions_very_small_terminal() {
        let area = Rect::new(0, 0, 5, 3);
        let panels = PanelVisibility::default();
        let regions = LayoutRegions::compute(area, &panels);
        assert_eq!(regions.diagram, area);
        assert_eq!(regions.side_panel.width, 0);
        assert_eq!(regions.controls.height, 0);
    }

    #[test]
    fn layout_regions_no_side_panel_below_min_width() {
        let area = Rect::new(0, 0, 50, 30);
        let panels = PanelVisibility::default();
        let regions = LayoutRegions::compute(area, &panels);
        // Below MIN_FULL_WIDTH — side panel should be zero-width
        assert_eq!(regions.side_panel.width, 0);
    }

    #[test]
    fn layout_regions_side_panel_at_full_width() {
        let area = Rect::new(0, 0, 120, 40);
        let panels = PanelVisibility {
            metrics: true,
            ..PanelVisibility::default()
        };
        let regions = LayoutRegions::compute(area, &panels);
        assert!(regions.side_panel.width > 0);
    }

    // ── Pan step magnitude ───────────────────────────────────────────

    #[test]
    fn pan_step_magnitude_correct() {
        let mut state = MermaidMegaState::default();
        state.apply(MegaAction::PanRight);
        assert_eq!(state.viewport_pan.0, PAN_STEP);
        state.apply(MegaAction::PanRight);
        assert_eq!(state.viewport_pan.0, PAN_STEP * 2);
    }

    // ── Default state verification ───────────────────────────────────

    #[test]
    fn default_state_all_fields() {
        let state = MermaidMegaState::default();
        assert_eq!(state.mode, ShowcaseMode::Normal);
        assert_eq!(state.layout_mode, LayoutMode::Auto);
        assert_eq!(state.tier, MermaidTier::Auto);
        assert_eq!(state.glyph_mode, MermaidGlyphMode::Unicode);
        assert_eq!(state.render_mode, MermaidRenderMode::Braille);
        assert_eq!(state.wrap_mode, MermaidWrapMode::WordChar);
        assert_eq!(state.palette, DiagramPalettePreset::Default);
        assert!(state.styles_enabled);
        assert!((state.viewport_zoom - 1.0).abs() < f32::EPSILON);
        assert_eq!(state.viewport_pan, (0, 0));
        assert!(state.viewport_size_override.is_none());
        assert_eq!(state.selected_sample, 0);
        assert_eq!(state.generator.nodes, 24);
        assert_eq!(state.generator.branching, 2);
        assert_eq!(state.generator.density, 25);
        assert_eq!(state.generator.label_len, 6);
        assert_eq!(state.generator.seed, 1);
        assert!(!state.generated_source.is_empty());
        assert!(state.selected_node.is_none());
        assert!(state.search_query.is_none());
        assert_eq!(state.analysis_epoch, 0);
        assert_eq!(state.layout_epoch, 0);
        assert_eq!(state.render_epoch, 0);
        assert!(state.status_log.is_empty());
    }

    #[test]
    fn default_panel_visibility() {
        let panels = PanelVisibility::default();
        assert!(panels.controls);
        assert!(panels.metrics);
        assert!(!panels.detail);
        assert!(!panels.status_log);
        assert!(!panels.help_overlay);
    }

    // ── Every action logs to status log ──────────────────────────────

    #[test]
    fn all_actions_log_to_status_log() {
        let actions = [
            MegaAction::NextSample,
            MegaAction::PrevSample,
            MegaAction::IncreaseGenNodes,
            MegaAction::DecreaseGenNodes,
            MegaAction::IncreaseGenBranching,
            MegaAction::DecreaseGenBranching,
            MegaAction::IncreaseGenDensity,
            MegaAction::DecreaseGenDensity,
            MegaAction::IncreaseGenLabelLen,
            MegaAction::DecreaseGenLabelLen,
            MegaAction::NextGenSeed,
            MegaAction::CycleTier,
            MegaAction::ToggleGlyphMode,
            MegaAction::CycleRenderMode,
            MegaAction::CycleWrapMode,
            MegaAction::ToggleStyles,
            MegaAction::CycleLayoutMode,
            MegaAction::ForceRelayout,
            MegaAction::CyclePalette,
            MegaAction::PrevPalette,
            MegaAction::ZoomIn,
            MegaAction::ZoomOut,
            MegaAction::ZoomReset,
            MegaAction::FitToView,
            MegaAction::PanLeft,
            MegaAction::PanRight,
            MegaAction::PanUp,
            MegaAction::PanDown,
            MegaAction::IncreaseViewportWidth,
            MegaAction::DecreaseViewportWidth,
            MegaAction::IncreaseViewportHeight,
            MegaAction::DecreaseViewportHeight,
            MegaAction::ResetViewportOverride,
            MegaAction::ToggleMetrics,
            MegaAction::ToggleControls,
            MegaAction::ToggleDetail,
            MegaAction::ToggleStatusLog,
            MegaAction::ToggleHelp,
            MegaAction::SelectNextNode,
            MegaAction::SelectPrevNode,
            MegaAction::DeselectNode,
            MegaAction::EnterSearch,
            MegaAction::ExitSearch,
            MegaAction::CollapsePanels,
        ];
        let mut state = MermaidMegaState::default();
        for action in actions {
            state.apply(action);
        }
        assert_eq!(state.status_log.len(), actions.len());
    }

    // ── Debounce and budget guardrail tests ─────────────────────────

    #[test]
    fn cache_debounce_fields_initialised() {
        let cache = MegaRenderCache::empty();
        assert!(cache.last_layout_instant.is_none());
        assert!(!cache.layout_budget_exceeded);
        // budget_warning_logged starts true so no false-positive log on init
        assert!(cache.budget_warning_logged);
        assert_eq!(cache.debounce_skips, 0);
    }

    #[test]
    fn ensure_cache_records_layout_instant() {
        let screen = MermaidMegaShowcaseScreen::new();
        let area = Rect::new(0, 0, 80, 24);
        screen.ensure_render_cache(area);

        let cache = screen.cache.borrow();
        assert!(
            cache.last_layout_instant.is_some(),
            "layout instant should be set after first render"
        );
    }

    #[test]
    fn debounce_skips_when_budget_exceeded_and_called_rapidly() {
        let mut screen = MermaidMegaShowcaseScreen::new();
        let area = Rect::new(0, 0, 80, 24);

        // First call populates the cache.
        screen.ensure_render_cache(area);

        // Simulate that the layout was expensive (exceeded budget).
        {
            let mut cache = screen.cache.borrow_mut();
            cache.layout_budget_exceeded = true;
            cache.last_layout_instant = Some(Instant::now());
        }

        let first_misses = screen.cache.borrow().cache_misses;

        // Immediately bump layout epoch and call again — should debounce
        // because budget was exceeded and the call is within the window.
        screen.state.bump_layout();
        screen.ensure_render_cache(area);

        let cache = screen.cache.borrow();
        assert_eq!(
            cache.cache_misses, first_misses,
            "layout should be debounced"
        );
        assert!(cache.debounce_skips > 0, "debounce_skips should increment");
    }

    #[test]
    fn no_debounce_when_budget_is_ok() {
        let mut screen = MermaidMegaShowcaseScreen::new();
        let area = Rect::new(0, 0, 80, 24);

        // First call populates the cache.
        screen.ensure_render_cache(area);

        // Budget is NOT exceeded, so debounce should not apply.
        assert!(!screen.cache.borrow().layout_budget_exceeded);

        let first_misses = screen.cache.borrow().cache_misses;

        // Bump layout epoch and call again — should NOT debounce.
        screen.state.bump_layout();
        screen.ensure_render_cache(area);

        let cache = screen.cache.borrow();
        assert!(
            cache.cache_misses > first_misses,
            "layout should run (no debounce)"
        );
        assert_eq!(cache.debounce_skips, 0, "no debounce skips expected");
    }

    #[test]
    fn budget_exceeded_flag_set_for_slow_layout() {
        // We can't easily simulate a slow layout, but we can verify the flag
        // logic by directly manipulating the cache.
        let mut cache = MegaRenderCache::empty();
        cache.layout_ms = Some(25.0);
        cache.layout_budget_exceeded = 25.0 > LAYOUT_BUDGET_MS;
        assert!(
            cache.layout_budget_exceeded,
            "25ms should exceed 16ms budget"
        );
    }

    #[test]
    fn budget_not_exceeded_for_fast_layout() {
        let mut cache = MegaRenderCache::empty();
        cache.layout_ms = Some(5.0);
        cache.layout_budget_exceeded = 5.0 > LAYOUT_BUDGET_MS;
        assert!(
            !cache.layout_budget_exceeded,
            "5ms should not exceed 16ms budget"
        );
    }

    #[test]
    fn tick_logs_budget_warning() {
        let mut screen = MermaidMegaShowcaseScreen::new();
        // Simulate a budget-exceeding layout by directly setting cache fields.
        {
            let mut cache = screen.cache.borrow_mut();
            cache.layout_ms = Some(20.0);
            cache.layout_budget_exceeded = true;
            cache.budget_warning_logged = false;
        }

        assert!(screen.state.status_log.is_empty());
        screen.tick(1);

        assert_eq!(screen.state.status_log.len(), 1);
        assert_eq!(screen.state.status_log[0].action, "budget");
        assert!(screen.state.status_log[0].detail.contains("20.0ms"));
        assert!(screen.state.status_log[0].detail.contains("budget"));

        // Second tick should not log again.
        screen.tick(2);
        assert_eq!(screen.state.status_log.len(), 1, "duplicate log prevented");
    }

    #[test]
    fn tick_does_nothing_when_budget_ok() {
        let mut screen = MermaidMegaShowcaseScreen::new();
        {
            let mut cache = screen.cache.borrow_mut();
            cache.layout_ms = Some(5.0);
            cache.layout_budget_exceeded = false;
        }
        screen.tick(1);
        assert!(
            screen.state.status_log.is_empty(),
            "no log when budget is fine"
        );
    }

    #[test]
    fn layout_debounce_constant_reasonable() {
        const { assert!(LAYOUT_DEBOUNCE_MS >= 20) };
        const { assert!(LAYOUT_DEBOUNCE_MS <= 200) };
    }

    #[test]
    fn layout_budget_constant_reasonable() {
        const { assert!(LAYOUT_BUDGET_MS >= 8.0) };
        const { assert!(LAYOUT_BUDGET_MS <= 33.0) };
    }

    // ── Help overlay tests ───────────────────────────────────────────

    #[test]
    fn help_overlay_renders_without_panic() {
        use ftui_render::grapheme_pool::GraphemePool;
        let screen = MermaidMegaShowcaseScreen::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        let area = Rect::new(0, 0, 120, 40);
        screen.render_help_overlay(area, &mut frame);
    }

    #[test]
    fn help_overlay_renders_on_small_area() {
        use ftui_render::grapheme_pool::GraphemePool;
        let screen = MermaidMegaShowcaseScreen::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 15, &mut pool);
        let area = Rect::new(0, 0, 40, 15);
        screen.render_help_overlay(area, &mut frame);
    }

    #[test]
    fn help_overlay_noop_on_empty() {
        use ftui_render::grapheme_pool::GraphemePool;
        let screen = MermaidMegaShowcaseScreen::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);
        let area = Rect::new(0, 0, 0, 0);
        screen.render_help_overlay(area, &mut frame);
    }

    #[test]
    fn view_renders_help_when_overlay_on() {
        use ftui_render::grapheme_pool::GraphemePool;
        let mut screen = MermaidMegaShowcaseScreen::new();
        screen.state.panels.help_overlay = true;
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        let area = Rect::new(0, 0, 120, 40);
        screen.view(&mut frame, area);
    }

    #[test]
    fn view_skips_help_when_overlay_off() {
        use ftui_render::grapheme_pool::GraphemePool;
        let screen = MermaidMegaShowcaseScreen::new();
        assert!(!screen.state.panels.help_overlay);
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        let area = Rect::new(0, 0, 80, 24);
        screen.view(&mut frame, area);
    }

    #[test]
    fn keybindings_include_all_categories() {
        let screen = MermaidMegaShowcaseScreen::new();
        let bindings = screen.keybindings();
        let keys: Vec<&str> = bindings.iter().map(|h| h.key).collect();
        // Verify key categories are represented
        assert!(keys.contains(&"j/\u{2193}"), "missing sample nav");
        assert!(keys.contains(&"t"), "missing tier");
        assert!(keys.contains(&"g"), "missing glyph");
        assert!(keys.contains(&"b"), "missing render mode");
        assert!(keys.contains(&"s"), "missing styles");
        assert!(keys.contains(&"w"), "missing wrap");
        assert!(keys.contains(&"l"), "missing layout");
        assert!(keys.contains(&"r"), "missing relayout");
        assert!(keys.contains(&"p/P"), "missing palette");
        assert!(keys.contains(&"+/-"), "missing zoom");
        assert!(keys.contains(&"0"), "missing zoom reset");
        assert!(keys.contains(&"f"), "missing fit");
        assert!(keys.contains(&"H/J/K/L"), "missing pan");
        assert!(keys.contains(&"]/["), "missing viewport width");
        assert!(keys.contains(&"}/{"), "missing viewport height");
        assert!(keys.contains(&"o"), "missing viewport reset");
        assert!(keys.contains(&"m"), "missing metrics toggle");
        assert!(keys.contains(&"c"), "missing controls toggle");
        assert!(keys.contains(&"d"), "missing detail toggle");
        assert!(keys.contains(&"i"), "missing status log toggle");
        assert!(keys.contains(&"?"), "missing help toggle");
        assert!(keys.contains(&"Esc"), "missing escape");
    }

    #[test]
    fn keybindings_count_covers_all_actions() {
        let screen = MermaidMegaShowcaseScreen::new();
        let bindings = screen.keybindings();
        // We have at least one entry per bound key category.
        // With merged entries (j/Down, +/-, etc.) we expect ~25 entries.
        assert!(
            bindings.len() >= 22,
            "expected at least 22 keybinding entries, got {}",
            bindings.len()
        );
    }

    // ── Comparison mode tests ────────────────────────────────────────

    #[test]
    fn comparison_toggle_flips_state() {
        let mut state = MermaidMegaState::default();
        assert!(!state.comparison_enabled);
        state.apply(MegaAction::ToggleComparison);
        assert!(state.comparison_enabled);
        state.apply(MegaAction::ToggleComparison);
        assert!(!state.comparison_enabled);
    }

    #[test]
    fn comparison_layout_mode_cycles() {
        let mut state = MermaidMegaState::default();
        assert!(matches!(state.comparison_layout_mode, LayoutMode::Dense));
        state.apply(MegaAction::CycleComparisonLayout);
        assert!(matches!(state.comparison_layout_mode, LayoutMode::Normal));
        state.apply(MegaAction::CycleComparisonLayout);
        assert!(matches!(state.comparison_layout_mode, LayoutMode::Spacious));
        state.apply(MegaAction::CycleComparisonLayout);
        assert!(matches!(state.comparison_layout_mode, LayoutMode::Auto));
        state.apply(MegaAction::CycleComparisonLayout);
        assert!(matches!(state.comparison_layout_mode, LayoutMode::Dense));
    }

    #[test]
    fn comparison_toggle_bumps_render_epoch() {
        let mut state = MermaidMegaState::default();
        let epoch = state.render_epoch;
        state.apply(MegaAction::ToggleComparison);
        assert!(state.render_epoch > epoch);
    }

    #[test]
    fn comparison_cycle_layout_bumps_render_epoch() {
        let mut state = MermaidMegaState::default();
        let epoch = state.render_epoch;
        state.apply(MegaAction::CycleComparisonLayout);
        assert!(state.render_epoch > epoch);
    }

    #[test]
    fn comparison_keybinding_v_toggles() {
        let screen = MermaidMegaShowcaseScreen::new();
        let event = ftui_core::event::KeyEvent {
            code: ftui_core::event::KeyCode::Char('v'),
            modifiers: ftui_core::event::Modifiers::NONE,
            kind: ftui_core::event::KeyEventKind::Press,
        };
        assert_eq!(
            screen.handle_key(&event),
            Some(MegaAction::ToggleComparison)
        );
    }

    #[test]
    fn comparison_keybinding_shift_v_cycles() {
        let screen = MermaidMegaShowcaseScreen::new();
        let event = ftui_core::event::KeyEvent {
            code: ftui_core::event::KeyCode::Char('V'),
            modifiers: ftui_core::event::Modifiers::NONE,
            kind: ftui_core::event::KeyEventKind::Press,
        };
        assert_eq!(
            screen.handle_key(&event),
            Some(MegaAction::CycleComparisonLayout)
        );
    }

    #[test]
    fn comparison_render_does_not_panic() {
        let screen = {
            let mut s = MermaidMegaShowcaseScreen::new();
            s.state.comparison_enabled = true;
            s
        };
        let mut pool = ftui_render::grapheme_pool::GraphemePool::new();
        let mut frame = ftui_render::frame::Frame::new(120, 40, &mut pool);
        let area = ftui_core::geometry::Rect::new(0, 0, 120, 40);
        // Should not panic even in comparison mode.
        screen.view(&mut frame, area);
    }

    #[test]
    fn comparison_default_layout_is_dense() {
        let state = MermaidMegaState::default();
        assert!(matches!(state.comparison_layout_mode, LayoutMode::Dense));
        assert!(!state.comparison_enabled);
    }

    #[test]
    fn comparison_independent_of_primary_layout() {
        let mut state = MermaidMegaState::default();
        state.apply(MegaAction::CycleLayoutMode);
        // Primary should have changed but comparison should remain Dense.
        assert!(matches!(state.comparison_layout_mode, LayoutMode::Dense));
    }

    // ── Diagnostics panel tests ──────────────────────────────────────

    #[test]
    fn toggle_diagnostics_panel() {
        let mut state = MermaidMegaState::default();
        assert!(!state.panels.diagnostics);
        state.apply(MegaAction::ToggleDiagnostics);
        assert!(state.panels.diagnostics);
        state.apply(MegaAction::ToggleDiagnostics);
        assert!(!state.panels.diagnostics);
    }

    #[test]
    fn diagnostics_key_maps_to_action() {
        use ftui_core::event::{KeyCode, KeyEvent};
        let screen = MermaidMegaShowcaseScreen::new();
        let event = KeyEvent::new(KeyCode::Char('e'));
        assert_eq!(
            screen.handle_key(&event),
            Some(MegaAction::ToggleDiagnostics)
        );
    }

    #[test]
    fn collapse_panels_clears_diagnostics() {
        let mut state = MermaidMegaState::default();
        state.panels.diagnostics = true;
        state.apply(MegaAction::CollapsePanels);
        assert!(!state.panels.diagnostics);
    }

    #[test]
    fn diagnostics_default_is_off() {
        let state = MermaidMegaState::default();
        assert!(!state.panels.diagnostics);
    }

    #[test]
    fn render_diagram_with_no_errors_does_not_panic() {
        use ftui_render::grapheme_pool::GraphemePool;
        let screen = MermaidMegaShowcaseScreen::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        let area = Rect::new(0, 0, 80, 24);
        // Diagram area is the full area; no errors should just render normally.
        screen.render_diagram(area, &mut frame);
    }

    #[test]
    fn render_diagram_with_diagnostics_on_does_not_panic() {
        use ftui_render::grapheme_pool::GraphemePool;
        let mut screen = MermaidMegaShowcaseScreen::new();
        screen.state.panels.diagnostics = true;
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        let area = Rect::new(0, 0, 80, 24);
        screen.render_diagram(area, &mut frame);
    }

    #[test]
    fn render_diagnostics_view_with_errors() {
        use ftui_extras::mermaid::MermaidError;
        use ftui_render::grapheme_pool::GraphemePool;
        let screen = MermaidMegaShowcaseScreen::new();
        // Inject errors into cache.
        {
            let mut cache = screen.cache.borrow_mut();
            cache.errors.push(MermaidError {
                message: "unexpected token".to_string(),
                span: ftui_extras::mermaid::Span {
                    start: ftui_extras::mermaid::Position {
                        line: 3,
                        col: 10,
                        byte: 0,
                    },
                    end: ftui_extras::mermaid::Position {
                        line: 3,
                        col: 15,
                        byte: 0,
                    },
                },
                expected: Some(vec!["-->", "---"]),
                code: ftui_extras::mermaid::MermaidErrorCode::Parse,
            });
        }
        // Call render_diagnostics_view directly (render_diagram would overwrite errors
        // via ensure_render_cache).
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        let area = Rect::new(0, 0, 80, 24);
        screen.render_diagnostics_view(area, &mut frame);
        // Verify diagnostics title is rendered (starts with "D" for "Diagnostics").
        let mut found_diagnostics = false;
        for col in 0..80 {
            if let Some(cell) = frame.buffer.get(col, 0)
                && let Some('D') = cell.content.as_char()
            {
                found_diagnostics = true;
                break;
            }
        }
        assert!(found_diagnostics, "Diagnostics title should be rendered");
    }

    #[test]
    fn error_overlay_renders_on_partial_diagram() {
        use ftui_extras::mermaid::MermaidError;
        use ftui_render::grapheme_pool::GraphemePool;
        let screen = MermaidMegaShowcaseScreen::new();
        // Diagnostics panel is OFF, but there are errors.
        assert!(!screen.state.panels.diagnostics);
        // Populate cache with errors and a valid IR (partial).
        screen.ensure_render_cache(Rect::new(0, 0, 60, 20));
        {
            let mut cache = screen.cache.borrow_mut();
            cache.errors.push(MermaidError {
                message: "test error".to_string(),
                span: ftui_extras::mermaid::Span {
                    start: ftui_extras::mermaid::Position {
                        line: 1,
                        col: 1,
                        byte: 0,
                    },
                    end: ftui_extras::mermaid::Position {
                        line: 1,
                        col: 5,
                        byte: 0,
                    },
                },
                expected: None,
                code: ftui_extras::mermaid::MermaidErrorCode::Parse,
            });
        }
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        let area = Rect::new(0, 0, 80, 24);
        // Should not panic; error overlay renders on top of diagram.
        screen.render_diagram(area, &mut frame);
    }

    #[test]
    fn keybindings_include_diagnostics() {
        let screen = MermaidMegaShowcaseScreen::new();
        let bindings = screen.keybindings();
        let keys: Vec<&str> = bindings.iter().map(|h| h.key).collect();
        assert!(
            keys.contains(&"e"),
            "keybindings should include 'e' for diagnostics"
        );
    }

    #[test]
    fn controls_strip_shows_error_count() {
        use ftui_extras::mermaid::MermaidError;
        use ftui_render::grapheme_pool::GraphemePool;
        let screen = MermaidMegaShowcaseScreen::new();
        // Inject an error.
        {
            let mut cache = screen.cache.borrow_mut();
            cache.errors.push(MermaidError {
                message: "bad input".to_string(),
                span: ftui_extras::mermaid::Span {
                    start: ftui_extras::mermaid::Position {
                        line: 1,
                        col: 1,
                        byte: 0,
                    },
                    end: ftui_extras::mermaid::Position {
                        line: 1,
                        col: 1,
                        byte: 0,
                    },
                },
                expected: None,
                code: ftui_extras::mermaid::MermaidErrorCode::Parse,
            });
        }
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 10, &mut pool);
        let area = Rect::new(0, 0, 120, 8);
        screen.render_controls(area, &mut frame);
        // Check that ERR:1 appears in the rendered controls strip.
        let mut found_err = false;
        for col in 0..120 {
            if let Some(cell) = frame.buffer.get(col, 1)
                && let Some('E') = cell.content.as_char()
                && let Some(r1) = frame.buffer.get(col + 1, 1)
                && let Some('R') = r1.content.as_char()
            {
                found_err = true;
                break;
            }
        }
        assert!(
            found_err,
            "controls strip should show ERR indicator when errors exist"
        );
    }

    #[test]
    fn mega_recompute_jsonl_schema_valid() {
        let screen = MermaidMegaShowcaseScreen::new();
        let area = Rect::new(0, 0, 120, 40);
        screen.ensure_render_cache(area);

        let viewport = screen.target_viewport_size(area);
        let zoom = screen.state.viewport_zoom;
        let render_cols = (f32::from(viewport.0) * zoom)
            .round()
            .clamp(1.0, f32::from(u16::MAX)) as u16;
        let render_rows = (f32::from(viewport.1) * zoom)
            .round()
            .clamp(1.0, f32::from(u16::MAX)) as u16;

        let flags = MegaRecomputeFlags {
            analysis_ran: true,
            layout_ran: true,
            render_ran: true,
        };
        let cache = screen.cache.borrow();
        let line = screen.recompute_jsonl_line(
            &cache,
            7,
            Some("run-1"),
            42,
            "alt",
            flags,
            viewport,
            (render_cols, render_rows),
        );
        crate::test_logging::validate_mega_recompute_jsonl_schema(&line)
            .expect("mega recompute JSONL schema");
    }

    // ── Telemetry schema validator tests (bd-3oaig.20) ─────────────

    /// Sample JSONL fixture: full recompute with layout stats.
    const SAMPLE_JSONL_FULL: &str = concat!(
        r#"{"schema_version":"ftui-mega-telemetry-v1","event":"mermaid_mega_recompute","#,
        r#""seq":0,"timestamp":"2026-02-05T12:00:00Z","run_id":"test-run-1","seed":42,"#,
        r#""screen_mode":"alt","sample":"Flow Basic","diagram_type":"flowchart","#,
        r#""layout_mode":"Auto","tier":"Normal","glyph_mode":"Unicode","#,
        r#""render_mode":"Auto","wrap_mode":"None","palette":"Default","#,
        r#""styles_enabled":true,"comparison_enabled":false,"comparison_layout_mode":"Auto","#,
        r#""viewport_cols":120,"viewport_rows":40,"render_cols":120,"render_rows":40,"#,
        r#""zoom":1.000,"pan_x":0,"pan_y":0,"#,
        r#""analysis_epoch":1,"layout_epoch":1,"render_epoch":1,"#,
        r#""analysis_ran":true,"layout_ran":true,"render_ran":true,"#,
        r#""cache_hits":0,"cache_misses":1,"cache_hit":false,"debounce_skips":0,"#,
        r#""layout_budget_exceeded":false,"#,
        r#""parse_ms":0.5,"layout_ms":2.1,"render_ms":1.3,"#,
        r#""node_count":5,"edge_count":4,"error_count":0,"#,
        r#""layout_iterations":12,"layout_iterations_max":200,"#,
        r#""layout_budget_exceeded_layout":false,"layout_crossings":0,"#,
        r#""layout_ranks":3,"layout_max_rank_width":2,"#,
        r#""layout_total_bends":2,"layout_position_variance":1.5}"#,
    );

    /// Sample JSONL fixture: cache hit with null layout stats.
    const SAMPLE_JSONL_CACHE_HIT: &str = concat!(
        r#"{"schema_version":"ftui-mega-telemetry-v1","event":"mermaid_mega_recompute","#,
        r#""seq":1,"timestamp":"2026-02-05T12:00:01Z","run_id":"test-run-1","seed":42,"#,
        r#""screen_mode":"alt","sample":"Flow Basic","diagram_type":"flowchart","#,
        r#""layout_mode":"Auto","tier":"Normal","glyph_mode":"Unicode","#,
        r#""render_mode":"Auto","wrap_mode":"None","palette":"Default","#,
        r#""styles_enabled":true,"comparison_enabled":false,"comparison_layout_mode":"Auto","#,
        r#""viewport_cols":120,"viewport_rows":40,"render_cols":120,"render_rows":40,"#,
        r#""zoom":1.000,"pan_x":0,"pan_y":0,"#,
        r#""analysis_epoch":1,"layout_epoch":1,"render_epoch":2,"#,
        r#""analysis_ran":false,"layout_ran":false,"render_ran":true,"#,
        r#""cache_hits":1,"cache_misses":1,"cache_hit":false,"debounce_skips":0,"#,
        r#""layout_budget_exceeded":false,"#,
        r#""parse_ms":null,"layout_ms":null,"render_ms":0.8,"#,
        r#""node_count":5,"edge_count":4,"error_count":0,"#,
        r#""layout_iterations":null,"layout_iterations_max":null,"#,
        r#""layout_budget_exceeded_layout":null,"layout_crossings":null,"#,
        r#""layout_ranks":null,"layout_max_rank_width":null,"#,
        r#""layout_total_bends":null,"layout_position_variance":null}"#,
    );

    #[test]
    fn validate_sample_fixture_full() {
        validate_mega_telemetry_line(SAMPLE_JSONL_FULL).expect("full fixture should validate");
    }

    #[test]
    fn validate_sample_fixture_cache_hit() {
        validate_mega_telemetry_line(SAMPLE_JSONL_CACHE_HIT)
            .expect("cache-hit fixture should validate");
    }

    #[test]
    fn validate_rejects_invalid_json() {
        let result = validate_mega_telemetry_line("not json at all");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid JSON"));
    }

    #[test]
    fn validate_rejects_non_object() {
        let result = validate_mega_telemetry_line("[1, 2, 3]");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("JSON object"));
    }

    #[test]
    fn validate_rejects_wrong_schema_version() {
        let line = SAMPLE_JSONL_FULL.replace("ftui-mega-telemetry-v1", "ftui-mega-telemetry-v99");
        let result = validate_mega_telemetry_line(&line);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("schema_version mismatch"));
    }

    #[test]
    fn validate_rejects_wrong_event_type() {
        let line = SAMPLE_JSONL_FULL.replace("mermaid_mega_recompute", "wrong_event");
        let result = validate_mega_telemetry_line(&line);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("event type mismatch"));
    }

    #[test]
    fn validate_rejects_missing_required_field() {
        // Remove the "zoom" required field.
        let line = SAMPLE_JSONL_FULL.replace(r#","zoom":1.000"#, "");
        let result = validate_mega_telemetry_line(&line);
        assert!(result.is_err(), "missing zoom should fail validation");
    }

    #[test]
    fn validate_rejects_missing_nullable_field() {
        // Remove the nullable "layout_position_variance" field.
        let line = SAMPLE_JSONL_FULL.replace(r#","layout_position_variance":1.5"#, "");
        let result = validate_mega_telemetry_line(&line);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("layout_position_variance"));
    }

    #[test]
    fn validate_rejects_negative_zoom() {
        let line = SAMPLE_JSONL_FULL.replace(r#""zoom":1.000"#, r#""zoom":-0.5"#);
        let result = validate_mega_telemetry_line(&line);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("zoom must be positive"));
    }

    #[test]
    fn validate_rejects_boolean_as_string() {
        let line = SAMPLE_JSONL_FULL.replace(r#""analysis_ran":true"#, r#""analysis_ran":"true""#);
        let result = validate_mega_telemetry_line(&line);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must be boolean"));
    }

    #[test]
    fn validate_multi_line_jsonl() {
        let content = format!("{}\n{}\n", SAMPLE_JSONL_FULL, SAMPLE_JSONL_CACHE_HIT);
        let errors = validate_mega_telemetry_jsonl(&content);
        assert!(
            errors.is_empty(),
            "multi-line JSONL should validate: {errors:?}"
        );
    }

    #[test]
    fn validate_multi_line_reports_bad_line() {
        let content = format!(
            "{}\nnot json\n{}\n",
            SAMPLE_JSONL_FULL, SAMPLE_JSONL_CACHE_HIT
        );
        let errors = validate_mega_telemetry_jsonl(&content);
        assert_eq!(errors.len(), 1, "should report exactly one bad line");
        assert_eq!(errors[0].0, 2, "bad line should be line 2");
    }

    #[test]
    fn validate_emitted_line_with_formal_validator() {
        let screen = MermaidMegaShowcaseScreen::new();
        let area = Rect::new(0, 0, 120, 40);
        screen.ensure_render_cache(area);

        let viewport = screen.target_viewport_size(area);
        let zoom = screen.state.viewport_zoom;
        let render_cols = (f32::from(viewport.0) * zoom)
            .round()
            .clamp(1.0, f32::from(u16::MAX)) as u16;
        let render_rows = (f32::from(viewport.1) * zoom)
            .round()
            .clamp(1.0, f32::from(u16::MAX)) as u16;

        let flags = MegaRecomputeFlags {
            analysis_ran: true,
            layout_ran: true,
            render_ran: true,
        };
        let cache = screen.cache.borrow();
        let line = screen.recompute_jsonl_line(
            &cache,
            0,
            Some("test-run"),
            99,
            "alt",
            flags,
            viewport,
            (render_cols, render_rows),
        );
        // Validate with BOTH the simple and formal validators.
        crate::test_logging::validate_mega_recompute_jsonl_schema(&line)
            .expect("simple validator should pass");
        validate_mega_telemetry_line(&line).expect("formal validator should pass");
    }

    #[test]
    fn validate_emitted_line_without_run_id() {
        let screen = MermaidMegaShowcaseScreen::new();
        let area = Rect::new(0, 0, 80, 24);
        screen.ensure_render_cache(area);

        let viewport = screen.target_viewport_size(area);
        let zoom = screen.state.viewport_zoom;
        let render_cols = (f32::from(viewport.0) * zoom)
            .round()
            .clamp(1.0, f32::from(u16::MAX)) as u16;
        let render_rows = (f32::from(viewport.1) * zoom)
            .round()
            .clamp(1.0, f32::from(u16::MAX)) as u16;

        let flags = MegaRecomputeFlags {
            analysis_ran: true,
            layout_ran: true,
            render_ran: true,
        };
        let cache = screen.cache.borrow();
        let line = screen.recompute_jsonl_line(
            &cache,
            0,
            None, // No run_id
            0,
            "inline",
            flags,
            viewport,
            (render_cols, render_rows),
        );
        // run_id is nullable, so validation should still pass.
        validate_mega_telemetry_line(&line).expect("line without run_id should validate");
    }

    #[test]
    fn validate_negative_timing_rejected() {
        let line = SAMPLE_JSONL_FULL.replace(r#""parse_ms":0.5"#, r#""parse_ms":-1.0"#);
        let result = validate_mega_telemetry_line(&line);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("non-negative"));
    }

    #[test]
    fn sample_fixture_field_count() {
        // Verify the sample fixtures contain exactly the expected number of fields.
        let value: serde_json::Value =
            serde_json::from_str(SAMPLE_JSONL_FULL).expect("fixture should parse");
        let obj = value.as_object().expect("fixture should be object");
        let total_expected =
            MEGA_TELEMETRY_REQUIRED_FIELDS.len() + MEGA_TELEMETRY_NULLABLE_FIELDS.len();
        assert_eq!(
            obj.len(),
            total_expected,
            "full fixture should have exactly {total_expected} fields, got {}",
            obj.len()
        );
    }

    /// Helper: build a minimal valid JSONL line that passes the schema validator.
    fn make_valid_mega_jsonl() -> String {
        let mut parts = Vec::new();
        parts.push(format!(
            "\"schema_version\":\"{}\",\"event\":\"mermaid_mega_recompute\"",
            MEGA_TELEMETRY_SCHEMA
        ));
        parts.push(r#""seq":1,"timestamp":"2026-02-05T12:00:00.000Z","seed":42"#.to_string());
        parts.push(
            r#""screen_mode":"alt","sample":"basic_flow","diagram_type":"flowchart""#.to_string(),
        );
        parts.push(r#""layout_mode":"dense","tier":"Full","glyph_mode":"Unicode""#.to_string());
        parts
            .push(r#""render_mode":"Cell","wrap_mode":"Truncate","palette":"Default""#.to_string());
        parts.push(r#""styles_enabled":true,"comparison_enabled":false"#.to_string());
        parts.push(r#""comparison_layout_mode":"dense""#.to_string());
        parts.push(r#""viewport_cols":120,"viewport_rows":40"#.to_string());
        parts.push(r#""render_cols":120,"render_rows":40"#.to_string());
        parts.push(r#""zoom":1.0,"pan_x":0,"pan_y":0"#.to_string());
        parts.push(r#""analysis_epoch":1,"layout_epoch":1,"render_epoch":1"#.to_string());
        parts.push(r#""analysis_ran":true,"layout_ran":true,"render_ran":true"#.to_string());
        parts.push(r#""cache_hits":0,"cache_misses":1,"cache_hit":false"#.to_string());
        parts.push(r#""debounce_skips":0,"layout_budget_exceeded":false"#.to_string());
        parts.push(r#""parse_ms":1.5,"layout_ms":2.0,"render_ms":0.8"#.to_string());
        parts.push(r#""node_count":5,"edge_count":4,"error_count":0"#.to_string());
        parts.push(r#""run_id":"test-run-1""#.to_string());
        parts.push(r#""layout_iterations":3,"layout_iterations_max":10"#.to_string());
        parts.push(r#""layout_budget_exceeded_layout":false"#.to_string());
        parts.push(r#""layout_crossings":0,"layout_ranks":3"#.to_string());
        parts.push(r#""layout_max_rank_width":2,"layout_total_bends":1"#.to_string());
        parts.push(r#""layout_position_variance":0.5"#.to_string());
        format!("{{{}}}", parts.join(","))
    }

    #[test]
    fn validate_mega_telemetry_accepts_valid_line() {
        let line = make_valid_mega_jsonl();
        validate_mega_telemetry_line(&line).expect("valid line should pass validation");
    }

    #[test]
    fn validate_mega_telemetry_rejects_invalid_json() {
        let result = validate_mega_telemetry_line("not json at all");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid JSON"));
    }

    #[test]
    fn validate_mega_telemetry_rejects_non_object() {
        let result = validate_mega_telemetry_line("[1,2,3]");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("JSON object"));
    }

    #[test]
    fn validate_mega_telemetry_rejects_wrong_schema_version() {
        let mut obj: serde_json::Value = serde_json::from_str(&make_valid_mega_jsonl()).unwrap();
        obj["schema_version"] = serde_json::json!("wrong-version");
        let result = validate_mega_telemetry_line(&obj.to_string());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("schema_version mismatch"));
    }

    #[test]
    fn validate_mega_telemetry_rejects_wrong_event() {
        let mut obj: serde_json::Value = serde_json::from_str(&make_valid_mega_jsonl()).unwrap();
        obj["event"] = serde_json::json!("wrong_event");
        let result = validate_mega_telemetry_line(&obj.to_string());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("event type mismatch"));
    }

    #[test]
    fn validate_mega_telemetry_rejects_missing_required_field() {
        let mut obj: serde_json::Value = serde_json::from_str(&make_valid_mega_jsonl()).unwrap();
        obj.as_object_mut().unwrap().remove("sample");
        let result = validate_mega_telemetry_line(&obj.to_string());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("sample"));
    }

    #[test]
    fn validate_mega_telemetry_rejects_null_required_field() {
        let mut obj: serde_json::Value = serde_json::from_str(&make_valid_mega_jsonl()).unwrap();
        obj["sample"] = serde_json::Value::Null;
        let result = validate_mega_telemetry_line(&obj.to_string());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("null"));
    }

    #[test]
    fn validate_mega_telemetry_rejects_missing_nullable_field() {
        let mut obj: serde_json::Value = serde_json::from_str(&make_valid_mega_jsonl()).unwrap();
        obj.as_object_mut().unwrap().remove("run_id");
        let result = validate_mega_telemetry_line(&obj.to_string());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("run_id"));
    }

    #[test]
    fn validate_mega_telemetry_accepts_null_nullable_field() {
        let mut obj: serde_json::Value = serde_json::from_str(&make_valid_mega_jsonl()).unwrap();
        obj["run_id"] = serde_json::Value::Null;
        obj["parse_ms"] = serde_json::Value::Null;
        obj["layout_ms"] = serde_json::Value::Null;
        obj["render_ms"] = serde_json::Value::Null;
        obj["layout_iterations"] = serde_json::Value::Null;
        obj["layout_iterations_max"] = serde_json::Value::Null;
        obj["layout_budget_exceeded_layout"] = serde_json::Value::Null;
        obj["layout_crossings"] = serde_json::Value::Null;
        obj["layout_ranks"] = serde_json::Value::Null;
        obj["layout_max_rank_width"] = serde_json::Value::Null;
        obj["layout_total_bends"] = serde_json::Value::Null;
        obj["layout_position_variance"] = serde_json::Value::Null;
        obj["node_count"] = serde_json::Value::Null;
        obj["edge_count"] = serde_json::Value::Null;
        obj["error_count"] = serde_json::Value::Null;
        validate_mega_telemetry_line(&obj.to_string())
            .expect("null nullable fields should be accepted");
    }

    #[test]
    fn validate_mega_telemetry_rejects_non_boolean_bool_field() {
        let mut obj: serde_json::Value = serde_json::from_str(&make_valid_mega_jsonl()).unwrap();
        obj["analysis_ran"] = serde_json::json!("yes");
        let result = validate_mega_telemetry_line(&obj.to_string());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("analysis_ran"));
    }

    #[test]
    fn validate_mega_telemetry_rejects_negative_nonneg_field() {
        let mut obj: serde_json::Value = serde_json::from_str(&make_valid_mega_jsonl()).unwrap();
        obj["seq"] = serde_json::json!(-1);
        let result = validate_mega_telemetry_line(&obj.to_string());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("non-negative"));
    }

    #[test]
    fn validate_mega_telemetry_rejects_zero_zoom() {
        let mut obj: serde_json::Value = serde_json::from_str(&make_valid_mega_jsonl()).unwrap();
        obj["zoom"] = serde_json::json!(0.0);
        let result = validate_mega_telemetry_line(&obj.to_string());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("zoom"));
    }

    #[test]
    fn validate_mega_telemetry_rejects_negative_timing() {
        let mut obj: serde_json::Value = serde_json::from_str(&make_valid_mega_jsonl()).unwrap();
        obj["parse_ms"] = serde_json::json!(-1.0);
        let result = validate_mega_telemetry_line(&obj.to_string());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("parse_ms"));
    }

    #[test]
    fn validate_mega_telemetry_rejects_non_string_string_field() {
        let mut obj: serde_json::Value = serde_json::from_str(&make_valid_mega_jsonl()).unwrap();
        obj["diagram_type"] = serde_json::json!(42);
        let result = validate_mega_telemetry_line(&obj.to_string());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("diagram_type"));
    }

    #[test]
    fn validate_mega_telemetry_jsonl_multi_line() {
        let valid = make_valid_mega_jsonl();
        let content = format!("{valid}\n{valid}\n\n{valid}\n");
        let errors = validate_mega_telemetry_jsonl(&content);
        assert!(errors.is_empty(), "all valid lines should pass: {errors:?}");
    }

    #[test]
    fn validate_mega_telemetry_jsonl_mixed_valid_invalid() {
        let valid = make_valid_mega_jsonl();
        let content = format!("{valid}\nnot-json\n{valid}\n");
        let errors = validate_mega_telemetry_jsonl(&content);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].0, 2); // line 2 (1-indexed)
    }

    #[test]
    fn mega_recompute_event_to_jsonl_produces_valid_json() {
        let event = MegaRecomputeEvent {
            schema_version: MEGA_TELEMETRY_SCHEMA,
            event: "mermaid_mega_recompute",
            seq: 0,
            sample: "test_flow".to_string(),
            sample_idx: 0,
            diagram_type: "flowchart".to_string(),
            trigger: "initial".to_string(),
            layout_mode: "dense".to_string(),
            tier: "Full".to_string(),
            glyph_mode: "Unicode".to_string(),
            render_mode: "Cell".to_string(),
            wrap_mode: "Truncate".to_string(),
            palette: "Default".to_string(),
            viewport_cols: 80,
            viewport_rows: 24,
            zoom: 1.0,
            parse_ms: Some(1.0),
            layout_ms: Some(2.0),
            render_ms: Some(0.5),
            total_ms: 3.5,
            analysis_ran: true,
            layout_ran: true,
            render_ran: true,
            node_count: 3,
            edge_count: 2,
            cluster_count: 0,
            label_count: 3,
            cache_hits: 0,
            cache_misses: 1,
            cache_hit_ratio: 0.0,
            debounce_skips: 0,
            layout_iterations: Some(5),
            layout_budget_exceeded: false,
            crossings: Some(0),
            total_bends: Some(1),
            position_variance: Some(0.25),
            ranks: Some(3),
            max_rank_width: Some(2),
            error_count: 0,
        };
        let line = event.to_jsonl();
        let parsed: serde_json::Value =
            serde_json::from_str(&line).expect("to_jsonl should produce valid JSON");
        let obj = parsed.as_object().unwrap();

        assert_eq!(obj["schema_version"], MEGA_TELEMETRY_SCHEMA);
        assert_eq!(obj["event"], "mermaid_mega_recompute");
        assert_eq!(obj["seq"], 0);
        assert_eq!(obj["sample"], "test_flow");
        assert_eq!(obj["trigger"], "initial");
        assert_eq!(obj["viewport_cols"], 80);
        assert_eq!(obj["zoom"], 1.0);
        assert_eq!(obj["analysis_ran"], true);
        assert_eq!(obj["node_count"], 3);
        assert_eq!(obj["cache_misses"], 1);
        assert_eq!(obj["crossings"], 0);
        assert!(obj["layout_iterations"].as_u64().is_some());
    }

    #[test]
    fn mega_recompute_event_null_optional_fields() {
        let event = MegaRecomputeEvent {
            schema_version: MEGA_TELEMETRY_SCHEMA,
            event: "mermaid_mega_recompute",
            seq: 1,
            sample: "empty".to_string(),
            sample_idx: 0,
            diagram_type: "flowchart".to_string(),
            trigger: "resize".to_string(),
            layout_mode: "dense".to_string(),
            tier: "Fast".to_string(),
            glyph_mode: "ASCII".to_string(),
            render_mode: "Cell".to_string(),
            wrap_mode: "Truncate".to_string(),
            palette: "Default".to_string(),
            viewport_cols: 40,
            viewport_rows: 10,
            zoom: 2.0,
            parse_ms: None,
            layout_ms: None,
            render_ms: None,
            total_ms: 0.0,
            analysis_ran: false,
            layout_ran: false,
            render_ran: false,
            node_count: 0,
            edge_count: 0,
            cluster_count: 0,
            label_count: 0,
            cache_hits: 5,
            cache_misses: 0,
            cache_hit_ratio: 1.0,
            debounce_skips: 2,
            layout_iterations: None,
            layout_budget_exceeded: false,
            crossings: None,
            total_bends: None,
            position_variance: None,
            ranks: None,
            max_rank_width: None,
            error_count: 0,
        };
        let line = event.to_jsonl();
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        let obj = parsed.as_object().unwrap();

        assert!(obj["parse_ms"].is_null());
        assert!(obj["layout_ms"].is_null());
        assert!(obj["render_ms"].is_null());
        assert!(obj["layout_iterations"].is_null());
        assert!(obj["crossings"].is_null());
        assert!(obj["total_bends"].is_null());
        assert!(obj["position_variance"].is_null());
        assert!(obj["ranks"].is_null());
        assert!(obj["max_rank_width"].is_null());
    }

    #[test]
    fn mega_telemetry_schema_constant_stable() {
        assert_eq!(MEGA_TELEMETRY_SCHEMA, "ftui-mega-telemetry-v1");
    }

    #[test]
    fn mega_telemetry_valid_triggers_non_empty() {
        assert!(!MEGA_TELEMETRY_VALID_TRIGGERS.is_empty());
        assert!(MEGA_TELEMETRY_VALID_TRIGGERS.contains(&"initial"));
        assert!(MEGA_TELEMETRY_VALID_TRIGGERS.contains(&"resize"));
        assert!(MEGA_TELEMETRY_VALID_TRIGGERS.contains(&"zoom"));
    }

    #[test]
    fn mega_telemetry_required_fields_non_empty() {
        assert!(MEGA_TELEMETRY_REQUIRED_FIELDS.len() > 10);
        assert!(MEGA_TELEMETRY_REQUIRED_FIELDS.contains(&"schema_version"));
        assert!(MEGA_TELEMETRY_REQUIRED_FIELDS.contains(&"event"));
        assert!(MEGA_TELEMETRY_REQUIRED_FIELDS.contains(&"seq"));
    }

    #[test]
    fn mega_telemetry_nullable_fields_non_empty() {
        assert!(!MEGA_TELEMETRY_NULLABLE_FIELDS.is_empty());
        assert!(MEGA_TELEMETRY_NULLABLE_FIELDS.contains(&"parse_ms"));
        assert!(MEGA_TELEMETRY_NULLABLE_FIELDS.contains(&"layout_ms"));
        assert!(MEGA_TELEMETRY_NULLABLE_FIELDS.contains(&"render_ms"));
    }

    #[test]
    fn mega_telemetry_recompute_jsonl_line_passes_validator() {
        let screen = MermaidMegaShowcaseScreen::new();
        let area = Rect::new(0, 0, 120, 40);
        screen.ensure_render_cache(area);

        let viewport = screen.target_viewport_size(area);
        let zoom = screen.state.viewport_zoom;
        let render_cols = (f32::from(viewport.0) * zoom)
            .round()
            .clamp(1.0, f32::from(u16::MAX)) as u16;
        let render_rows = (f32::from(viewport.1) * zoom)
            .round()
            .clamp(1.0, f32::from(u16::MAX)) as u16;

        let flags = MegaRecomputeFlags {
            analysis_ran: true,
            layout_ran: true,
            render_ran: true,
        };
        let cache = screen.cache.borrow();
        let line = screen.recompute_jsonl_line(
            &cache,
            42,
            Some("test-run"),
            99,
            "alt",
            flags,
            viewport,
            (render_cols, render_rows),
        );

        // Must pass the structured validator too.
        validate_mega_telemetry_line(&line)
            .expect("recompute_jsonl_line output should pass structured validator");
    }
}
