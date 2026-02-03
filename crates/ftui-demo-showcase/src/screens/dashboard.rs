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
use ftui_style::Style;
use ftui_text::{Line, Span, Text, WrapMode};
use ftui_widgets::Badge;
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::paragraph::Paragraph;
use ftui_widgets::progress::{MiniBar, MiniBarColors};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::{HelpEntry, Screen};
use crate::data::{AlertSeverity, SimulatedData};
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
                .set_raw(i as u16, 0, RenderCell::from_char(ch));
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
ORDER BY j.avg_ratio DESC;"###,
    },
    CodeSample {
        label: "JSON",
        lang: "json",
        code: r###"{
  "mode": "inline",
  "uiHeight": 12,
  "renderer": {
    "diff": "row-major",
    "strategy": "bayes",
    "cellBytes": 16,
    "budgets": {
      "renderMs": 8.0,
      "presentMs": 4.0,
      "degradation": "auto"
    }
  },
  "features": ["mouse", "paste", "focus", "synchronized-output"],
  "theme": {
    "name": "NordicFrost",
    "accent": "#6AD1E3",
    "bg": "#0E141B",
    "fg": "#E6EEF5"
  },
  "telemetry": {
    "enabled": true,
    "sampleRate": 0.25,
    "exporter": "otlp",
    "endpoint": "http://localhost:4317"
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
"###,
    },
    CodeSample {
        label: "Bash",
        lang: "sh",
        code: r###"#!/usr/bin/env bash
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
"###,
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
"###,
    },
    CodeSample {
        label: "Ruby",
        lang: "rb",
        code: r###"# pipeline.rb
require "set"

Frame = Struct.new(:id, :dirty, :tags, keyword_init: true)

module Pipeline
  def self.run(frames, budget_ms: 12)
    start = Process.clock_gettime(Process::CLOCK_MONOTONIC)
    count = frames.select(&:dirty).sum { |f| f.tags.size }
    elapsed = (Process.clock_gettime(Process::CLOCK_MONOTONIC) - start) * 1000.0
    raise "over budget" if elapsed > budget_ms
    count
  end

  def self.diff(prev, nxt)
    seen = prev.to_set
    nxt.reject { |x| seen.include?(x) }
  end
end

frames = (1..5).map { |i| Frame.new(id: i, dirty: i.even?, tags: { idx: i.to_s }) }
Pipeline.run(frames)
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
        print("count=\(count ?? -1) diff=\(diff(frames, frames).count) over=\(await budget.overBudget(elapsed))")
    }
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
echo "count={$count} diff=" . count($pipeline->diff($frames, $frames)) . " over=" . ($pipeline->overBudget($elapsed) ? "yes" : "no");
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
      <template id="card">
        <article><slot></slot></article>
      </template>
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <path d="M4 12h16M12 4v16" />
      </svg>
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
}

@supports (backdrop-filter: blur(12px)) {
  .glass { backdrop-filter: blur(12px); }
}

@keyframes pulse {
  0% { transform: translateY(0); opacity: 0.7; }
  50% { transform: translateY(-6px); opacity: 1; }
  100% { transform: translateY(0); opacity: 0.7; }
}

@media (prefers-reduced-motion: reduce) {
  .pulse { animation: none; }
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

local frames = {
  Frame.new(1, true, { "inline", "focus" }),
  Frame.new(2, false, { "alt" }),
}

local result = render(frames)
local changed = diff(frames, frames)
print(("count=%d diff=%d"):format(result, #changed))
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

> [!NOTE]
> Math: `E = mc^2` and `∑ᵢ xᵢ`

Footnote[^1] and **links**: https://ftui.dev

[^1]: Determinism beats magic.
"###,
    r###"# Rendering Playbook

1. **Build** the frame
2. **Diff** buffers
3. **Present** ANSI

## Task List
- [x] Dirty-row tracking
- [x] ANSI cost model
- [ ] GPU? nope

| Metric | Target |
| --- | --- |
| Frame | <16ms |
| Diff | <4ms |

```bash
FTUI_HARNESS_SCREEN_MODE=inline cargo run -p ftui-harness
```

> [!TIP]
> Use `Cmd::batch` for side effects.
"###,
];

const EFFECT_GFM_SAMPLES: &[&str] = &[
    r#"# FX Lab
> *"Render truth, not pixels."*

- [x] Inline scrollback
- [x] Deterministic diff
- [ ] GPU hype

| Key | Action |
| --- | --- |
| `e` | next FX |
| `c` | next code |

```bash
ftui run --inline --height 12
```

[^1]: Effects are deterministic.
"#,
    r#"## GFM Stress
1. **Bold** + _italic_
2. `code` + ~~strike~~
3. Link: https://ftui.dev

| op | cost |
| -- | --- |
| diff | O(n) |

> [!TIP]
> Use `Cmd::batch`.
"#,
    r#"### Mixed
- [x] Tasks
- [ ] Benchmarks

```sql
SELECT * FROM diff WHERE dirty = true;
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

impl ChartMode {
    fn next(self) -> Self {
        match self {
            Self::Pulse => Self::Lines,
            Self::Lines => Self::Bars,
            Self::Bars => Self::Heatmap,
            Self::Heatmap => Self::Pulse,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Pulse => "Pulse",
            Self::Lines => "Lines",
            Self::Bars => "Bars",
            Self::Heatmap => "Heatmap",
        }
    }

    fn subtitle(self) -> &'static str {
        match self {
            Self::Pulse => "sparklines + mini bars",
            Self::Lines => "braille line chart + legend",
            Self::Bars => "grouped or stacked volumes",
            Self::Heatmap => "animated intensity grid",
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
    highlighter: SyntaxHighlighter,

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

        Self {
            tick_count: 30,
            time: 0.0,
            simulated_data,
            frame_times: VecDeque::with_capacity(60),
            last_frame: None,
            fps: 0.0,
            highlighter,
            md_renderer: MarkdownRenderer::new(MarkdownTheme::default()),
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
        self.highlighter.set_theme(theme::syntax_theme());
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
        let mut new_pos = self.md_stream_pos.saturating_add(6);
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
            let hint = format!("g:cycle · {}", self.chart_mode.subtitle());
            Paragraph::new(hint)
                .style(Style::new().fg(theme::fg::MUTED))
                .render(header_area, frame);
        }

        match self.chart_mode {
            ChartMode::Pulse => self.render_pulse_charts(frame, content),
            ChartMode::Lines => self.render_line_charts(frame, content),
            ChartMode::Bars => self.render_bar_charts(frame, content),
            ChartMode::Heatmap => self.render_heatmap_charts(frame, content),
        }
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

        let label_width = label.chars().count().max(3) as u16 + 1;
        let label_width = label_width.min(area.width);
        let value_width = value_label.chars().count().min(area.width as usize) as u16;
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

        let highlighted = self.highlighter.highlight(sample.code, sample.lang);

        // Render as paragraph with styled text
        render_text(frame, inner, &highlighted);
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
        let bars_height = if remaining >= 4 { 3 } else { 0 };
        let stats_height = remaining.saturating_sub(bars_height).max(1);
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
        }
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
                wavelength: 8.0,
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

        let cols = Flex::horizontal()
            .constraints([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)])
            .split(area);

        let left_lines = [
            format!("FPS: {:.0}  Tick: {}", self.fps, self.tick_count),
            format!("Events/s: {:.0}", events_per_sec),
            format!("CPU: {:>3.0}%  MEM: {:>3.0}%", cpu, mem),
            format!("NET: {:>4.0}↓ / {:>4.0}↑", net_in, net_out),
        ];
        let right_lines = [
            format!(
                "Size: {}x{}  Cells: {}",
                dashboard_size.0, dashboard_size.1, cells
            ),
            format!("Theme: {theme_name}"),
            format!("Alerts: {alerts}"),
            "Pipeline: BUF→DIFF→ANSI".to_string(),
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
        let mut rendered = self.md_renderer.render_streaming(fragment);

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

        Paragraph::new(rendered)
            .wrap(WrapMode::Word)
            .render(inner, frame);
    }

    /// Render text effects showcase using complex GFM samples.
    fn render_text_effects(&self, frame: &mut Frame, area: Rect) {
        if area.is_empty() || area.height < 2 {
            return;
        }

        let preview_area = Rect::new(area.x, area.y, area.width, area.height.saturating_sub(2));
        let effect_slots = if preview_area.height >= 9 {
            3
        } else if preview_area.height >= 6 {
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
                let text_len: usize = lines.iter().map(|l| l.chars().count()).sum();
                let effect = self.build_effect(demo.kind, text_len);
                let styled = StyledMultiLine::new(lines)
                    .effect(effect)
                    .base_color(theme::fg::PRIMARY.into())
                    .time(self.time + idx as f64 * 0.35)
                    .seed(self.tick_count + idx as u64 * 17);
                styled.render(sub_rows[1], frame);
            }
        }

        Paragraph::new("e: next set · stacked multi-preview")
            .style(theme::muted())
            .render(rows[2], frame);
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

        // Get recent alerts from simulated data
        let max_items = inner.height as usize;
        let alerts: Vec<_> = self
            .simulated_data
            .alerts
            .iter()
            .rev()
            .take(max_items)
            .collect();

        for (i, alert) in alerts.iter().enumerate() {
            if i as u16 >= inner.height {
                break;
            }

            let y = inner.y + i as u16;

            let (label, indicator, color, effect) = match alert.severity {
                AlertSeverity::Error => (
                    "CRIT",
                    "✖",
                    theme::intent::error_text(),
                    TextEffect::PulsingGlow {
                        color: PackedRgba::rgb(255, 80, 100),
                        speed: 1.6,
                    },
                ),
                AlertSeverity::Warning => (
                    "WARN",
                    "▲",
                    theme::intent::warning_text(),
                    TextEffect::Pulse {
                        speed: 1.4,
                        min_alpha: 0.35,
                    },
                ),
                AlertSeverity::Info => (
                    "INFO",
                    "●",
                    theme::intent::info_text(),
                    TextEffect::ColorWave {
                        color1: theme::accent::PRIMARY.into(),
                        color2: theme::accent::ACCENT_8.into(),
                        speed: 1.1,
                        wavelength: 6.0,
                    },
                ),
            };

            // Format timestamp as MM:SS
            let ts_secs = (alert.timestamp / 10) % 3600;
            let ts_min = ts_secs / 60;
            let ts_sec = ts_secs % 60;
            let time_str = format!("{:02}:{:02}", ts_min, ts_sec);

            let prefix_plain = format!("{indicator} {label} {time_str} · ");
            let prefix_width = UnicodeWidthStr::width(prefix_plain.as_str()) as u16;
            let prefix_area = Rect::new(inner.x, y, prefix_width.min(inner.width), 1);

            let prefix_line = Line::from_spans([
                Span::styled(format!("{indicator} "), Style::new().fg(color).bold()),
                Span::styled(format!("{label} "), Style::new().fg(color).bold()),
                Span::styled(time_str.clone(), theme::muted()),
                Span::styled(" · ", theme::muted()),
            ]);

            Paragraph::new(Text::from_lines([prefix_line])).render(prefix_area, frame);

            if inner.width <= prefix_width + 1 {
                continue;
            }

            let msg_area = Rect::new(
                inner.x + prefix_width,
                y,
                inner.width.saturating_sub(prefix_width),
                1,
            );
            let msg = truncate_to_width(&alert.message, msg_area.width);
            let styled = StyledText::new(msg)
                .base_color(color)
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
        if !right_rows[0].is_empty() {
            let spark_rows = Flex::vertical()
                .constraints([Constraint::Fixed(1), Constraint::Fixed(1)])
                .split(right_rows[0]);

            let cpu_data: Vec<f64> = self.simulated_data.cpu_history.iter().copied().collect();
            let mem_data: Vec<f64> = self.simulated_data.memory_history.iter().copied().collect();

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

            if spark_rows.len() > 1 && !spark_rows[1].is_empty() && !mem_data.is_empty() {
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
            let text_len = span.content.chars().count() as u16;
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
    for ch in text.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + w > max {
            break;
        }
        out.push(ch);
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
                // Reset animations
                KeyCode::Char('r') => {
                    self.tick_count = 0;
                    self.time = 0.0;
                    self.reset_markdown_stream();
                }
                // Cycle code samples
                KeyCode::Char('c') => {
                    if matches!(self.focus, DashboardFocus::Code | DashboardFocus::None) {
                        self.code_index = (self.code_index + 1) % CODE_SAMPLES.len();
                    }
                }
                // Cycle text effects (also rotates sample)
                KeyCode::Char('e') => {
                    if matches!(self.focus, DashboardFocus::TextFx | DashboardFocus::None) {
                        self.effect_index = (self.effect_index + 1) % EFFECT_DEMOS.len();
                    }
                }
                // Cycle markdown samples + restart stream
                KeyCode::Char('m') => {
                    if matches!(self.focus, DashboardFocus::Markdown | DashboardFocus::None) {
                        self.md_sample_index =
                            (self.md_sample_index + 1) % DASH_MARKDOWN_SAMPLES.len();
                        self.reset_markdown_stream();
                    }
                }
                // Cycle chart modes
                KeyCode::Char('g') => {
                    if matches!(self.focus, DashboardFocus::Charts | DashboardFocus::None) {
                        self.chart_mode = self.chart_mode.next();
                    }
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
}
