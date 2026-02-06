#![forbid(unsafe_code)]

//! Mind-blowing dashboard screen.
//!
//! Showcases EVERY major FrankenTUI capability simultaneously:
//! - Animated gradient title
//! - Live plasma visual effect (Braille canvas)
//! - Real-time sparkline charts
//! - Syntax-highlighted code preview
//! - GFM markdown preview
//! - System stats (FPS, theme, size)
//! - Keyboard shortcuts
//!
//! Dynamically reflowable from 40x10 to 200x50+.

use std::cell::Cell as StdCell;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEventKind};
use ftui_core::geometry::Rect;
use ftui_extras::canvas::{Canvas, Mode, Painter};
use ftui_extras::charts::{
    BarChart, BarDirection, BarGroup, BarMode, LineChart, Series, Sparkline, heatmap_gradient,
};
use ftui_extras::markdown::{MarkdownRenderer, MarkdownTheme};
use ftui_extras::syntax::SyntaxHighlighter;
use ftui_extras::text_effects::{
    ColorGradient, CursorPosition, CursorStyle, Direction, DissolveMode, RevealMode,
    StyledMultiLine, StyledText, TextEffect,
};
use ftui_layout::{Constraint, Flex};
use ftui_render::cell::{Cell as RenderCell, PackedRgba};
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::{Style, StyleFlags};
use ftui_text::{Line, Span, Text, WrapMode};
use ftui_text::{display_width, grapheme_count, grapheme_width, graphemes};
use ftui_widgets::Badge;
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::progress::{MiniBar, MiniBarColors};

use super::{HelpEntry, Screen};
use crate::app::ScreenId;
use crate::chrome;
use crate::data::{AlertSeverity, SimulatedData};
use crate::determinism;
use crate::theme;

struct CodeSample {
    label: &'static str,
    lang: &'static str,
    code: &'static str,
}

const CODE_SAMPLES: &[CodeSample] = &[
    CodeSample {
        label: "Rust",
        lang: "rs",
        code: r###"// runtime.rs
use std::{collections::HashMap, sync::Arc, time::Duration};

use ftui_core::event::Event;
use ftui_render::cell::Cell;
use ftui_render::frame::Frame;
use ftui_runtime::{Cmd, Model, Program};

#[derive(Debug, Clone)]
enum Msg {
    Tick(u64),
    Resize { w: u16, h: u16 },
    Log(String),
    Quit,
}

struct App<T: Send + Sync + 'static> {
    frames: u64,
    last_size: (u16, u16),
    cache: HashMap<String, Arc<T>>,
}

impl<T: Send + Sync + 'static> App<T> {
    fn push(&mut self, key: impl Into<String>, value: T) {
        self.cache.insert(key.into(), Arc::new(value));
    }
}

impl<T: Send + Sync + 'static> Model for App<T> {
    type Message = Msg;

    fn init(&mut self) -> Cmd<Msg> {
        Cmd::Tick(Duration::from_millis(16))
    }

    fn update(&mut self, msg: Msg) -> Cmd<Msg> {
        match msg {
            Msg::Tick(n) => {
                self.frames = self.frames.saturating_add(n);
                Cmd::Tick(Duration::from_millis(16))
            }
            Msg::Resize { w, h } => {
                self.last_size = (w, h);
                Cmd::none()
            }
            Msg::Log(line) => Cmd::log(format!("[{:#x}] {line}", self.frames)),
            Msg::Quit => Cmd::quit(),
        }
    }

    fn view(&self, frame: &mut Frame) {
        let label = format!("frames={} size={:?}", self.frames, self.last_size);
        for (i, ch) in label.chars().enumerate() {
            frame
                .buffer
                .set_raw(i as u16, 0, Cell::from_char(ch));
        }
    }
}

fn main() -> ftui::Result<()> {
    let mut app = App::<u64> {
        frames: 0,
        last_size: (0, 0),
        cache: HashMap::new(),
    };
    app.push("seed", 42);
    Program::new(&mut app).run()
}

#[derive(Debug, Clone)]
struct Budget {
    frame_limit_ms: u64,
    dirty_rows: usize,
    violations: Vec<String>,
}

impl Budget {
    fn new(frame_limit_ms: u64) -> Self {
        Self {
            frame_limit_ms,
            dirty_rows: 0,
            violations: Vec::new(),
        }
    }

    fn note_dirty(&mut self) {
        self.dirty_rows = self.dirty_rows.saturating_add(1);
        if self.dirty_rows > 120 {
            self.violations.push("Diff overflow".to_string());
        }
    }

    fn seal(mut self) -> Self {
        if self.violations.is_empty() {
            self.violations.push("OK".to_string());
        }
        self
    }
}

fn coalesce<T: Clone>(primary: Option<T>, fallback: T) -> T {
    primary.unwrap_or(fallback)
}

fn build_budget(frame_time_ms: u64, dirty_rows: usize) -> Budget {
    let mut budget = Budget::new(frame_time_ms);
    for _ in 0..dirty_rows {
        budget.note_dirty();
    }
    budget.seal()
}

#[derive(Debug)]
enum BudgetPolicy {
    Strict,
    Adaptive { headroom_ms: u64 },
    Burst { max_rows: usize },
}

fn enforce_policy(budget: &Budget, policy: BudgetPolicy) -> Result<(), String> {
    match policy {
        BudgetPolicy::Strict if budget.violations.len() > 1 => {
            Err("strict budget exceeded".to_string())
        }
        BudgetPolicy::Adaptive { headroom_ms } => {
            if budget.frame_limit_ms.saturating_sub(headroom_ms) < 10 {
                Err("headroom too low".to_string())
            } else {
                Ok(())
            }
        }
        BudgetPolicy::Burst { max_rows } => {
            if budget.dirty_rows > max_rows {
                Err("burst rows exceeded".to_string())
            } else {
                Ok(())
            }
        }
        _ => Ok(()),
    }
}

#[derive(Debug)]
struct RenderTrace {
    frame: u64,
    dirty: usize,
    violations: Vec<String>,
}

impl RenderTrace {
    fn new(frame: u64, dirty: usize) -> Self {
        Self {
            frame,
            dirty,
            violations: Vec::new(),
        }
    }

    fn push_violation(&mut self, msg: impl Into<String>) {
        self.violations.push(msg.into());
    }

    fn commit(self) -> String {
        format!(
            "trace frame={} dirty={} violations={:?}",
            self.frame, self.dirty, self.violations
        )
    }
}

fn checksum_cells(cells: &[Cell]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for cell in cells {
        hash ^= cell.fg.0 as u64;
        hash = hash.wrapping_mul(0x100000001b3);
        hash ^= cell.bg.0 as u64;
        hash = hash.wrapping_mul(0x100000001b3);
        hash ^= cell.attrs.bits() as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

struct EvidenceRecord {
    frame: u64,
    checksum: u64,
    dirty_rows: usize,
    strategy: &'static str,
}

impl EvidenceRecord {
    fn to_json(&self) -> String {
        format!(
            "{{\"frame\":{},\"checksum\":\"{:x}\",\"dirty_rows\":{},\"strategy\":\"{}\"}}",
            self.frame, self.checksum, self.dirty_rows, self.strategy
        )
    }
}
"###,
    },
    CodeSample {
        label: "TypeScript",
        lang: "ts",
        code: r###"// api.ts
type Mode = "inline" | "alt";
type Result<T> = { ok: true; value: T } | { ok: false; error: string };

interface Session {
  id: string;
  mode: Mode;
  caps: ReadonlyArray<string>;
}

interface Cache<K, V> {
  get(key: K): V | undefined;
  set(key: K, value: V): void;
}

class LruCache<K, V> implements Cache<K, V> {
  private store = new Map<K, V>();
  constructor(private limit = 128) {}

  get(key: K): V | undefined {
    const value = this.store.get(key);
    if (value !== undefined) {
      this.store.delete(key);
      this.store.set(key, value);
    }
    return value;
  }

  set(key: K, value: V): void {
    if (this.store.size >= this.limit) {
      const oldest = this.store.keys().next().value;
      this.store.delete(oldest);
    }
    this.store.set(key, value);
  }
}

const CAPS = ["mouse", "paste", "focus"] as const;
type Cap = typeof CAPS[number];

export async function boot(mode: Mode): Promise<Result<Session>> {
  const res = await fetch("/api/session", {
    method: "POST",
    body: JSON.stringify({ mode }),
  });
  if (!res.ok) return { ok: false, error: "boot failed" };
  const data = (await res.json()) as Session;
  return { ok: true, value: data };
}

export function diff<T>(a: readonly T[], b: readonly T[]): T[] {
  const set = new Set(b);
  return a.filter((x) => !set.has(x));
}

type Budget = {
  frameMs: number;
  dirtyRows: number;
  violations: string[];
};

export class EventBus<T> {
  private listeners = new Set<(event: T) => void>();
  on(fn: (event: T) => void): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }
  emit(event: T): void {
    for (const fn of this.listeners) fn(event);
  }
}

const THEME = {
  accent: ["#67e8f9", "#f472b6", "#fbbf24"],
  surface: "#0b1220",
} as const;

export async function buildBudget(
  dirtyRows: number,
  frameMs = 16
): Promise<Budget> {
  const violations = dirtyRows > 120 ? ["Diff overflow"] : ["OK"];
  return { frameMs, dirtyRows, violations };
}

export async function streamFrames(
  source: AsyncIterable<Frame>,
  bus: EventBus<Frame>
): Promise<number> {
  let count = 0;
  for await (const frame of source) {
    bus.emit(frame);
    if (frame.dirty) count++;
  }
  return count;
}

type BudgetPolicy =
  | { kind: "strict" }
  | { kind: "adaptive"; headroomMs: number }
  | { kind: "burst"; maxRows: number };

export function enforcePolicy(budget: Budget, policy: BudgetPolicy): Result<Budget> {
  if (policy.kind === "strict" && budget.violations.length > 0) {
    return { ok: false, error: "budget exceeded" };
  }
  if (policy.kind === "adaptive" && budget.frameMs - policy.headroomMs < 10) {
    return { ok: false, error: "headroom too low" };
  }
  if (policy.kind === "burst" && budget.dirtyRows > policy.maxRows) {
    return { ok: false, error: "burst rows exceeded" };
  }
  return { ok: true, value: budget };
}

export class TraceBuffer {
  private buf: string[] = [];
  push(frame: number, dirty: number, note: string) {
    this.buf.push(`[${frame}] dirty=${dirty} ${note}`);
  }
  flush(): string {
    const out = this.buf.join("\n");
    this.buf = [];
    return out;
  }
}

export type Evidence = {
  frame: number;
  checksum: string;
  dirtyRows: number;
  strategy: "full" | "dirty" | "redraw";
};

export function checksumCells(cells: ReadonlyArray<{ fg: number; bg: number; attrs: number }>): string {
  let hash = 0xcbf29ce484222325n;
  for (const cell of cells) {
    hash ^= BigInt(cell.fg);
    hash *= 0x100000001b3n;
    hash ^= BigInt(cell.bg);
    hash *= 0x100000001b3n;
    hash ^= BigInt(cell.attrs);
    hash *= 0x100000001b3n;
  }
  return `0x${hash.toString(16)}`;
}

export async function loadScenario(name: string): Promise<Result<Session>> {
  const res = await fetch(`/api/scenario/${name}`);
  if (!res.ok) return { ok: false, error: "scenario not found" };
  return { ok: true, value: (await res.json()) as Session };
}
"###,
    },
    CodeSample {
        label: "Python",
        lang: "py",
        code: r###"# pipeline.py
from __future__ import annotations

from dataclasses import dataclass, field
from typing import Iterable, AsyncIterator, Protocol

class Sink(Protocol):
    async def write(self, payload: str) -> None: ...

@dataclass(slots=True)
class Frame:
    id: int
    dirty: bool
    tags: dict[str, str] = field(default_factory=dict)

async def render(frames: AsyncIterator[Frame], sink: Sink) -> int:
    count = 0
    async for f in frames:
        if f.dirty:
            await sink.write(f"frame={f.id} tags={len(f.tags)}")
            count += 1
    return count

def diff(prev: Iterable[str], nxt: Iterable[str]) -> list[str]:
    seen = set(prev)
    return [x for x in nxt if x not in seen]

def bucket(frames: Iterable[Frame]) -> dict[bool, list[Frame]]:
    out: dict[bool, list[Frame]] = {True: [], False: []}
    for f in frames:
        out[f.dirty].append(f)
    return out

@dataclass(slots=True)
class Budget:
    frame_ms: int
    dirty_rows: int
    violations: list[str] = field(default_factory=list)

    def seal(self) -> "Budget":
        if not self.violations:
            self.violations.append("OK")
        return self

def build_budget(frame_ms: int, dirty_rows: int) -> Budget:
    violations = ["Diff overflow"] if dirty_rows > 120 else []
    return Budget(frame_ms, dirty_rows, violations).seal()

async def stream(frames: AsyncIterator[Frame], sink: Sink) -> None:
    async for f in frames:
        if f.dirty:
            await sink.write(f"stream id={f.id} tags={f.tags}")

class BudgetPolicy(Protocol):
    def enforce(self, budget: Budget) -> None: ...

@dataclass(slots=True)
class AdaptivePolicy:
    headroom_ms: int = 6

    def enforce(self, budget: Budget) -> None:
        if budget.frame_ms - self.headroom_ms < 10:
            budget.violations.append("low headroom")

@dataclass(slots=True)
class Trace:
    frame: int
    dirty: int
    notes: list[str] = field(default_factory=list)

    def add(self, note: str) -> None:
        self.notes.append(note)

    def render(self) -> str:
        joined = ", ".join(self.notes) if self.notes else "OK"
        return f"trace frame={self.frame} dirty={self.dirty} notes={joined}"

@dataclass(slots=True)
class Evidence:
    frame: int
    checksum: int
    dirty_rows: int
    strategy: str

def checksum_cells(cells: Iterable[Frame]) -> int:
    h = 0xcbf29ce484222325
    for cell in cells:
        h ^= cell.id
        h = (h * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
    return h

async def load_scenario(name: str) -> Frame:
    if not name:
        return Frame(id=0, dirty=False, tags={"error": "missing scenario"})
    return Frame(id=1, dirty=False, tags={"scenario": name})
"###,
    },
    CodeSample {
        label: "Go",
        lang: "go",
        code: r###"// runtime.go
package runtime

import (
    "context"
    "errors"
    "fmt"
    "time"
)

type Frame struct {
    ID    int
    Dirty bool
    Tags  map[string]string
}

type Result[T any] struct {
    Val T
    Err error
}

func (r Result[T]) Ok() bool { return r.Err == nil }

func Map[T any, U any](in []T, fn func(T) U) []U {
    out := make([]U, 0, len(in))
    for _, v := range in {
        out = append(out, fn(v))
    }
    return out
}

func Stream(ctx context.Context, in <-chan Frame) <-chan Result[Frame] {
    out := make(chan Result[Frame])
    go func() {
        defer close(out)
        for {
            select {
            case <-ctx.Done():
                out <- Result[Frame]{Err: ctx.Err()}
                return
            case f, ok := <-in:
                if !ok {
                    return
                }
                if f.Dirty {
                    out <- Result[Frame]{Val: f}
                }
            }
        }
    }()
    return out
}

func Compute(ctx context.Context, a, b []Frame) (int, error) {
    if len(a) != len(b) {
        return 0, errors.New("size mismatch")
    }
    changed := 0
    for i := range a {
        select {
        case <-ctx.Done():
            return changed, ctx.Err()
        default:
            if a[i].Dirty != b[i].Dirty {
                changed++
            }
        }
    }
    fmt.Println("done in", 5*time.Millisecond)
    return changed, nil
}

type Evidence struct {
	Frame    int
	Checksum string
	Dirty    int
	Strategy string
}

func ChecksumCells(frames []Frame) string {
	var hash uint64 = 1469598103934665603
	for _, f := range frames {
		hash ^= uint64(f.ID)
		hash *= 1099511628211
		hash ^= uint64(len(f.Tags))
		hash *= 1099511628211
	}
	return fmt.Sprintf("0x%x", hash)
}

func BuildEvidence(frame int, dirty int, strat string, frames []Frame) Evidence {
	return Evidence{
		Frame:    frame,
		Checksum: ChecksumCells(frames),
		Dirty:    dirty,
		Strategy: strat,
	}
}

type Budget struct {
    FrameMS   int
    DirtyRows int
    Notes     []string
}

func BuildBudget(frameMS, dirtyRows int) Budget {
    notes := []string{"OK"}
    if dirtyRows > 120 {
        notes = []string{"Diff overflow"}
    }
    return Budget{FrameMS: frameMS, DirtyRows: dirtyRows, Notes: notes}
}

func WithTimeout(parent context.Context, d time.Duration) (context.Context, context.CancelFunc) {
    ctx, cancel := context.WithTimeout(parent, d)
    return ctx, cancel
}
"###,
    },
    CodeSample {
        label: "SQL",
        lang: "sql",
        code: r###"WITH recent AS (
  SELECT frame_id, ts, changed_cells, total_cells
  FROM frame_metrics
  WHERE ts >= now() - interval '7 days'
),
ratio AS (
  SELECT frame_id,
         changed_cells::numeric / NULLIF(total_cells, 0) AS change_ratio,
         ts
  FROM recent
),
ranked AS (
  SELECT frame_id,
         avg(change_ratio) AS avg_ratio,
         percentile_disc(0.95) WITHIN GROUP (ORDER BY change_ratio) AS p95_ratio,
         row_number() OVER (ORDER BY avg(change_ratio) DESC) AS rn
  FROM ratio
  GROUP BY frame_id
),
joined AS (
  SELECT r.frame_id, r.avg_ratio, r.p95_ratio, m.mode, m.theme
  FROM ranked r
  JOIN frame_meta m ON m.frame_id = r.frame_id
  WHERE r.rn <= 10
)
SELECT j.frame_id, j.mode, j.theme,
       j.avg_ratio, j.p95_ratio,
       COALESCE(t.tag, 'none') AS top_tag
FROM joined j
LEFT JOIN LATERAL (
  SELECT tag
  FROM frame_tags t
  WHERE t.frame_id = j.frame_id
  ORDER BY t.count DESC
  LIMIT 1
) t ON true
ORDER BY j.avg_ratio DESC;

WITH perf AS (
  SELECT frame_id,
         max(changed_cells) AS spike,
         avg(changed_cells) AS mean_cells,
         count(*) FILTER (WHERE changed_cells > 500) AS bursts
  FROM frame_metrics
  WHERE ts >= now() - interval '24 hours'
  GROUP BY frame_id
)
SELECT p.frame_id,
       p.spike,
       p.mean_cells,
       p.bursts,
       (p.bursts > 3) AS needs_budget
FROM perf p
ORDER BY p.spike DESC;

WITH policy AS (
  SELECT frame_id,
         case
           when needs_budget then 'degrade'
           when mean_cells < 120 then 'full'
           else 'adaptive'
         end AS policy
  FROM perf
)
SELECT p.frame_id,
       p.policy,
       jsonb_build_object(
         'spike', perf.spike,
         'mean', perf.mean_cells,
         'bursts', perf.bursts
       ) AS evidence
FROM policy p
JOIN perf ON perf.frame_id = p.frame_id
ORDER BY p.policy, perf.spike DESC;

-- Schema + materialized rollup
CREATE TABLE IF NOT EXISTS frame_metrics (
  frame_id bigint,
  ts timestamptz,
  changed_cells int,
  total_cells int,
  mode text,
  theme text
);

CREATE INDEX IF NOT EXISTS frame_metrics_ts_idx
  ON frame_metrics (ts DESC);

CREATE MATERIALIZED VIEW IF NOT EXISTS frame_metrics_daily AS
SELECT date_trunc('day', ts) AS day,
       avg(changed_cells) AS avg_changed,
       percentile_disc(0.99) WITHIN GROUP (ORDER BY changed_cells) AS p99_changed
FROM frame_metrics
GROUP BY 1;

REFRESH MATERIALIZED VIEW CONCURRENTLY frame_metrics_daily;

EXPLAIN (ANALYZE, BUFFERS)
SELECT * FROM frame_metrics_daily
WHERE day >= now() - interval '7 days'
ORDER BY p99_changed DESC;
"###,
    },
    CodeSample {
        label: "JSON",
        lang: "json",
        code: r###"{
  "runtime": {
    "screenMode": "inline",
    "uiHeight": 12,
    "uiMinHeight": 8,
    "uiMaxHeight": 16,
    "focus": {
      "mode": "pane",
      "defaultPane": "code",
      "mouse": true
    },
    "input": {
      "paste": true,
      "mouse": true,
      "bracketed": true,
      "keys": ["tab", "shift+tab", "arrows", "ctrl+arrows"]
    }
  },
  "renderer": {
    "diff": "row-major",
    "strategy": "bayes",
    "cellBytes": 16,
    "budgets": {
      "renderMs": 8.0,
      "presentMs": 4.0,
      "degradation": "auto",
      "tiers": [
        { "name": "full", "maxMs": 12 },
        { "name": "simple", "maxMs": 16 },
        { "name": "textOnly", "maxMs": 20 }
      ]
    }
  },
  "diff": {
    "dirtyRows": true,
    "spanUnion": true,
    "tileSkip": true,
    "posterior": { "alpha": 3.5, "beta": 92.5 }
  },
  "presenter": {
    "syncOutput": true,
    "flushBytes": 65536,
    "cursorModel": "tracked",
    "ansiBudget": "adaptive"
  },
  "features": ["mouse", "paste", "focus", "synchronized-output", "hyperlinks"],
  "theme": {
    "name": "NordicFrost",
    "accent": "#6AD1E3",
    "bg": "#0E141B",
    "fg": "#E6EEF5",
    "muted": "#6D7A86"
  },
  "themes": [
    { "name": "NordicFrost", "accent": "#6AD1E3" },
    { "name": "EmberLab", "accent": "#FF8C5A" },
    { "name": "Aurora", "accent": "#8AF7C8" }
  ],
  "telemetry": {
    "enabled": true,
    "sampleRate": 0.25,
    "exporter": "otlp",
    "endpoint": "http://localhost:4317",
    "tags": { "service": "ftui", "build": "nightly" }
  },
  "logging": {
    "level": "info",
    "channels": ["stderr", "ndjson"],
    "ndjsonPath": "/var/log/ftui.ndjson"
  },
  "pipelines": [
    {
      "name": "diff",
      "maxRows": 120,
      "priorities": ["text", "widgets", "chrome"],
      "steps": ["sanitize", "scan", "union", "emit"]
    },
    {
      "name": "present",
      "writer": "inline",
      "flushBytes": 65536,
      "rateLimitFps": 60
    }
  ],
  "subscriptions": {
    "tickMs": 16,
    "resizeCoalesceMs": 20,
    "logTail": { "path": "/var/log/app.log", "lines": 50 }
  },
  "alerts": {
    "errorBudget": 0.02,
    "p95Ms": 14,
    "p99Ms": 20,
    "channels": ["stderr", "otlp", "ndjson"],
    "rules": [
      { "name": "cpu_hot", "threshold": 0.85, "severity": "warning" },
      { "name": "frame_drop", "threshold": 0.12, "severity": "error" }
    ]
  },
  "evidenceLedger": {
    "enabled": true,
    "fields": ["bayes_factor", "risk", "decision", "posterior"],
    "retention": "7d"
  },
  "profiles": [
    { "name": "modern", "syncOutput": true, "scrollRegion": true },
    { "name": "mux", "syncOutput": false, "scrollRegion": false }
  ],
  "renderTrace": {
    "seed": 42,
    "runId": "demo-001",
    "checksums": ["9f2d", "a120", "b3cc"],
    "mode": "inline"
  },
  "streaming": {
    "markdownCharsPerTick": 720,
    "codeRotationMs": 3200,
    "fxRotationMs": 1800,
    "chartRotationMs": 2400
  },
  "panes": [
    { "id": "charts", "title": "Charts", "focusable": true },
    { "id": "code", "title": "Code", "focusable": true },
    { "id": "info", "title": "Info", "focusable": true },
    { "id": "text_fx", "title": "Text FX", "focusable": true },
    { "id": "activity", "title": "Activity", "focusable": true },
    { "id": "markdown", "title": "Markdown", "focusable": true }
  ],
  "chartModes": ["pulse", "lines", "bars", "heatmap", "matrix", "composite", "radar"],
  "textEffects": {
    "stack": ["gradient", "glow", "wave"],
    "maxLayers": 3,
    "seed": 1337
  },
  "cache": {
    "type": "lru",
    "maxEntries": 512,
    "evictPolicy": "lfu-backoff",
    "ttlSeconds": 180
  },
  "snapshots": {
    "sizes": ["80x24", "120x40"],
    "bless": false,
    "path": "./snapshots"
  },
  "backpressure": {
    "maxQueuedFrames": 2,
    "dropPolicy": "oldest"
  }
}"###,
    },
    CodeSample {
        label: "YAML",
        lang: "yaml",
        code: r###"defaults: &defaults
  retries: 3
  timeout_ms: 250
  features: [mouse, paste, focus]

pipeline:
  - name: render
    <<: *defaults
    budget_ms: 12
    steps:
      - sanitize
      - diff
      - present
  - name: snapshots
    <<: *defaults
    sizes:
      - { w: 80, h: 24 }
      - { w: 120, h: 40 }
      - { w: 160, h: 50 }

theme:
  name: NordicFrost
  accent: "#6AD1E3"
  background: "#0E141B"
  foreground: "#E6EEF5"
  muted: "#6D7A86"

alerts:
  error_budget: 0.02
  p95_ms: 14
  p99_ms: 20
  rules:
    - { name: cpu_hot, threshold: 0.85, severity: warning }
    - { name: frame_drop, threshold: 0.12, severity: error }

profiles:
  - name: modern
    sync_output: true
    scroll_region: true
    ansi_budget: adaptive
  - name: mux
    sync_output: false
    scroll_region: false
    ansi_budget: conservative

evidence_ledger:
  enabled: true
  fields: [bayes_factor, risk, decision]
  retention: 7d
  posterior: { alpha: 3.5, beta: 92.5 }

ui:
  inline_mode:
    height: 12
    preserve_scrollback: true
  alt_screen:
    enabled: true
  channels: [stderr, otlp, ndjson]
  focus:
    default_pane: code
    mouse: true

telemetry:
  sampling: 0.25
  tags:
    service: ftui
    build: nightly
  exporters:
    - type: otlp
      endpoint: http://localhost:4317
    - type: file
      path: /var/log/ftui.ndjson

subscriptions:
  tick_ms: 16
  resize_coalesce_ms: 20
  log_tail:
    path: /var/log/app.log
    lines: 50

streams:
  markdown:
    chars_per_tick: 720
    cursor: block
  code:
    rotate_ms: 3200
  effects:
    rotate_ms: 1800
  charts:
    rotate_ms: 2400

panes:
  - id: charts
    title: Charts
    focusable: true
  - id: code
    title: Code
    focusable: true
  - id: markdown
    title: Markdown
    focusable: true
  - id: activity
    title: Activity
    focusable: true

charts:
  modes: [pulse, lines, bars, heatmap, matrix, composite, radar]
  palette: aurora

effects:
  stack: [gradient, glow, wave]
  max_layers: 3

cache:
  type: lru
  max_entries: 512
  ttl_seconds: 180

backpressure:
  max_queued_frames: 2
  drop_policy: oldest
"###,
    },
    CodeSample {
        label: "Bash",
        lang: "sh",
        code: r####"#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

ROOT="${ROOT:-/var/lib/ftui}"
LOG="${ROOT}/run.log"

mkdir -p "${ROOT}"/{cache,tmp,frames}
trap 'echo "[ERR] failed at line $LINENO" >> "$LOG"' ERR

function checkpoint() {
  local name="$1"
  printf "[%(%H:%M:%S)T] %s\n" -1 "$name" >> "$LOG"
}

checksum() {
  local input="$1"
  echo -n "$input" | sha256sum | cut -d' ' -f1
}

frames=("boot" "diff" "present" "idle")
for f in "${frames[@]}"; do
  case "$f" in
    boot) checkpoint "booting" ;;
    diff) checkpoint "diffing" ;;
    present) checkpoint "presenting" ;;
    *) checkpoint "idle" ;;
  esac
done

payload="$(jq -nc '{mode:"inline",uiHeight:12,features:["mouse","paste","focus"]}')"
curl -sS -H "Content-Type: application/json" -d "$payload" http://localhost:8080/session \
  | tee "${ROOT}/session.json" \
  | jq -r '.id' \
  | while read -r id; do
      printf "session=%s checksum=%s\n" "$id" "$(checksum "$id")"
    done

rotate_logs() {
  local max=5
  for ((i=max; i>=1; i--)); do
    if [[ -f "${LOG}.${i}" ]]; then
      mv "${LOG}.${i}" "${LOG}.$((i+1))"
    fi
  done
  [[ -f "$LOG" ]] && mv "$LOG" "${LOG}.1"
}

if [[ "${1:-}" == "--rotate" ]]; then
  rotate_logs
fi

usage() {
  cat <<EOF
Usage: $0 [--rotate] [--budget <rows>]
EOF
}

if [[ "${1:-}" == "--budget" ]]; then
  rows="${2:-0}"
  if (( rows > 120 )); then
    echo "budget: degrade (rows=$rows)" >> "$LOG"
  else
    echo "budget: full (rows=$rows)" >> "$LOG"
  fi
elif [[ "${1:-}" == "--help" ]]; then
  usage
fi

if [[ -n "${FTUI_TRACE:-}" ]]; then
  echo "trace enabled: ${FTUI_TRACE}" >> "$LOG"
  jq -nc --arg run "$(date +%s)" '{run:$run,mode:"inline",uiHeight:12}' \
    | tee "${ROOT}/trace.json" >/dev/null
fi

if [[ "${FTUI_HARNESS_VIEW:-}" == "" ]]; then
  export FTUI_HARNESS_VIEW="dashboard"
fi

echo "ready" >> "$LOG"
"####,
    },
    CodeSample {
        label: "C++",
        lang: "cpp",
        code: r###"// pipeline.cpp
#include <algorithm>
#include <chrono>
#include <cstdint>
#include <optional>
#include <string>
#include <unordered_map>
#include <vector>

template <typename T>
struct Frame {
  std::uint64_t id;
  bool dirty;
  T payload;
};

template <typename T>
auto diff(const std::vector<Frame<T>>& a, const std::vector<Frame<T>>& b)
    -> std::vector<std::uint64_t> {
  std::vector<std::uint64_t> out;
  out.reserve(a.size());
  for (std::size_t i = 0; i < a.size(); ++i) {
    if (a[i].dirty != b[i].dirty) {
      out.push_back(a[i].id);
    }
  }
  return out;
}

struct Budget {
  std::chrono::microseconds render;
  std::chrono::microseconds present;
};

int main() {
  Budget budget{std::chrono::microseconds{8000}, std::chrono::microseconds{4000}};
  std::vector<Frame<std::string>> frames;
  frames.push_back({1, true, "hello"});

  std::unordered_map<std::string, std::optional<int>> stats;
  stats["fps"] = 60;

  auto ids = diff(frames, frames);
  std::sort(ids.begin(), ids.end());
  return static_cast<int>(budget.render.count() + budget.present.count() + ids.size());
}

struct Metrics {
  double fps = 0.0;
  double p95 = 0.0;
  std::vector<std::string> notes;
};

inline Metrics summarize(const std::vector<Frame<std::string>>& frames) {
  Metrics m;
  m.fps = 60.0 - static_cast<double>(frames.size());
  m.p95 = 12.8;
  m.notes.push_back("inline");
  return m;
}

class Scheduler {
 public:
  explicit Scheduler(int budget_ms) : budget_ms_(budget_ms) {}
  bool allow(int dirty_rows) const { return dirty_rows <= budget_ms_ * 8; }
  std::string explain(int dirty_rows) const {
    return allow(dirty_rows) ? "full" : "degrade";
  }
 private:
  int budget_ms_;
};

static inline std::string trace_line(std::uint64_t frame, int dirty, const Scheduler& s) {
  return "frame=" + std::to_string(frame) + " dirty=" + std::to_string(dirty)
         + " policy=" + s.explain(dirty);
}

struct Evidence {
  std::uint64_t frame;
  std::uint64_t checksum;
  int dirty;
  std::string strategy;
};

inline std::uint64_t checksum_cells(const std::vector<Frame<std::string>>& frames) {
  std::uint64_t hash = 1469598103934665603ull;
  for (const auto& f : frames) {
    hash ^= f.id;
    hash *= 1099511628211ull;
    hash ^= static_cast<std::uint64_t>(f.payload.size());
    hash *= 1099511628211ull;
  }
  return hash;
}

inline Evidence build_evidence(std::uint64_t frame, int dirty,
                              const std::vector<Frame<std::string>>& frames) {
  Scheduler scheduler{8};
  return {frame, checksum_cells(frames), dirty, scheduler.explain(dirty)};
}
"###,
    },
    CodeSample {
        label: "Kotlin",
        lang: "kt",
        code: r###"// pipeline.kt
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.filter
import kotlinx.coroutines.flow.flow

sealed interface Event {
  data class Tick(val n: Long) : Event
  data class Resize(val w: Int, val h: Int) : Event
  data class Log(val line: String) : Event
}

data class Frame(val id: Long, val dirty: Boolean, val tags: Map<String, String>)

class Budget(private val renderMs: Double, private val presentMs: Double) {
  fun overBudget(elapsedMs: Double) = elapsedMs > (renderMs + presentMs)
}

fun stream(frames: List<Frame>): Flow<Frame> = flow {
  for (f in frames) {
    emit(f)
    delay(4)
  }
}

suspend fun pipeline(frames: List<Frame>, budget: Budget): Int {
  var count = 0
  stream(frames)
    .filter { it.dirty }
    .collect { count += it.tags.size }
  return if (budget.overBudget(5.0)) -1 else count
}

data class BudgetSnapshot(val frameMs: Double, val dirty: Int, val ok: Boolean)

suspend fun analyze(frames: List<Frame>): BudgetSnapshot {
  val dirty = frames.count { it.dirty }
  delay(2)
  return BudgetSnapshot(frameMs = 12.0, dirty = dirty, ok = dirty < 120)
}

data class Evidence(val frame: Long, val checksum: Long, val dirty: Int, val strategy: String)

fun checksum(frames: List<Frame>): Long {
  var hash = 0xcbf29ce484222325
  for (f in frames) {
    hash = (hash xor f.id) * 0x100000001b3
    hash = (hash xor f.tags.size.toLong()) * 0x100000001b3
  }
  return hash
}

fun buildEvidence(frame: Long, frames: List<Frame>): Evidence {
  val dirty = frames.count { it.dirty }
  val strat = if (dirty > 120) "degrade" else "full"
  return Evidence(frame, checksum(frames), dirty, strat)
}
"###,
    },
    CodeSample {
        label: "PowerShell",
        lang: "ps1",
        code: r###"#requires -Version 7.0
$ErrorActionPreference = "Stop"

class Frame {
  [long]$Id
  [bool]$Dirty
  [hashtable]$Tags
  Frame([long]$id, [bool]$dirty, [hashtable]$tags) {
    $this.Id = $id
    $this.Dirty = $dirty
    $this.Tags = $tags
  }
}

function Invoke-Pipeline {
  param(
    [Parameter(Mandatory)][Frame[]]$Frames,
    [int]$BudgetMs = 12
  )

  $start = Get-Date
  $dirty = $Frames | Where-Object { $_.Dirty }
  $count = 0

  foreach ($f in $dirty) {
    $count += $f.Tags.Count
  }

  $elapsed = (Get-Date) - $start
  if ($elapsed.TotalMilliseconds -gt $BudgetMs) {
    throw "over budget: $($elapsed.TotalMilliseconds)"
  }

  return $count
}

$frames = 1..5 | ForEach-Object {
  [Frame]::new($_, ($_ % 2 -eq 0), @{ idx = $_; ts = (Get-Date) })
}
Invoke-Pipeline -Frames $frames

$budget = [pscustomobject]@{
  frameMs = 12
  dirty = ($frames | Where-Object { $_.Dirty }).Count
  ok = $true
}
$budget | ConvertTo-Json -Depth 4

function Get-Checksum {
  param([Frame[]]$Frames)
  $hash = [UInt64]0xcbf29ce484222325
  foreach ($f in $Frames) {
    $hash = ($hash -bxor [UInt64]$f.Id) * 0x100000001b3
    $hash = ($hash -bxor [UInt64]$f.Tags.Count) * 0x100000001b3
  }
  return ("0x{0:x}" -f $hash)
}

$evidence = [pscustomobject]@{
  frame = 42
  checksum = (Get-Checksum -Frames $frames)
  dirty = ($frames | Where-Object { $_.Dirty }).Count
  strategy = "full"
}
$evidence | ConvertTo-Json -Depth 3
"###,
    },
    CodeSample {
        label: "C#",
        lang: "cs",
        code: r###"// Pipeline.cs
using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;

record Frame(long Id, bool Dirty, IReadOnlyDictionary<string, string> Tags);

static class Pipeline
{
    public static async Task<int> RunAsync(IEnumerable<Frame> frames, TimeSpan budget, CancellationToken ct)
    {
        var start = DateTime.UtcNow;
        var total = 0;

        foreach (var frame in frames.Where(f => f.Dirty))
        {
            ct.ThrowIfCancellationRequested();
            total += frame.Tags.Count;
            await Task.Delay(1, ct);
        }

        if (DateTime.UtcNow - start > budget)
            throw new TimeoutException("over budget");

        return total;
    }
}

static class Budget
{
    public static (bool ok, int dirty) Check(IEnumerable<Frame> frames, int limit)
    {
        var dirty = frames.Count(f => f.Dirty);
        return (dirty <= limit, dirty);
    }
}

record Evidence(long Frame, ulong Checksum, int Dirty, string Strategy);

static class EvidenceBuilder
{
    public static ulong Checksum(IEnumerable<Frame> frames)
    {
        ulong hash = 1469598103934665603;
        foreach (var f in frames)
        {
            hash ^= (ulong)f.Id;
            hash *= 1099511628211;
            hash ^= (ulong)f.Tags.Count;
            hash *= 1099511628211;
        }
        return hash;
    }

    public static Evidence Build(IEnumerable<Frame> frames, long frameId)
    {
        var dirty = frames.Count(f => f.Dirty);
        var strategy = dirty > 120 ? "degrade" : "full";
        return new Evidence(frameId, Checksum(frames), dirty, strategy);
    }
}
"###,
    },
    CodeSample {
        label: "Ruby",
        lang: "rb",
        code: r###"# pipeline.rb
require "set"

Frame = Struct.new(:id, :dirty, :tags, :ts, keyword_init: true)

module Telemetry
  def self.span(name)
    start = Process.clock_gettime(Process::CLOCK_MONOTONIC)
    yield
  ensure
    elapsed = (Process.clock_gettime(Process::CLOCK_MONOTONIC) - start) * 1000.0
    puts "[span=#{name}] #{elapsed.round(2)}ms"
  end
end

class Pipeline
  attr_reader :frames, :budget_ms

  def initialize(budget_ms: 12, max_frames: 60)
    @budget_ms = budget_ms
    @max_frames = max_frames
    @frames = []
  end

  def push(frame)
    @frames.unshift(frame.merge(ts: Time.now.to_f))
    @frames = @frames.take(@max_frames)
  end

  def render
    Telemetry.span("render") do
      start = Process.clock_gettime(Process::CLOCK_MONOTONIC)
      dirty = @frames.select(&:dirty)
      nodes = dirty.sum { |f| f.tags.size }
      elapsed = (Process.clock_gettime(Process::CLOCK_MONOTONIC) - start) * 1000.0
      raise "over budget" if elapsed > @budget_ms
      nodes
    end
  end

  def diff(prev, nxt)
    seen = prev.to_set
    nxt.reject { |x| seen.include?(x) }
  end
end

module Evidence
  def self.checksum(frames)
    hash = 0xcbf29ce484222325
    frames.each do |f|
      hash ^= f.id
      hash = (hash * 0x100000001b3) & 0xffffffffffffffff
      hash ^= f.tags.size
      hash = (hash * 0x100000001b3) & 0xffffffffffffffff
    end
    "0x#{hash.to_s(16)}"
  end

  def self.build(frames, frame_id)
    dirty = frames.count(&:dirty)
    strategy = dirty > 120 ? "degrade" : "full"
    {
      frame: frame_id,
      checksum: checksum(frames),
      dirty: dirty,
      strategy: strategy
    }
  end
end

pipe = Pipeline.new(budget_ms: 12)
5.times do |i|
  pipe.push(Frame.new(id: i + 1, dirty: i.even?, tags: { idx: i.to_s }))
end

budget = { frame_ms: 12, dirty_rows: pipe.frames.count(&:dirty) }
puts "budget=#{budget}"
puts "nodes=#{pipe.render}"
puts "evidence=#{Evidence.build(pipe.frames, 42)}"

class Scheduler
  def initialize(pipe)
    @pipe = pipe
  end

  def tick!
    frame = Frame.new(id: rand(1000), dirty: rand < 0.5, tags: { mode: "inline" })
    @pipe.push(frame)
    @pipe.render
  rescue => e
    warn "[violation] #{e}"
  end
end

sched = Scheduler.new(pipe)
3.times { sched.tick! }
"###,
    },
    CodeSample {
        label: "Java",
        lang: "java",
        code: r###"// Pipeline.java
package ftui.runtime;

import java.time.Duration;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.Executor;
import java.util.concurrent.Executors;
import java.util.stream.Collectors;

sealed interface Event permits Tick, Resize, Log, Quit {}
record Tick(long n) implements Event {}
record Resize(int w, int h) implements Event {}
record Log(String line) implements Event {}
record Quit() implements Event {}

record Frame(long id, boolean dirty, Map<String, String> tags) {}

final class Budget {
    private final Duration render;
    private final Duration present;

    Budget(Duration render, Duration present) {
        this.render = render;
        this.present = present;
    }

    boolean overBudget(Duration elapsed) {
        return elapsed.compareTo(render.plus(present)) > 0;
    }
}

final class Pipeline {
    private final Executor io = Executors.newFixedThreadPool(2);

    List<Long> diff(List<Frame> a, List<Frame> b) {
        var out = new ArrayList<Long>(a.size());
        for (int i = 0; i < a.size(); i++) {
            if (a.get(i).dirty() != b.get(i).dirty()) {
                out.add(a.get(i).id());
            }
        }
        return out;
    }

    CompletableFuture<Integer> renderAsync(List<Frame> frames, Budget budget) {
        var start = System.nanoTime();
        return CompletableFuture.supplyAsync(() -> {
            int count = frames.stream()
                .filter(Frame::dirty)
                .mapToInt(f -> f.tags().size())
                .sum();
            var elapsed = Duration.ofNanos(System.nanoTime() - start);
            if (budget.overBudget(elapsed)) {
                throw new IllegalStateException("over budget");
            }
            return count;
        }, io);
    }
}

class Main {
    public static void main(String[] args) {
        var frames = List.of(
            new Frame(1, true, Map.of("mode", "inline")),
            new Frame(2, false, Map.of("mode", "alt"))
        );
        var budget = new Budget(Duration.ofMillis(8), Duration.ofMillis(4));
        var pipeline = new Pipeline();
        var dirty = frames.stream().filter(Frame::dirty).collect(Collectors.toList());
        var result = pipeline.renderAsync(dirty, budget).join();
        System.out.println("count=" + result + " diff=" + pipeline.diff(frames, frames).size());
    }
}

record BudgetSnapshot(int frameMs, int dirty, boolean ok) {}

record Evidence(long frame, long checksum, int dirty, String strategy) {}

final class Checksums {
    static long hashFrames(List<Frame> frames) {
        long hash = 0xcbf29ce484222325L;
        for (var f : frames) {
            hash ^= f.id();
            hash *= 0x100000001b3L;
            hash ^= f.tags().size();
            hash *= 0x100000001b3L;
        }
        return hash;
    }

    static Evidence buildEvidence(long frame, List<Frame> frames) {
        var dirty = (int)frames.stream().filter(Frame::dirty).count();
        var strategy = dirty > 120 ? "degrade" : "full";
        return new Evidence(frame, hashFrames(frames), dirty, strategy);
    }
}
"###,
    },
    CodeSample {
        label: "C",
        lang: "c",
        code: r###"// pipeline.c
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#define MAX_TAGS 16u
#define CLAMP(x, lo, hi) ((x) < (lo) ? (lo) : ((x) > (hi) ? (hi) : (x)))

typedef struct {
    const char *key;
    const char *value;
} tag_t;

typedef struct {
    uint64_t id;
    bool dirty;
    tag_t tags[MAX_TAGS];
    uint8_t tag_count;
} frame_t;

typedef struct {
    uint32_t render_ms;
    uint32_t present_ms;
} budget_t;

static inline uint32_t hash_u32(uint32_t x) {
    x ^= x >> 16;
    x *= 0x7feb352d;
    x ^= x >> 15;
    x *= 0x846ca68b;
    x ^= x >> 16;
    return x;
}

static uint32_t diff_ids(const frame_t *a, const frame_t *b, size_t n, uint64_t *out) {
    uint32_t count = 0;
    for (size_t i = 0; i < n; i++) {
        if (a[i].dirty != b[i].dirty) {
            out[count++] = a[i].id;
        }
    }
    return count;
}

static uint32_t render(const frame_t *frames, size_t n) {
    uint32_t total = 0;
    for (size_t i = 0; i < n; i++) {
        if (frames[i].dirty) {
            total += frames[i].tag_count;
        }
    }
    return total;
}

int main(void) {
    frame_t frames[2] = {
        { .id = 1, .dirty = true, .tags = {{"mode", "inline"}}, .tag_count = 1 },
        { .id = 2, .dirty = false, .tags = {{"mode", "alt"}}, .tag_count = 1 },
    };
    budget_t budget = { .render_ms = 8, .present_ms = 4 };
    uint64_t ids[8] = {0};

    uint32_t count = render(frames, 2);
    uint32_t changed = diff_ids(frames, frames, 2, ids);
    uint32_t score = hash_u32((uint32_t)count + changed + CLAMP(budget.render_ms, 1, 16));
    printf("count=%u changed=%u score=%u\n", count, changed, score);
    return 0;
}

static bool budget_ok(budget_t budget, uint32_t elapsed_ms) {
    return elapsed_ms <= (budget.render_ms + budget.present_ms);
}

static uint64_t checksum_frames(const frame_t *frames, size_t n) {
    uint64_t hash = 0xcbf29ce484222325ULL;
    for (size_t i = 0; i < n; i++) {
        hash ^= frames[i].id;
        hash *= 0x100000001b3ULL;
        hash ^= frames[i].tag_count;
        hash *= 0x100000001b3ULL;
    }
    return hash;
}

typedef struct {
    uint64_t frame;
    uint64_t checksum;
    uint32_t dirty;
    const char *strategy;
} evidence_t;

static evidence_t build_evidence(uint64_t frame, const frame_t *frames, size_t n) {
    uint32_t dirty = 0;
    for (size_t i = 0; i < n; i++) {
        dirty += frames[i].dirty ? 1u : 0u;
    }
    const char *strategy = dirty > 120 ? "degrade" : "full";
    evidence_t e = { frame, checksum_frames(frames, n), dirty, strategy };
    return e;
}
"###,
    },
    CodeSample {
        label: "Swift",
        lang: "swift",
        code: r###"// Pipeline.swift
import Foundation

protocol Event {}
struct Tick: Event { let n: Int }
struct Resize: Event { let w: Int; let h: Int }
struct Log: Event { let line: String }

@propertyWrapper
struct Clamped<Value: Comparable> {
    private var value: Value
    private let range: ClosedRange<Value>
    init(wrappedValue: Value, _ range: ClosedRange<Value>) {
        self.range = range
        self.value = min(max(wrappedValue, range.lowerBound), range.upperBound)
    }
    var wrappedValue: Value {
        get { value }
        set { value = min(max(newValue, range.lowerBound), range.upperBound) }
    }
}

struct Frame: Sendable {
    let id: UUID
    let dirty: Bool
    let tags: [String: String]
}

actor Budget {
    @Clamped(0...32) var renderMs: Int
    @Clamped(0...32) var presentMs: Int
    init(renderMs: Int, presentMs: Int) {
        self.renderMs = renderMs
        self.presentMs = presentMs
    }
    func overBudget(_ elapsedMs: Int) -> Bool {
        elapsedMs > (renderMs + presentMs)
    }
}

func diff(_ a: [Frame], _ b: [Frame]) -> [UUID] {
    zip(a, b).compactMap { $0.dirty != $1.dirty ? $0.id : nil }
}

func render(frames: [Frame]) async throws -> Int {
    try await Task.sleep(nanoseconds: 1_000_000)
    return frames.filter { $0.dirty }.map { $0.tags.count }.reduce(0, +)
}

struct Metric {
    let name: String
    let value: Int
}

func summarize(_ frames: [Frame]) -> [Metric] {
    let dirty = frames.filter { $0.dirty }.count
    return [
        Metric(name: "dirty", value: dirty),
        Metric(name: "total", value: frames.count),
    ]
}

@main
struct Main {
    static func main() async {
        let frames = [
            Frame(id: UUID(), dirty: true, tags: ["mode": "inline"]),
            Frame(id: UUID(), dirty: false, tags: ["mode": "alt"]),
        ]
        let budget = Budget(renderMs: 8, presentMs: 4)
        let count = try? await render(frames: frames)
        let elapsed = 7
        let metrics = summarize(frames)
        for metric in metrics {
            print("\(metric.name)=\(metric.value)")
        }
        print("count=\(count ?? -1) diff=\(diff(frames, frames).count) over=\(await budget.overBudget(elapsed))")
    }
}

struct BudgetSnapshot: Codable {
    let frameMs: Int
    let dirty: Int
    let ok: Bool
}

struct Evidence: Codable {
    let frame: Int
    let checksum: UInt64
    let dirty: Int
    let strategy: String
}

func checksum(_ frames: [Frame]) -> UInt64 {
    var hash: UInt64 = 0xcbf29ce484222325
    for frame in frames {
        hash ^= UInt64(frame.tags.count)
        hash &*= 0x100000001b3
        hash ^= UInt64(frame.id.uuidString.count)
        hash &*= 0x100000001b3
    }
    return hash
}
"###,
    },
    CodeSample {
        label: "PHP",
        lang: "php",
        code: r###"<?php
declare(strict_types=1);

namespace Ftui\Runtime;

use DateTimeImmutable;
use Generator;

#[\Attribute(\Attribute::TARGET_CLASS)]
final class Trace {}

enum Mode: string { case Inline = "inline"; case Alt = "alt"; }

final readonly class Frame {
    public function __construct(
        public int $id,
        public bool $dirty,
        public array $tags,
    ) {}
}

trait Budgeted {
    public function overBudget(float $elapsedMs): bool;
}

#[Trace]
final class Pipeline implements Budgeted {
    public function __construct(
        private float $renderMs,
        private float $presentMs,
    ) {}

    public function overBudget(float $elapsedMs): bool {
        return $elapsedMs > ($this->renderMs + $this->presentMs);
    }

    public function diff(array $a, array $b): array {
        $out = [];
        foreach ($a as $i => $frame) {
            if ($frame->dirty !== $b[$i]->dirty) {
                $out[] = $frame->id;
            }
        }
        return $out;
    }

    public function stream(array $frames): Generator {
        foreach ($frames as $frame) {
            if ($frame->dirty) {
                yield $frame;
            }
        }
    }
}

$frames = [
    new Frame(1, true, ["mode" => Mode::Inline->value]),
    new Frame(2, false, ["mode" => Mode::Alt->value]),
];
$pipeline = new Pipeline(8.0, 4.0);
$count = 0;
foreach ($pipeline->stream($frames) as $frame) {
    $count += count($frame->tags);
}
$elapsed = (float) (new DateTimeImmutable())->format("v");
function summarize(array $frames): array {
    $dirty = array_filter($frames, fn(Frame $f) => $f->dirty);
    return [
        "dirty" => count($dirty),
        "total" => count($frames),
    ];
}

$summary = summarize($frames);
echo "count={$count} diff=" . count($pipeline->diff($frames, $frames)) .
     " over=" . ($pipeline->overBudget($elapsed) ? "yes" : "no") .
     " dirty=" . $summary["dirty"] . "/" . $summary["total"];

echo "\n" . json_encode([
    "budget" => ["frameMs" => 12, "dirty" => $summary["dirty"]],
    "mode" => Mode::Inline->value,
], JSON_PRETTY_PRINT);

function checksum(array $frames): string {
    $hash = 0xcbf29ce484222325;
    foreach ($frames as $frame) {
        $hash ^= $frame->id;
        $hash *= 0x100000001b3;
        $hash ^= count($frame->tags);
        $hash *= 0x100000001b3;
    }
    return sprintf("0x%x", $hash);
}

$evidence = [
    "frame" => 42,
    "checksum" => checksum($frames),
    "dirty" => $summary["dirty"],
    "strategy" => $summary["dirty"] > 120 ? "degrade" : "full",
];
echo "\n" . json_encode($evidence, JSON_PRETTY_PRINT);
"###,
    },
    CodeSample {
        label: "HTML",
        lang: "html",
        code: r###"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>FrankenTUI</title>
    <link rel="preconnect" href="https://fonts.example">
    <style>
      :root { color-scheme: dark; }
      main { display: grid; gap: 16px; }
    </style>
    <script type="module">
      const $ = (sel) => document.querySelector(sel);
      const bus = new EventTarget();
      bus.addEventListener("tick", (e) => $("#fps").textContent = e.detail);
      setInterval(() => bus.dispatchEvent(new CustomEvent("tick", { detail: 60 })), 1000);
    </script>
  </head>
  <body>
    <header>
      <h1>FrankenTUI</h1>
      <p data-mode="inline">Deterministic output</p>
    </header>
    <main>
      <section>
        <button type="button" aria-pressed="false">Toggle Mode</button>
        <span id="fps">60</span>
      </section>
      <section class="grid" aria-label="metrics">
        <article class="card">
          <h2>Deterministic Render</h2>
          <p>Buffer → Diff → Presenter</p>
        </article>
        <article class="card">
          <h2>Inline UI</h2>
          <p>Scrollback preserved</p>
        </article>
      </section>
      <section aria-live="polite" id="log">ready…</section>
      <template id="card">
        <article><slot></slot></article>
      </template>
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <path d="M4 12h16M12 4v16" />
      </svg>
      <section class="grid" aria-label="panels">
        <article class="card">
          <h3>Inline Mode</h3>
          <p>Scrollback preserved, cursor saved/restored.</p>
          <pre><code>ScreenMode::Inline { ui_height: 12 }</code></pre>
        </article>
        <article class="card">
          <h3>Alt Screen</h3>
          <p>Full-screen takeover for immersive apps.</p>
          <pre><code>ScreenMode::AltScreen</code></pre>
        </article>
      </section>
      <section aria-label="evidence">
        <h3>Evidence Ledger</h3>
        <table>
          <thead><tr><th>Frame</th><th>Dirty</th><th>Strategy</th></tr></thead>
          <tbody>
            <tr><td>421</td><td>84</td><td>full</td></tr>
            <tr><td>422</td><td>164</td><td>degrade</td></tr>
          </tbody>
        </table>
        <pre><code>{
  "frame": 422,
  "checksum": "0x9f2d",
  "policy": "degrade",
  "notes": ["row overflow", "p95>16ms"]
}</code></pre>
      </section>
    </main>
  </body>
</html>
"###,
    },
    CodeSample {
        label: "CSS",
        lang: "css",
        code: r###":root {
  --bg: #0e141b;
  --accent: #6ad1e3;
  --text: #e6eef5;
  --radius: 14px;
}

@layer base, components, utilities;

@layer base {
  * { box-sizing: border-box; }
  body {
    margin: 0;
    font-family: "Space Grotesk", system-ui, sans-serif;
    color: var(--text);
    background: radial-gradient(1200px circle at 20% 10%, #1b2a3a, var(--bg));
  }
}

@layer components {
  .card {
    border-radius: var(--radius);
    padding: clamp(16px, 3vw, 28px);
    background: color-mix(in oklab, var(--bg), var(--accent) 12%);
    box-shadow: 0 20px 60px rgb(0 0 0 / 0.35);
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
    gap: 12px;
  }

  #log {
    font-family: "IBM Plex Mono", ui-monospace, monospace;
    opacity: 0.8;
  }
}

@supports (backdrop-filter: blur(12px)) {
  .glass { backdrop-filter: blur(12px); }
}

@container (min-width: 300px) {
  .card { transform: translateY(-2px); }
}

@property --glow {
  syntax: "<number>";
  inherits: false;
  initial-value: 0.0;
}

@keyframes pulse {
  0% { transform: translateY(0); opacity: 0.7; }
  50% { transform: translateY(-6px); opacity: 1; }
  100% { transform: translateY(0); opacity: 0.7; }
}

@media (prefers-reduced-motion: reduce) {
  .pulse { animation: none; }
}

.badge {
  display: inline-flex;
  gap: 6px;
  padding: 4px 10px;
  border-radius: 999px;
  background: color-mix(in oklab, var(--accent), black 60%);
  font-size: 12px;
  text-transform: uppercase;
}

.pulse {
  animation: pulse 2.4s ease-in-out infinite;
}

.glow {
  box-shadow: 0 0 calc(var(--glow) * 24px) rgba(106, 209, 227, 0.4);
}

table {
  width: 100%;
  border-collapse: collapse;
  font-size: 13px;
}
th, td {
  padding: 6px 10px;
  border-bottom: 1px solid color-mix(in oklab, var(--accent), transparent 80%);
  text-align: left;
}
tbody tr:hover {
  background: color-mix(in oklab, var(--accent), transparent 88%);
}

.panel-title {
  letter-spacing: 0.06em;
  text-transform: uppercase;
  font-weight: 600;
}

.chip {
  display: inline-block;
  padding: 2px 8px;
  border-radius: 6px;
  background: rgb(255 255 255 / 0.08);
}
"###,
    },
    CodeSample {
        label: "Fish",
        lang: "fish",
        code: r###"# pipeline.fish
function checksum --argument value
  echo -n $value | sha256sum | string split ' ' | head -n 1
end

function render --argument budget
  set -l frames boot diff present idle
  set -l count 0

  for f in $frames
    switch $f
      case boot diff present
        set count (math $count + 1)
      case '*'
        continue
    end
  end

  if test $count -gt $budget
    echo "over budget"
    return 1
  end

  echo "count=$count checksum="(checksum $count)
end

set -l budget 3
render $budget

if test (count $argv) -gt 0
  argparse 'b/budget=' -- $argv
  if set -q _flag_budget
    set budget $_flag_budget
    render $budget
  end
end

function budget_report --argument budget
  set -l dirty (math $budget + 2)
  echo "budget=$budget dirty=$dirty ok="(test $dirty -lt 8; and echo yes; or echo no)
end

budget_report $budget

function evidence --argument frame budget
  set -l dirty (math $budget + 2)
  set -l strategy (test $dirty -lt 8; and echo full; or echo degrade)
  echo "frame=$frame dirty=$dirty strategy=$strategy checksum="(checksum $dirty)
end

evidence 42 $budget
"###,
    },
    CodeSample {
        label: "Lua",
        lang: "lua",
        code: r###"-- pipeline.lua
local Frame = {}
Frame.__index = Frame

function Frame.new(id, dirty, tags)
  return setmetatable({ id = id, dirty = dirty, tags = tags or {} }, Frame)
end

local function diff(a, b)
  local out = {}
  for i, f in ipairs(a) do
    if f.dirty ~= b[i].dirty then
      out[#out + 1] = f.id
    end
  end
  return out
end

local function render(frames)
  local total = 0
  for _, f in ipairs(frames) do
    if f.dirty then
      total = total + #f.tags
    end
  end
  return total
end

local function stats(frames)
  local dirty = 0
  for _, f in ipairs(frames) do
    if f.dirty then dirty = dirty + 1 end
  end
  return { dirty = dirty, total = #frames }
end

local frames = {
  Frame.new(1, true, { "inline", "focus" }),
  Frame.new(2, false, { "alt" }),
}

local result = render(frames)
local changed = diff(frames, frames)
local summary = stats(frames)
print(("count=%d diff=%d"):format(result, #changed))
print(("dirty=%d/%d"):format(summary.dirty, summary.total))

local function budget(frames, limit)
  local summary = stats(frames)
  return { ok = summary.dirty <= limit, dirty = summary.dirty, limit = limit }
end

local report = budget(frames, 3)
print(("budget ok=%s dirty=%d limit=%d"):format(tostring(report.ok), report.dirty, report.limit))

local function checksum(frames)
  local hash = 0xcbf29ce484222325
  for _, f in ipairs(frames) do
    hash = (hash ~ f.id) * 0x100000001b3
    hash = (hash ~ #f.tags) * 0x100000001b3
  end
  return string.format("0x%x", hash)
end

local evidence = {
  frame = 42,
  checksum = checksum(frames),
  dirty = summary.dirty,
  strategy = summary.dirty > 120 and "degrade" or "full",
}
print(("evidence=%s"):format(require("json").encode(evidence)))
"###,
    },
    CodeSample {
        label: "R",
        lang: "r",
        code: r###"# pipeline.R
suppressPackageStartupMessages({
  library(dplyr)
  library(purrr)
})

set.seed(42)
frames <- tibble(
  id = 1:6,
  dirty = c(TRUE, FALSE, TRUE, FALSE, TRUE, FALSE),
  tags = map(id, ~ list(mode = ifelse(.x %% 2 == 0, "alt", "inline")))
)

diff_ids <- function(prev, next) {
  prev %>%
    left_join(next, by = "id", suffix = c("_prev", "_next")) %>%
    filter(dirty_prev != dirty_next) %>%
    pull(id)
}

render <- function(frames) {
  frames %>%
    filter(dirty) %>%
    mutate(tag_count = map_int(tags, length)) %>%
    summarise(total = sum(tag_count)) %>%
    pull(total)
}

count <- render(frames)
changed <- diff_ids(frames, frames)
message(glue::glue("count={count} diff={length(changed)}"))

summary <- frames %>%
  mutate(tag_count = map_int(tags, length)) %>%
  summarise(dirty = sum(dirty), total = n(), tags = sum(tag_count))

print(summary)

budget <- tibble(frame_ms = 12, dirty = summary$dirty, ok = summary$dirty < 5)
print(budget)

checksum_frames <- function(frames) {
  hash <- as.numeric(0xcbf29ce484222325)
  for (row in seq_len(nrow(frames))) {
    hash <- bitwXor(hash, frames$id[row])
    hash <- (hash * 0x100000001b3) %% 2^64
  }
  hash
}

evidence <- tibble(
  frame = 42,
  checksum = checksum_frames(frames),
  dirty = summary$dirty,
  strategy = ifelse(summary$dirty > 120, "degrade", "full")
)
print(evidence)
"###,
    },
    CodeSample {
        label: "TOML",
        lang: "toml",
        code: r###"# Cargo.toml
[package]
name = "ftui"
version = "0.1.0"
edition = "2024"
authors = ["FrankenTUI Team"]
license = "UNLICENSED"
description = "Deterministic TUI kernel"
keywords = ["tui", "terminal", "determinism", "rendering"]

[dependencies]
crossterm = "0.27"
unicode-width = "0.1"
tracing = { version = "0.1", features = ["log"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
lru = "0.16.3"

[features]
default = ["widgets"]
widgets = []
simd = ["dep:simd-json"]
telemetry = ["dep:opentelemetry", "dep:opentelemetry-otlp"]

[profile.release]
opt-level = "z"
lto = true
codegen-units = 1
panic = "abort"
strip = true

[[bench]]
name = "diff"
harness = false

[workspace]
members = [
  "crates/ftui-core",
  "crates/ftui-render",
  "crates/ftui-runtime",
  "crates/ftui-widgets",
  "crates/ftui-text",
  "crates/ftui-style",
]

[profile.dev]
opt-level = 1

[profile.bench]
debug = true
strip = false

[workspace.dependencies]
unicode-segmentation = "1.12.0"
insta = "1.43.0"

[package.metadata.ftui]
screen_mode = "inline"
ui_height = 12
theme = "NordicFrost"
evidence_ledger = true
snapshots = ["80x24", "120x40"]

[lints.rust]
unsafe_code = "forbid"

[lints.clippy]
pedantic = "deny"
nursery = "deny"
"###,
    },
    CodeSample {
        label: "JavaScript",
        lang: "js",
        code: r###"// pipeline.js
const { EventEmitter } = require('events');

class Pipeline extends EventEmitter {
  constructor(budgetMs = 12) {
    super();
    this.budgetMs = budgetMs;
    this.frames = [];
  }

  push(frame) {
    this.frames.push({ ...frame, ts: Date.now() });
    if (this.frames.length > 60) this.frames.shift();
  }

  render() {
    const start = performance.now();
    const dirty = this.frames.filter(f => f.dirty);
    
    // Simulate work
    const nodes = dirty.reduce((acc, f) => acc + Object.keys(f.tags).length, 0);
    
    const elapsed = performance.now() - start;
    if (elapsed > this.budgetMs) {
      this.emit('violation', { elapsed, budget: this.budgetMs });
    }
    
    return { nodes, elapsed };
  }
}

const pipe = new Pipeline();
pipe.on('violation', console.warn);

setInterval(() => {
  pipe.push({ id: 1, dirty: Math.random() > 0.5, tags: { mode: 'inline' } });
  console.log(pipe.render());
}, 16);

function checksum(frames) {
  let hash = 0xcbf29ce484222325n;
  for (const f of frames) {
    hash ^= BigInt(f.id);
    hash *= 0x100000001b3n;
    hash ^= BigInt(Object.keys(f.tags).length);
    hash *= 0x100000001b3n;
  }
  return `0x${hash.toString(16)}`;
}

function evidence(pipe) {
  const dirty = pipe.frames.filter(f => f.dirty).length;
  return {
    frame: pipe.frames.length,
    checksum: checksum(pipe.frames),
    dirty,
    strategy: dirty > 120 ? 'degrade' : 'full',
  };
}

console.log(evidence(pipe));
"###,
    },
    CodeSample {
        label: "Markdown",
        lang: "md",
        code: r###"# FrankenTUI Launch Plan

> **Premise:** render *truth*, not pixels.

## Goals
- [x] Deterministic output
- [x] Inline mode + scrollback
- [x] One-writer rule
- [ ] GPU raster (not needed)

## Architecture Map
1. **Core** — input + terminal session
2. **Render** — buffer, diff, presenter
3. **Runtime** — Elm loop + subscriptions
4. **Widgets** — composable UI

```rust
fn view(frame: &mut Frame) {
    let area = Rect::new(0, 0, frame.width(), 1);
    Paragraph::new("ok").render(area, frame);
}
```

```diff
- write_stdout_immediately()
+ buffer_diff_then_present()
```

```json
{ "mode": "inline", "ui_height": 12, "sync": true }
```

```toml
[profile.release]
opt-level = "z"
lto = true
panic = "abort"
```

## Evidence Ledger
| Frame | Dirty | Strategy | Checksum |
| --: | --: | :-- | :-- |
| 120 | 84 | full | `0x9f2d` |
| 121 | 164 | degrade | `0xa120` |
| 122 | 44 | spans | `0xb3cc` |

> [!NOTE]
> The posterior `α=3.5, β=92.5` favors sparse diffs.

### Control Surface
| Panel | Shortcut | Notes |
| --- | --- | --- |
| Charts | `g` | cycle modes |
| Code | `c` | 27 languages |
| Markdown | `m` | stream GFM |

#### Task Checklist
- [x] Dirty-row tracking
- [x] Sync update brackets
- [x] ANSI cost model
- [ ] Cross-terminal golden tests

```mermaid
graph TD
  A[Input] --> B[Runtime]
  B --> C[Frame]
  C --> D[BufferDiff]
  D --> E[Presenter]
  E --> F[Terminal]
```

> [!TIP]
> Use `Cmd::batch` for side effects without blocking.

### Edge Notes
1. **No unsafe code**
2. **Cells are 16 bytes**
3. **Output is atomic**

> [!WARNING]
> Never call `process::exit()` before `TerminalSession` drops.

<details>
<summary>Trace Snapshot</summary>

```json
{
  "frame": 42,
  "dirty": 84,
  "strategy": "full",
  "checksum": "0x9f2d"
}
```
</details>

Inline mode keeps logs above UI:
> "Scrollback survives; chrome stays stable."

[^1]: Determinism beats magic.
"###,
    },
    CodeSample {
        label: "Elixir",
        lang: "ex",
        code: r###"# pipeline.ex
defmodule Pipeline do
  use GenServer

  def start_link(budget_ms) do
    GenServer.start_link(__MODULE__, budget_ms, name: __MODULE__)
  end

  @impl true
  def init(budget_ms) do
    {:ok, %{budget: budget_ms, frames: []}}
  end

  @impl true
  def handle_cast({:push, frame}, state) do
    # Keep last 60 frames
    new_frames = [frame | state.frames] |> Enum.take(60)
    {:noreply, %{state | frames: new_frames}}
  end

  @impl true
  def handle_call(:render, _from, state) do
    start_time = System.monotonic_time(:millisecond)

    dirty = Enum.filter(state.frames, & &1.dirty)
    count = Enum.reduce(dirty, 0, fn f, acc -> acc + map_size(f.tags) end)

    elapsed = System.monotonic_time(:millisecond) - start_time
    if elapsed > state.budget do
      {:reply, {:error, :over_budget}, state}
    else
      {:reply, {:ok, count}, state}
    end
  end
end

# Usage
{:ok, pid} = Pipeline.start_link(12)
GenServer.cast(pid, {:push, %{id: 1, dirty: true, tags: %{mode: "inline"}}})

defmodule Evidence do
  use Bitwise

  def checksum(frames) do
    Enum.reduce(frames, 0xcbf29ce484222325, fn f, acc ->
      acc
      |> bxor(f.id)
      |> Kernel.*(0x100000001b3)
      |> bxor(map_size(f.tags))
      |> Kernel.*(0x100000001b3)
    end)
  end

  def build(frames, frame_id) do
    dirty = Enum.count(frames, & &1.dirty)
    %{
      frame: frame_id,
      checksum: checksum(frames),
      dirty: dirty,
      strategy: if(dirty > 120, do: "degrade", else: "full")
    }
  end
end
"###,
    },
    CodeSample {
        label: "Haskell",
        lang: "hs",
        code: r###"-- Pipeline.hs
module Pipeline where

import Data.List (foldl')
import Data.Bits (xor)
import qualified Data.Map as M
import System.CPUTime

data Frame = Frame {
    frameId :: Int,
    dirty   :: Bool,
    tags    :: M.Map String String
} deriving (Show, Eq)

type Budget = Integer

render :: [Frame] -> Budget -> Either String Int
render frames budget =
    let dirtyFrames = filter dirty frames
        cost = foldl' (\acc f -> acc + M.size (tags f)) 0 dirtyFrames
    in if toInteger (length dirtyFrames) > budget
       then Left "Over Budget"
       else Right cost

main :: IO ()
main = do
    start <- getCPUTime
    let frames = [Frame 1 True M.empty, Frame 2 False M.empty]
    let result = render frames 100
    end <- getCPUTime
    let diff = (end - start) `div` 1000000000
    print $ "Result: " ++ show result ++ " Time: " ++ show diff ++ "ms"

checksum :: [Frame] -> Integer
checksum = foldl' (\acc f -> (acc `xor` toInteger (frameId f)) * 0x100000001b3) 0xcbf29ce484222325

data Evidence = Evidence { eFrame :: Int, eChecksum :: Integer, eDirty :: Int, eStrategy :: String }

buildEvidence :: [Frame] -> Int -> Evidence
buildEvidence frames fid =
  let dirtyCount = length (filter dirty frames)
      strategy = if dirtyCount > 120 then "degrade" else "full"
  in Evidence fid (checksum frames) dirtyCount strategy
"###,
    },
    CodeSample {
        label: "Zig",
        lang: "zig",
        code: r###"// pipeline.zig
const std = @import("std");

const Frame = struct {
    id: u64,
    dirty: bool,
    tags: std.StringHashMap([]const u8),
};

pub fn render(allocator: std.mem.Allocator, frames: []const Frame, budget: u64) !u64 {
    var timer = try std.time.Timer.start();
    var count: u64 = 0;
    
    for (frames) |f| {
        if (f.dirty) {
            count += f.tags.count();
        }
    }

    const elapsed = timer.read() / std.time.ns_per_ms;
    if (elapsed > budget) {
        return error.BudgetExceeded;
    }
    return count;
}

test "pipeline benchmark" {
    var frames = [_]Frame{};
    // Zig compile-time checks ensure correctness
    try std.testing.expectEqual(try render(std.testing.allocator, &frames, 1000), 0);
}

fn checksum(frames: []const Frame) u64 {
    var hash: u64 = 0xcbf29ce484222325;
    for (frames) |f| {
        hash = (hash ^ f.id) * 0x100000001b3;
        hash = (hash ^ @intCast(u64, f.tags.count())) * 0x100000001b3;
    }
    return hash;
}

const Evidence = struct {
    frame: u64,
    checksum: u64,
    dirty: u64,
    strategy: []const u8,
};

fn buildEvidence(frames: []const Frame, frame: u64) Evidence {
    var dirty: u64 = 0;
    for (frames) |f| if (f.dirty) dirty += 1;
    const strategy = if (dirty > 120) "degrade" else "full";
    return Evidence{ .frame = frame, .checksum = checksum(frames), .dirty = dirty, .strategy = strategy };
}
"###,
    },
];

const DASH_MARKDOWN_SAMPLES: &[&str] = &[
    r###"# FrankenTUI Field Notes

> **Goal:** deterministic output, no surprises.

## Highlights
- [x] Inline mode with scrollback
- [x] One-writer rule
- [x] 16-byte cell invariant
- [ ] GPU raster (not needed)

### Architecture Table
| Layer | Role | Notes |
| --- | --- | --- |
| core | input | crossterm events |
| render | diff | row-major scan |
| runtime | loop | Elm-style model |

```rust
fn render(frame: &mut Frame) {
    frame.clear();
}
```

```json
{ "mode": "inline", "ui_height": 12 }
```

```mermaid
graph TD
  A[Input] --> B[Runtime]
  B --> C[Frame]
  C --> D[BufferDiff]
  D --> E[Presenter]
```

```diff
- write-stdout
+ buffered-presenter
```

| Stat | Value | Trend |
| :-- | --: | :--: |
| FPS | 60 | ▲ |
| Diff | 3.2ms | ▼ |

<details>
<summary>Budget Policy</summary>

- render: 8ms
- present: 4ms
- degrade: on overflow

</details>

> [!NOTE]
> Math: `E = mc^2` and `∑ᵢ xᵢ`

Footnote[^1] and **links**: https://ftui.dev

[^1]: Determinism beats magic.

## Diff Ledger
- **Strategy:** Full → DirtyRows → Spans
- **Posterior:** α=3.5, β=92.5
- **Decision:** DirtyRows (expected cost 0.41)

### Code Snippets
```toml
[profile.release]
opt-level = "z"
lto = true
codegen-units = 1
```

```yaml
streaming:
  markdown:
    chars_per_tick: 240
```

> [!WARNING]
> Overflow detected in 2/120 rows. Degraded to safe mode.
"###,
    r###"# Rendering Playbook

1. **Build** the frame
2. **Diff** buffers
3. **Present** ANSI

## Task List
- [x] Dirty-row tracking
- [x] ANSI cost model
- [ ] Benchmarks

| Metric | Target |
| --- | --- |
| Frame | <16ms |
| Diff | <4ms |

```bash
FTUI_HARNESS_SCREEN_MODE=inline cargo run -p ftui-harness
```

> [!TIP]
> Use `Cmd::batch` for side effects.

```toml
[profile.release]
opt-level = "z"
lto = true
panic = "abort"
```

> [!WARNING]
> Never call `process::exit()` before `TerminalSession` drops.

### Evidence Table
| Frame | Dirty | Strategy | Checksum |
| --: | --: | :-- | :-- |
| 120 | 84 | full | `0x9f2d` |
| 121 | 164 | degrade | `0xa120` |

```diff
- emit-per-cell
+ coalesce-run
```

> [!NOTE]
> Inline mode preserves scrollback while the UI remains stable.
"###,
    r###"# Determinism Checklist

- [x] Fixed seed
- [x] Evidence ledger enabled
- [x] Checksums chained
- [ ] External I/O in render path (forbidden)

## Control Surface
| Panel | Focus | Shortcut |
| --- | --- | --- |
| Charts | yes | `g` |
| Code | yes | `c` |
| Markdown | yes | `m` |

### Inline vs Alt
> Inline keeps logs visible above the UI.
> Alt-screen is immersive, but scrollback is hidden.

```json
{ "screen": "inline", "ui_height": 12, "sync": true }
```

```sql
SELECT frame, strategy, checksum
FROM evidence
ORDER BY frame DESC
LIMIT 3;
```

[^det]: All demos are deterministic under fixed inputs.
"###,
];

const EFFECT_GFM_SAMPLES: &[&str] = &[
    r#"# FX Lab · Deterministic Spectra
> *"Render truth, not pixels."* — kernel memo

- [x] Inline scrollback (DEC 7/8)
- [x] One-writer rule
- [x] Synced presenter (DEC 2026)
- [ ] GPU raster (not needed)

| Key | Action | Notes |
| --- | --- | --- |
| `e` | cycle FX | multi-stack |
| `c` | cycle code | 27 langs |
| `g` | cycle charts | 6 modes |

```bash
FTUI_DEMO_SCREEN=1 FTUI_DEMO_EXIT_AFTER_MS=2000 \
  cargo run -p ftui-demo-showcase
```

> [!NOTE]
> This panel renders **three** effects at once.

[^fx]: Effects are deterministic with fixed seeds.
"#,
    r#"## GFM Stress · Ledger
1. **Bold** + _italic_ + ~~strike~~
2. `code` + [links](https://ftui.dev)
3. Nested:
   - alpha
   - beta

| op | cost | policy |
| -- | ---: | --- |
| diff | 3.2ms | bayes |
| present | 1.1ms | sync |

```diff
- emit-per-cell
+ coalesce-run
```

> [!TIP]
> Use `Cmd::batch` for side effects.
"#,
    r#"### Runtime Ledger · Mixed
- [x] Tasks
- [ ] Benchmarks
- [x] Evidence log

```sql
SELECT frame, strategy, checksum
FROM evidence
WHERE dirty_rows > 0
ORDER BY frame DESC
LIMIT 3;
```

```mermaid
graph LR
  A[Frame] --> B[Diff]
  B --> C[Presenter]
```

Math: `∫ f(x) dx` and `α + β`
"#,
];

#[derive(Clone, Copy)]
enum EffectKind {
    None,
    FadeIn,
    FadeOut,
    Pulse,
    OrganicPulse,
    HorizontalGradient,
    AnimatedGradient,
    RainbowGradient,
    VerticalGradient,
    DiagonalGradient,
    RadialGradient,
    ColorCycle,
    ColorWave,
    Glow,
    PulsingGlow,
    Typewriter,
    Scramble,
    Glitch,
    Wave,
    Bounce,
    Shake,
    Cascade,
    Cursor,
    Reveal,
    RevealMask,
    ChromaticAberration,
    Scanline,
    ParticleDissolve,
}

struct EffectDemo {
    name: &'static str,
    kind: EffectKind,
}

const EFFECT_DEMOS: &[EffectDemo] = &[
    EffectDemo {
        name: "None",
        kind: EffectKind::None,
    },
    EffectDemo {
        name: "FadeIn",
        kind: EffectKind::FadeIn,
    },
    EffectDemo {
        name: "FadeOut",
        kind: EffectKind::FadeOut,
    },
    EffectDemo {
        name: "Pulse",
        kind: EffectKind::Pulse,
    },
    EffectDemo {
        name: "OrganicPulse",
        kind: EffectKind::OrganicPulse,
    },
    EffectDemo {
        name: "HorizontalGradient",
        kind: EffectKind::HorizontalGradient,
    },
    EffectDemo {
        name: "AnimatedGradient",
        kind: EffectKind::AnimatedGradient,
    },
    EffectDemo {
        name: "RainbowGradient",
        kind: EffectKind::RainbowGradient,
    },
    EffectDemo {
        name: "VerticalGradient",
        kind: EffectKind::VerticalGradient,
    },
    EffectDemo {
        name: "DiagonalGradient",
        kind: EffectKind::DiagonalGradient,
    },
    EffectDemo {
        name: "RadialGradient",
        kind: EffectKind::RadialGradient,
    },
    EffectDemo {
        name: "ColorCycle",
        kind: EffectKind::ColorCycle,
    },
    EffectDemo {
        name: "ColorWave",
        kind: EffectKind::ColorWave,
    },
    EffectDemo {
        name: "Glow",
        kind: EffectKind::Glow,
    },
    EffectDemo {
        name: "PulsingGlow",
        kind: EffectKind::PulsingGlow,
    },
    EffectDemo {
        name: "Typewriter",
        kind: EffectKind::Typewriter,
    },
    EffectDemo {
        name: "Scramble",
        kind: EffectKind::Scramble,
    },
    EffectDemo {
        name: "Glitch",
        kind: EffectKind::Glitch,
    },
    EffectDemo {
        name: "Wave",
        kind: EffectKind::Wave,
    },
    EffectDemo {
        name: "Bounce",
        kind: EffectKind::Bounce,
    },
    EffectDemo {
        name: "Shake",
        kind: EffectKind::Shake,
    },
    EffectDemo {
        name: "Cascade",
        kind: EffectKind::Cascade,
    },
    EffectDemo {
        name: "Cursor",
        kind: EffectKind::Cursor,
    },
    EffectDemo {
        name: "Reveal",
        kind: EffectKind::Reveal,
    },
    EffectDemo {
        name: "RevealMask",
        kind: EffectKind::RevealMask,
    },
    EffectDemo {
        name: "ChromaticAberration",
        kind: EffectKind::ChromaticAberration,
    },
    EffectDemo {
        name: "Scanline",
        kind: EffectKind::Scanline,
    },
    EffectDemo {
        name: "ParticleDissolve",
        kind: EffectKind::ParticleDissolve,
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChartMode {
    Pulse,
    Lines,
    Bars,
    Heatmap,
    Matrix,
    Composite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DashboardFocus {
    Plasma,
    Charts,
    Code,
    Info,
    TextFx,
    Activity,
    Markdown,
    None,
}

impl DashboardFocus {
    fn next(self) -> Self {
        match self {
            Self::Plasma => Self::Charts,
            Self::Charts => Self::Code,
            Self::Code => Self::Info,
            Self::Info => Self::TextFx,
            Self::TextFx => Self::Activity,
            Self::Activity => Self::Markdown,
            Self::Markdown | Self::None => Self::Plasma,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::Plasma => Self::Markdown,
            Self::Charts => Self::Plasma,
            Self::Code => Self::Charts,
            Self::Info => Self::Code,
            Self::TextFx => Self::Info,
            Self::Activity => Self::TextFx,
            Self::Markdown | Self::None => Self::Activity,
        }
    }
}

impl ChartMode {
    fn next(self) -> Self {
        match self {
            Self::Pulse => Self::Lines,
            Self::Lines => Self::Bars,
            Self::Bars => Self::Heatmap,
            Self::Heatmap => Self::Matrix,
            Self::Matrix => Self::Composite,
            Self::Composite => Self::Pulse,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::Pulse => Self::Composite,
            Self::Lines => Self::Pulse,
            Self::Bars => Self::Lines,
            Self::Heatmap => Self::Bars,
            Self::Matrix => Self::Heatmap,
            Self::Composite => Self::Matrix,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Pulse => "Pulse",
            Self::Lines => "Lines",
            Self::Bars => "Bars",
            Self::Heatmap => "Heatmap",
            Self::Matrix => "Matrix",
            Self::Composite => "Composite",
        }
    }

    fn subtitle(self) -> &'static str {
        match self {
            Self::Pulse => "sparklines + mini bars",
            Self::Lines => "braille line chart + legend",
            Self::Bars => "grouped or stacked volumes",
            Self::Heatmap => "animated intensity grid",
            Self::Matrix => "quad tiles: radar, heat, volumes",
            Self::Composite => "multilayer KPI + trend deck",
        }
    }
}

/// Dashboard state.
pub struct Dashboard {
    // Animation
    tick_count: u64,
    time: f64,

    // Data sources
    simulated_data: SimulatedData,

    // FPS tracking
    frame_times: VecDeque<u64>,
    last_frame: Option<Instant>,
    fps: f64,

    // Syntax highlighter (cached)
    highlighter: Arc<SyntaxHighlighter>,

    // Cached highlighted code samples
    code_cache: Vec<Text>,

    // Markdown renderer (cached)
    md_renderer: MarkdownRenderer,

    // Code showcase state
    code_index: usize,

    // Markdown streaming state
    md_sample_index: usize,
    md_stream_pos: usize,

    // Text effects showcase state
    effect_index: usize,

    // Chart showcase state
    chart_mode: ChartMode,

    // Focus + hit testing
    focus: DashboardFocus,
    layout_plasma: StdCell<Rect>,
    layout_charts: StdCell<Rect>,
    layout_code: StdCell<Rect>,
    layout_info: StdCell<Rect>,
    layout_text_fx: StdCell<Rect>,
    layout_activity: StdCell<Rect>,
    layout_markdown: StdCell<Rect>,
}

impl Default for Dashboard {
    fn default() -> Self {
        Self::new()
    }
}

impl Dashboard {
    pub fn new() -> Self {
        let mut simulated_data = SimulatedData::default();
        // Pre-populate some history
        for t in 0..30 {
            simulated_data.tick(t);
        }

        let mut highlighter = SyntaxHighlighter::new();
        highlighter.set_theme(theme::syntax_theme());
        let highlighter = Arc::new(highlighter);
        let code_cache = Self::build_code_cache(&highlighter);

        Self {
            tick_count: 30,
            time: 0.0,
            simulated_data,
            frame_times: VecDeque::with_capacity(60),
            last_frame: None,
            fps: 0.0,
            highlighter: Arc::clone(&highlighter),
            code_cache,
            md_renderer: MarkdownRenderer::new(MarkdownTheme::default())
                .with_syntax_highlighter(Arc::clone(&highlighter)),
            code_index: 0,
            md_sample_index: 0,
            md_stream_pos: 0,
            effect_index: 0,
            chart_mode: ChartMode::Pulse,
            focus: DashboardFocus::Code,
            layout_plasma: StdCell::new(Rect::default()),
            layout_charts: StdCell::new(Rect::default()),
            layout_code: StdCell::new(Rect::default()),
            layout_info: StdCell::new(Rect::default()),
            layout_text_fx: StdCell::new(Rect::default()),
            layout_activity: StdCell::new(Rect::default()),
            layout_markdown: StdCell::new(Rect::default()),
        }
    }

    pub fn apply_theme(&mut self) {
        let mut highlighter = SyntaxHighlighter::new();
        highlighter.set_theme(theme::syntax_theme());
        let highlighter = Arc::new(highlighter);
        self.code_cache = Self::build_code_cache(&highlighter);
        self.highlighter = Arc::clone(&highlighter);
        self.md_renderer =
            MarkdownRenderer::new(MarkdownTheme::default()).with_syntax_highlighter(highlighter);
    }

    fn build_code_cache(highlighter: &SyntaxHighlighter) -> Vec<Text> {
        CODE_SAMPLES
            .iter()
            .map(|sample| highlighter.highlight(sample.code, sample.lang))
            .collect()
    }

    fn is_focused(&self, panel: DashboardFocus) -> bool {
        self.focus == panel
    }

    fn focus_from_point(&mut self, x: u16, y: u16) {
        let plasma = self.layout_plasma.get();
        let charts = self.layout_charts.get();
        let code = self.layout_code.get();
        let info = self.layout_info.get();
        let text_fx = self.layout_text_fx.get();
        let activity = self.layout_activity.get();
        let markdown = self.layout_markdown.get();

        self.focus = if plasma.contains(x, y) {
            DashboardFocus::Plasma
        } else if charts.contains(x, y) {
            DashboardFocus::Charts
        } else if code.contains(x, y) {
            DashboardFocus::Code
        } else if info.contains(x, y) {
            DashboardFocus::Info
        } else if text_fx.contains(x, y) {
            DashboardFocus::TextFx
        } else if activity.contains(x, y) {
            DashboardFocus::Activity
        } else if markdown.contains(x, y) {
            DashboardFocus::Markdown
        } else {
            DashboardFocus::None
        };
    }

    fn current_code_sample(&self) -> &'static CodeSample {
        &CODE_SAMPLES[self.code_index % CODE_SAMPLES.len()]
    }

    fn current_code_text(&self) -> &Text {
        let idx = self.code_index % self.code_cache.len().max(1);
        &self.code_cache[idx]
    }

    fn current_markdown_sample(&self) -> &'static str {
        DASH_MARKDOWN_SAMPLES[self.md_sample_index % DASH_MARKDOWN_SAMPLES.len()]
    }

    fn markdown_stream_complete(&self) -> bool {
        self.md_stream_pos >= self.current_markdown_sample().len()
    }

    fn tick_markdown_stream(&mut self) {
        if self.markdown_stream_complete() {
            return;
        }
        let md = self.current_markdown_sample();
        let max_len = md.len();
        // Triple streaming speed to keep the dashboard markdown lively.
        let mut new_pos = self.md_stream_pos.saturating_add(2160);
        while new_pos < max_len && !md.is_char_boundary(new_pos) {
            new_pos += 1;
        }
        self.md_stream_pos = new_pos.min(max_len);
    }

    fn reset_markdown_stream(&mut self) {
        self.md_stream_pos = 0;
    }

    fn current_effect_demo(&self) -> &'static EffectDemo {
        &EFFECT_DEMOS[self.effect_index % EFFECT_DEMOS.len()]
    }

    fn build_effect(&self, kind: EffectKind, text_len: usize) -> TextEffect {
        let progress = (self.time * 0.6).sin() * 0.5 + 0.5;
        let progress = progress.clamp(0.0, 1.0);
        let visible = (progress * text_len.max(1) as f64).max(1.0);

        match kind {
            EffectKind::None => TextEffect::None,
            EffectKind::FadeIn => TextEffect::FadeIn { progress },
            EffectKind::FadeOut => TextEffect::FadeOut { progress },
            EffectKind::Pulse => TextEffect::Pulse {
                speed: 1.8,
                min_alpha: 0.25,
            },
            EffectKind::OrganicPulse => TextEffect::OrganicPulse {
                speed: 0.6,
                min_brightness: 0.35,
                asymmetry: 0.55,
                phase_variation: 0.25,
                seed: 42,
            },
            EffectKind::HorizontalGradient => TextEffect::HorizontalGradient {
                gradient: ColorGradient::sunset(),
            },
            EffectKind::AnimatedGradient => TextEffect::AnimatedGradient {
                gradient: ColorGradient::cyberpunk(),
                speed: 0.4,
            },
            EffectKind::RainbowGradient => TextEffect::RainbowGradient { speed: 0.6 },
            EffectKind::VerticalGradient => TextEffect::VerticalGradient {
                gradient: ColorGradient::ocean(),
            },
            EffectKind::DiagonalGradient => TextEffect::DiagonalGradient {
                gradient: ColorGradient::lavender(),
                angle: 45.0,
            },
            EffectKind::RadialGradient => TextEffect::RadialGradient {
                gradient: ColorGradient::fire(),
                center: (0.5, 0.5),
                aspect: 1.2,
            },
            EffectKind::ColorCycle => TextEffect::ColorCycle {
                colors: vec![
                    theme::accent::PRIMARY.into(),
                    theme::accent::ACCENT_3.into(),
                    theme::accent::ACCENT_6.into(),
                    theme::accent::ACCENT_9.into(),
                ],
                speed: 0.9,
            },
            EffectKind::ColorWave => TextEffect::ColorWave {
                color1: theme::accent::PRIMARY.into(),
                color2: theme::accent::ACCENT_8.into(),
                speed: 1.2,
                wavelength: 8.0,
            },
            EffectKind::Glow => TextEffect::Glow {
                color: PackedRgba::rgb(255, 200, 100),
                intensity: 0.6,
            },
            EffectKind::PulsingGlow => TextEffect::PulsingGlow {
                color: PackedRgba::rgb(255, 120, 180),
                speed: 1.4,
            },
            EffectKind::Typewriter => TextEffect::Typewriter {
                visible_chars: visible,
            },
            EffectKind::Scramble => TextEffect::Scramble { progress },
            EffectKind::Glitch => TextEffect::Glitch {
                intensity: 0.25 + 0.35 * progress,
            },
            EffectKind::Wave => TextEffect::Wave {
                amplitude: 1.2,
                wavelength: 10.0,
                speed: 1.0,
                direction: Direction::Down,
            },
            EffectKind::Bounce => TextEffect::Bounce {
                height: 2.0,
                speed: 1.2,
                stagger: 0.15,
                damping: 0.88,
            },
            EffectKind::Shake => TextEffect::Shake {
                intensity: 0.8,
                speed: 6.0,
                seed: 7,
            },
            EffectKind::Cascade => TextEffect::Cascade {
                speed: 18.0,
                direction: Direction::Right,
                stagger: 0.08,
            },
            EffectKind::Cursor => TextEffect::Cursor {
                style: CursorStyle::Block,
                blink_speed: 2.5,
                position: CursorPosition::End,
            },
            EffectKind::Reveal => TextEffect::Reveal {
                mode: RevealMode::CenterOut,
                progress,
                seed: 13,
            },
            EffectKind::RevealMask => TextEffect::RevealMask {
                angle: 35.0,
                progress,
                softness: 0.3,
            },
            EffectKind::ChromaticAberration => TextEffect::ChromaticAberration {
                offset: 2,
                direction: Direction::Right,
                animated: true,
                speed: 0.4,
            },
            EffectKind::Scanline => TextEffect::Scanline {
                intensity: 0.35,
                line_gap: 2,
                scroll: true,
                scroll_speed: 0.7,
                flicker: 0.05,
            },
            EffectKind::ParticleDissolve => TextEffect::ParticleDissolve {
                progress,
                mode: DissolveMode::Dissolve,
                speed: 0.8,
                gravity: 0.4,
                seed: 9,
            },
        }
    }

    /// Update FPS calculation.
    fn update_fps(&mut self) {
        if determinism::is_demo_deterministic() {
            let dt_us = determinism::demo_tick_ms(100).max(1) * 1000;
            self.frame_times.push_back(dt_us);
            if self.frame_times.len() > 30 {
                self.frame_times.pop_front();
            }
            if !self.frame_times.is_empty() {
                let avg_us: u64 =
                    self.frame_times.iter().sum::<u64>() / self.frame_times.len() as u64;
                if avg_us > 0 {
                    self.fps = 1_000_000.0 / avg_us as f64;
                }
            }
            self.last_frame = None;
            return;
        }
        let now = Instant::now();
        if let Some(last) = self.last_frame {
            let elapsed_us = now.duration_since(last).as_micros() as u64;
            self.frame_times.push_back(elapsed_us);
            if self.frame_times.len() > 30 {
                self.frame_times.pop_front();
            }
            if !self.frame_times.is_empty() {
                let avg_us: u64 =
                    self.frame_times.iter().sum::<u64>() / self.frame_times.len() as u64;
                if avg_us > 0 {
                    self.fps = 1_000_000.0 / avg_us as f64;
                }
            }
        }
        self.last_frame = Some(now);
    }

    fn render_panel_hint(&self, frame: &mut Frame, area: Rect, hint: &str) {
        if area.is_empty() || area.height < 1 || area.width < 8 {
            return;
        }
        let hint_area = Rect::new(area.x, area.y + area.height - 1, area.width, 1);
        let text = truncate_to_width(hint, hint_area.width);
        Paragraph::new(text)
            .style(Style::new().fg(theme::fg::MUTED).bg(theme::alpha::SURFACE))
            .render(hint_area, frame);
    }

    // =========================================================================
    // Panel Renderers
    // =========================================================================

    /// Render animated gradient header.
    fn render_header(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() || area.height < 1 {
            return;
        }

        let title = "FRANKENTUI DASHBOARD";
        let gradient = ColorGradient::new(vec![
            (0.0, theme::accent::ACCENT_2.into()),
            (0.5, theme::accent::ACCENT_1.into()),
            (1.0, theme::accent::ACCENT_3.into()),
        ]);
        let effect = TextEffect::AnimatedGradient {
            gradient,
            speed: 0.3,
        };

        let styled = StyledText::new(title).effect(effect).bold().time(self.time);

        styled.render(area, frame);
    }

    /// Render mini plasma effect using Braille canvas.
    fn render_plasma(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() || area.width < 4 || area.height < 3 {
            return;
        }

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Plasma")
            .title_alignment(Alignment::Center)
            .style(theme::panel_border_style(
                self.is_focused(DashboardFocus::Plasma),
                theme::screen_accent::DASHBOARD,
            ));

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() || inner.width < 2 || inner.height < 2 {
            return;
        }

        let mut painter = Painter::for_area(inner, Mode::Braille);
        let (pw, ph) = painter.size();

        // Simple plasma using two sine waves
        let t = self.time * 0.5;
        let hue_shift = (t * 0.07).rem_euclid(1.0);
        for py in 0..ph as i32 {
            for px in 0..pw as i32 {
                let x = px as f64 / pw as f64;
                let y = py as f64 / ph as f64;

                // Two-wave plasma formula
                let v1 = (x * 10.0 + t * 2.0).sin();
                let v2 = (y * 10.0 + t * 1.5).sin();
                let v3 = ((x + y) * 8.0 + t).sin();
                let v = (v1 + v2 + v3) / 3.0;

                // Map plasma value to a theme-coherent accent gradient.
                let color = theme::accent_gradient((v + 1.0) * 0.5 + hue_shift);

                painter.point_colored(px, py, color);
            }
        }

        Canvas::from_painter(&painter)
            .style(Style::new().fg(theme::fg::PRIMARY))
            .render(inner, frame);
        self.render_panel_hint(frame, inner, "Click → Visual Effects");
    }

    /// Render the charts showcase panel.
    fn render_charts(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() || area.height < 3 {
            return;
        }

        let title = format!("Charts · {}", self.chart_mode.label());
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(title.as_str())
            .title_alignment(Alignment::Center)
            .style(theme::panel_border_style(
                self.is_focused(DashboardFocus::Charts),
                theme::screen_accent::DATA_VIZ,
            ));

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let (header, content) = if inner.height >= 4 {
            let rows = Flex::vertical()
                .constraints([Constraint::Fixed(1), Constraint::Min(1)])
                .split(inner);
            (Some(rows[0]), rows[1])
        } else {
            (None, inner)
        };

        if let Some(header_area) = header {
            let cpu_last = self
                .simulated_data
                .cpu_history
                .back()
                .copied()
                .unwrap_or(0.0);
            let mem_last = self
                .simulated_data
                .memory_history
                .back()
                .copied()
                .unwrap_or(0.0);
            let eps = self.simulated_data.events_per_second;
            let hint = format!(
                "g:cycle · {} · CPU {} · MEM {} · EPS {}",
                self.chart_mode.subtitle(),
                Self::format_percent(cpu_last),
                Self::format_percent(mem_last),
                eps.round() as u64
            );
            Paragraph::new(hint)
                .style(Style::new().fg(theme::fg::MUTED))
                .render(header_area, frame);
        }

        match self.chart_mode {
            ChartMode::Pulse => self.render_pulse_charts(frame, content),
            ChartMode::Lines => self.render_line_charts(frame, content),
            ChartMode::Bars => self.render_bar_charts(frame, content),
            ChartMode::Heatmap => self.render_heatmap_charts(frame, content),
            ChartMode::Matrix => self.render_matrix_charts(frame, content),
            ChartMode::Composite => self.render_composite_charts(frame, content),
        }
        self.render_panel_hint(frame, inner, "Click → Data Viz");
    }

    fn render_pulse_charts(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        if area.height < 3 {
            self.render_minimal_sparkline(frame, area);
            return;
        }

        let cpu_data: Vec<f64> = self.simulated_data.cpu_history.iter().copied().collect();
        let mem_data: Vec<f64> = self.simulated_data.memory_history.iter().copied().collect();
        let net_in_data: Vec<f64> = self.simulated_data.network_in.iter().copied().collect();
        let net_out_data: Vec<f64> = self.simulated_data.network_out.iter().copied().collect();

        let cpu_last = cpu_data.last().copied().unwrap_or(0.0);
        let mem_last = mem_data.last().copied().unwrap_or(0.0);
        let net_in_last = net_in_data.last().copied().unwrap_or(0.0);
        let net_out_last = net_out_data.last().copied().unwrap_or(0.0);

        if area.height < 5 {
            let rows = Flex::vertical()
                .constraints([
                    Constraint::Fixed(1),
                    Constraint::Fixed(1),
                    Constraint::Fixed(1),
                ])
                .split(area);
            self.render_labeled_sparkline(
                frame,
                rows[0],
                "CPU",
                Self::format_percent(cpu_last),
                &cpu_data,
                theme::accent::PRIMARY.into(),
                (
                    theme::accent::PRIMARY.into(),
                    theme::accent::ACCENT_7.into(),
                ),
            );
            self.render_labeled_sparkline(
                frame,
                rows[1],
                "MEM",
                Self::format_percent(mem_last),
                &mem_data,
                theme::accent::SUCCESS.into(),
                (
                    theme::accent::SUCCESS.into(),
                    theme::accent::ACCENT_9.into(),
                ),
            );
            self.render_labeled_sparkline(
                frame,
                rows[2],
                "NET",
                Self::format_rate((net_in_last + net_out_last) * 0.5),
                &net_in_data,
                theme::accent::WARNING.into(),
                (
                    theme::accent::WARNING.into(),
                    theme::accent::ACCENT_10.into(),
                ),
            );
            return;
        }

        let rows = Flex::vertical()
            .constraints([
                Constraint::Fixed(1),
                Constraint::Fixed(1),
                Constraint::Fixed(1),
                Constraint::Fixed(1),
                Constraint::Fixed(1),
            ])
            .split(area);

        self.render_labeled_sparkline(
            frame,
            rows[0],
            "CPU",
            Self::format_percent(cpu_last),
            &cpu_data,
            theme::accent::PRIMARY.into(),
            (
                theme::accent::PRIMARY.into(),
                theme::accent::ACCENT_7.into(),
            ),
        );
        self.render_labeled_sparkline(
            frame,
            rows[1],
            "MEM",
            Self::format_percent(mem_last),
            &mem_data,
            theme::accent::SUCCESS.into(),
            (
                theme::accent::SUCCESS.into(),
                theme::accent::ACCENT_9.into(),
            ),
        );
        self.render_labeled_sparkline(
            frame,
            rows[2],
            "NET",
            Self::format_rate(net_in_last),
            &net_in_data,
            theme::accent::WARNING.into(),
            (
                theme::accent::WARNING.into(),
                theme::accent::ACCENT_10.into(),
            ),
        );
        self.render_labeled_sparkline(
            frame,
            rows[3],
            "OUT",
            Self::format_rate(net_out_last),
            &net_out_data,
            theme::accent::SECONDARY.into(),
            (
                theme::accent::SECONDARY.into(),
                theme::accent::ACCENT_6.into(),
            ),
        );

        let mini_cols = Flex::horizontal()
            .gap(theme::spacing::XS)
            .constraints([
                Constraint::Percentage(34.0),
                Constraint::Percentage(33.0),
                Constraint::Percentage(33.0),
            ])
            .split(rows[4]);

        let eps_value = (self.simulated_data.events_per_second / 1500.0).clamp(0.0, 1.0);
        let disk_value = self
            .simulated_data
            .disk_usage
            .first()
            .map(|(_, v)| *v / 100.0)
            .unwrap_or(0.0);
        let net_value = ((net_in_last + net_out_last) / 2400.0).clamp(0.0, 1.0);
        let colors = MiniBarColors::new(
            theme::accent::PRIMARY.into(),
            theme::accent::SUCCESS.into(),
            theme::accent::WARNING.into(),
            theme::accent::ACCENT_10.into(),
        );

        self.render_mini_bar_row(frame, mini_cols[0], "EPS", eps_value, colors);
        self.render_mini_bar_row(frame, mini_cols[1], "DSK", disk_value, colors);
        self.render_mini_bar_row(frame, mini_cols[2], "IO", net_value, colors);
    }

    fn render_line_charts(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        if area.width < 16 || area.height < 6 {
            self.render_minimal_sparkline(frame, area);
            return;
        }

        let cpu_points: Vec<(f64, f64)> = self
            .simulated_data
            .cpu_history
            .iter()
            .enumerate()
            .map(|(i, v)| (i as f64, *v))
            .collect();
        let mem_points: Vec<(f64, f64)> = self
            .simulated_data
            .memory_history
            .iter()
            .enumerate()
            .map(|(i, v)| (i as f64, *v))
            .collect();

        let net_max = self
            .simulated_data
            .network_in
            .iter()
            .chain(self.simulated_data.network_out.iter())
            .copied()
            .fold(1.0, f64::max);
        let net_points: Vec<(f64, f64)> = self
            .simulated_data
            .network_in
            .iter()
            .zip(self.simulated_data.network_out.iter())
            .enumerate()
            .map(|(i, (net_in, net_out))| {
                let avg = (*net_in + *net_out) * 0.5;
                let scaled = (avg / net_max * 100.0).clamp(0.0, 100.0);
                (i as f64, scaled)
            })
            .collect();

        let palette = Self::chart_palette_extended();
        let series = vec![
            Series::new("CPU", &cpu_points, palette[0]).markers(true),
            Series::new("MEM", &mem_points, palette[1]),
            Series::new("NET", &net_points, palette[2]),
        ];

        let chart = LineChart::new(series)
            .style(Style::new().fg(theme::fg::PRIMARY))
            .x_labels(vec!["-30", "-15", "now"])
            .y_labels(vec!["0", "50", "100"])
            .legend(true)
            .y_bounds(0.0, 100.0);

        chart.render(area, frame);
    }

    fn render_bar_charts(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        if area.width < 12 || area.height < 4 {
            self.render_minimal_sparkline(frame, area);
            return;
        }

        let palette = Self::chart_palette_extended();

        let (legend_area, chart_area) = if area.height >= 6 {
            let rows = Flex::vertical()
                .constraints([Constraint::Fixed(1), Constraint::Min(1)])
                .split(area);
            (Some(rows[0]), rows[1])
        } else {
            (None, area)
        };

        if chart_area.is_empty() {
            return;
        }

        let use_stacked = chart_area.width < 26;

        if let Some(legend_area) = legend_area {
            if use_stacked {
                let legend_items = [
                    ("SYS", palette[0]),
                    ("APPS", palette[1]),
                    ("DOCS", palette[2]),
                    ("MEDIA", palette[3]),
                    ("CACHE", palette[4]),
                ];
                self.render_color_legend(frame, legend_area, &legend_items);
            } else {
                let legend_items = [("USED", palette[0]), ("FREE", palette[3])];
                self.render_color_legend(frame, legend_area, &legend_items);
            }
        }

        if use_stacked {
            let values: Vec<f64> = self
                .simulated_data
                .disk_usage
                .iter()
                .map(|(_, v)| *v)
                .collect();
            let groups = vec![BarGroup::new("DISK", values)];
            BarChart::new(groups)
                .mode(BarMode::Stacked)
                .bar_width(2)
                .colors(palette.to_vec())
                .style(Style::new().fg(theme::fg::PRIMARY))
                .max(100.0)
                .render(chart_area, frame);
        } else {
            let groups: Vec<BarGroup<'_>> = self
                .simulated_data
                .disk_usage
                .iter()
                .map(|(name, usage)| BarGroup::new(name.as_str(), vec![*usage, 100.0 - *usage]))
                .collect();
            BarChart::new(groups)
                .direction(BarDirection::Vertical)
                .mode(BarMode::Grouped)
                .bar_width(1)
                .bar_gap(theme::spacing::XS)
                .group_gap(theme::spacing::SM)
                .colors(vec![palette[0], palette[3]])
                .style(Style::new().fg(theme::fg::PRIMARY))
                .max(100.0)
                .render(chart_area, frame);
        }
    }

    fn render_heatmap_charts(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        if area.width < 8 || area.height < 4 {
            self.render_minimal_sparkline(frame, area);
            return;
        }

        let w = area.width.max(1) as f64;
        let h = area.height.max(1) as f64;
        let phase = self.time * 0.35;
        let mut max_value = -1.0;
        let mut max_pos = (area.x, area.y);

        for dy in 0..area.height {
            let ny = dy as f64 / (h - 1.0).max(1.0);
            for dx in 0..area.width {
                let nx = dx as f64 / (w - 1.0).max(1.0);
                let wave_x = (nx * std::f64::consts::TAU * 1.3 + phase).sin() * 0.5 + 0.5;
                let wave_y = (ny * std::f64::consts::TAU * 0.8 - phase * 1.2).cos() * 0.5 + 0.5;
                let wave_z = ((nx + ny) * 3.0 + phase * 0.6).sin() * 0.5 + 0.5;
                let value = (0.45 * wave_x + 0.35 * wave_y + 0.2 * wave_z).clamp(0.0, 1.0);

                if value > max_value {
                    max_value = value;
                    max_pos = (area.x + dx, area.y + dy);
                }

                let color = heatmap_gradient(value);
                let mut cell = RenderCell::from_char(' ');
                cell.bg = color;
                if let Some(slot) = frame.buffer.get_mut(area.x + dx, area.y + dy) {
                    *slot = cell;
                }
            }
        }

        if let Some(cell) = frame.buffer.get_mut(max_pos.0, max_pos.1) {
            cell.content = ftui_render::cell::CellContent::from_char('●');
            cell.fg = theme::fg::PRIMARY.into();
        }
    }

    fn render_chart_tile<F>(
        &self,
        frame: &mut Frame,
        area: Rect,
        title: &str,
        accent: PackedRgba,
        render: F,
    ) where
        F: FnOnce(&Self, &mut Frame, Rect),
    {
        if area.is_empty() || area.width < 6 || area.height < 3 {
            return;
        }

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Square)
            .title(title)
            .title_alignment(Alignment::Center)
            .style(Style::new().fg(accent));
        let inner = block.inner(area);
        block.render(area, frame);
        if inner.is_empty() {
            return;
        }
        render(self, frame, inner);
    }

    fn render_matrix_charts(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        if area.width < 22 || area.height < 8 {
            self.render_minimal_sparkline(frame, area);
            return;
        }

        let rows = Flex::vertical()
            .gap(theme::spacing::XS)
            .constraints([Constraint::Percentage(50.0), Constraint::Percentage(50.0)])
            .split(area);
        let top = Flex::horizontal()
            .gap(theme::spacing::XS)
            .constraints([Constraint::Percentage(50.0), Constraint::Percentage(50.0)])
            .split(rows[0]);
        let bottom = Flex::horizontal()
            .gap(theme::spacing::XS)
            .constraints([Constraint::Percentage(50.0), Constraint::Percentage(50.0)])
            .split(rows[1]);

        let net_in: Vec<f64> = self.simulated_data.network_in.iter().copied().collect();
        let net_out: Vec<f64> = self.simulated_data.network_out.iter().copied().collect();

        self.render_chart_tile(
            frame,
            top[0],
            "Heat Pulse",
            theme::accent::ACCENT_7.into(),
            |this, frame, inner| {
                this.render_heatmap_charts(frame, inner);
            },
        );

        self.render_chart_tile(
            frame,
            top[1],
            "Throughput",
            theme::accent::PRIMARY.into(),
            |this, frame, inner| {
                let rows = Flex::vertical()
                    .constraints([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)])
                    .split(inner);
                let net_in_last = net_in.last().copied().unwrap_or(0.0);
                let net_out_last = net_out.last().copied().unwrap_or(0.0);
                this.render_labeled_sparkline(
                    frame,
                    rows[0],
                    "IN",
                    Self::format_rate(net_in_last),
                    &net_in,
                    theme::accent::PRIMARY.into(),
                    (
                        theme::accent::PRIMARY.into(),
                        theme::accent::ACCENT_6.into(),
                    ),
                );
                this.render_labeled_sparkline(
                    frame,
                    rows[1],
                    "OUT",
                    Self::format_rate(net_out_last),
                    &net_out,
                    theme::accent::SECONDARY.into(),
                    (
                        theme::accent::SECONDARY.into(),
                        theme::accent::ACCENT_10.into(),
                    ),
                );
            },
        );

        self.render_chart_tile(
            frame,
            bottom[0],
            "Latency Buckets",
            theme::accent::WARNING.into(),
            |_this, frame, inner| {
                let base = 28.0 + (self.time * 1.7).sin() * 6.0;
                let p50 = (base + 6.0).clamp(10.0, 60.0);
                let p95 = (base + 20.0).clamp(20.0, 95.0);
                let p99 = (base + 40.0).clamp(30.0, 140.0);
                let groups = vec![
                    BarGroup::new("p50", vec![p50]),
                    BarGroup::new("p95", vec![p95]),
                    BarGroup::new("p99", vec![p99]),
                ];
                BarChart::new(groups)
                    .direction(BarDirection::Vertical)
                    .mode(BarMode::Grouped)
                    .bar_width(2)
                    .bar_gap(1)
                    .colors(vec![
                        theme::accent::SUCCESS.into(),
                        theme::accent::WARNING.into(),
                        theme::accent::ERROR.into(),
                    ])
                    .max(150.0)
                    .render(inner, frame);
            },
        );

        self.render_chart_tile(
            frame,
            bottom[1],
            "Budget + Errors",
            theme::accent::ACCENT_4.into(),
            |_this, frame, inner| {
                let rows = Flex::vertical()
                    .constraints([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)])
                    .split(inner);
                let error_budget = (0.6 + (self.time * 0.8).sin() * 0.2).clamp(0.0, 1.0);
                let sla = (0.92 + (self.time * 0.35).cos() * 0.04).clamp(0.0, 1.0);
                let colors = MiniBarColors::new(
                    theme::accent::PRIMARY.into(),
                    theme::accent::SUCCESS.into(),
                    theme::accent::WARNING.into(),
                    theme::accent::ACCENT_10.into(),
                );
                self.render_mini_bar_row(frame, rows[0], "SLA", sla, colors);
                self.render_mini_bar_row(frame, rows[1], "BGT", error_budget, colors);
            },
        );
    }

    fn render_composite_charts(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        if area.width < 24 || area.height < 6 {
            self.render_minimal_sparkline(frame, area);
            return;
        }

        let rows = Flex::vertical()
            .constraints([
                Constraint::Fixed(2),
                Constraint::Min(3),
                Constraint::Fixed(2),
            ])
            .split(area);

        let cpu_last = self
            .simulated_data
            .cpu_history
            .back()
            .copied()
            .unwrap_or(0.0);
        let mem_last = self
            .simulated_data
            .memory_history
            .back()
            .copied()
            .unwrap_or(0.0);
        let net_in_last = self
            .simulated_data
            .network_in
            .back()
            .copied()
            .unwrap_or(0.0);
        let net_out_last = self
            .simulated_data
            .network_out
            .back()
            .copied()
            .unwrap_or(0.0);

        let spans = vec![
            Span::styled("CPU ", Style::new().fg(theme::accent::PRIMARY).bold()),
            Span::styled(
                Self::format_percent(cpu_last),
                Style::new().fg(theme::fg::PRIMARY),
            ),
            Span::raw("  "),
            Span::styled("MEM ", Style::new().fg(theme::accent::SUCCESS).bold()),
            Span::styled(
                Self::format_percent(mem_last),
                Style::new().fg(theme::fg::PRIMARY),
            ),
            Span::raw("  "),
            Span::styled("NET ", Style::new().fg(theme::accent::WARNING).bold()),
            Span::styled(
                Self::format_rate((net_in_last + net_out_last) * 0.5),
                Style::new().fg(theme::fg::PRIMARY),
            ),
        ];
        Paragraph::new(Line::from_spans(spans))
            .style(Style::new().bg(theme::alpha::SURFACE))
            .render(rows[0], frame);

        let cpu_points: Vec<(f64, f64)> = self
            .simulated_data
            .cpu_history
            .iter()
            .enumerate()
            .map(|(i, v)| (i as f64, *v))
            .collect();
        let mem_points: Vec<(f64, f64)> = self
            .simulated_data
            .memory_history
            .iter()
            .enumerate()
            .map(|(i, v)| (i as f64, *v))
            .collect();
        let palette = Self::chart_palette_extended();
        let series = vec![
            Series::new("CPU", &cpu_points, palette[0]).markers(true),
            Series::new("MEM", &mem_points, palette[1]),
        ];
        LineChart::new(series)
            .style(Style::new().fg(theme::fg::PRIMARY))
            .legend(false)
            .y_bounds(0.0, 100.0)
            .render(rows[1], frame);

        let cols = Flex::horizontal()
            .constraints([Constraint::Percentage(50.0), Constraint::Percentage(50.0)])
            .split(rows[2]);
        let colors = MiniBarColors::new(
            theme::accent::PRIMARY.into(),
            theme::accent::SUCCESS.into(),
            theme::accent::WARNING.into(),
            theme::accent::ACCENT_10.into(),
        );
        let ingest = (0.55 + (self.time * 0.6).sin() * 0.2).clamp(0.0, 1.0);
        let cache = (0.7 + (self.time * 0.4).cos() * 0.15).clamp(0.0, 1.0);
        self.render_mini_bar_row(frame, cols[0], "ING", ingest, colors);
        self.render_mini_bar_row(frame, cols[1], "CCH", cache, colors);
    }

    fn render_minimal_sparkline(&self, frame: &mut Frame, area: Rect) {
        let data: Vec<f64> = self.simulated_data.cpu_history.iter().copied().collect();
        if data.is_empty() {
            return;
        }
        Sparkline::new(&data)
            .style(Style::new().fg(theme::accent::PRIMARY))
            .gradient(
                theme::accent::PRIMARY.into(),
                theme::accent::ACCENT_7.into(),
            )
            .render(area, frame);
    }

    #[allow(clippy::too_many_arguments)]
    fn render_labeled_sparkline(
        &self,
        frame: &mut Frame,
        area: Rect,
        label: &str,
        value_label: String,
        data: &[f64],
        color: PackedRgba,
        gradient: (PackedRgba, PackedRgba),
    ) {
        if area.is_empty() {
            return;
        }

        let label_width = display_width(label).max(3) as u16 + 1;
        let label_width = label_width.min(area.width);
        let value_width = display_width(value_label.as_str()).min(area.width as usize) as u16;
        let value_x = area.x + area.width.saturating_sub(value_width);
        let label_area = Rect::new(area.x, area.y, label_width, 1);
        let value_area = Rect::new(value_x, area.y, value_width, 1);
        let spark_x = area.x + label_width;
        let spark_area = Rect::new(spark_x, area.y, value_x.saturating_sub(spark_x), 1);

        Paragraph::new(format!("{label} "))
            .style(Style::new().fg(theme::fg::SECONDARY))
            .render(label_area, frame);

        if !spark_area.is_empty() && !data.is_empty() {
            Sparkline::new(data)
                .style(Style::new().fg(color))
                .gradient(gradient.0, gradient.1)
                .render(spark_area, frame);
        }

        if !value_area.is_empty() {
            Paragraph::new(value_label)
                .style(Style::new().fg(theme::fg::PRIMARY))
                .render(value_area, frame);
        }
    }

    fn render_color_legend(&self, frame: &mut Frame, area: Rect, entries: &[(&str, PackedRgba)]) {
        if area.is_empty() || entries.is_empty() {
            return;
        }

        let mut spans = Vec::new();
        for (idx, (label, color)) in entries.iter().enumerate() {
            if idx > 0 {
                spans.push(Span::raw("  "));
            }
            spans.push(Span::styled("■", Style::new().fg(*color)));
            spans.push(Span::styled(
                format!(" {label}"),
                Style::new().fg(theme::fg::SECONDARY),
            ));
        }

        Paragraph::new(Text::from_lines([Line::from_spans(spans)])).render(area, frame);
    }

    /// Render syntax-highlighted code preview.
    fn render_code(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() || area.height < 3 {
            return;
        }

        let sample = self.current_code_sample();
        let title = format!(
            "Code · {} ({}/{})",
            sample.label,
            self.code_index + 1,
            CODE_SAMPLES.len()
        );

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(title.as_str())
            .title_alignment(Alignment::Center)
            .style(theme::panel_border_style(
                self.is_focused(DashboardFocus::Code),
                theme::screen_accent::CODE_EXPLORER,
            ));

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let highlighted = self.current_code_text();

        // Render as paragraph with styled text
        render_text(frame, inner, highlighted);
        self.render_panel_hint(frame, inner, "Click → Code Explorer");
    }

    /// Render system info panel.
    ///
    /// `dashboard_size` is the total dashboard area (width, height) for display.
    fn render_info(&self, frame: &mut Frame, area: Rect, dashboard_size: (u16, u16)) {
        if area.is_empty() || area.height < 3 {
            return;
        }

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Info")
            .title_alignment(Alignment::Center)
            .style(theme::panel_border_style(
                self.is_focused(DashboardFocus::Info),
                theme::screen_accent::PERFORMANCE,
            ));

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let theme_name = theme::current_theme_name();
        let events_per_sec = self.simulated_data.events_per_second;
        let cpu = self
            .simulated_data
            .cpu_history
            .back()
            .copied()
            .unwrap_or(0.0);
        let mem = self
            .simulated_data
            .memory_history
            .back()
            .copied()
            .unwrap_or(0.0);
        let (status, status_color) = if cpu > 85.0 || mem > 85.0 {
            ("HOT", theme::intent::error_text())
        } else if cpu > 70.0 || mem > 70.0 {
            ("BUSY", theme::intent::warning_text())
        } else {
            ("NOMINAL", theme::intent::success_text())
        };

        if inner.height < 5 {
            let info = format!(
                "Status:{status} FPS:{:.0}\nEPS:{:.0}  {}x{}\n{}",
                self.fps, events_per_sec, dashboard_size.0, dashboard_size.1, theme_name
            );
            Paragraph::new(info)
                .style(Style::new().fg(theme::fg::SECONDARY))
                .render(inner, frame);
            self.render_panel_hint(frame, inner, "Click → Performance");
            return;
        }

        let header_text = format!(
            "FrankenTUI Kernel · LIVE · {}x{} · {:.0} FPS",
            dashboard_size.0, dashboard_size.1, self.fps
        );
        let header = truncate_to_width(&header_text, inner.width);
        let header_gradient = ColorGradient::new(vec![
            (0.0, theme::accent::ACCENT_2.into()),
            (0.5, theme::accent::ACCENT_1.into()),
            (1.0, theme::accent::ACCENT_3.into()),
        ]);
        let header_effect = TextEffect::AnimatedGradient {
            gradient: header_gradient,
            speed: 0.35,
        };
        StyledText::new(header)
            .effect(header_effect)
            .bold()
            .time(self.time)
            .render(Rect::new(inner.x, inner.y, inner.width, 1), frame);

        let mut cursor_y = inner.y + 1;
        if inner.height >= 6 {
            let badges_area = Rect::new(inner.x, cursor_y, inner.width, 1);
            self.render_info_badges(frame, badges_area);
            cursor_y += 1;
        }

        if inner.height >= 7 {
            let signal_area = Rect::new(inner.x, cursor_y, inner.width, 1);
            self.render_info_signal(frame, signal_area, status, status_color, events_per_sec);
            cursor_y += 1;
        }

        let remaining = inner.height.saturating_sub(cursor_y - inner.y);
        let spark_height = if remaining >= 6 { 2 } else { 0 };
        let bars_height = if remaining.saturating_sub(spark_height) >= 4 {
            3
        } else {
            0
        };
        let stats_height = remaining.saturating_sub(bars_height + spark_height).max(1);
        let stats_area = Rect::new(inner.x, cursor_y, inner.width, stats_height);
        self.render_info_stats(
            frame,
            stats_area,
            dashboard_size,
            theme_name,
            events_per_sec,
        );
        cursor_y = cursor_y.saturating_add(stats_height);

        if bars_height > 0 {
            let bars_area = Rect::new(inner.x, cursor_y, inner.width, bars_height);
            self.render_mini_bars(frame, bars_area);
            cursor_y = cursor_y.saturating_add(bars_height);
        }
        if spark_height > 0 {
            let spark_area = Rect::new(inner.x, cursor_y, inner.width, spark_height);
            self.render_info_sparkline_strip(frame, spark_area);
        }
        self.render_panel_hint(frame, inner, "Click → Performance");
    }

    fn render_info_badges(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let badges = [
            ("INLINE", theme::accent::PRIMARY),
            ("BRAILLE FX", theme::accent::ACCENT_4),
            ("STREAMING GFM", theme::accent::INFO),
            ("ZERO-TEAR", theme::accent::ACCENT_7),
            ("ONE-WRITER", theme::accent::SUCCESS),
            ("16B CELL", theme::accent::ACCENT_2),
        ];

        let mut x = area.x;
        for (label, color) in badges {
            let style = Style::new().fg(theme::bg::BASE).bg(color).bold();
            let badge = Badge::new(label).with_style(style).with_padding(1, 1);
            let width = badge.width().min(area.width);
            if x + width > area.right() {
                break;
            }
            badge.render(Rect::new(x, area.y, width, 1), frame);
            x = x.saturating_add(width + 1);
        }
    }

    fn render_info_signal(
        &self,
        frame: &mut Frame,
        area: Rect,
        status: &str,
        status_color: PackedRgba,
        events_per_sec: f64,
    ) {
        if area.is_empty() {
            return;
        }

        let line = format!(
            "Status: {status} │ Events: {:.0}/s │ Tick: {}",
            events_per_sec, self.tick_count
        );
        let line = truncate_to_width(&line, area.width);
        let styled = StyledText::new(line)
            .bold()
            .effect(TextEffect::ColorWave {
                color1: theme::accent::PRIMARY.into(),
                color2: theme::accent::ACCENT_8.into(),
                speed: 1.2,
                wavelength: 6.0,
            })
            .base_color(status_color)
            .time(self.time);
        styled.render(area, frame);
    }

    fn render_info_stats(
        &self,
        frame: &mut Frame,
        area: Rect,
        dashboard_size: (u16, u16),
        theme_name: &str,
        events_per_sec: f64,
    ) {
        if area.is_empty() {
            return;
        }

        let cpu = self
            .simulated_data
            .cpu_history
            .back()
            .copied()
            .unwrap_or(0.0);
        let mem = self
            .simulated_data
            .memory_history
            .back()
            .copied()
            .unwrap_or(0.0);
        let status_color = if cpu > 85.0 || mem > 85.0 {
            theme::intent::error_text()
        } else if cpu > 70.0 || mem > 70.0 {
            theme::intent::warning_text()
        } else {
            theme::intent::success_text()
        };
        let net_in = self
            .simulated_data
            .network_in
            .back()
            .copied()
            .unwrap_or(0.0);
        let net_out = self
            .simulated_data
            .network_out
            .back()
            .copied()
            .unwrap_or(0.0);
        let alerts = self.simulated_data.alerts.len();
        let cells = dashboard_size.0 as u32 * dashboard_size.1 as u32;
        let frame_ms = if self.fps > 0.0 {
            1000.0 / self.fps
        } else {
            0.0
        };
        let (min_frame, max_frame) = self.frame_times.iter().fold(
            (f64::INFINITY, f64::NEG_INFINITY),
            |(min_v, max_v), sample| {
                let ms = *sample as f64 / 1000.0;
                (min_v.min(ms), max_v.max(ms))
            },
        );
        let jitter_ms = if min_frame.is_finite() {
            (max_frame - min_frame).abs()
        } else {
            0.0
        };
        let headroom = 16.0 - frame_ms;

        let cols = Flex::horizontal()
            .constraints([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)])
            .split(area);

        let left_lines = [
            format!("FPS: {:.0}  Tick: {}", self.fps, self.tick_count),
            format!("Events/s: {:.0}", events_per_sec),
            format!("CPU: {:>3.0}%  MEM: {:>3.0}%", cpu, mem),
            format!("NET: {:>4.0}↓ / {:>4.0}↑", net_in, net_out),
            format!("Frame: {frame_ms:>4.1}ms ±{jitter_ms:>3.1}"),
        ];
        let right_lines = [
            format!(
                "Size: {}x{}  Cells: {}",
                dashboard_size.0, dashboard_size.1, cells
            ),
            format!("Theme: {theme_name}"),
            format!("Alerts: {alerts}"),
            "Pipeline: BUF→DIFF→ANSI".to_string(),
            format!("Headroom: {headroom:+.1}ms"),
        ];

        let left_count = left_lines
            .len()
            .min(cols[0].height as usize)
            .min(area.height as usize);
        let right_count = right_lines
            .len()
            .min(cols[1].height as usize)
            .min(area.height as usize);

        let left_rows = Flex::vertical()
            .constraints(vec![Constraint::Fixed(1); left_count])
            .split(cols[0]);
        let right_rows = Flex::vertical()
            .constraints(vec![Constraint::Fixed(1); right_count])
            .split(cols[1]);

        for (idx, (row, line)) in left_rows.iter().zip(left_lines.iter()).enumerate() {
            if row.is_empty() {
                continue;
            }
            let text = truncate_to_width(line, row.width);
            match idx {
                0 => {
                    StyledText::new(text)
                        .bold()
                        .effect(TextEffect::Pulse {
                            speed: 1.6,
                            min_alpha: 0.35,
                        })
                        .base_color(status_color)
                        .time(self.time)
                        .render(*row, frame);
                }
                1 => {
                    StyledText::new(text)
                        .bold()
                        .effect(TextEffect::ColorWave {
                            color1: theme::accent::PRIMARY.into(),
                            color2: theme::accent::ACCENT_8.into(),
                            speed: 1.2,
                            wavelength: 6.0,
                        })
                        .base_color(theme::fg::PRIMARY.into())
                        .time(self.time)
                        .render(*row, frame);
                }
                2 => {
                    Paragraph::new(text)
                        .style(
                            Style::new()
                                .fg(theme::fg::PRIMARY)
                                .bg(theme::alpha::SURFACE),
                        )
                        .render(*row, frame);
                }
                _ => {
                    Paragraph::new(text)
                        .style(
                            Style::new()
                                .fg(theme::fg::SECONDARY)
                                .bg(theme::alpha::OVERLAY),
                        )
                        .render(*row, frame);
                }
            }
        }

        for (idx, (row, line)) in right_rows.iter().zip(right_lines.iter()).enumerate() {
            if row.is_empty() {
                continue;
            }
            let text = truncate_to_width(line, row.width);
            match idx {
                2 => {
                    StyledText::new(text)
                        .bold()
                        .effect(TextEffect::PulsingGlow {
                            color: theme::accent::WARNING.into(),
                            speed: 1.1,
                        })
                        .base_color(theme::intent::warning_text())
                        .time(self.time)
                        .render(*row, frame);
                }
                3 => {
                    StyledText::new(text)
                        .effect(TextEffect::AnimatedGradient {
                            gradient: ColorGradient::lavender(),
                            speed: 0.45,
                        })
                        .base_color(theme::fg::PRIMARY.into())
                        .time(self.time)
                        .render(*row, frame);
                }
                4 => {
                    let accent = if headroom < 0.0 {
                        theme::intent::warning_text()
                    } else {
                        theme::intent::success_text()
                    };
                    StyledText::new(text)
                        .effect(TextEffect::Pulse {
                            speed: 1.1,
                            min_alpha: 0.5,
                        })
                        .base_color(accent)
                        .time(self.time)
                        .render(*row, frame);
                }
                _ => {
                    Paragraph::new(text)
                        .style(
                            Style::new()
                                .fg(theme::fg::SECONDARY)
                                .bg(theme::alpha::SURFACE),
                        )
                        .render(*row, frame);
                }
            }
        }
    }

    fn render_info_sparkline_strip(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() || area.height < 1 {
            return;
        }

        let rows = Flex::vertical()
            .constraints(vec![Constraint::Fixed(1); area.height.min(2) as usize])
            .split(area);

        let frame_data: Vec<f64> = self
            .frame_times
            .iter()
            .map(|sample| *sample as f64 / 1000.0)
            .collect();
        let frame_avg = if frame_data.is_empty() {
            0.0
        } else {
            frame_data.iter().sum::<f64>() / frame_data.len() as f64
        };
        let frame_label = if frame_avg > 0.0 {
            format!("{frame_avg:.1}ms")
        } else {
            "n/a".to_string()
        };
        let net_data: Vec<f64> = self
            .simulated_data
            .network_in
            .iter()
            .zip(self.simulated_data.network_out.iter())
            .map(|(a, b)| a + b)
            .collect();
        let net_last = net_data.last().copied().unwrap_or(0.0);
        let net_label = Self::format_rate(net_last);

        if let Some(row) = rows.first() {
            self.render_labeled_sparkline(
                frame,
                *row,
                "FRM",
                frame_label,
                &frame_data,
                theme::accent::ACCENT_6.into(),
                (
                    theme::accent::ACCENT_6.into(),
                    theme::accent::ACCENT_3.into(),
                ),
            );
        }
        if let Some(row) = rows.get(1) {
            self.render_labeled_sparkline(
                frame,
                *row,
                "NET",
                net_label,
                &net_data,
                theme::accent::ACCENT_8.into(),
                (
                    theme::accent::ACCENT_8.into(),
                    theme::accent::ACCENT_10.into(),
                ),
            );
        }
    }

    /// Render compact mini-bars for CPU/MEM/Disk usage.
    fn render_mini_bars(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() || area.height < 1 {
            return;
        }

        let cpu = self
            .simulated_data
            .cpu_history
            .back()
            .copied()
            .unwrap_or(0.0)
            / 100.0;
        let mem = self
            .simulated_data
            .memory_history
            .back()
            .copied()
            .unwrap_or(0.0)
            / 100.0;
        let net_in = self
            .simulated_data
            .network_in
            .back()
            .copied()
            .unwrap_or(0.0);
        let net_out = self
            .simulated_data
            .network_out
            .back()
            .copied()
            .unwrap_or(0.0);
        let net = ((net_in + net_out) / 2000.0).clamp(0.0, 1.0);
        let disk = self
            .simulated_data
            .disk_usage
            .first()
            .map(|(_, v)| *v / 100.0)
            .unwrap_or(0.0);

        let colors = MiniBarColors::new(
            theme::intent::success_text(),
            theme::intent::warning_text(),
            theme::intent::info_text(),
            theme::intent::error_text(),
        );

        let mut constraints = Vec::new();
        let bar_count = area.height.min(4) as usize;
        for _ in 0..bar_count {
            constraints.push(Constraint::Fixed(1));
        }
        let rows = Flex::vertical().constraints(constraints).split(area);
        let bars = [("CPU", cpu), ("MEM", mem), ("NET", net), ("DSK", disk)];
        for (row, (label, value)) in rows.iter().zip(bars.iter()) {
            self.render_mini_bar_row(frame, *row, label, *value, colors);
        }
    }

    fn render_mini_bar_row(
        &self,
        frame: &mut Frame,
        area: Rect,
        label: &str,
        value: f64,
        colors: MiniBarColors,
    ) {
        if area.is_empty() {
            return;
        }

        let label_width = 4.min(area.width);
        let label_area = Rect::new(area.x, area.y, label_width, 1);
        Paragraph::new(format!("{label} "))
            .style(Style::new().fg(theme::fg::SECONDARY))
            .render(label_area, frame);

        let bar_width = area.width.saturating_sub(label_width);
        if bar_width == 0 {
            return;
        }

        let bar_area = Rect::new(area.x + label_width, area.y, bar_width, 1);
        MiniBar::new(value, bar_width)
            .colors(colors)
            .show_percent(true)
            .render(bar_area, frame);
    }

    fn wrap_markdown_for_panel(&self, text: &Text, width: u16) -> Text {
        let width = usize::from(width);
        if width == 0 {
            return text.clone();
        }

        let mut lines = Vec::new();
        for line in text.lines() {
            let plain = line.to_plain_text();
            let table_like = Self::is_table_line(&plain) || Self::is_table_like_line(&plain);
            if table_like {
                if line.width() <= width {
                    lines.push(line.clone());
                } else {
                    let mut text = Text::from_lines([line.clone()]);
                    text.truncate(width, None);
                    lines.extend(text.lines().iter().cloned());
                }
                continue;
            }
            if line.width() <= width {
                lines.push(line.clone());
                continue;
            }

            for wrapped in line.wrap(width, WrapMode::Word) {
                if wrapped.width() <= width {
                    lines.push(wrapped);
                } else {
                    let mut text = Text::from_lines([wrapped]);
                    text.truncate(width, None);
                    lines.extend(text.lines().iter().cloned());
                }
            }
        }

        Text::from_lines(lines)
    }

    fn is_table_line(plain: &str) -> bool {
        plain.chars().any(|c| {
            matches!(
                c,
                '┌' | '┬' | '┐' | '├' | '┼' | '┤' | '└' | '┴' | '┘' | '│' | '─'
            )
        })
    }

    fn is_table_like_line(plain: &str) -> bool {
        let trimmed = plain.trim_start();
        if !trimmed.starts_with('|') {
            return false;
        }
        trimmed.chars().filter(|&c| c == '|').count() >= 2
    }

    /// Render markdown preview.
    fn render_markdown(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() || area.height < 2 {
            return;
        }

        let progress_pct = (self.md_stream_pos as f64
            / self.current_markdown_sample().len().max(1) as f64
            * 100.0) as u8;
        let status = if self.markdown_stream_complete() {
            "Complete".to_string()
        } else {
            format!("Streaming… {progress_pct}%")
        };
        let title = format!("Markdown · {status}");

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(title.as_str())
            .title_alignment(Alignment::Center)
            .style(theme::panel_border_style(
                self.is_focused(DashboardFocus::Markdown),
                theme::screen_accent::MARKDOWN,
            ));

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let md = self.current_markdown_sample();
        let end = self.md_stream_pos.min(md.len());
        let fragment = &md[..end];
        let renderer = self
            .md_renderer
            .clone()
            .rule_width(inner.width)
            .table_max_width(inner.width);
        let mut rendered = renderer.render_streaming(fragment);

        if !self.markdown_stream_complete() {
            let cursor = Span::styled("▌", Style::new().fg(theme::accent::PRIMARY).blink());
            let mut lines: Vec<Line> = rendered.lines().to_vec();
            if let Some(last_line) = lines.last_mut() {
                last_line.push_span(cursor);
            } else {
                lines.push(Line::from_spans([cursor]));
            }
            rendered = Text::from_lines(lines);
        }

        let wrapped = self.wrap_markdown_for_panel(&rendered, inner.width);
        Paragraph::new(wrapped)
            .wrap(WrapMode::None)
            .render(inner, frame);
        self.render_panel_hint(frame, inner, "Click → Markdown");
    }

    /// Render text effects showcase using complex GFM samples.
    fn render_text_effects(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() || area.height < 2 {
            return;
        }

        let preview_area = Rect::new(area.x, area.y, area.width, area.height.saturating_sub(2));
        let effect_slots = if preview_area.height >= 8 {
            3
        } else if preview_area.height >= 5 {
            2
        } else {
            1
        };
        let title = format!(
            "Text FX · {}-Up ({}/{})",
            effect_slots,
            self.effect_index + 1,
            EFFECT_DEMOS.len()
        );

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(title.as_str())
            .title_alignment(Alignment::Center)
            .style(theme::panel_border_style(
                self.is_focused(DashboardFocus::TextFx),
                theme::screen_accent::WIDGET_GALLERY,
            ));

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let rows = Flex::vertical()
            .constraints([
                Constraint::Fixed(1),
                Constraint::Min(1),
                Constraint::Fixed(1),
            ])
            .split(inner);

        let sample_index = self.effect_index % EFFECT_GFM_SAMPLES.len();
        let header = format!(
            "Sample {} of {} · {}-Up stack",
            sample_index + 1,
            EFFECT_GFM_SAMPLES.len(),
            effect_slots
        );
        Paragraph::new(truncate_to_width(&header, rows[0].width))
            .style(theme::muted())
            .render(rows[0], frame);

        if !rows[1].is_empty() {
            let mut constraints = Vec::new();
            for _ in 0..effect_slots {
                constraints.push(Constraint::Ratio(1, effect_slots as u32));
            }
            let slots = Flex::vertical().constraints(constraints).split(rows[1]);
            for (idx, slot) in slots.iter().enumerate() {
                if slot.is_empty() {
                    continue;
                }
                let effect_idx = (self.effect_index + idx) % EFFECT_DEMOS.len();
                let demo = &EFFECT_DEMOS[effect_idx];
                let sample_idx = (self.effect_index + idx) % EFFECT_GFM_SAMPLES.len();
                let sample = EFFECT_GFM_SAMPLES[sample_idx];
                let sub_rows = Flex::vertical()
                    .constraints([Constraint::Fixed(1), Constraint::Min(1)])
                    .split(*slot);
                let label = format!("{} · {} · S{}", idx + 1, demo.name, sample_idx + 1);
                Paragraph::new(truncate_to_width(&label, sub_rows[0].width))
                    .style(theme::muted())
                    .render(sub_rows[0], frame);

                let max_width = sub_rows[1].width;
                let max_lines = sub_rows[1].height;
                let mut lines = Vec::new();
                for raw in sample.lines() {
                    if lines.len() as u16 >= max_lines {
                        break;
                    }
                    let clipped = truncate_to_width(raw, max_width);
                    lines.push(clipped);
                }
                let text_len: usize = lines.iter().map(|l| grapheme_count(l)).sum();
                let effect = self.build_effect(demo.kind, text_len);
                let styled = StyledMultiLine::new(lines)
                    .effect(effect)
                    .base_color(theme::fg::PRIMARY.into())
                    .time(self.time + idx as f64 * 0.35)
                    .seed(self.tick_count + idx as u64 * 17);
                styled.render(sub_rows[1], frame);
            }
        }

        self.render_panel_hint(frame, rows[2], "e: next set · click → Visual Effects");
    }

    /// Render activity feed showing recent simulated events.
    fn render_activity_feed(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() || area.height < 3 {
            return;
        }

        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Activity")
            .title_alignment(Alignment::Center)
            .style(theme::panel_border_style(
                self.is_focused(DashboardFocus::Activity),
                theme::screen_accent::ADVANCED,
            ));

        let inner = block.inner(area);
        block.render(area, frame);

        if inner.is_empty() {
            return;
        }

        let header_rows = if inner.height >= 4 { 1 } else { 0 };
        if header_rows > 0 {
            let header = Line::from_spans([
                Span::styled("SEV ", theme::muted().bold()),
                Span::styled("TIME  ", theme::muted()),
                Span::styled("COMP  ", theme::muted()),
                Span::styled("MESSAGE", theme::muted()),
            ]);
            Paragraph::new(Text::from_lines([header]))
                .render(Rect::new(inner.x, inner.y, inner.width, 1), frame);
        }

        // Get recent alerts from simulated data
        let max_items = inner.height.saturating_sub(header_rows) as usize;
        let alerts: Vec<_> = self
            .simulated_data
            .alerts
            .iter()
            .rev()
            .take(max_items)
            .collect();

        for (i, alert) in alerts.iter().enumerate() {
            let y = inner.y + header_rows + i as u16;
            if y >= inner.bottom() {
                break;
            }

            let (label, indicator, fg, bg, effect) = match alert.severity {
                AlertSeverity::Error => (
                    "CRIT",
                    "✖",
                    theme::intent::error_text(),
                    theme::with_alpha(theme::intent::ERROR, 180),
                    TextEffect::PulsingGlow {
                        color: PackedRgba::rgb(255, 80, 100),
                        speed: 1.6,
                    },
                ),
                AlertSeverity::Warning => (
                    "WARN",
                    "▲",
                    theme::intent::warning_text(),
                    theme::with_alpha(theme::intent::WARNING, 160),
                    TextEffect::Pulse {
                        speed: 1.4,
                        min_alpha: 0.35,
                    },
                ),
                AlertSeverity::Info => (
                    "INFO",
                    "●",
                    theme::intent::info_text(),
                    theme::with_alpha(theme::intent::INFO, 150),
                    TextEffect::ColorWave {
                        color1: theme::accent::PRIMARY.into(),
                        color2: theme::accent::ACCENT_8.into(),
                        speed: 1.1,
                        wavelength: 6.0,
                    },
                ),
            };

            let component = if alert.message.contains("CPU") {
                "CPU"
            } else if alert.message.contains("Memory") {
                "MEM"
            } else if alert.message.contains("Network") || alert.message.contains("latency") {
                "NET"
            } else if alert.message.contains("Disk") {
                "IO"
            } else if alert.message.contains("TLS") {
                "SEC"
            } else if alert.message.contains("Cache") {
                "CACHE"
            } else {
                "SYS"
            };

            // Format timestamp as MM:SS
            let ts_secs = (alert.timestamp / 10) % 3600;
            let ts_min = ts_secs / 60;
            let ts_sec = ts_secs % 60;
            let time_str = format!("{:02}:{:02}", ts_min, ts_sec);

            let badge = Badge::new(label).with_style(
                Style::new()
                    .fg(theme::fg::PRIMARY)
                    .bg(bg)
                    .attrs(StyleFlags::BOLD),
            );
            let badge_width = badge.width();
            let badge_area = Rect::new(inner.x, y, badge_width.min(inner.width), 1);
            badge.render(badge_area, frame);

            let time_area = Rect::new(
                inner.x + badge_width + 1,
                y,
                6.min(inner.width.saturating_sub(badge_width + 1)),
                1,
            );
            Paragraph::new(time_str.clone())
                .style(theme::muted())
                .render(time_area, frame);

            let comp_area = Rect::new(
                time_area.right().saturating_add(1),
                y,
                6.min(
                    inner
                        .width
                        .saturating_sub(time_area.right().saturating_add(1)),
                ),
                1,
            );
            Paragraph::new(format!("{indicator} {component}"))
                .style(Style::new().fg(fg))
                .render(comp_area, frame);

            let msg_area = Rect::new(
                comp_area.right().saturating_add(1),
                y,
                inner
                    .width
                    .saturating_sub(comp_area.right().saturating_add(1) - inner.x),
                1,
            );
            let msg = truncate_to_width(&alert.message, msg_area.width);
            let styled = StyledText::new(msg)
                .base_color(fg)
                .effect(effect)
                .time(self.time)
                .seed(alert.timestamp);
            styled.render(msg_area, frame);
        }

        // If no alerts yet, show placeholder
        if alerts.is_empty() {
            let styled = StyledText::new("All systems nominal")
                .effect(TextEffect::AnimatedGradient {
                    gradient: ColorGradient::ocean(),
                    speed: 0.4,
                })
                .time(self.time);
            styled.render(inner, frame);
        }
        self.render_panel_hint(frame, inner, "Click → Action Timeline");
    }

    /// Render navigation footer.
    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        let hint = "g:charts c:code e:fx m:md | 1-9:screens | Tab:next | t:theme | ?:help | q:quit";
        Paragraph::new(hint)
            .style(Style::new().fg(theme::fg::MUTED).bg(theme::alpha::SURFACE))
            .render(area, frame);
    }

    fn register_panel_links(&self, frame: &mut Frame) {
        chrome::register_pane_hit(frame, self.layout_plasma.get(), ScreenId::VisualEffects);
        chrome::register_pane_hit(frame, self.layout_charts.get(), ScreenId::DataViz);
        chrome::register_pane_hit(frame, self.layout_code.get(), ScreenId::CodeExplorer);
        chrome::register_pane_hit(frame, self.layout_info.get(), ScreenId::Performance);
        chrome::register_pane_hit(frame, self.layout_text_fx.get(), ScreenId::VisualEffects);
        chrome::register_pane_hit(frame, self.layout_activity.get(), ScreenId::ActionTimeline);
        chrome::register_pane_hit(
            frame,
            self.layout_markdown.get(),
            ScreenId::MarkdownRichText,
        );
    }

    // =========================================================================
    // Layout Variants
    // =========================================================================

    /// Large layout (100x30+).
    fn render_large(&self, frame: &mut Frame, area: Rect) {
        // Main vertical split: header, content, footer
        let main = Flex::vertical()
            .constraints([
                Constraint::Fixed(1), // Header
                Constraint::Min(10),  // Content
                Constraint::Fixed(1), // Footer
            ])
            .split(area);

        self.render_header(frame, main[0]);
        self.render_footer(frame, main[2]);

        // Content area: split into top row and bottom row
        let content_rows = Flex::vertical()
            .constraints([Constraint::Percentage(55.0), Constraint::Percentage(45.0)])
            .split(main[1]);

        // Top row: 4 panels (plasma, charts, code, info)
        let top_cols = Flex::horizontal()
            .constraints([
                Constraint::Percentage(20.0),
                Constraint::Percentage(30.0),
                Constraint::Percentage(30.0),
                Constraint::Percentage(20.0),
            ])
            .split(content_rows[0]);

        self.layout_plasma.set(top_cols[0]);
        self.layout_charts.set(top_cols[1]);
        self.layout_code.set(top_cols[2]);
        self.layout_info.set(top_cols[3]);
        self.render_plasma(frame, top_cols[0]);
        self.render_charts(frame, top_cols[1]);
        self.render_code(frame, top_cols[2]);
        self.render_info(frame, top_cols[3], (area.width, area.height));

        // Bottom row: stats, activity feed, markdown
        let bottom_cols = Flex::horizontal()
            .constraints([
                Constraint::Percentage(25.0),
                Constraint::Percentage(40.0),
                Constraint::Percentage(35.0),
            ])
            .split(content_rows[1]);

        self.layout_text_fx.set(bottom_cols[0]);
        self.layout_activity.set(bottom_cols[1]);
        self.layout_markdown.set(bottom_cols[2]);
        self.render_text_effects(frame, bottom_cols[0]);
        self.render_activity_feed(frame, bottom_cols[1]);
        self.render_markdown(frame, bottom_cols[2]);
    }

    /// Medium layout (70x20+).
    fn render_medium(&self, frame: &mut Frame, area: Rect) {
        let main = Flex::vertical()
            .constraints([
                Constraint::Fixed(1), // Header
                Constraint::Min(8),   // Content
                Constraint::Fixed(1), // Footer
            ])
            .split(area);

        self.render_header(frame, main[0]);
        self.render_footer(frame, main[2]);

        // Content: top row with panels, bottom row with stats + activity
        let content_rows = Flex::vertical()
            .constraints([Constraint::Percentage(60.0), Constraint::Percentage(40.0)])
            .split(main[1]);

        // Top row: 3 panels
        let top_cols = Flex::horizontal()
            .constraints([
                Constraint::Percentage(25.0),
                Constraint::Percentage(40.0),
                Constraint::Percentage(35.0),
            ])
            .split(content_rows[0]);

        self.layout_plasma.set(top_cols[0]);
        self.layout_charts.set(top_cols[1]);
        self.render_plasma(frame, top_cols[0]);
        self.render_charts(frame, top_cols[1]);

        // Combined code + info in the third column
        let right_split = Flex::vertical()
            .constraints([Constraint::Percentage(60.0), Constraint::Percentage(40.0)])
            .split(top_cols[2]);

        self.layout_code.set(right_split[0]);
        self.layout_info.set(right_split[1]);
        self.render_code(frame, right_split[0]);
        self.render_info(frame, right_split[1], (area.width, area.height));

        // Bottom row: text effects, activity feed, markdown stream
        let bottom_cols = Flex::horizontal()
            .constraints([
                Constraint::Percentage(30.0),
                Constraint::Percentage(40.0),
                Constraint::Percentage(30.0),
            ])
            .split(content_rows[1]);

        self.layout_text_fx.set(bottom_cols[0]);
        self.layout_activity.set(bottom_cols[1]);
        self.layout_markdown.set(bottom_cols[2]);
        self.render_text_effects(frame, bottom_cols[0]);
        self.render_activity_feed(frame, bottom_cols[1]);
        self.render_markdown(frame, bottom_cols[2]);
    }

    /// Tiny layout (<70x20).
    fn render_tiny(&self, frame: &mut Frame, area: Rect) {
        let main = Flex::vertical()
            .constraints([
                Constraint::Fixed(1), // Header
                Constraint::Min(4),   // Content
                Constraint::Fixed(1), // Footer
            ])
            .split(area);

        self.render_header(frame, main[0]);

        // Compact footer
        let hint = "t:theme q:quit";
        Paragraph::new(hint)
            .style(Style::new().fg(theme::fg::MUTED).bg(theme::alpha::SURFACE))
            .render(main[2], frame);

        // Content: two columns
        let cols = Flex::horizontal()
            .constraints([Constraint::Percentage(35.0), Constraint::Percentage(65.0)])
            .split(main[1]);

        // Left: plasma
        self.layout_plasma.set(cols[0]);
        self.render_plasma(frame, cols[0]);

        // Right: compact info with sparklines
        let right_rows = Flex::vertical()
            .constraints([Constraint::Min(1), Constraint::Fixed(2)])
            .split(cols[1]);
        self.layout_charts.set(right_rows[0]);
        self.layout_info.set(right_rows[1]);
        self.layout_code.set(Rect::default());
        self.layout_text_fx.set(Rect::default());
        self.layout_activity.set(Rect::default());
        self.layout_markdown.set(Rect::default());

        // Sparklines (just CPU and MEM)
        if !right_rows[0].is_empty() && !self.simulated_data.cpu_history.is_empty() {
            let spark_rows = Flex::vertical()
                .constraints([Constraint::Fixed(1), Constraint::Fixed(1)])
                .split(right_rows[0]);

            let cpu_data: Vec<f64> = self.simulated_data.cpu_history.iter().copied().collect();
            if !spark_rows[0].is_empty() && !cpu_data.is_empty() {
                let label_w = 4.min(spark_rows[0].width);
                Paragraph::new("CPU ")
                    .style(Style::new().fg(theme::fg::SECONDARY))
                    .render(
                        Rect::new(spark_rows[0].x, spark_rows[0].y, label_w, 1),
                        frame,
                    );
                let spark_area = Rect::new(
                    spark_rows[0].x + label_w,
                    spark_rows[0].y,
                    spark_rows[0].width.saturating_sub(label_w),
                    1,
                );
                if !spark_area.is_empty() {
                    Sparkline::new(&cpu_data)
                        .style(Style::new().fg(theme::accent::PRIMARY))
                        .render(spark_area, frame);
                }
            }

            if spark_rows.len() > 1
                && !spark_rows[1].is_empty()
                && !self.simulated_data.memory_history.is_empty()
            {
                let label_w = 4.min(spark_rows[1].width);
                Paragraph::new("MEM ")
                    .style(Style::new().fg(theme::fg::SECONDARY))
                    .render(
                        Rect::new(spark_rows[1].x, spark_rows[1].y, label_w, 1),
                        frame,
                    );
                let spark_area = Rect::new(
                    spark_rows[1].x + label_w,
                    spark_rows[1].y,
                    spark_rows[1].width.saturating_sub(label_w),
                    1,
                );
                if !spark_area.is_empty() {
                    let mem_data: Vec<f64> =
                        self.simulated_data.memory_history.iter().copied().collect();
                    Sparkline::new(&mem_data)
                        .style(Style::new().fg(theme::accent::SUCCESS))
                        .render(spark_area, frame);
                }
            }
        }

        // Compact info
        if !right_rows[1].is_empty() {
            let info = format!("FPS:{:.0} {}x{}", self.fps, area.width, area.height);
            Paragraph::new(info)
                .style(Style::new().fg(theme::fg::MUTED))
                .render(right_rows[1], frame);
        }
    }

    fn format_percent(value: f64) -> String {
        let clamped = if value.is_finite() {
            value.clamp(0.0, 100.0)
        } else {
            0.0
        };
        format!("{:>3.0}%", clamped)
    }

    fn format_rate(value: f64) -> String {
        if !value.is_finite() {
            return "---".to_string();
        }
        if value >= 1000.0 {
            format!("{:>3.0}k", value / 1000.0)
        } else {
            format!("{:>3.0}", value)
        }
    }

    fn chart_palette_extended() -> [PackedRgba; 5] {
        [
            theme::accent::PRIMARY.into(),
            theme::accent::SUCCESS.into(),
            theme::accent::WARNING.into(),
            theme::accent::ACCENT_8.into(),
            theme::accent::ACCENT_10.into(),
        ]
    }
}

/// Helper to render Text widget line by line.
fn render_text(frame: &mut Frame, area: Rect, text: &Text) {
    if area.is_empty() {
        return;
    }

    let lines = text.lines();
    for (i, line) in lines.iter().enumerate() {
        if i as u16 >= area.height {
            break;
        }
        let line_y = area.y + i as u16;
        // Render each span in the line
        let mut x_offset = 0u16;
        for span in line.spans() {
            let text_len = span.width().min(u16::MAX as usize) as u16;
            if x_offset >= area.width {
                break;
            }
            let span_area = Rect::new(
                area.x + x_offset,
                line_y,
                (area.width - x_offset).min(text_len),
                1,
            );
            let style = span.style.unwrap_or_default();
            Paragraph::new(span.content.as_ref())
                .style(style)
                .render(span_area, frame);
            x_offset += text_len;
        }
    }
}

fn truncate_to_width(text: &str, max_width: u16) -> String {
    if max_width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut width = 0usize;
    let max = max_width as usize;
    for grapheme in graphemes(text) {
        let w = grapheme_width(grapheme);
        if width + w > max {
            break;
        }
        out.push_str(grapheme);
        width += w;
    }
    out
}

impl Screen for Dashboard {
    type Message = Event;

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if let Event::Mouse(mouse) = event {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                self.focus_from_point(mouse.x, mouse.y);
            }
            return Cmd::None;
        }

        if let Event::Key(KeyEvent {
            code,
            kind: KeyEventKind::Press,
            ..
        }) = event
        {
            match code {
                // Focus navigation
                KeyCode::Tab | KeyCode::Right | KeyCode::Down => {
                    self.focus = self.focus.next();
                }
                KeyCode::BackTab | KeyCode::Left | KeyCode::Up => {
                    self.focus = self.focus.prev();
                }
                // Reset animations
                KeyCode::Char('r') => {
                    self.tick_count = 0;
                    self.time = 0.0;
                    self.reset_markdown_stream();
                }
                // Cycle code samples
                KeyCode::Char('c') => {
                    self.code_index = (self.code_index + 1) % CODE_SAMPLES.len();
                }
                KeyCode::Char('C') => {
                    self.code_index =
                        (self.code_index + CODE_SAMPLES.len() - 1) % CODE_SAMPLES.len();
                }
                // Cycle text effects (also rotates sample)
                KeyCode::Char('e') => {
                    self.effect_index = (self.effect_index + 1) % EFFECT_DEMOS.len();
                }
                KeyCode::Char('E') => {
                    self.effect_index =
                        (self.effect_index + EFFECT_DEMOS.len() - 1) % EFFECT_DEMOS.len();
                }
                // Cycle markdown samples + restart stream
                KeyCode::Char('m') => {
                    self.md_sample_index = (self.md_sample_index + 1) % DASH_MARKDOWN_SAMPLES.len();
                    self.reset_markdown_stream();
                }
                KeyCode::Char('M') => {
                    self.md_sample_index = (self.md_sample_index + DASH_MARKDOWN_SAMPLES.len() - 1)
                        % DASH_MARKDOWN_SAMPLES.len();
                    self.reset_markdown_stream();
                }
                // Cycle chart modes
                KeyCode::Char('g') => {
                    self.chart_mode = self.chart_mode.next();
                }
                KeyCode::Char('G') => {
                    self.chart_mode = self.chart_mode.prev();
                }
                _ => {}
            }
        }

        Cmd::None
    }

    fn tick(&mut self, tick_count: u64) {
        self.tick_count = tick_count;
        self.time = tick_count as f64 * 0.1; // 100ms per tick
        self.tick_markdown_stream();
        self.simulated_data.tick(tick_count);
        self.update_fps();
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }

        // Choose layout based on terminal size
        let _layout = match (area.width, area.height) {
            (w, h) if w >= 100 && h >= 30 => {
                self.render_large(frame, area);
                "large"
            }
            (w, h) if w >= 70 && h >= 20 => {
                self.render_medium(frame, area);
                "medium"
            }
            _ => {
                self.render_tiny(frame, area);
                "tiny"
            }
        };
        self.register_panel_links(frame);
        crate::debug_render!(
            "dashboard",
            "layout={_layout}, area={}x{}, tick={}",
            area.width,
            area.height,
            self.tick_count
        );
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        vec![
            HelpEntry {
                key: "r",
                action: "Reset animations",
            },
            HelpEntry {
                key: "c",
                action: "Cycle code language",
            },
            HelpEntry {
                key: "e",
                action: "Cycle text effects",
            },
            HelpEntry {
                key: "m",
                action: "Cycle markdown sample",
            },
            HelpEntry {
                key: "g",
                action: "Cycle chart mode",
            },
            HelpEntry {
                key: "t",
                action: "Cycle theme",
            },
            HelpEntry {
                key: "Mouse",
                action: "Click pane to focus",
            },
        ]
    }

    fn title(&self) -> &'static str {
        "Dashboard"
    }

    fn tab_label(&self) -> &'static str {
        "Dashboard"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_extras::syntax::SyntaxHighlighter;
    use ftui_render::grapheme_pool::GraphemePool;

    #[test]
    fn dashboard_renders_header() {
        let mut state = Dashboard::new();
        state.tick(10);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);

        state.view(&mut frame, Rect::new(0, 0, 120, 40));

        // Header should be present (first row should not be empty)
        let mut has_content = false;
        for x in 0..120 {
            if let Some(cell) = frame.buffer.get(x, 0)
                && cell.content.as_char() != Some(' ')
                && !cell.is_empty()
            {
                has_content = true;
                break;
            }
        }
        assert!(has_content, "Header should render content");
    }

    #[test]
    fn dashboard_mouse_click_focuses_markdown_panel() {
        let mut state = Dashboard::new();
        state.tick(1);

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(120, 40, &mut pool);
        state.view(&mut frame, Rect::new(0, 0, 120, 40));

        let md = state.layout_markdown.get();
        assert!(!md.is_empty(), "markdown panel should be laid out");

        let click = Event::Mouse(ftui_core::event::MouseEvent::new(
            MouseEventKind::Down(MouseButton::Left),
            md.x + 1,
            md.y + 1,
        ));
        state.update(&click);

        assert_eq!(state.focus, DashboardFocus::Markdown);
    }

    #[test]
    fn dashboard_shows_metrics() {
        let mut state = Dashboard::new();
        // Populate some history
        for t in 0..50 {
            state.tick(t);
        }

        assert!(
            !state.simulated_data.cpu_history.is_empty(),
            "CPU history should be populated"
        );
        assert!(
            !state.simulated_data.memory_history.is_empty(),
            "Memory history should be populated"
        );
    }

    #[test]
    fn dashboard_sparklines_update() {
        let mut state = Dashboard::new();
        let initial_len = state.simulated_data.cpu_history.len();

        state.tick(100);

        assert!(
            state.simulated_data.cpu_history.len() > initial_len,
            "CPU history should grow on tick"
        );
    }

    #[test]
    fn dashboard_handles_resize() {
        let state = Dashboard::new();

        // Small terminal
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(40, 15, &mut pool);
        state.view(&mut frame, Rect::new(0, 0, 40, 15));
        // Should not panic

        // Large terminal
        let mut pool2 = GraphemePool::new();
        let mut frame2 = Frame::new(200, 60, &mut pool2);
        state.view(&mut frame2, Rect::new(0, 0, 200, 60));
        // Should not panic
    }

    #[test]
    fn dashboard_activity_feed_populates() {
        let mut state = Dashboard::new();

        // Run enough ticks to generate alerts (ALERT_INTERVAL is 20)
        for t in 0..100 {
            state.tick(t);
        }

        // Should have alerts
        assert!(
            !state.simulated_data.alerts.is_empty(),
            "Alerts should be generated after sufficient ticks"
        );
    }

    #[test]
    fn dashboard_text_effects_renders() {
        let state = Dashboard::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(50, 5, &mut pool);

        // Render just the text effects panel
        state.render_text_effects(&mut frame, Rect::new(0, 0, 50, 5));

        // Check that content was rendered (border + stats)
        let top_left = frame.buffer.get(0, 0).and_then(|c| c.content.as_char());
        assert!(
            top_left.is_some(),
            "Text effects panel should render border character"
        );
    }

    #[test]
    fn dashboard_activity_feed_renders() {
        let mut state = Dashboard::new();
        // Generate some alerts first
        for t in 0..100 {
            state.tick(t);
        }

        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(60, 10, &mut pool);

        // Render just the activity feed panel
        state.render_activity_feed(&mut frame, Rect::new(0, 0, 60, 10));

        // Check that border was rendered
        let top_left = frame.buffer.get(0, 0).and_then(|c| c.content.as_char());
        assert!(
            top_left.is_some(),
            "Activity feed should render border character"
        );
    }

    #[test]
    fn dashboard_tick_updates_time() {
        let mut state = Dashboard::new();
        assert_eq!(state.tick_count, 30); // Pre-populated in new()

        state.tick(50);
        assert_eq!(state.tick_count, 50);
        assert!(
            (state.time - 5.0).abs() < f64::EPSILON,
            "time should be tick * 0.1"
        );
    }

    #[test]
    fn dashboard_empty_area_handled() {
        let state = Dashboard::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(1, 1, &mut pool);

        // Should not panic with empty area
        state.view(&mut frame, Rect::new(0, 0, 0, 0));
    }

    #[test]
    fn dashboard_layout_large_threshold() {
        let state = Dashboard::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(100, 30, &mut pool);

        // At exactly 100x30, should use large layout
        state.view(&mut frame, Rect::new(0, 0, 100, 30));
        // Should not panic
    }

    #[test]
    fn dashboard_layout_medium_threshold() {
        let state = Dashboard::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(70, 20, &mut pool);

        // At exactly 70x20, should use medium layout
        state.view(&mut frame, Rect::new(0, 0, 70, 20));
        // Should not panic
    }

    #[test]
    fn dashboard_layout_tiny_threshold() {
        let state = Dashboard::new();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(50, 15, &mut pool);

        // Below medium thresholds, should use tiny layout
        state.view(&mut frame, Rect::new(0, 0, 50, 15));
        // Should not panic
    }

    #[test]
    fn dashboard_code_samples_have_highlighters() {
        let highlighter = SyntaxHighlighter::new();
        for sample in CODE_SAMPLES {
            assert!(
                highlighter.supports_language(sample.lang),
                "Missing syntax highlighter for language: {}",
                sample.lang
            );
        }
    }
}
