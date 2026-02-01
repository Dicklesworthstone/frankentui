# Unit Test Coverage Matrix

This document encodes the project’s expectations for unit test coverage by crate and module.

This exists so we can:
- avoid “test later” drift
- keep kernel invariants continuously verified
- make CI decisions explicit

See Bead: bd-3hy.

## Coverage Targets (v1)
- ftui-render: ≥ 85%
- ftui-core: ≥ 80%
- ftui-style: ≥ 80%
- ftui-text: ≥ 80%
- ftui-layout: ≥ 75%
- ftui-runtime: ≥ 75%
- ftui-widgets: ≥ 70%
- ftui-extras: ≥ 60%

Note: Integration-heavy PTY tests are enforced separately; do not “unit test” around reality.

## ftui-render
Kernel correctness lives here.

Cell/Buffer/Diff/Presenter are expected to have dense unit tests and property tests.

## ftui-core
TerminalSession lifecycle + InputParser correctness.

## ftui-style
Deterministic style semantics and theme behaviors.

## ftui-text
Unicode width correctness and wrap/truncate behaviors.

## ftui-layout
Solver invariants and rect operations.

## ftui-runtime
Deterministic scheduling, simulator behavior, and subscriptions.

## ftui-widgets
Harness-essential widgets must have snapshot tests and key unit tests.

## ftui-extras
Feature-gated, but correctness still matters.
