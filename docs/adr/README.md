# Architecture Decision Records (ADRs)

This directory records decisions that are likely to be controversial later, or that we want to lock to avoid churn.

Format:
- Context
- Decision
- Alternatives considered
- Consequences
- Test plan / verification

ADRs should be short, opinionated, and operational.

Current ADRs:

| ADR | Title | Status |
|-----|-------|--------|
| [ADR-001](ADR-001-inline-mode.md) | Inline Mode Strategy | ACCEPTED |
| [ADR-002](ADR-002-presenter-emission.md) | Presenter Emission Strategy | ACCEPTED |
| [ADR-003](ADR-003-terminal-backend.md) | Terminal Backend Selection | ACCEPTED |
| [ADR-004](ADR-004-windows-v1-scope.md) | Windows v1 Scope | ACCEPTED (2026-10-19 — crossterm default on Windows for v1; native `ftui-win32` deferred) |
| [ADR-005](ADR-005-one-writer-rule.md) | One-Writer Rule Enforcement | ACCEPTED (2026-10-16) |
| [ADR-006](ADR-006-untrusted-output-policy.md) | Untrusted Output Policy | ACCEPTED (2026-10-14) |
| [ADR-007](ADR-007-sdk-modularization.md) | SDK Modularization | ACCEPTED (2026-10-16 — host-specific bindings via `ftui-web`) |
| [ADR-008](ADR-008-terminal-backend-strategy.md) | Terminal Backend Strategy (v2+) | ACCEPTED (2026-10-16 — scope: `ftui-tty` + `crossterm` + `ftui-web`; presenter seam in G13) |
| [ADR-009](ADR-009-webgpu-renderer-architecture.md) | FrankenTerm WebGPU Renderer Architecture | ACCEPTED (2026-10-16 — out-of-tree `frankenterm-web`) |
| [ADR-010](ADR-010-asupersync-targeted-adoption.md) | Targeted Asupersync Adoption For Runtime Shell And Doctor | ACCEPTED (2026-10-16 — scope: `ftui-runtime/asupersync-executor`) |
| [ADR-011](ADR-011-no-continuous-time-frp.md) | No Continuous-Time FRP for Animations | ACCEPTED (defer indefinitely — 2026-07-06) |
