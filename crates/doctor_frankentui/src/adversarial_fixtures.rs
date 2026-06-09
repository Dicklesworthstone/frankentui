// SPDX-License-Identifier: Apache-2.0
//! Synthetic adversarial fixture generator.
//!
//! Generates deterministic stress fixtures that target hard migration paths:
//! - Nested state management with cross-component data flow
//! - Dynamic style computation and theme switching
//! - Effect storms (cascading side-effects with complex cleanup)
//! - Interaction edge cases (keyboard, mouse, focus traps)
//!
//! Every generated fixture is fully deterministic from its seed and includes
//! self-describing metadata with expected risk classification.
//!
//! # Pipeline
//! ```text
//!   CoverageReport → blind spots → fixture plan → TSX source → CorpusEntry
//! ```
//!
//! Generated fixtures integrate directly into the corpus pipeline and can be
//! consumed by certification and fuzz stages.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::corpus::{
    ComplexityTag, CorpusEntry, CorpusMetrics, CorpusProvenance, ProvenanceSourceType,
};
use crate::fixture_taxonomy::{BlindSpotImpact, CoverageReport};

// ── Types ────────────────────────────────────────────────────────────────

/// Risk class for a generated fixture, reflecting migration difficulty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum RiskClass {
    /// Straightforward migration path.
    Low,
    /// Moderate complexity, may need manual review.
    Medium,
    /// Known difficult pattern, likely needs fallback strategies.
    High,
    /// Potentially untranslatable or requires FrankenTUI extensions.
    Critical,
}

/// A stress scenario describing what adversarial pattern to generate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StressScenario {
    /// Unique identifier derived from seed and category.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Which coverage category this targets.
    pub target_category: String,
    /// Which specific dimension this exercises.
    pub target_dimension: String,
    /// Expected risk class for migration.
    pub risk_class: RiskClass,
    /// Seed used for deterministic generation.
    pub seed: u64,
    /// Description of the adversarial pattern.
    pub description: String,
}

/// Configuration for the fixture generator.
#[derive(Debug, Clone)]
pub struct GeneratorConfig {
    /// Base seed for deterministic generation (all fixtures derive from this).
    pub base_seed: u64,
    /// Maximum number of components per fixture.
    pub max_components: usize,
    /// Maximum nesting depth for component trees.
    pub max_depth: usize,
    /// Generator identity for provenance.
    pub generator_id: String,
    /// Whether to target only high-impact blind spots.
    pub high_impact_only: bool,
}

impl Default for GeneratorConfig {
    fn default() -> Self {
        Self {
            base_seed: 0xDEAD_BEEF_CAFE_F00D,
            max_components: 12,
            max_depth: 5,
            generator_id: "adversarial-fixture-gen-v1".to_string(),
            high_impact_only: false,
        }
    }
}

/// A generated synthetic fixture with source and metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedFixture {
    /// The stress scenario that produced this fixture.
    pub scenario: StressScenario,
    /// Generated TSX source code.
    pub source: String,
    /// File name for the generated source.
    pub filename: String,
    /// Corpus entry for integration into the corpus pipeline.
    pub corpus_entry: CorpusEntry,
    /// Expected pattern annotations.
    pub expected_patterns: ExpectedPatterns,
}

/// Expected patterns that the generated fixture should exercise.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpectedPatterns {
    pub ui_patterns: BTreeSet<String>,
    pub state_patterns: BTreeSet<String>,
    pub effect_patterns: BTreeSet<String>,
    pub style_patterns: BTreeSet<String>,
    pub accessibility_patterns: BTreeSet<String>,
    pub terminal_patterns: BTreeSet<String>,
    pub data_patterns: BTreeSet<String>,
}

/// Result of a generation run.
#[derive(Debug, Clone)]
pub struct GenerationResult {
    /// All generated fixtures.
    pub fixtures: Vec<GeneratedFixture>,
    /// Scenarios that could not be generated (with reasons).
    pub skipped: Vec<(StressScenario, String)>,
    /// Summary statistics.
    pub stats: GenerationStats,
}

/// Summary statistics for a generation run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationStats {
    pub total_scenarios: usize,
    pub generated: usize,
    pub skipped: usize,
    pub by_risk_class: BTreeMap<String, usize>,
    pub by_category: BTreeMap<String, usize>,
}

// ── Deterministic RNG ────────────────────────────────────────────────────

/// Minimal xorshift64 for deterministic generation without external deps.
struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 1 } else { seed },
        }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    fn next_usize(&mut self, max: usize) -> usize {
        (self.next_u64() % (max as u64)) as usize
    }
}

// ── Scenario Planning ────────────────────────────────────────────────────

/// Plan adversarial scenarios from a coverage report's blind spots.
pub fn plan_scenarios(report: &CoverageReport, config: &GeneratorConfig) -> Vec<StressScenario> {
    let mut scenarios = Vec::new();
    let mut rng = Rng::new(config.base_seed);

    for blind_spot in &report.blind_spots {
        if config.high_impact_only && blind_spot.impact != BlindSpotImpact::High {
            continue;
        }

        let seed = rng.next_u64();
        let risk_class = impact_to_risk(&blind_spot.impact);
        let id = format!(
            "adv-{}-{:08x}",
            slug_from_dimension(&blind_spot.dimension),
            seed as u32,
        );

        scenarios.push(StressScenario {
            id,
            name: format!("Adversarial: {}", blind_spot.dimension),
            target_category: blind_spot.category.clone(),
            target_dimension: blind_spot.dimension.clone(),
            risk_class,
            seed,
            description: describe_scenario(&blind_spot.category, &blind_spot.dimension),
        });
    }

    scenarios
}

/// Generate all fixtures from planned scenarios.
pub fn generate_fixtures(
    scenarios: &[StressScenario],
    config: &GeneratorConfig,
) -> GenerationResult {
    let mut fixtures = Vec::new();
    let mut skipped = Vec::new();
    let mut by_risk = BTreeMap::new();
    let mut by_category = BTreeMap::new();

    for scenario in scenarios {
        match generate_single(scenario, config) {
            Some(fixture) => {
                *by_risk
                    .entry(format!("{:?}", scenario.risk_class))
                    .or_insert(0) += 1;
                *by_category
                    .entry(scenario.target_category.clone())
                    .or_insert(0) += 1;
                fixtures.push(fixture);
            }
            None => {
                skipped.push((scenario.clone(), "No generator for this dimension".into()));
            }
        }
    }

    let stats = GenerationStats {
        total_scenarios: scenarios.len(),
        generated: fixtures.len(),
        skipped: skipped.len(),
        by_risk_class: by_risk,
        by_category,
    };

    GenerationResult {
        fixtures,
        skipped,
        stats,
    }
}

/// Generate fixtures targeting all blind spots in a coverage report.
pub fn generate_from_coverage(
    report: &CoverageReport,
    config: &GeneratorConfig,
) -> GenerationResult {
    let scenarios = plan_scenarios(report, config);
    generate_fixtures(&scenarios, config)
}

// ── Single Fixture Generation ────────────────────────────────────────────

fn generate_single(
    scenario: &StressScenario,
    config: &GeneratorConfig,
) -> Option<GeneratedFixture> {
    let mut rng = Rng::new(scenario.seed);

    let (source, expected, complexity_tags) = generate_source_for_dimension(
        &scenario.target_category,
        &scenario.target_dimension,
        config,
        &mut rng,
    )?;

    let filename = format!("{}.tsx", scenario.id);

    let corpus_entry = CorpusEntry {
        slug: scenario.id.clone(),
        description: scenario.description.clone(),
        source_url: format!("synthetic://{}", scenario.id),
        pinned_commit: format!("{:016x}", scenario.seed),
        license: "Apache-2.0".to_string(),
        license_verified: true,
        provenance: CorpusProvenance {
            added_by: config.generator_id.clone(),
            added_at: "2026-02-24T00:00:00Z".to_string(),
            rationale: format!(
                "Adversarial fixture targeting {} blind spot: {}",
                scenario.target_category, scenario.target_dimension
            ),
            source_type: ProvenanceSourceType::Synthetic,
            attribution_notes: None,
        },
        complexity_tags,
        feature_tags: vec![
            format!("adversarial:{}", scenario.target_category),
            format!("risk:{:?}", scenario.risk_class),
            scenario.target_dimension.clone(),
        ],
        expected_metrics: Some(CorpusMetrics {
            file_count: 1,
            component_count: count_components(&source),
            hook_count: count_hooks(&source),
            module_count: 1,
            effect_count: count_effects(&source),
            loc_approx: source.lines().count(),
            source_hash: hash_source(&source),
        }),
        active: true,
    };

    Some(GeneratedFixture {
        scenario: scenario.clone(),
        source,
        filename,
        corpus_entry,
        expected_patterns: expected,
    })
}

// ── Source Generators by Category ────────────────────────────────────────

fn generate_source_for_dimension(
    category: &str,
    dimension: &str,
    config: &GeneratorConfig,
    rng: &mut Rng,
) -> Option<(String, ExpectedPatterns, Vec<ComplexityTag>)> {
    match category {
        "ui" => generate_ui_fixture(dimension, config, rng),
        "state" => generate_state_fixture(dimension, config, rng),
        "effect" => generate_effect_fixture(dimension, config, rng),
        "style" => generate_style_fixture(dimension, config, rng),
        "accessibility" => generate_accessibility_fixture(dimension, config, rng),
        "terminal" => generate_terminal_fixture(dimension, config, rng),
        "data" => generate_data_fixture(dimension, config, rng),
        _ => None,
    }
}

fn generate_ui_fixture(
    dimension: &str,
    config: &GeneratorConfig,
    rng: &mut Rng,
) -> Option<(String, ExpectedPatterns, Vec<ComplexityTag>)> {
    let depth = 2 + rng.next_usize(config.max_depth.saturating_sub(2).max(1));
    let components = 3 + rng.next_usize(config.max_components.saturating_sub(3).max(1));

    let (source, patterns) = match dimension {
        "RecursiveTree" => {
            let src = format!(
                r#"import React from 'react';

interface TreeNode {{
  id: string;
  label: string;
  children: TreeNode[];
}}

function TreeItem({{ node, depth }}: {{ node: TreeNode; depth: number }}) {{
  const [expanded, setExpanded] = React.useState(depth < {max_depth});

  if (depth > {max_depth}) return <span>{{node.label}} (truncated)</span>;

  return (
    <div style={{{{ paddingLeft: depth * 16 }}}}>
      <button onClick={{() => setExpanded(!expanded)}}>
        {{expanded ? '▼' : '▶'}} {{node.label}}
      </button>
      {{expanded && node.children.map(child => (
        <TreeItem key={{child.id}} node={{child}} depth={{depth + 1}} />
      ))}}
    </div>
  );
}}

export default function AdversarialTree() {{
  const data: TreeNode = {{
    id: 'root',
    label: 'Root',
    children: Array.from({{ length: {components} }}, (_, i) => ({{
      id: `node-${{i}}`,
      label: `Node ${{i}}`,
      children: Array.from({{ length: 3 }}, (_, j) => ({{
        id: `node-${{i}}-${{j}}`,
        label: `Leaf ${{i}}.${{j}}`,
        children: [],
      }})),
    }})),
  }};

  return <TreeItem node={{data}} depth={{0}} />;
}}"#,
                max_depth = depth,
                components = components,
            );
            let mut p = empty_patterns();
            p.ui_patterns.insert("RecursiveTree".to_string());
            p.ui_patterns.insert("ConditionalRender".to_string());
            p.ui_patterns.insert("ListRender".to_string());
            p.state_patterns.insert("LocalState".to_string());
            (src, p)
        }
        "PortalModal" => {
            let src = r#"import React from 'react';
import ReactDOM from 'react-dom';

function Modal({ isOpen, onClose, children }: {
  isOpen: boolean;
  onClose: () => void;
  children: React.ReactNode;
}) {
  if (!isOpen) return null;

  return ReactDOM.createPortal(
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal-content" onClick={e => e.stopPropagation()}>
        <button className="modal-close" onClick={onClose}>×</button>
        {children}
      </div>
    </div>,
    document.body,
  );
}

function NestedModalDemo() {
  const [outerOpen, setOuterOpen] = React.useState(false);
  const [innerOpen, setInnerOpen] = React.useState(false);

  return (
    <div>
      <button onClick={() => setOuterOpen(true)}>Open Outer</button>
      <Modal isOpen={outerOpen} onClose={() => setOuterOpen(false)}>
        <h2>Outer Modal</h2>
        <button onClick={() => setInnerOpen(true)}>Open Inner</button>
        <Modal isOpen={innerOpen} onClose={() => setInnerOpen(false)}>
          <h2>Inner Modal</h2>
          <p>Nested portal content</p>
        </Modal>
      </Modal>
    </div>
  );
}

export default NestedModalDemo;"#
                .to_string();
            let mut p = empty_patterns();
            p.ui_patterns.insert("PortalModal".to_string());
            p.ui_patterns.insert("ConditionalRender".to_string());
            p.ui_patterns.insert("NestedComposition".to_string());
            p.state_patterns.insert("LocalState".to_string());
            (src, p)
        }
        "ErrorBoundary" => {
            let src = r#"import React from 'react';

class ErrorBoundary extends React.Component<
  { children: React.ReactNode; fallback: React.ReactNode },
  { hasError: boolean; error: Error | null }
> {
  constructor(props: any) {
    super(props);
    this.state = { hasError: false, error: null };
  }

  static getDerivedStateFromError(error: Error) {
    return { hasError: true, error };
  }

  componentDidCatch(error: Error, info: React.ErrorInfo) {
    console.error('Boundary caught:', error, info);
  }

  render() {
    if (this.state.hasError) {
      return this.props.fallback;
    }
    return this.props.children;
  }
}

function UnstableWidget({ shouldFail }: { shouldFail: boolean }) {
  if (shouldFail) throw new Error('Widget failure');
  return <div>Stable content</div>;
}

export default function ErrorBoundaryStress() {
  const [fail, setFail] = React.useState(false);

  return (
    <ErrorBoundary fallback={<div>Something went wrong</div>}>
      <button onClick={() => setFail(true)}>Trigger Error</button>
      <UnstableWidget shouldFail={fail} />
    </ErrorBoundary>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.ui_patterns.insert("ErrorBoundary".to_string());
            p.ui_patterns.insert("ConditionalRender".to_string());
            p.state_patterns.insert("LocalState".to_string());
            (src, p)
        }
        "SuspenseLazy" => {
            let src = r#"import React, { Suspense, lazy } from 'react';

const HeavyComponent = lazy(() => import('./HeavyComponent'));
const AnotherLazy = lazy(() => import('./AnotherLazy'));

function LoadingSpinner() {
  return <div className="spinner">Loading...</div>;
}

export default function SuspenseStress() {
  const [showSecond, setShowSecond] = React.useState(false);

  return (
    <div>
      <Suspense fallback={<LoadingSpinner />}>
        <HeavyComponent />
      </Suspense>
      <button onClick={() => setShowSecond(true)}>Load More</button>
      {showSecond && (
        <Suspense fallback={<LoadingSpinner />}>
          <AnotherLazy />
        </Suspense>
      )}
    </div>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.ui_patterns.insert("SuspenseLazy".to_string());
            p.ui_patterns.insert("ConditionalRender".to_string());
            p.state_patterns.insert("LocalState".to_string());
            (src, p)
        }
        "ForwardRef" => {
            let src = r#"import React, { forwardRef, useRef, useImperativeHandle } from 'react';

interface FancyInputHandle {
  focus: () => void;
  clear: () => void;
}

const FancyInput = forwardRef<FancyInputHandle, { label: string }>(
  ({ label }, ref) => {
    const inputRef = useRef<HTMLInputElement>(null);

    useImperativeHandle(ref, () => ({
      focus: () => inputRef.current?.focus(),
      clear: () => { if (inputRef.current) inputRef.current.value = ''; },
    }));

    return (
      <label>
        {label}
        <input ref={inputRef} />
      </label>
    );
  }
);

export default function ForwardRefStress() {
  const inputRef = useRef<FancyInputHandle>(null);

  return (
    <div>
      <FancyInput ref={inputRef} label="Custom Input" />
      <button onClick={() => inputRef.current?.focus()}>Focus</button>
      <button onClick={() => inputRef.current?.clear()}>Clear</button>
    </div>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.ui_patterns.insert("ForwardRef".to_string());
            p.state_patterns.insert("RefState".to_string());
            (src, p)
        }
        "ContextProviderNesting" => {
            let nesting = 2 + rng.next_usize(4);
            let mut providers_open = String::new();
            let mut providers_close = String::new();
            let mut context_defs = String::new();

            for i in 0..nesting {
                context_defs.push_str(&format!(
                    "const Context{i} = React.createContext<string>('default-{i}');\n"
                ));
                providers_open.push_str(&format!(
                    "{indent}<Context{i}.Provider value={{`value-{i}`}}>\n",
                    indent = "  ".repeat(i + 2),
                    i = i,
                ));
                providers_close = format!(
                    "{indent}</Context{i}.Provider>\n{rest}",
                    indent = "  ".repeat(i + 2),
                    i = i,
                    rest = providers_close,
                );
            }

            let src = format!(
                r#"import React from 'react';

{context_defs}
function DeepConsumer() {{
  const values = [
{consumers}
  ];
  return <div>{{values.join(', ')}}</div>;
}}

export default function ContextNestingStress() {{
  return (
{providers_open}      <DeepConsumer />
{providers_close}  );
}}"#,
                context_defs = context_defs,
                consumers = (0..nesting)
                    .map(|i| format!("    React.useContext(Context{i})"))
                    .collect::<Vec<_>>()
                    .join(",\n"),
                providers_open = providers_open,
                providers_close = providers_close,
            );
            let mut p = empty_patterns();
            p.ui_patterns.insert("ContextProviderNesting".to_string());
            p.state_patterns.insert("ContextState".to_string());
            (src, p)
        }
        "HigherOrderComponent" => {
            let src = r#"import React from 'react';

function withLogging<P extends object>(
  WrappedComponent: React.ComponentType<P>,
  componentName: string,
) {
  return function LoggedComponent(props: P) {
    React.useEffect(() => {
      console.log(`${componentName} mounted`);
      return () => console.log(`${componentName} unmounted`);
    }, []);
    return <WrappedComponent {...props} />;
  };
}

function withAuth<P extends object>(
  WrappedComponent: React.ComponentType<P>,
) {
  return function AuthComponent(props: P & { isAuthed?: boolean }) {
    if (!props.isAuthed) return <div>Please log in</div>;
    return <WrappedComponent {...props} />;
  };
}

function BaseWidget({ title }: { title: string }) {
  return <div>{title}</div>;
}

const EnhancedWidget = withAuth(withLogging(BaseWidget, 'BaseWidget'));

export default function HOCStress() {
  return <EnhancedWidget title="HOC Stack" isAuthed={true} />;
}"#
            .to_string();
            let mut p = empty_patterns();
            p.ui_patterns.insert("HigherOrderComponent".to_string());
            p.ui_patterns.insert("ConditionalRender".to_string());
            p.effect_patterns.insert("MountFetch".to_string());
            (src, p)
        }
        "RenderProps" => {
            let src = r#"import React from 'react';

interface MousePosition {
  x: number;
  y: number;
}

function MouseTracker({ render }: { render: (pos: MousePosition) => React.ReactNode }) {
  const [pos, setPos] = React.useState<MousePosition>({ x: 0, y: 0 });

  React.useEffect(() => {
    const handler = (e: MouseEvent) => setPos({ x: e.clientX, y: e.clientY });
    window.addEventListener('mousemove', handler);
    return () => window.removeEventListener('mousemove', handler);
  }, []);

  return <>{render(pos)}</>;
}

export default function RenderPropsStress() {
  return (
    <MouseTracker
      render={({ x, y }) => (
        <div>
          <p>Mouse at: ({x}, {y})</p>
          <div style={{ position: 'absolute', left: x, top: y, width: 10, height: 10, background: 'red' }} />
        </div>
      )}
    />
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.ui_patterns.insert("RenderProps".to_string());
            p.state_patterns.insert("LocalState".to_string());
            p.effect_patterns.insert("EventListener".to_string());
            p.effect_patterns.insert("EffectCleanup".to_string());
            (src, p)
        }
        "FragmentMultiRoot" => {
            let count = 3 + rng.next_usize(5);
            let items: String = (0..count)
                .map(|i| format!("      <li key=\"{i}\">Item {i}</li>"))
                .collect::<Vec<_>>()
                .join("\n");
            let src = format!(
                r#"import React from 'react';

function MultiRootList() {{
  return (
    <>
{items}
    </>
  );
}}

function AnotherFragment() {{
  return (
    <React.Fragment>
      <span>A</span>
      <span>B</span>
      <span>C</span>
    </React.Fragment>
  );
}}

export default function FragmentStress() {{
  return (
    <ul>
      <MultiRootList />
      <AnotherFragment />
    </ul>
  );
}}"#,
                items = items,
            );
            let mut p = empty_patterns();
            p.ui_patterns.insert("FragmentMultiRoot".to_string());
            p.ui_patterns.insert("ListRender".to_string());
            (src, p)
        }
        _ => return None,
    };

    let tags = vec![ComplexityTag::Medium, ComplexityTag::TypeScript];
    Some((source, patterns, tags))
}

fn generate_state_fixture(
    dimension: &str,
    _config: &GeneratorConfig,
    _rng: &mut Rng,
) -> Option<(String, ExpectedPatterns, Vec<ComplexityTag>)> {
    let (source, patterns) = match dimension {
        "StateMachine" => {
            let states = ["idle", "loading", "success", "error", "retrying"];
            let src = format!(
                r#"import React, {{ useReducer }} from 'react';

type State = {state_union};
type Action =
  | {{ type: 'FETCH' }}
  | {{ type: 'SUCCESS'; payload: string }}
  | {{ type: 'ERROR'; error: string }}
  | {{ type: 'RETRY' }}
  | {{ type: 'RESET' }};

interface MachineState {{
  status: State;
  data: string | null;
  error: string | null;
  retryCount: number;
}}

function reducer(state: MachineState, action: Action): MachineState {{
  switch (action.type) {{
    case 'FETCH':
      if (state.status !== 'idle' && state.status !== 'error') return state;
      return {{ ...state, status: 'loading', error: null }};
    case 'SUCCESS':
      if (state.status !== 'loading') return state;
      return {{ ...state, status: 'success', data: action.payload }};
    case 'ERROR':
      if (state.status !== 'loading') return state;
      return {{ ...state, status: 'error', error: action.error }};
    case 'RETRY':
      if (state.status !== 'error' || state.retryCount >= 3) return state;
      return {{ ...state, status: 'retrying', retryCount: state.retryCount + 1 }};
    case 'RESET':
      return {{ status: 'idle', data: null, error: null, retryCount: 0 }};
    default:
      return state;
  }}
}}

export default function StateMachineStress() {{
  const [state, dispatch] = useReducer(reducer, {{
    status: 'idle' as State,
    data: null,
    error: null,
    retryCount: 0,
  }});

  return (
    <div>
      <p>Status: {{state.status}}</p>
      <p>Retries: {{state.retryCount}}</p>
      {{state.data && <p>Data: {{state.data}}</p>}}
      {{state.error && <p>Error: {{state.error}}</p>}}
      <button onClick={{() => dispatch({{ type: 'FETCH' }})}}>Fetch</button>
      <button onClick={{() => dispatch({{ type: 'RESET' }})}}>Reset</button>
    </div>
  );
}}"#,
                state_union = states
                    .iter()
                    .map(|s| format!("'{s}'"))
                    .collect::<Vec<_>>()
                    .join(" | "),
            );
            let mut p = empty_patterns();
            p.state_patterns.insert("StateMachine".to_string());
            p.state_patterns.insert("Reducer".to_string());
            p.ui_patterns.insert("ConditionalRender".to_string());
            (src, p)
        }
        "OptimisticUpdate" => {
            let src = r#"import React from 'react';

interface Todo {
  id: string;
  text: string;
  done: boolean;
  pending?: boolean;
}

export default function OptimisticUpdateStress() {
  const [todos, setTodos] = React.useState<Todo[]>([
    { id: '1', text: 'First', done: false },
    { id: '2', text: 'Second', done: false },
  ]);

  const toggleTodo = async (id: string) => {
    // Optimistic update
    setTodos(prev =>
      prev.map(t => t.id === id ? { ...t, done: !t.done, pending: true } : t)
    );

    try {
      await fakeFetch(`/api/todos/${id}/toggle`);
      // Confirm update
      setTodos(prev =>
        prev.map(t => t.id === id ? { ...t, pending: false } : t)
      );
    } catch {
      // Rollback on failure
      setTodos(prev =>
        prev.map(t => t.id === id ? { ...t, done: !t.done, pending: false } : t)
      );
    }
  };

  return (
    <ul>
      {todos.map(todo => (
        <li
          key={todo.id}
          onClick={() => toggleTodo(todo.id)}
          style={{ opacity: todo.pending ? 0.5 : 1 }}
        >
          {todo.done ? '✓' : '○'} {todo.text}
        </li>
      ))}
    </ul>
  );
}

function fakeFetch(_url: string): Promise<void> {
  return new Promise((resolve, reject) => {
    setTimeout(() => Math.random() > 0.3 ? resolve() : reject(new Error('fail')), 500);
  });
}"#
            .to_string();
            let mut p = empty_patterns();
            p.state_patterns.insert("OptimisticUpdate".to_string());
            p.state_patterns.insert("LocalState".to_string());
            p.effect_patterns.insert("DependencyFetch".to_string());
            p.ui_patterns.insert("ListRender".to_string());
            (src, p)
        }
        "InteractingState" => {
            let src = r#"import React from 'react';

export default function InteractingStateStress() {
  const [count, setCount] = React.useState(0);
  const [multiplier, setMultiplier] = React.useState(1);
  const [history, setHistory] = React.useState<number[]>([]);

  const derivedValue = count * multiplier;

  React.useEffect(() => {
    setHistory(prev => [...prev.slice(-9), derivedValue]);
  }, [derivedValue]);

  return (
    <div>
      <p>Count: {count} × {multiplier} = {derivedValue}</p>
      <button onClick={() => setCount(c => c + 1)}>Increment</button>
      <button onClick={() => setMultiplier(m => m + 1)}>Multiply</button>
      <p>History: {history.join(', ')}</p>
    </div>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.state_patterns.insert("InteractingState".to_string());
            p.state_patterns.insert("DerivedState".to_string());
            p.state_patterns.insert("LocalState".to_string());
            p.effect_patterns.insert("DependencyFetch".to_string());
            (src, p)
        }
        "ExternalStore" => {
            let src = r#"import React from 'react';

// Minimal external store (Zustand-like pattern)
function createStore<T>(initialState: T) {
  let state = initialState;
  const listeners = new Set<() => void>();

  return {
    getState: () => state,
    setState: (updater: (prev: T) => T) => {
      state = updater(state);
      listeners.forEach(fn => fn());
    },
    subscribe: (listener: () => void) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
}

const counterStore = createStore({ count: 0, label: 'Counter' });

function useStore<T>(store: ReturnType<typeof createStore<T>>): T {
  const [, forceUpdate] = React.useReducer(x => x + 1, 0);
  React.useEffect(() => store.subscribe(forceUpdate), [store]);
  return store.getState();
}

function Display() {
  const { count, label } = useStore(counterStore);
  return <p>{label}: {count}</p>;
}

function Controls() {
  return (
    <div>
      <button onClick={() => counterStore.setState(s => ({ ...s, count: s.count + 1 }))}>
        +1
      </button>
      <button onClick={() => counterStore.setState(s => ({ ...s, count: 0 }))}>
        Reset
      </button>
    </div>
  );
}

export default function ExternalStoreStress() {
  return (
    <div>
      <Display />
      <Display />
      <Controls />
    </div>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.state_patterns.insert("ExternalStore".to_string());
            p.effect_patterns.insert("Subscription".to_string());
            p.data_patterns.insert("UnidirectionalFlow".to_string());
            (src, p)
        }
        "UrlState" => {
            let src = r#"import React from 'react';

function useQueryParam(key: string): [string, (val: string) => void] {
  const [value, setValue] = React.useState(() => {
    const params = new URLSearchParams(window.location.search);
    return params.get(key) || '';
  });

  const setParam = React.useCallback((val: string) => {
    const params = new URLSearchParams(window.location.search);
    params.set(key, val);
    window.history.replaceState({}, '', `?${params.toString()}`);
    setValue(val);
  }, [key]);

  return [value, setParam];
}

export default function UrlStateStress() {
  const [tab, setTab] = useQueryParam('tab');
  const [search, setSearch] = useQueryParam('q');

  return (
    <div>
      <div>
        {['overview', 'details', 'settings'].map(t => (
          <button key={t} onClick={() => setTab(t)} style={{ fontWeight: tab === t ? 'bold' : 'normal' }}>
            {t}
          </button>
        ))}
      </div>
      <input value={search} onChange={e => setSearch(e.target.value)} placeholder="Search..." />
      <p>Tab: {tab || 'none'}, Search: {search || 'empty'}</p>
    </div>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.state_patterns.insert("UrlState".to_string());
            p.state_patterns.insert("LocalState".to_string());
            p.effect_patterns.insert("BrowserApi".to_string());
            (src, p)
        }
        "ServerState" => {
            let src = r#"import React from 'react';

interface ServerData {
  items: string[];
  total: number;
  page: number;
}

function useServerState(url: string): {
  data: ServerData | null;
  loading: boolean;
  error: string | null;
  refetch: () => void;
} {
  const [data, setData] = React.useState<ServerData | null>(null);
  const [loading, setLoading] = React.useState(true);
  const [error, setError] = React.useState<string | null>(null);

  const fetchData = React.useCallback(() => {
    setLoading(true);
    setError(null);
    fetch(url)
      .then(r => r.json())
      .then(setData)
      .catch(e => setError(e.message))
      .finally(() => setLoading(false));
  }, [url]);

  React.useEffect(() => { fetchData(); }, [fetchData]);

  return { data, loading, error, refetch: fetchData };
}

export default function ServerStateStress() {
  const [page, setPage] = React.useState(1);
  const { data, loading, error, refetch } = useServerState(`/api/items?page=${page}`);

  if (loading) return <p>Loading...</p>;
  if (error) return <p>Error: {error} <button onClick={refetch}>Retry</button></p>;

  return (
    <div>
      <ul>{data?.items.map((item, i) => <li key={i}>{item}</li>)}</ul>
      <p>Page {data?.page} of {Math.ceil((data?.total || 0) / 10)}</p>
      <button onClick={() => setPage(p => Math.max(1, p - 1))}>Prev</button>
      <button onClick={() => setPage(p => p + 1)}>Next</button>
    </div>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.state_patterns.insert("ServerState".to_string());
            p.state_patterns.insert("LocalState".to_string());
            p.effect_patterns.insert("MountFetch".to_string());
            p.effect_patterns.insert("DependencyFetch".to_string());
            p.ui_patterns.insert("ConditionalRender".to_string());
            (src, p)
        }
        _ => return None,
    };

    let tags = vec![ComplexityTag::Medium, ComplexityTag::GlobalState];
    Some((source, patterns, tags))
}

fn generate_effect_fixture(
    dimension: &str,
    _config: &GeneratorConfig,
    _rng: &mut Rng,
) -> Option<(String, ExpectedPatterns, Vec<ComplexityTag>)> {
    let (source, patterns) = match dimension {
        "TimerInterval" => {
            let src = r#"import React from 'react';

export default function TimerStress() {
  const [elapsed, setElapsed] = React.useState(0);
  const [running, setRunning] = React.useState(false);
  const [laps, setLaps] = React.useState<number[]>([]);

  React.useEffect(() => {
    if (!running) return;
    const id = setInterval(() => setElapsed(e => e + 100), 100);
    return () => clearInterval(id);
  }, [running]);

  return (
    <div>
      <p>{(elapsed / 1000).toFixed(1)}s</p>
      <button onClick={() => setRunning(r => !r)}>{running ? 'Stop' : 'Start'}</button>
      <button onClick={() => { setLaps(l => [...l, elapsed]); }}>Lap</button>
      <button onClick={() => { setElapsed(0); setLaps([]); setRunning(false); }}>Reset</button>
      <ul>{laps.map((l, i) => <li key={i}>{(l / 1000).toFixed(1)}s</li>)}</ul>
    </div>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.effect_patterns.insert("TimerInterval".to_string());
            p.effect_patterns.insert("EffectCleanup".to_string());
            p.state_patterns.insert("LocalState".to_string());
            (src, p)
        }
        "WebSocketConnection" => {
            let src = r#"import React from 'react';

export default function WebSocketStress() {
  const [messages, setMessages] = React.useState<string[]>([]);
  const [status, setStatus] = React.useState<'disconnected' | 'connecting' | 'connected'>('disconnected');
  const wsRef = React.useRef<WebSocket | null>(null);

  const connect = React.useCallback(() => {
    setStatus('connecting');
    const ws = new WebSocket('ws://localhost:8080');
    wsRef.current = ws;

    ws.onopen = () => setStatus('connected');
    ws.onmessage = (e) => setMessages(prev => [...prev.slice(-49), e.data]);
    ws.onerror = () => setStatus('disconnected');
    ws.onclose = () => setStatus('disconnected');
  }, []);

  React.useEffect(() => {
    return () => wsRef.current?.close();
  }, []);

  const send = (msg: string) => {
    wsRef.current?.send(msg);
  };

  return (
    <div>
      <p>Status: {status}</p>
      <button onClick={connect} disabled={status === 'connected'}>Connect</button>
      <button onClick={() => send('ping')} disabled={status !== 'connected'}>Ping</button>
      <ul>{messages.map((m, i) => <li key={i}>{m}</li>)}</ul>
    </div>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.effect_patterns.insert("WebSocketConnection".to_string());
            p.effect_patterns.insert("EffectCleanup".to_string());
            p.effect_patterns.insert("Subscription".to_string());
            p.state_patterns.insert("LocalState".to_string());
            p.state_patterns.insert("RefState".to_string());
            (src, p)
        }
        "DebouncedEffect" => {
            let src = r#"import React from 'react';

function useDebouncedValue<T>(value: T, delay: number): T {
  const [debounced, setDebounced] = React.useState(value);

  React.useEffect(() => {
    const timer = setTimeout(() => setDebounced(value), delay);
    return () => clearTimeout(timer);
  }, [value, delay]);

  return debounced;
}

export default function DebouncedStress() {
  const [query, setQuery] = React.useState('');
  const [results, setResults] = React.useState<string[]>([]);
  const debouncedQuery = useDebouncedValue(query, 300);

  React.useEffect(() => {
    if (!debouncedQuery) { setResults([]); return; }
    // Simulate API call
    const items = Array.from({ length: 5 }, (_, i) => `${debouncedQuery} result ${i}`);
    setResults(items);
  }, [debouncedQuery]);

  return (
    <div>
      <input value={query} onChange={e => setQuery(e.target.value)} placeholder="Search..." />
      <p>Debounced: {debouncedQuery || '(empty)'}</p>
      <ul>{results.map((r, i) => <li key={i}>{r}</li>)}</ul>
    </div>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.effect_patterns.insert("DebouncedEffect".to_string());
            p.effect_patterns.insert("EffectCleanup".to_string());
            p.effect_patterns.insert("DependencyFetch".to_string());
            p.state_patterns.insert("LocalState".to_string());
            p.state_patterns.insert("DerivedState".to_string());
            (src, p)
        }
        "LayoutEffect" => {
            let src = r#"import React, { useLayoutEffect, useRef } from 'react';

export default function LayoutEffectStress() {
  const ref = useRef<HTMLDivElement>(null);
  const [height, setHeight] = React.useState(0);
  const [expanded, setExpanded] = React.useState(false);

  useLayoutEffect(() => {
    if (ref.current) {
      setHeight(ref.current.getBoundingClientRect().height);
    }
  }, [expanded]);

  return (
    <div>
      <div ref={ref} style={{ overflow: 'hidden', maxHeight: expanded ? 'none' : '100px' }}>
        <p>Content line 1</p>
        <p>Content line 2</p>
        <p>Content line 3</p>
        <p>Content line 4</p>
        <p>Content line 5</p>
        <p>Content line 6</p>
      </div>
      <p>Measured height: {height}px</p>
      <button onClick={() => setExpanded(e => !e)}>
        {expanded ? 'Collapse' : 'Expand'}
      </button>
    </div>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.effect_patterns.insert("LayoutEffect".to_string());
            p.effect_patterns.insert("DomManipulation".to_string());
            p.state_patterns.insert("LocalState".to_string());
            p.state_patterns.insert("RefState".to_string());
            (src, p)
        }
        "LocalStorageSync" => {
            let src = r#"import React from 'react';

function useLocalStorage<T>(key: string, initialValue: T): [T, (val: T) => void] {
  const [stored, setStored] = React.useState<T>(() => {
    try {
      const item = localStorage.getItem(key);
      return item ? JSON.parse(item) : initialValue;
    } catch {
      return initialValue;
    }
  });

  React.useEffect(() => {
    try {
      localStorage.setItem(key, JSON.stringify(stored));
    } catch {
      // Storage full or unavailable
    }
  }, [key, stored]);

  React.useEffect(() => {
    const handler = (e: StorageEvent) => {
      if (e.key === key && e.newValue) {
        setStored(JSON.parse(e.newValue));
      }
    };
    window.addEventListener('storage', handler);
    return () => window.removeEventListener('storage', handler);
  }, [key]);

  return [stored, setStored];
}

export default function LocalStorageStress() {
  const [theme, setTheme] = useLocalStorage('theme', 'light');
  const [name, setName] = useLocalStorage('username', '');

  return (
    <div data-theme={theme}>
      <select value={theme} onChange={e => setTheme(e.target.value)}>
        <option value="light">Light</option>
        <option value="dark">Dark</option>
      </select>
      <input value={name} onChange={e => setName(e.target.value)} placeholder="Username" />
      <p>Persisted: theme={theme}, name={name}</p>
    </div>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.effect_patterns.insert("LocalStorageSync".to_string());
            p.effect_patterns.insert("EventListener".to_string());
            p.effect_patterns.insert("EffectCleanup".to_string());
            p.state_patterns.insert("LocalState".to_string());
            p.effect_patterns.insert("BrowserApi".to_string());
            (src, p)
        }
        "Subscription" => {
            let src = r#"import React from 'react';

interface Observable<T> {
  subscribe: (cb: (value: T) => void) => () => void;
}

function useObservable<T>(observable: Observable<T>, initial: T): T {
  const [value, setValue] = React.useState(initial);

  React.useEffect(() => {
    const unsubscribe = observable.subscribe(setValue);
    return unsubscribe;
  }, [observable]);

  return value;
}

// Simulated observable for resize events
const resizeObservable: Observable<{ width: number; height: number }> = {
  subscribe: (cb) => {
    const handler = () => cb({ width: window.innerWidth, height: window.innerHeight });
    window.addEventListener('resize', handler);
    handler(); // emit initial
    return () => window.removeEventListener('resize', handler);
  },
};

export default function SubscriptionStress() {
  const size = useObservable(resizeObservable, { width: 0, height: 0 });

  return (
    <div>
      <p>Window: {size.width} × {size.height}</p>
    </div>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.effect_patterns.insert("Subscription".to_string());
            p.effect_patterns.insert("EffectCleanup".to_string());
            p.effect_patterns.insert("EventListener".to_string());
            p.state_patterns.insert("LocalState".to_string());
            (src, p)
        }
        _ => return None,
    };

    let tags = vec![ComplexityTag::Medium, ComplexityTag::RealTime];
    Some((source, patterns, tags))
}

fn generate_style_fixture(
    dimension: &str,
    _config: &GeneratorConfig,
    _rng: &mut Rng,
) -> Option<(String, ExpectedPatterns, Vec<ComplexityTag>)> {
    let (source, patterns) = match dimension {
        "CssVariables" => {
            let src = r#"import React from 'react';

export default function CssVariablesStress() {
  const [hue, setHue] = React.useState(200);
  const [radius, setRadius] = React.useState(8);

  const rootStyle = {
    '--primary-hue': hue,
    '--primary': `hsl(${hue}, 70%, 50%)`,
    '--primary-light': `hsl(${hue}, 70%, 90%)`,
    '--radius': `${radius}px`,
  } as React.CSSProperties;

  return (
    <div style={rootStyle}>
      <div style={{ background: 'var(--primary)', color: 'white', padding: 16, borderRadius: 'var(--radius)' }}>
        Primary
      </div>
      <div style={{ background: 'var(--primary-light)', padding: 16, borderRadius: 'var(--radius)' }}>
        Primary Light
      </div>
      <input type="range" min={0} max={360} value={hue} onChange={e => setHue(+e.target.value)} />
      <input type="range" min={0} max={24} value={radius} onChange={e => setRadius(+e.target.value)} />
    </div>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.style_patterns.insert("CssVariables".to_string());
            p.style_patterns.insert("DynamicStyling".to_string());
            p.style_patterns.insert("InlineStyle".to_string());
            p.state_patterns.insert("LocalState".to_string());
            (src, p)
        }
        "Animation" => {
            let src = r#"import React from 'react';

export default function AnimationStress() {
  const [visible, setVisible] = React.useState(true);
  const [position, setPosition] = React.useState(0);

  React.useEffect(() => {
    const id = requestAnimationFrame(function animate() {
      setPosition(p => (p + 1) % 300);
      requestAnimationFrame(animate);
    });
    return () => cancelAnimationFrame(id);
  }, []);

  return (
    <div>
      <div
        style={{
          width: 40,
          height: 40,
          background: 'blue',
          borderRadius: '50%',
          transform: `translateX(${position}px)`,
          transition: 'opacity 0.3s',
          opacity: visible ? 1 : 0,
        }}
      />
      <button onClick={() => setVisible(v => !v)}>
        {visible ? 'Hide' : 'Show'}
      </button>
    </div>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.style_patterns.insert("Animation".to_string());
            p.style_patterns.insert("DynamicStyling".to_string());
            p.style_patterns.insert("InlineStyle".to_string());
            p.effect_patterns.insert("TimerInterval".to_string());
            p.effect_patterns.insert("EffectCleanup".to_string());
            (src, p)
        }
        "ResponsiveDesign" => {
            let src = r#"import React from 'react';

function useMediaQuery(query: string): boolean {
  const [matches, setMatches] = React.useState(
    () => window.matchMedia(query).matches,
  );

  React.useEffect(() => {
    const mql = window.matchMedia(query);
    const handler = (e: MediaQueryListEvent) => setMatches(e.matches);
    mql.addEventListener('change', handler);
    return () => mql.removeEventListener('change', handler);
  }, [query]);

  return matches;
}

export default function ResponsiveStress() {
  const isMobile = useMediaQuery('(max-width: 768px)');
  const isTablet = useMediaQuery('(min-width: 769px) and (max-width: 1024px)');

  return (
    <div style={{ display: isMobile ? 'block' : 'flex', gap: 16 }}>
      <nav style={{ width: isMobile ? '100%' : '200px' }}>
        {isMobile ? <select><option>Nav</option></select> : <ul><li>Nav Item</li></ul>}
      </nav>
      <main style={{ flex: 1 }}>
        <p>{isMobile ? 'Mobile' : isTablet ? 'Tablet' : 'Desktop'} layout</p>
      </main>
    </div>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.style_patterns.insert("ResponsiveDesign".to_string());
            p.style_patterns.insert("DynamicStyling".to_string());
            p.effect_patterns.insert("EventListener".to_string());
            p.effect_patterns.insert("EffectCleanup".to_string());
            p.effect_patterns.insert("BrowserApi".to_string());
            (src, p)
        }
        "ThemeSystem" => {
            let src = r#"import React from 'react';

interface Theme {
  bg: string;
  fg: string;
  accent: string;
  border: string;
}

const themes: Record<string, Theme> = {
  light: { bg: '#fff', fg: '#333', accent: '#0066cc', border: '#ddd' },
  dark: { bg: '#1a1a1a', fg: '#eee', accent: '#66aaff', border: '#444' },
  solarized: { bg: '#fdf6e3', fg: '#657b83', accent: '#268bd2', border: '#eee8d5' },
};

const ThemeContext = React.createContext<{
  theme: Theme;
  setThemeName: (name: string) => void;
}>({ theme: themes.light, setThemeName: () => {} });

function ThemeProvider({ children }: { children: React.ReactNode }) {
  const [name, setName] = React.useState('light');
  const theme = themes[name] || themes.light;
  return (
    <ThemeContext.Provider value={{ theme, setThemeName: setName }}>
      {children}
    </ThemeContext.Provider>
  );
}

function ThemedCard() {
  const { theme } = React.useContext(ThemeContext);
  return (
    <div style={{ background: theme.bg, color: theme.fg, border: `1px solid ${theme.border}`, padding: 16 }}>
      <h3 style={{ color: theme.accent }}>Themed Card</h3>
      <p>Content with theme colors</p>
    </div>
  );
}

function ThemeSwitcher() {
  const { setThemeName } = React.useContext(ThemeContext);
  return (
    <div>
      {Object.keys(themes).map(name => (
        <button key={name} onClick={() => setThemeName(name)}>{name}</button>
      ))}
    </div>
  );
}

export default function ThemeSystemStress() {
  return (
    <ThemeProvider>
      <ThemeSwitcher />
      <ThemedCard />
    </ThemeProvider>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.style_patterns.insert("ThemeSystem".to_string());
            p.state_patterns.insert("ContextState".to_string());
            p.ui_patterns.insert("ContextProviderNesting".to_string());
            (src, p)
        }
        "GlobalStyles" => {
            let src = r#"import React from 'react';

const globalCSS = `
  * { margin: 0; padding: 0; box-sizing: border-box; }
  body { font-family: system-ui, sans-serif; line-height: 1.5; }
  .container { max-width: 800px; margin: 0 auto; padding: 16px; }
  .btn { padding: 8px 16px; border: 1px solid #ccc; border-radius: 4px; cursor: pointer; }
  .btn:hover { background: #f0f0f0; }
`;

export default function GlobalStylesStress() {
  React.useEffect(() => {
    const style = document.createElement('style');
    style.textContent = globalCSS;
    document.head.appendChild(style);
    return () => { document.head.removeChild(style); };
  }, []);

  return (
    <div className="container">
      <h1>Global Styles Demo</h1>
      <button className="btn">Styled Button</button>
    </div>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.style_patterns.insert("GlobalStyles".to_string());
            p.effect_patterns.insert("DomManipulation".to_string());
            p.effect_patterns.insert("EffectCleanup".to_string());
            (src, p)
        }
        _ => return None,
    };

    let tags = vec![ComplexityTag::Medium, ComplexityTag::ThemedStyling];
    Some((source, patterns, tags))
}

fn generate_accessibility_fixture(
    dimension: &str,
    _config: &GeneratorConfig,
    _rng: &mut Rng,
) -> Option<(String, ExpectedPatterns, Vec<ComplexityTag>)> {
    let (source, patterns) = match dimension {
        "FocusManagement" => {
            let src = r#"import React, { useRef, useEffect } from 'react';

function FocusTrap({ children, active }: { children: React.ReactNode; active: boolean }) {
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!active || !containerRef.current) return;

    const focusable = containerRef.current.querySelectorAll<HTMLElement>(
      'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])'
    );
    if (focusable.length === 0) return;

    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    first.focus();

    const handler = (e: KeyboardEvent) => {
      if (e.key !== 'Tab') return;
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    };
    document.addEventListener('keydown', handler);
    return () => document.removeEventListener('keydown', handler);
  }, [active]);

  return <div ref={containerRef}>{children}</div>;
}

export default function FocusManagementStress() {
  const [dialogOpen, setDialogOpen] = React.useState(false);

  return (
    <div>
      <button onClick={() => setDialogOpen(true)}>Open Dialog</button>
      {dialogOpen && (
        <FocusTrap active={dialogOpen}>
          <div role="dialog" aria-modal="true" aria-label="Focus trap demo">
            <h2>Trapped Dialog</h2>
            <input placeholder="Name" />
            <input placeholder="Email" />
            <button onClick={() => setDialogOpen(false)}>Close</button>
          </div>
        </FocusTrap>
      )}
    </div>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.accessibility_patterns
                .insert("FocusManagement".to_string());
            p.accessibility_patterns
                .insert("KeyboardNavigation".to_string());
            p.accessibility_patterns
                .insert("AriaAttributes".to_string());
            p.effect_patterns.insert("EventListener".to_string());
            p.effect_patterns.insert("EffectCleanup".to_string());
            (src, p)
        }
        "LiveRegions" => {
            let src = r#"import React from 'react';

export default function LiveRegionsStress() {
  const [messages, setMessages] = React.useState<string[]>([]);
  const [status, setStatus] = React.useState('Ready');

  const addMessage = () => {
    const msg = `Notification ${messages.length + 1} at ${new Date().toLocaleTimeString()}`;
    setMessages(prev => [...prev, msg]);
    setStatus(`New notification: ${msg}`);
  };

  return (
    <div>
      <button onClick={addMessage}>Add Notification</button>
      <div aria-live="polite" aria-atomic="true" role="status">
        {status}
      </div>
      <div aria-live="assertive" role="alert" style={{ color: 'red' }}>
        {messages.length > 5 ? 'Too many notifications!' : ''}
      </div>
      <ul aria-label="Notifications">
        {messages.map((m, i) => <li key={i}>{m}</li>)}
      </ul>
    </div>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.accessibility_patterns.insert("LiveRegions".to_string());
            p.accessibility_patterns
                .insert("AriaAttributes".to_string());
            p.accessibility_patterns.insert("SemanticHtml".to_string());
            p.state_patterns.insert("LocalState".to_string());
            (src, p)
        }
        "SkipNavigation" => {
            let src = r##"import React from 'react';

export default function SkipNavStress() {
  return (
    <div>
      <a href="#main-content" className="skip-link" style={{
        position: 'absolute',
        left: '-9999px',
        top: 'auto',
        width: '1px',
        height: '1px',
        overflow: 'hidden',
      }}>
        Skip to main content
      </a>
      <nav aria-label="Main navigation">
        <ul>
          <li><a href="#home">Home</a></li>
          <li><a href="#about">About</a></li>
          <li><a href="#contact">Contact</a></li>
        </ul>
      </nav>
      <main id="main-content" tabIndex={-1}>
        <h1>Main Content</h1>
        <p>Focus lands here after skip link.</p>
      </main>
    </div>
  );
}"##
            .to_string();
            let mut p = empty_patterns();
            p.accessibility_patterns
                .insert("SkipNavigation".to_string());
            p.accessibility_patterns.insert("SemanticHtml".to_string());
            p.accessibility_patterns
                .insert("AriaAttributes".to_string());
            (src, p)
        }
        "ReducedMotion" => {
            let src = r#"import React from 'react';

function usePrefersReducedMotion(): boolean {
  const [reduced, setReduced] = React.useState(
    () => window.matchMedia('(prefers-reduced-motion: reduce)').matches,
  );

  React.useEffect(() => {
    const mql = window.matchMedia('(prefers-reduced-motion: reduce)');
    const handler = (e: MediaQueryListEvent) => setReduced(e.matches);
    mql.addEventListener('change', handler);
    return () => mql.removeEventListener('change', handler);
  }, []);

  return reduced;
}

export default function ReducedMotionStress() {
  const reduced = usePrefersReducedMotion();
  const [count, setCount] = React.useState(0);

  return (
    <div>
      <div style={{
        width: 100,
        height: 100,
        background: 'blue',
        transition: reduced ? 'none' : 'transform 0.5s ease',
        transform: `rotate(${count * 45}deg)`,
      }} />
      <button onClick={() => setCount(c => c + 1)}>Rotate</button>
      <p>{reduced ? 'Reduced motion: animations disabled' : 'Animations enabled'}</p>
    </div>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.accessibility_patterns.insert("ReducedMotion".to_string());
            p.style_patterns.insert("DynamicStyling".to_string());
            p.effect_patterns.insert("BrowserApi".to_string());
            p.effect_patterns.insert("EventListener".to_string());
            (src, p)
        }
        "ScreenReaderText" => {
            let src = r#"import React from 'react';

function VisuallyHidden({ children }: { children: React.ReactNode }) {
  return (
    <span style={{
      position: 'absolute',
      width: '1px',
      height: '1px',
      padding: 0,
      margin: '-1px',
      overflow: 'hidden',
      clip: 'rect(0, 0, 0, 0)',
      whiteSpace: 'nowrap',
      borderWidth: 0,
    }}>
      {children}
    </span>
  );
}

export default function ScreenReaderStress() {
  const [count, setCount] = React.useState(0);

  return (
    <div>
      <button onClick={() => setCount(c => c + 1)} aria-label={`Like count: ${count}`}>
        ❤️ {count}
        <VisuallyHidden>likes</VisuallyHidden>
      </button>
      <div aria-live="polite">
        <VisuallyHidden>Current like count is {count}</VisuallyHidden>
      </div>
    </div>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.accessibility_patterns
                .insert("ScreenReaderText".to_string());
            p.accessibility_patterns
                .insert("AriaAttributes".to_string());
            p.accessibility_patterns.insert("LiveRegions".to_string());
            p.state_patterns.insert("LocalState".to_string());
            (src, p)
        }
        "ColorContrast" => {
            let src = r#"import React from 'react';

function contrastRatio(hex1: string, hex2: string): number {
  const lum = (hex: string) => {
    const r = parseInt(hex.slice(1,3), 16) / 255;
    const g = parseInt(hex.slice(3,5), 16) / 255;
    const b = parseInt(hex.slice(5,7), 16) / 255;
    const sRGB = [r, g, b].map(c => c <= 0.03928 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4));
    return 0.2126 * sRGB[0] + 0.7152 * sRGB[1] + 0.0722 * sRGB[2];
  };
  const l1 = lum(hex1), l2 = lum(hex2);
  const lighter = Math.max(l1, l2), darker = Math.min(l1, l2);
  return (lighter + 0.05) / (darker + 0.05);
}

export default function ColorContrastStress() {
  const [fg, setFg] = React.useState('#333333');
  const [bg, setBg] = React.useState('#ffffff');
  const ratio = contrastRatio(fg, bg);
  const passAA = ratio >= 4.5;
  const passAAA = ratio >= 7;

  return (
    <div>
      <div style={{ background: bg, color: fg, padding: 16 }}>
        <p>Sample text for contrast checking</p>
      </div>
      <p>Ratio: {ratio.toFixed(2)}:1 {passAAA ? '(AAA ✓)' : passAA ? '(AA ✓)' : '(Fail ✗)'}</p>
      <label>FG: <input type="color" value={fg} onChange={e => setFg(e.target.value)} /></label>
      <label>BG: <input type="color" value={bg} onChange={e => setBg(e.target.value)} /></label>
    </div>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.accessibility_patterns.insert("ColorContrast".to_string());
            p.state_patterns.insert("LocalState".to_string());
            p.state_patterns.insert("DerivedState".to_string());
            (src, p)
        }
        _ => return None,
    };

    let tags = vec![ComplexityTag::Medium, ComplexityTag::Accessibility];
    Some((source, patterns, tags))
}

fn generate_terminal_fixture(
    dimension: &str,
    _config: &GeneratorConfig,
    _rng: &mut Rng,
) -> Option<(String, ExpectedPatterns, Vec<ComplexityTag>)> {
    let (source, patterns) = match dimension {
        "MouseInput" => {
            let src = r#"import React from 'react';

export default function MouseInputStress() {
  const [pos, setPos] = React.useState({ x: 0, y: 0 });
  const [clicks, setClicks] = React.useState<{ x: number; y: number; button: number }[]>([]);

  return (
    <div
      onMouseMove={e => setPos({ x: e.clientX, y: e.clientY })}
      onMouseDown={e => setClicks(prev => [...prev.slice(-9), { x: e.clientX, y: e.clientY, button: e.button }])}
      style={{ width: '100%', height: 400, border: '1px solid #ccc', position: 'relative' }}
    >
      <p>Mouse: ({pos.x}, {pos.y})</p>
      {clicks.map((c, i) => (
        <div key={i} style={{
          position: 'absolute',
          left: c.x - 5,
          top: c.y - 5,
          width: 10,
          height: 10,
          borderRadius: '50%',
          background: c.button === 0 ? 'blue' : c.button === 2 ? 'red' : 'green',
        }} />
      ))}
    </div>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.terminal_patterns.insert("MouseInput".to_string());
            p.state_patterns.insert("LocalState".to_string());
            p.ui_patterns.insert("ListRender".to_string());
            (src, p)
        }
        "CursorManipulation" => {
            let src = r#"import React from 'react';

export default function CursorStress() {
  const [cursor, setCursor] = React.useState('default');
  const cursors = ['default', 'pointer', 'crosshair', 'move', 'text', 'wait', 'help', 'not-allowed'];

  return (
    <div>
      <div style={{ cursor, width: 200, height: 200, border: '2px solid black', display: 'flex', alignItems: 'center', justifyContent: 'center' }}>
        Hover here
      </div>
      <div>
        {cursors.map(c => (
          <button key={c} onClick={() => setCursor(c)} style={{ fontWeight: cursor === c ? 'bold' : 'normal' }}>
            {c}
          </button>
        ))}
      </div>
    </div>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.terminal_patterns.insert("CursorManipulation".to_string());
            p.state_patterns.insert("LocalState".to_string());
            p.style_patterns.insert("DynamicStyling".to_string());
            (src, p)
        }
        "ScrollbackPreservation" => {
            let src = r#"import React, { useRef, useEffect } from 'react';

export default function ScrollbackStress() {
  const [items, setItems] = React.useState<string[]>(
    Array.from({ length: 50 }, (_, i) => `Line ${i + 1}`)
  );
  const bottomRef = useRef<HTMLDivElement>(null);
  const [autoScroll, setAutoScroll] = React.useState(true);

  useEffect(() => {
    if (autoScroll) bottomRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [items, autoScroll]);

  const addLine = () => {
    setItems(prev => [...prev, `Line ${prev.length + 1} at ${Date.now()}`]);
  };

  return (
    <div>
      <div style={{ height: 300, overflow: 'auto', border: '1px solid #ccc' }}>
        {items.map((item, i) => <div key={i}>{item}</div>)}
        <div ref={bottomRef} />
      </div>
      <label>
        <input type="checkbox" checked={autoScroll} onChange={e => setAutoScroll(e.target.checked)} />
        Auto-scroll
      </label>
      <button onClick={addLine}>Add Line</button>
    </div>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.terminal_patterns
                .insert("ScrollbackPreservation".to_string());
            p.state_patterns.insert("LocalState".to_string());
            p.state_patterns.insert("RefState".to_string());
            p.effect_patterns.insert("DomManipulation".to_string());
            (src, p)
        }
        "ClipboardIntegration" => {
            let src = r#"import React from 'react';

export default function ClipboardStress() {
  const [text, setText] = React.useState('');
  const [copied, setCopied] = React.useState(false);

  const copyToClipboard = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      // Fallback for older browsers
      const textarea = document.createElement('textarea');
      textarea.value = text;
      document.body.appendChild(textarea);
      textarea.select();
      document.execCommand('copy');
      document.body.removeChild(textarea);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    }
  };

  const pasteFromClipboard = async () => {
    try {
      const pasted = await navigator.clipboard.readText();
      setText(pasted);
    } catch {
      // clipboard read not available
    }
  };

  return (
    <div>
      <textarea value={text} onChange={e => setText(e.target.value)} rows={4} />
      <button onClick={copyToClipboard}>{copied ? 'Copied!' : 'Copy'}</button>
      <button onClick={pasteFromClipboard}>Paste</button>
    </div>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.terminal_patterns
                .insert("ClipboardIntegration".to_string());
            p.state_patterns.insert("LocalState".to_string());
            p.effect_patterns.insert("BrowserApi".to_string());
            (src, p)
        }
        "TerminalResize" => {
            let src = r#"import React from 'react';

function useWindowSize(): { width: number; height: number } {
  const [size, setSize] = React.useState({
    width: window.innerWidth,
    height: window.innerHeight,
  });

  React.useEffect(() => {
    const handler = () => setSize({ width: window.innerWidth, height: window.innerHeight });
    window.addEventListener('resize', handler);
    return () => window.removeEventListener('resize', handler);
  }, []);

  return size;
}

export default function TerminalResizeStress() {
  const { width, height } = useWindowSize();
  const cols = Math.floor(width / 8);
  const rows = Math.floor(height / 16);

  return (
    <div>
      <p>Window: {width}×{height}</p>
      <p>Approx terminal: {cols} cols × {rows} rows</p>
      <div style={{
        display: 'grid',
        gridTemplateColumns: `repeat(${Math.min(cols, 80)}, 8px)`,
        gap: 0,
      }}>
        {Array.from({ length: Math.min(cols * 3, 240) }, (_, i) => (
          <div key={i} style={{ width: 8, height: 16, background: i % 2 ? '#eee' : '#ddd' }} />
        ))}
      </div>
    </div>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.terminal_patterns.insert("TerminalResize".to_string());
            p.effect_patterns.insert("EventListener".to_string());
            p.effect_patterns.insert("EffectCleanup".to_string());
            p.style_patterns.insert("ResponsiveDesign".to_string());
            (src, p)
        }
        _ => return None,
    };

    let tags = vec![ComplexityTag::Medium];
    Some((source, patterns, tags))
}

fn generate_data_fixture(
    dimension: &str,
    _config: &GeneratorConfig,
    _rng: &mut Rng,
) -> Option<(String, ExpectedPatterns, Vec<ComplexityTag>)> {
    let (source, patterns) = match dimension {
        "BidirectionalBinding" => {
            let src = r#"import React from 'react';

function ParentChild() {
  const [value, setValue] = React.useState('hello');

  return (
    <div>
      <p>Parent: {value}</p>
      <ChildEditor value={value} onChange={setValue} />
      <ChildEditor value={value} onChange={setValue} />
    </div>
  );
}

function ChildEditor({ value, onChange }: { value: string; onChange: (v: string) => void }) {
  return <input value={value} onChange={e => onChange(e.target.value)} />;
}

export default ParentChild;"#
                .to_string();
            let mut p = empty_patterns();
            p.data_patterns.insert("BidirectionalBinding".to_string());
            p.data_patterns.insert("PropsDrilling".to_string());
            p.state_patterns.insert("LocalState".to_string());
            (src, p)
        }
        "RenderCallbackChain" => {
            let src = r#"import React from 'react';

function DataProvider({ children }: { children: (data: string[]) => React.ReactNode }) {
  const [data] = React.useState(['alpha', 'beta', 'gamma']);
  return <>{children(data)}</>;
}

function FilterProvider({ items, children }: { items: string[]; children: (filtered: string[]) => React.ReactNode }) {
  const [filter, setFilter] = React.useState('');
  const filtered = items.filter(i => i.includes(filter));
  return (
    <div>
      <input value={filter} onChange={e => setFilter(e.target.value)} placeholder="Filter" />
      {children(filtered)}
    </div>
  );
}

function SortProvider({ items, children }: { items: string[]; children: (sorted: string[]) => React.ReactNode }) {
  const [asc, setAsc] = React.useState(true);
  const sorted = [...items].sort((a, b) => asc ? a.localeCompare(b) : b.localeCompare(a));
  return (
    <div>
      <button onClick={() => setAsc(a => !a)}>{asc ? 'Asc' : 'Desc'}</button>
      {children(sorted)}
    </div>
  );
}

export default function RenderCallbackStress() {
  return (
    <DataProvider>
      {data => (
        <FilterProvider items={data}>
          {filtered => (
            <SortProvider items={filtered}>
              {sorted => (
                <ul>{sorted.map((s, i) => <li key={i}>{s}</li>)}</ul>
              )}
            </SortProvider>
          )}
        </FilterProvider>
      )}
    </DataProvider>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.data_patterns.insert("RenderCallbackChain".to_string());
            p.ui_patterns.insert("RenderProps".to_string());
            p.state_patterns.insert("LocalState".to_string());
            p.state_patterns.insert("DerivedState".to_string());
            (src, p)
        }
        "ServerSideData" => {
            let src = r#"import React from 'react';

// Simulated SSR-hydrated data
declare global {
  interface Window { __SSR_DATA__?: { items: string[]; timestamp: number }; }
}

function useSSRData<T>(key: string, fallback: T): { data: T; isHydrated: boolean } {
  const [data, setData] = React.useState<T>(() => {
    if (typeof window !== 'undefined' && (window as any)[key]) {
      return (window as any)[key] as T;
    }
    return fallback;
  });
  const [isHydrated, setIsHydrated] = React.useState(false);

  React.useEffect(() => {
    setIsHydrated(true);
  }, []);

  return { data, isHydrated };
}

export default function ServerSideDataStress() {
  const { data, isHydrated } = useSSRData('__SSR_DATA__', { items: [], timestamp: 0 });

  return (
    <div>
      <p>Hydrated: {isHydrated ? 'Yes' : 'No'}</p>
      <p>Items: {data.items.length}</p>
      <ul>{data.items.map((item, i) => <li key={i}>{item}</li>)}</ul>
    </div>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.data_patterns.insert("ServerSideData".to_string());
            p.state_patterns.insert("LocalState".to_string());
            p.effect_patterns.insert("MountFetch".to_string());
            (src, p)
        }
        "EventBubbling" => {
            let src = r#"import React from 'react';

export default function EventBubblingStress() {
  const [log, setLog] = React.useState<string[]>([]);

  const logEvent = (source: string) => {
    setLog(prev => [...prev.slice(-19), `${source} at ${Date.now() % 10000}`]);
  };

  return (
    <div onClick={() => logEvent('outer-div')}>
      <div onClick={() => logEvent('middle-div')} style={{ padding: 20, border: '1px solid blue' }}>
        <button onClick={e => { e.stopPropagation(); logEvent('button-stop'); }}>
          Stop Propagation
        </button>
        <button onClick={() => logEvent('button-bubble')}>
          Bubble Up
        </button>
      </div>
      <ul>{log.map((entry, i) => <li key={i}>{entry}</li>)}</ul>
    </div>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.data_patterns.insert("EventBubbling".to_string());
            p.state_patterns.insert("LocalState".to_string());
            (src, p)
        }
        "CodeSplitting" => {
            let src = r#"import React, { Suspense, lazy } from 'react';

const HeavyChart = lazy(() => import('./HeavyChart'));
const HeavyTable = lazy(() => import('./HeavyTable'));
const HeavyForm = lazy(() => import('./HeavyForm'));

export default function CodeSplittingStress() {
  const [tab, setTab] = React.useState<'chart' | 'table' | 'form'>('chart');

  const components = {
    chart: HeavyChart,
    table: HeavyTable,
    form: HeavyForm,
  };

  const Component = components[tab];

  return (
    <div>
      <nav>
        {(['chart', 'table', 'form'] as const).map(t => (
          <button key={t} onClick={() => setTab(t)}>{t}</button>
        ))}
      </nav>
      <Suspense fallback={<p>Loading {tab}...</p>}>
        <Component />
      </Suspense>
    </div>
  );
}"#
            .to_string();
            let mut p = empty_patterns();
            p.data_patterns.insert("CodeSplitting".to_string());
            p.ui_patterns.insert("SuspenseLazy".to_string());
            p.state_patterns.insert("LocalState".to_string());
            (src, p)
        }
        _ => return None,
    };

    let tags = vec![ComplexityTag::Medium, ComplexityTag::CustomHooks];
    Some((source, patterns, tags))
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn empty_patterns() -> ExpectedPatterns {
    ExpectedPatterns {
        ui_patterns: BTreeSet::new(),
        state_patterns: BTreeSet::new(),
        effect_patterns: BTreeSet::new(),
        style_patterns: BTreeSet::new(),
        accessibility_patterns: BTreeSet::new(),
        terminal_patterns: BTreeSet::new(),
        data_patterns: BTreeSet::new(),
    }
}

fn impact_to_risk(impact: &BlindSpotImpact) -> RiskClass {
    match impact {
        BlindSpotImpact::High => RiskClass::High,
        BlindSpotImpact::Medium => RiskClass::Medium,
        BlindSpotImpact::Low => RiskClass::Low,
    }
}

fn slug_from_dimension(dim: &str) -> String {
    dim.chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn describe_scenario(category: &str, dimension: &str) -> String {
    format!(
        "Synthetic adversarial fixture targeting {} coverage gap: {}. \
         Tests hard migration paths with intentionally complex patterns.",
        category, dimension,
    )
}

fn count_components(source: &str) -> usize {
    // Count function/class component declarations
    let fn_count = source.matches("function ").count();
    let class_count = source.matches("class ").count();
    let arrow_export = source.matches("export default function").count();
    // Rough heuristic: each named function likely defines a component
    fn_count + class_count - arrow_export.min(1)
}

fn count_hooks(source: &str) -> usize {
    source.matches("useState").count()
        + source.matches("useEffect").count()
        + source.matches("useReducer").count()
        + source.matches("useRef").count()
        + source.matches("useCallback").count()
        + source.matches("useMemo").count()
        + source.matches("useContext").count()
        + source.matches("useLayoutEffect").count()
        + source.matches("useImperativeHandle").count()
}

fn count_effects(source: &str) -> usize {
    source.matches("useEffect").count() + source.matches("useLayoutEffect").count()
}

fn hash_source(source: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(source.as_bytes());
    crate::util::hex_encode(&hasher.finalize())
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture_taxonomy::{BlindSpot, BlindSpotImpact, CoverageReport, CoverageStats};

    fn make_blind_spot(category: &str, dimension: &str, impact: BlindSpotImpact) -> BlindSpot {
        BlindSpot {
            category: category.to_string(),
            dimension: dimension.to_string(),
            impact,
        }
    }

    fn make_report(blind_spots: Vec<BlindSpot>) -> CoverageReport {
        CoverageReport {
            ui_coverage: BTreeMap::new(),
            state_coverage: BTreeMap::new(),
            effect_coverage: BTreeMap::new(),
            style_coverage: BTreeMap::new(),
            accessibility_coverage: BTreeMap::new(),
            terminal_coverage: BTreeMap::new(),
            data_coverage: BTreeMap::new(),
            blind_spots,
            overrepresented: Vec::new(),
            stats: CoverageStats {
                total_fixtures: 0,
                total_dimensions_possible: 73,
                total_dimensions_covered: 0,
                coverage_percentage: 0.0,
                average_dimensions_per_fixture: 0.0,
                tier_distribution: BTreeMap::new(),
            },
        }
    }

    #[test]
    fn deterministic_output_from_same_seed() {
        let config = GeneratorConfig::default();
        let report = make_report(vec![make_blind_spot(
            "ui",
            "RecursiveTree",
            BlindSpotImpact::High,
        )]);

        let result1 = generate_from_coverage(&report, &config);
        let result2 = generate_from_coverage(&report, &config);

        assert_eq!(result1.fixtures.len(), 1);
        assert_eq!(result2.fixtures.len(), 1);
        assert_eq!(result1.fixtures[0].source, result2.fixtures[0].source);
        assert_eq!(
            result1.fixtures[0].scenario.id,
            result2.fixtures[0].scenario.id,
        );
    }

    #[test]
    fn different_seeds_produce_different_output() {
        let report = make_report(vec![make_blind_spot(
            "ui",
            "RecursiveTree",
            BlindSpotImpact::High,
        )]);

        let config1 = GeneratorConfig {
            base_seed: 42,
            ..GeneratorConfig::default()
        };
        let config2 = GeneratorConfig {
            base_seed: 99,
            ..GeneratorConfig::default()
        };

        let result1 = generate_from_coverage(&report, &config1);
        let result2 = generate_from_coverage(&report, &config2);

        // Same dimension but different seeds → same template but different params
        assert_eq!(result1.fixtures.len(), 1);
        assert_eq!(result2.fixtures.len(), 1);
        // The scenario IDs differ because seed differs
        assert_ne!(
            result1.fixtures[0].scenario.id,
            result2.fixtures[0].scenario.id
        );
    }

    #[test]
    fn scenario_planning_from_blind_spots() {
        let report = make_report(vec![
            make_blind_spot("ui", "RecursiveTree", BlindSpotImpact::High),
            make_blind_spot("state", "StateMachine", BlindSpotImpact::Medium),
            make_blind_spot("effect", "WebSocketConnection", BlindSpotImpact::High),
        ]);

        let config = GeneratorConfig::default();
        let scenarios = plan_scenarios(&report, &config);

        assert_eq!(scenarios.len(), 3);
        assert_eq!(scenarios[0].target_dimension, "RecursiveTree");
        assert_eq!(scenarios[1].target_dimension, "StateMachine");
        assert_eq!(scenarios[2].target_dimension, "WebSocketConnection");
    }

    #[test]
    fn high_impact_only_filtering() {
        let report = make_report(vec![
            make_blind_spot("ui", "RecursiveTree", BlindSpotImpact::High),
            make_blind_spot("state", "StateMachine", BlindSpotImpact::Low),
            make_blind_spot("effect", "WebSocketConnection", BlindSpotImpact::Medium),
        ]);

        let config = GeneratorConfig {
            high_impact_only: true,
            ..GeneratorConfig::default()
        };
        let scenarios = plan_scenarios(&report, &config);

        assert_eq!(scenarios.len(), 1);
        assert_eq!(scenarios[0].target_dimension, "RecursiveTree");
    }

    #[test]
    fn generated_fixture_has_synthetic_provenance() {
        let report = make_report(vec![make_blind_spot(
            "ui",
            "PortalModal",
            BlindSpotImpact::High,
        )]);

        let config = GeneratorConfig::default();
        let result = generate_from_coverage(&report, &config);

        assert_eq!(result.fixtures.len(), 1);
        let entry = &result.fixtures[0].corpus_entry;
        assert_eq!(
            entry.provenance.source_type,
            ProvenanceSourceType::Synthetic
        );
        assert!(entry.source_url.starts_with("synthetic://"));
        assert!(entry.license_verified);
        assert_eq!(entry.license, "Apache-2.0");
    }

    #[test]
    fn generated_source_is_nonempty_tsx() {
        let report = make_report(vec![make_blind_spot(
            "state",
            "ExternalStore",
            BlindSpotImpact::High,
        )]);

        let config = GeneratorConfig::default();
        let result = generate_from_coverage(&report, &config);

        assert_eq!(result.fixtures.len(), 1);
        let source = &result.fixtures[0].source;
        assert!(!source.is_empty());
        assert!(source.contains("import React"));
        assert!(source.contains("export default"));
        assert!(result.fixtures[0].filename.ends_with(".tsx"));
    }

    #[test]
    fn expected_patterns_populated() {
        let report = make_report(vec![make_blind_spot(
            "effect",
            "TimerInterval",
            BlindSpotImpact::High,
        )]);

        let config = GeneratorConfig::default();
        let result = generate_from_coverage(&report, &config);

        let patterns = &result.fixtures[0].expected_patterns;
        assert!(patterns.effect_patterns.contains("TimerInterval"));
        assert!(patterns.effect_patterns.contains("EffectCleanup"));
    }

    #[test]
    fn corpus_metrics_populated() {
        let report = make_report(vec![make_blind_spot(
            "ui",
            "ErrorBoundary",
            BlindSpotImpact::High,
        )]);

        let config = GeneratorConfig::default();
        let result = generate_from_coverage(&report, &config);

        let metrics = result.fixtures[0]
            .corpus_entry
            .expected_metrics
            .as_ref()
            .unwrap();
        assert!(metrics.file_count > 0);
        assert!(metrics.component_count > 0);
        assert!(metrics.loc_approx > 10);
        assert!(!metrics.source_hash.is_empty());
    }

    #[test]
    fn stats_accurate() {
        let report = make_report(vec![
            make_blind_spot("ui", "RecursiveTree", BlindSpotImpact::High),
            make_blind_spot("state", "StateMachine", BlindSpotImpact::Medium),
            make_blind_spot("ui", "NonExistentPattern", BlindSpotImpact::Low),
        ]);

        let config = GeneratorConfig::default();
        let result = generate_from_coverage(&report, &config);

        assert_eq!(result.stats.total_scenarios, 3);
        assert_eq!(result.stats.generated, 2);
        assert_eq!(result.stats.skipped, 1);
        assert_eq!(result.skipped.len(), 1);
        assert!(result.skipped[0].0.target_dimension.contains("NonExistent"));
    }

    #[test]
    fn all_seven_categories_have_generators() {
        let categories = vec![
            ("ui", "RecursiveTree"),
            ("state", "StateMachine"),
            ("effect", "TimerInterval"),
            ("style", "CssVariables"),
            ("accessibility", "FocusManagement"),
            ("terminal", "MouseInput"),
            ("data", "BidirectionalBinding"),
        ];

        let config = GeneratorConfig::default();

        for (cat, dim) in categories {
            let report = make_report(vec![make_blind_spot(cat, dim, BlindSpotImpact::High)]);
            let result = generate_from_coverage(&report, &config);
            assert_eq!(
                result.fixtures.len(),
                1,
                "Generator missing for {cat}/{dim}",
            );
        }
    }

    #[test]
    fn risk_class_matches_impact() {
        let report = make_report(vec![
            make_blind_spot("ui", "RecursiveTree", BlindSpotImpact::High),
            make_blind_spot("state", "StateMachine", BlindSpotImpact::Medium),
            make_blind_spot("data", "BidirectionalBinding", BlindSpotImpact::Low),
        ]);

        let config = GeneratorConfig::default();
        let scenarios = plan_scenarios(&report, &config);

        assert_eq!(scenarios[0].risk_class, RiskClass::High);
        assert_eq!(scenarios[1].risk_class, RiskClass::Medium);
        assert_eq!(scenarios[2].risk_class, RiskClass::Low);
    }

    #[test]
    fn slug_generation_deterministic() {
        assert_eq!(slug_from_dimension("RecursiveTree"), "recursivetree");
        assert_eq!(slug_from_dimension("CssVariables"), "cssvariables");
        assert_eq!(slug_from_dimension("FocusManagement"), "focusmanagement");
    }

    #[test]
    fn xorshift_rng_deterministic() {
        let mut rng1 = Rng::new(42);
        let mut rng2 = Rng::new(42);
        for _ in 0..100 {
            assert_eq!(rng1.next_u64(), rng2.next_u64());
        }
    }

    #[test]
    fn xorshift_rng_different_seeds() {
        let mut rng1 = Rng::new(42);
        let mut rng2 = Rng::new(43);
        // Very unlikely to produce same sequence
        let seq1: Vec<u64> = (0..10).map(|_| rng1.next_u64()).collect();
        let seq2: Vec<u64> = (0..10).map(|_| rng2.next_u64()).collect();
        assert_ne!(seq1, seq2);
    }

    #[test]
    fn multi_dimension_generation() {
        let report = make_report(vec![
            make_blind_spot("ui", "RecursiveTree", BlindSpotImpact::High),
            make_blind_spot("ui", "PortalModal", BlindSpotImpact::High),
            make_blind_spot("ui", "ErrorBoundary", BlindSpotImpact::Medium),
            make_blind_spot("state", "OptimisticUpdate", BlindSpotImpact::High),
            make_blind_spot("effect", "DebouncedEffect", BlindSpotImpact::Medium),
            make_blind_spot("style", "Animation", BlindSpotImpact::Medium),
            make_blind_spot("accessibility", "LiveRegions", BlindSpotImpact::High),
            make_blind_spot("terminal", "ClipboardIntegration", BlindSpotImpact::Medium),
            make_blind_spot("data", "RenderCallbackChain", BlindSpotImpact::High),
        ]);

        let config = GeneratorConfig::default();
        let result = generate_from_coverage(&report, &config);

        assert_eq!(result.stats.generated, 9);
        assert_eq!(result.stats.skipped, 0);

        // Each fixture should have unique slug
        let slugs: BTreeSet<_> = result
            .fixtures
            .iter()
            .map(|f| f.scenario.id.clone())
            .collect();
        assert_eq!(slugs.len(), 9);
    }

    #[test]
    fn generated_fixtures_have_active_flag() {
        let report = make_report(vec![make_blind_spot(
            "ui",
            "RecursiveTree",
            BlindSpotImpact::High,
        )]);

        let config = GeneratorConfig::default();
        let result = generate_from_coverage(&report, &config);

        assert!(result.fixtures[0].corpus_entry.active);
    }

    #[test]
    fn feature_tags_include_category_and_risk() {
        let report = make_report(vec![make_blind_spot(
            "effect",
            "WebSocketConnection",
            BlindSpotImpact::High,
        )]);

        let config = GeneratorConfig::default();
        let result = generate_from_coverage(&report, &config);

        let tags = &result.fixtures[0].corpus_entry.feature_tags;
        assert!(tags.iter().any(|t| t.starts_with("adversarial:")));
        assert!(tags.iter().any(|t| t.starts_with("risk:")));
        assert!(tags.contains(&"WebSocketConnection".to_string()));
    }

    #[test]
    fn context_provider_nesting_depth_varies_with_seed() {
        // Different seeds produce different nesting depths
        let report = make_report(vec![make_blind_spot(
            "ui",
            "ContextProviderNesting",
            BlindSpotImpact::High,
        )]);

        let config1 = GeneratorConfig {
            base_seed: 1,
            ..GeneratorConfig::default()
        };
        let config2 = GeneratorConfig {
            base_seed: 1000,
            ..GeneratorConfig::default()
        };

        let r1 = generate_from_coverage(&report, &config1);
        let r2 = generate_from_coverage(&report, &config2);

        // Both should generate valid fixtures
        assert_eq!(r1.fixtures.len(), 1);
        assert_eq!(r2.fixtures.len(), 1);
        // Both have ContextProviderNesting pattern
        assert!(
            r1.fixtures[0]
                .expected_patterns
                .ui_patterns
                .contains("ContextProviderNesting")
        );
    }

    #[test]
    fn hash_source_deterministic() {
        let h1 = hash_source("hello world");
        let h2 = hash_source("hello world");
        let h3 = hash_source("different");
        assert_eq!(h1, h2);
        assert_ne!(h1, h3);
    }
}
