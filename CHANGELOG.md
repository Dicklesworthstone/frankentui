# Changelog

All notable changes to [FrankenTUI](https://github.com/Dicklesworthstone/frankentui) are documented here, organized by capabilities rather than diff order.

**Repo:** <https://github.com/Dicklesworthstone/frankentui>
**Crate:** `ftui` (facade) plus 19 workspace crates
**License:** MIT + OpenAI/Anthropic Rider

Scope window: [v0.3.0](https://github.com/Dicklesworthstone/frankentui/releases/tag/v0.3.0) (2026-04-12) through HEAD on 2026-08-19 ([`69642d35f56b18a6125ba329b469f7c106d34c59`](https://github.com/Dicklesworthstone/frankentui/commit/69642d35f56b18a6125ba329b469f7c106d34c59)). `Kind` below: GitHub Release vs plain git tag. **v0.4.0 is a tag only** — there is no GitHub Release at that tag. v0.3.1, v0.4.1, and v0.5.0 are published Releases.

## Version Timeline

| Version | Kind | Date | Summary |
|---------|------|------|---------|
| [Unreleased](https://github.com/Dicklesworthstone/frankentui/compare/v0.5.0...main) | HEAD | 2026-08-19 | Color depth, perf-rollout, janitor docs-reorg |
| [v0.5.0](https://github.com/Dicklesworthstone/frankentui/releases/tag/v0.5.0) | Release | 2026-07-05 | OpenTUI-import closeout + alien-governance |
| [v0.4.1](https://github.com/Dicklesworthstone/frankentui/releases/tag/v0.4.1) | Release | 2026-06-13 | Allocation-reduction skill-loop |
| [v0.4.0](https://github.com/Dicklesworthstone/frankentui/tree/v0.4.0) | Tag | 2026-04-24 | Load governor + WebGPU context-loss (no GitHub Release) |
| [v0.3.1](https://github.com/Dicklesworthstone/frankentui/releases/tag/v0.3.1) | Release | 2026-04-12 | crates.io publish-order patch |
| [v0.3.0](https://github.com/Dicklesworthstone/frankentui/releases/tag/v0.3.0) | Release | 2026-04-12 | ftui-a11y + render pipeline (previous changelog ceiling) |

---

## [Unreleased] -- development on `main` since [v0.5.0](https://github.com/Dicklesworthstone/frankentui/releases/tag/v0.5.0) (as of 2026-08-19)

Compare: <https://github.com/Dicklesworthstone/frankentui/compare/v0.5.0...main>

111 non-merge commits after the v0.5.0 tag.

### Janitor docs-reorg (2026-08-19)

- Untracked skill-loop scratch and ignored the workspace pattern ([`ce4b228164f442838438bf65d28c47be80c2a2e3`](https://github.com/Dicklesworthstone/frankentui/commit/ce4b228164f442838438bf65d28c47be80c2a2e3)).
- Relocated remaining root reports: `ACCESSIBILITY.md` → [`docs/ACCESSIBILITY.md`](https://github.com/Dicklesworthstone/frankentui/blob/main/docs/ACCESSIBILITY.md); `UPGRADE_LOG.md` → [`docs/planning/UPGRADE_LOG.md`](https://github.com/Dicklesworthstone/frankentui/blob/main/docs/planning/UPGRADE_LOG.md) ([`69642d35f56b18a6125ba329b469f7c106d34c59`](https://github.com/Dicklesworthstone/frankentui/commit/69642d35f56b18a6125ba329b469f7c106d34c59)).

### Representative commits

- [`69642d35f56b18a6125ba329b469f7c106d34c59`](https://github.com/Dicklesworthstone/frankentui/commit/69642d35f56b18a6125ba329b469f7c106d34c59) — janitor relocate remaining root reports.
- [`ce4b228164f442838438bf65d28c47be80c2a2e3`](https://github.com/Dicklesworthstone/frankentui/commit/ce4b228164f442838438bf65d28c47be80c2a2e3) — untrack skill-loop scratch.

### Color depth as a first-class capability

- Terminal depth is first-class rather than an env-var afterthought ([`ef5efdea137c51ea61fe091688b1bf66b7bf60fd`](https://github.com/Dicklesworthstone/frankentui/commit/ef5efdea137c51ea61fe091688b1bf66b7bf60fd)).
- Capability and trace contracts made truthful ([`7cd1ef0c9a30629c6784cfa10bad756b7a1c22ff`](https://github.com/Dicklesworthstone/frankentui/commit/7cd1ef0c9a30629c6784cfa10bad756b7a1c22ff)).
- WezTerm mux sessions detected in terminal-capability probing ([`20b0d78fa820ee6c1ef802d40fc040aba6ae697c`](https://github.com/Dicklesworthstone/frankentui/commit/20b0d78fa820ee6c1ef802d40fc040aba6ae697c)).

### Render, perf-rollout, and FrankenTermJS

- Certificate-driven render work elimination in the production diff path ([`9ae7f03eaf4d7ac5fb0722de78f19bae89ef78a1`](https://github.com/Dicklesworthstone/frankentui/commit/9ae7f03eaf4d7ac5fb0722de78f19bae89ef78a1)).
- Perf-rollout: executable shadow/canary/fallback/rollback/recovery drills ([`1a0869b453316025f2bd60d8176e12c6ae442d8e`](https://github.com/Dicklesworthstone/frankentui/commit/1a0869b453316025f2bd60d8176e12c6ae442d8e)); promotion scorecard and go/no-go gates ([`5f5b7f82ee16a634a04770aa320dcc4b39b8c6ed`](https://github.com/Dicklesworthstone/frankentui/commit/5f5b7f82ee16a634a04770aa320dcc4b39b8c6ed)).
- FrankenTermJS release-readiness program: scorecard, matrix, rollout, stress campaign, rehearsal ([`d3dcbca004a7bf7bcf69585ff0c5e98ff8387bd5`](https://github.com/Dicklesworthstone/frankentui/commit/d3dcbca004a7bf7bcf69585ff0c5e98ff8387bd5)).
- Workspace crates modernized, including asupersync 0.3.9 ([`2747ec6d9f44e43becc50ddad38361d148cfd077`](https://github.com/Dicklesworthstone/frankentui/commit/2747ec6d9f44e43becc50ddad38361d148cfd077)); parallel rustc front-end (`-Z threads=4`) ([`052f1ecee072110657af3be10455d165d898aa91`](https://github.com/Dicklesworthstone/frankentui/commit/052f1ecee072110657af3be10455d165d898aa91)).

---

## [v0.5.0] -- 2026-07-05 (GitHub Release)

> GitHub Release: <https://github.com/Dicklesworthstone/frankentui/releases/tag/v0.5.0>
> Compare: <https://github.com/Dicklesworthstone/frankentui/compare/v0.4.1...v0.5.0>
> 175 non-merge commits since v0.4.1.

OpenTUI-import epic closeout (bd-3bxhj family): alien-governance formal guarantees, doctor killer-demo contract, flagship example migrations, and render/runtime correctness.

### Alien governance and doctor contracts

- Sequential multiple-testing control with e-BH + alpha-investing ([`7fc0945cdaa809a88178dbdf6898c9fef49d2e0a`](https://github.com/Dicklesworthstone/frankentui/commit/7fc0945cdaa809a88178dbdf6898c9fef49d2e0a)).
- Conformal + e-process + PAC-Bayes formal guarantee layer ([`b51156541162567a76bb95dc884bb23bf4c6033d`](https://github.com/Dicklesworthstone/frankentui/commit/b51156541162567a76bb95dc884bb23bf4c6033d)).
- Doctor: CI-executable killer-demo contract ([`4487e85d761bfe75ffcd386cdc641b80ee380b6e`](https://github.com/Dicklesworthstone/frankentui/commit/4487e85d761bfe75ffcd386cdc641b80ee380b6e)); cross-validator governance diagnostic harness ([`de701ef4615160bdffa4979223a9c88b2c4f1f9a`](https://github.com/Dicklesworthstone/frankentui/commit/de701ef4615160bdffa4979223a9c88b2c4f1f9a)).
- OpenTUI rollout: flagship example migrations with before/after evidence packs ([`36b27cb10b8c7a4251fc43a405041e69c4d767a7`](https://github.com/Dicklesworthstone/frankentui/commit/36b27cb10b8c7a4251fc43a405041e69c4d767a7)).

### Render, input, and capabilities

- Presenter cursor desync at last column + viewport `clear_screen` + wide-cell fill residue ([`49e55c750dd29654eab7357be1e3ad5c59225b4a`](https://github.com/Dicklesworthstone/frankentui/commit/49e55c750dd29654eab7357be1e3ad5c59225b4a)).
- Runtime XTGETTCAP truecolor probe — auto-recover 24-bit color when env detection is degraded ([`9fb84adb4b22750768fe3abc91c9c677ae554048`](https://github.com/Dicklesworthstone/frankentui/commit/9fb84adb4b22750768fe3abc91c9c677ae554048)).
- Workspace bump to 0.5.0 ([`b922db6976e8cc370ab6e3e625f059b005750642`](https://github.com/Dicklesworthstone/frankentui/commit/b922db6976e8cc370ab6e3e625f059b005750642)).

---

## [v0.4.1] -- 2026-06-13 (GitHub Release)

> GitHub Release: <https://github.com/Dicklesworthstone/frankentui/releases/tag/v0.4.1>
> Compare: <https://github.com/Dicklesworthstone/frankentui/compare/v0.4.0...v0.4.1>
> 187 non-merge commits since v0.4.0.

Allocation-reduction skill-loop across markdown, dashboard, table gallery, and extras; load-governor Unsafe state made actually terminal.

- Load-governor `Unsafe` state is terminal ([`9e71e3d67df91a28dac726e60a6b32e6c7e7f4ca`](https://github.com/Dicklesworthstone/frankentui/commit/9e71e3d67df91a28dac726e60a6b32e6c7e7f4ca)); subscription reconcile started/stopped counts corrected ([`b95541769fe8d496036d7d8d9814dd9cf1af31c3`](https://github.com/Dicklesworthstone/frankentui/commit/b95541769fe8d496036d7d8d9814dd9cf1af31c3)).
- Flex constraints stack-inlined with SmallVec ([`1d9a40b7d78c8e5bb2bebbe3b7ca3c9c4b3eba78`](https://github.com/Dicklesworthstone/frankentui/commit/1d9a40b7d78c8e5bb2bebbe3b7ca3c9c4b3eba78)); branch-free sparse-mode pixel setter in extras ([`ae75a8fd8d96a59bd91f8f89726f4f2e7c5d19ab`](https://github.com/Dicklesworthstone/frankentui/commit/ae75a8fd8d96a59bd91f8f89726f4f2e7c5d19ab)).
- Justify spaces via render-hint instead of re-slicing source text ([`04ff79fcb48e67a157a120a3872f2e236ee9a9ef`](https://github.com/Dicklesworthstone/frankentui/commit/04ff79fcb48e67a157a120a3872f2e236ee9a9ef)).
- Workspace bump to 0.4.1 ([`436e917a9d42fa3c1778c9bdf4d25363739cb843`](https://github.com/Dicklesworthstone/frankentui/commit/436e917a9d42fa3c1778c9bdf4d25363739cb843)).

---

## [v0.4.0] -- 2026-04-24 (Tag only — no GitHub Release)

> Git tag `v0.4.0` exists; **no GitHub Release was published for this tag.**
> Compare: <https://github.com/Dicklesworthstone/frankentui/compare/v0.3.1...v0.4.0>
> 34 non-merge commits since v0.3.1.

Load governor, WebGPU context-loss handling, markdown math cache, asupersync 0.3.x.

- `LoadGovernorConfig` for adaptive render degradation ([`54373939f853d1d6547d3b702255989e5104c1eb`](https://github.com/Dicklesworthstone/frankentui/commit/54373939f853d1d6547d3b702255989e5104c1eb)).
- WebGPU context loss + render stall interruptions ([`a2e4c5dd71391315af61d15ad59675968f6bd42f`](https://github.com/Dicklesworthstone/frankentui/commit/a2e4c5dd71391315af61d15ad59675968f6bd42f)).
- Latex-math cache + shared `MarkdownRenderer` across per-frame renders ([`30e138224c6a74be4884511c0b3bfe987f55ae23`](https://github.com/Dicklesworthstone/frankentui/commit/30e138224c6a74be4884511c0b3bfe987f55ae23)).
- Pane resize history coalesced by gesture ([`d5b0e9db6f698729dd68d0fe49bed84366e680f2`](https://github.com/Dicklesworthstone/frankentui/commit/d5b0e9db6f698729dd68d0fe49bed84366e680f2)).
- asupersync 0.2.9 → 0.3.0 → 0.3.1 ([`5f78cfa08016b968b49c16d245234285154917d9`](https://github.com/Dicklesworthstone/frankentui/commit/5f78cfa08016b968b49c16d245234285154917d9)).
- Workspace bump to 0.4.0 ([`c8a0384671d82013d8f4953c31ec1e20fd04ef6a`](https://github.com/Dicklesworthstone/frankentui/commit/c8a0384671d82013d8f4953c31ec1e20fd04ef6a)).

---

## [v0.3.1] -- 2026-04-12 (GitHub Release)

> GitHub Release: <https://github.com/Dicklesworthstone/frankentui/releases/tag/v0.3.1>
> Compare: <https://github.com/Dicklesworthstone/frankentui/compare/v0.3.0...v0.3.1>
> 2 non-merge commits since v0.3.0.

Patch release fixing crates.io publish ordering: removes version specifiers from dev-dependencies on `ftui-harness` in `ftui-layout` and `ftui-runtime` so layered crates.io publish no longer hits circular resolution. All 16 publishable crates publish cleanly in dependency order. Feature changelog remains v0.3.0.

- Remove version specifier from dev-deps on `ftui-harness` ([`1efb7cad2817a413031923950e33960c65c40441`](https://github.com/Dicklesworthstone/frankentui/commit/1efb7cad2817a413031923950e33960c65c40441)).
- Workspace bump to 0.3.1 ([`095c4f90a407650b77a640d915fb2b9468d7f8f3`](https://github.com/Dicklesworthstone/frankentui/commit/095c4f90a407650b77a640d915fb2b9468d7f8f3)).

---

## [0.3.0] (2026-04-12)

> 190 commits since v0.2.1 (2026-03-07).
> Compare: <https://github.com/Dicklesworthstone/frankentui/compare/v0.2.1...v0.3.0>

### Accessibility (ftui-a11y) -- New Crate

- New `ftui-a11y` crate with accessibility tree infrastructure (Phase 1, issue #44): node diffing, tree construction, and role definitions. ([b9952a01](https://github.com/Dicklesworthstone/frankentui/commit/b9952a01826b6592e8b0182a59313bce8c0519f2))
- `Accessible` trait implemented for 9 core widgets: Table, List, TextInput, Tabs, Tree (initial five), plus Block, Paragraph, Scrollbar, and Spinner. ([756d42bc](https://github.com/Dicklesworthstone/frankentui/commit/756d42bc35c20970823afb31bba683e22a61d850), [0d044784](https://github.com/Dicklesworthstone/frankentui/commit/0d044784c2cbf470005a1840030b6f6763ea1e82))
- Accessibility proxy layer added to demo showcase with ACCESSIBILITY.md documentation. ([b3f12877](https://github.com/Dicklesworthstone/frankentui/commit/b3f12877b47dd48535e4ff8bddf7181ede695148))
- Diff detection for description, shortcut, and parent changes. ([5a9d9dd9](https://github.com/Dicklesworthstone/frankentui/commit/5a9d9dd95824b03039dcb71b12f3dffc1ae60922))
- Grapheme-aware title fitting, scissor-respecting styles, and soft-wrap page nav with a11y hardening. ([63188cb6](https://github.com/Dicklesworthstone/frankentui/commit/63188cb6f9f205fc2f25e5005ba04ea4fa943e80))

### Render Pipeline Performance

- Hand-rolled byte-buffer ANSI emission replaces `format!`/`write!` in the presenter hot path. ([3e298b64](https://github.com/Dicklesworthstone/frankentui/commit/3e298b64890ad9a0659c7f9e0718fe9b0afbe7a4))
- Span-aware tile diff builder skips clean rows. ([870d24f1](https://github.com/Dicklesworthstone/frankentui/commit/870d24f141b96aa008fdb765fdb6321cf59f4512))
- Hyperlink bookkeeping skipped when no links are active or registered. ([7a910893](https://github.com/Dicklesworthstone/frankentui/commit/7a91089366bd4644e086d5a422cb76b052e3de17))
- `PreparedContent` enum introduced for optimized presenter dispatch. ([59d7709e](https://github.com/Dicklesworthstone/frankentui/commit/59d7709ea88050e0b3e3f2b242a94697e6275efe))
- Quad-cell diff skip, same-row CUP elimination, ASCII `PreparedContent` fast path, and cached hyperlink policy. ([10c341f5](https://github.com/Dicklesworthstone/frankentui/commit/10c341f5e4166eece025101428be741b9ffff0a2))
- `row_cells_mut_span()` for bulk mutable row access with dirty tracking. ([be300dda](https://github.com/Dicklesworthstone/frankentui/commit/be300dda659a706cc762735e9b2ceaf6d1b175ca))
- Thread-local paragraph metrics and wrap caching in widgets. ([e1fb48d3](https://github.com/Dicklesworthstone/frankentui/commit/e1fb48d31355edbc8593868fdf63f6832b70e207))

### Render Gauntlet and Certificate-Based Diff Elision

- `DiffSkipHint` for certificate-based diff elision in the render pipeline. ([e99c4448](https://github.com/Dicklesworthstone/frankentui/commit/e99c444805bf5ff07472bac0f62d7f0527861ea3))
- Render gauntlet framework with equivalence modules in the harness. ([c5332777](https://github.com/Dicklesworthstone/frankentui/commit/c5332777f9b865a7b079ffa7c872fd94848b54da))
- Fixture runner and doctor cost profiling modules. ([ba46183d](https://github.com/Dicklesworthstone/frankentui/commit/ba46183d13a1a9ab2331e9a95656955fe3dfb57d))
- Baseline capture and hotspot extraction modules. ([0eee1e19](https://github.com/Dicklesworthstone/frankentui/commit/0eee1e192b98d102c2258ac97ee30c49d2d19270))

### Asupersync Integration

- ADR-010 for targeted Asupersync adoption in runtime shell and doctor. ([dbfad8c1](https://github.com/Dicklesworthstone/frankentui/commit/dbfad8c16184d4bb24d22b0190af03765a2c1c9b))
- Asupersync-executor backend for blocking task execution in the runtime. ([2c3d6b17](https://github.com/Dicklesworthstone/frankentui/commit/2c3d6b1711311eb6c3b48f82cd48d66b47efb1bc))
- `TaskExecutor` seam extracted; doctor summary artifact persisted. ([6d0f1c2e](https://github.com/Dicklesworthstone/frankentui/commit/6d0f1c2e1f3b954e4db2f841c191bbcbe6309f6a))
- Panic-resilient task executor with backpressure evidence. ([24ab6918](https://github.com/Dicklesworthstone/frankentui/commit/24ab6918584f512ee18cee832b22a2d49df6ff32))
- Seam inventory spec and invariant/metrics/evidence documentation. ([04a1c021](https://github.com/Dicklesworthstone/frankentui/commit/04a1c0217c6b8d464d03edc6333cdf476c4265e1), [2e40451d](https://github.com/Dicklesworthstone/frankentui/commit/2e40451d6d805a2e72e24364a6a72d3f1eaa434f))
- Updated asupersync dependency to 0.2.9. ([20a71939](https://github.com/Dicklesworthstone/frankentui/commit/20a719394bcb442d14bef2e408c621e88bdeb1a2))

### Doctor Diagnostics Expansion

- Tmux observe mode for app smoke fallback. ([8462bb28](https://github.com/Dicklesworthstone/frankentui/commit/8462bb2847722cf96e1f7df85fc5cf24c9d54e21))
- Enhanced diagnostic capture, reporting, and test suite. ([533a55ca](https://github.com/Dicklesworthstone/frankentui/commit/533a55cab8f43c24ffef46ff8a93622b25f28729))
- Trace, fallback, and capture error profile tracking in reports. ([ad189fd6](https://github.com/Dicklesworthstone/frankentui/commit/ad189fd6bbd30560d98160d14a61235b8c289a39))
- Orchestration workflow documentation and profiling scripts. ([f9f1e4e2](https://github.com/Dicklesworthstone/frankentui/commit/f9f1e4e2d5826435097aab197d2a50b2457bd888))
- MCP tool-level `isError` response detection and default seed agent rename. ([54acfc59](https://github.com/Dicklesworthstone/frankentui/commit/54acfc59b6fe30178f97e1f0b74036554d946834))
- `fallback_error` surfaced in diagnostic summary. ([6df2e589](https://github.com/Dicklesworthstone/frankentui/commit/6df2e589af357228ad0bfc971daf33f6c56c6e0d))

### Runtime and Effect System

- Effect executor, lab harness, and subscription engine expansion (+1184 lines). ([4f3fe05f](https://github.com/Dicklesworthstone/frankentui/commit/4f3fe05f5cc0116a045be1ee0a9831941767bc75))
- Subscription engine and lifecycle contract tests (+993 lines). ([014ee3a9](https://github.com/Dicklesworthstone/frankentui/commit/014ee3a9a223da82c8927d15476e50b3d3e090e8))
- Rollout scorecard, seed RPC expansion, and program lifecycle improvements. ([816060bb](https://github.com/Dicklesworthstone/frankentui/commit/816060bb37180a382db969e3d47ce60c2683acfd), [19d7f04b](https://github.com/Dicklesworthstone/frankentui/commit/19d7f04b4aa029da8af149c65990a2dc7092518e))
- Rollout runbook module and failure E2E script (+475 lines). ([cd9005ab](https://github.com/Dicklesworthstone/frankentui/commit/cd9005ab784512108fe74b46ef0319545f9ad2d1))
- Artifact manifest, doctor topology, failure signatures, and proof oracle modules. ([2cfce9d4](https://github.com/Dicklesworthstone/frankentui/commit/2cfce9d4faea1bc06e4e69f210715f7e8c626f00))
- Telemetry schema module and subscription engine expansion. ([624db88c](https://github.com/Dicklesworthstone/frankentui/commit/624db88c75f54bfe065ecf4ea8e79c85fda3cc98))
- Go/no-go scorecard test for runtime readiness. ([9c378c17](https://github.com/Dicklesworthstone/frankentui/commit/9c378c17605515e249d7aa4756401d6a08ac1e54))
- Terminal writer capabilities extended. ([2b6aa236](https://github.com/Dicklesworthstone/frankentui/commit/2b6aa236ec957d127c69757c788505f9ed6373d9))
- Effect queue drained on shutdown; suite manifest enriched with report paths. ([ce686be6](https://github.com/Dicklesworthstone/frankentui/commit/ce686be60b7fc8039c6d0438044bdcc0304badac))
- Major README refresh expanding effect system and subscription engine (+1035 lines). ([bd377ca4](https://github.com/Dicklesworthstone/frankentui/commit/bd377ca4a9d0d9683feef221b2df590e43f918c6))

### Layout and Pane Profiling

- Pane profiling benchmark harness and scripts. ([9595eb1b](https://github.com/Dicklesworthstone/frankentui/commit/9595eb1b80957549f75ccee986d9adc207430194))
- Pane layout engine expansion with Asupersync seam inventory. ([3aa074ad](https://github.com/Dicklesworthstone/frankentui/commit/3aa074ad61424f6e8f462619bc2bbcc63d931bf2))
- Pane layout algorithms and capture diagnostics. ([d2791608](https://github.com/Dicklesworthstone/frankentui/commit/d2791608567646093d7023c6dcf0db451d41d004))
- Validation pipeline E2E, benchmarks, and profiling (+665 lines). ([f18cec4e](https://github.com/Dicklesworthstone/frankentui/commit/f18cec4eb24c2b14022b65977bd2e32b8f84dbdf))
- Pane terminal benchmarks and profiling support. ([475f3dbd](https://github.com/Dicklesworthstone/frankentui/commit/475f3dbdf12b450d0a9d1e429c9116bb8efd47c2))

### Web Backend

- Pane pointer capture state tracking corrected. ([b1ee6e2f](https://github.com/Dicklesworthstone/frankentui/commit/b1ee6e2f35a6fb2058e76e744a9753bc54f5f1ad))
- Pane pointer benchmark and refined tests. ([3c48d347](https://github.com/Dicklesworthstone/frankentui/commit/3c48d3475b3d1793b0276509fe776a0fe565997d))
- Demo routed to crossterm-compat backend on non-Unix hosts. ([73821127](https://github.com/Dicklesworthstone/frankentui/commit/738211276539b1061d23466056ec336188b4dc61))

### Fixes

- Evidence sink capped at 50 MiB to prevent unbounded disk growth. ([c673643c](https://github.com/Dicklesworthstone/frankentui/commit/c673643c88175b57bf664bf6b80d2cf69b973559))
- Post-quit subscription reconciliation and event draining guarded. ([69484709](https://github.com/Dicklesworthstone/frankentui/commit/69484709e9b8d21cc97aa779af858a2c9d466597))
- Post-shutdown task rejection with decay-dirtied state persistence. ([d7856692](https://github.com/Dicklesworthstone/frankentui/commit/d78566921db4f290bf84958fda51b381e22ce4df))
- SGR sub-parameter flattening for colon-separated sequences (previously discarded). ([a48d33ee](https://github.com/Dicklesworthstone/frankentui/commit/a48d33eec5498e41b06884ad80b85b58cd1ec64c))
- Pipeline metrics helper extracted; wrap logic skipped for `WrapMode::None`. ([5451ec3d](https://github.com/Dicklesworthstone/frankentui/commit/5451ec3d2fb7f7e55cd2760dffbf9ad8cc23edf7))
- Artifact paths resolved relative to run dir. ([031b9f3f](https://github.com/Dicklesworthstone/frankentui/commit/031b9f3fea66bb0d08d575c944d6b6e7d80765af))
- Program lifecycle state transitions corrected. ([3b80e02a](https://github.com/Dicklesworthstone/frankentui/commit/3b80e02ad3b9d4301a830a60f6f91c8fc7678a4f))
- Log compaction uses `split_off` instead of `drain`. ([5499d598](https://github.com/Dicklesworthstone/frankentui/commit/5499d5984aef0c10414b9ab03d1f7ed491870ee3))
- Tile builder test uses invariant-based assertions. ([f612df2b](https://github.com/Dicklesworthstone/frankentui/commit/f612df2b9346e3001a854c89ef017e91edd9cf5d))

### Refactoring

- Inner-match if-guards converted to match-arm guards across crates. ([d528fd2b](https://github.com/Dicklesworthstone/frankentui/commit/d528fd2bee522a5829e786b1b75458bcb69c22bf))
- Shared comparator extracted for widget sorting. ([9dbdd237](https://github.com/Dicklesworthstone/frankentui/commit/9dbdd237ecae1a298f111a6fb76a115aaa4734c4))
- Doctor/suite path structs extracted to decouple report artifact resolution. ([6145e2a6](https://github.com/Dicklesworthstone/frankentui/commit/6145e2a6e596b7fea25898b5c70c59daf86d1f1c))
- Pipeline render mode added to profile_sweep and demo_pipeline_bench. ([689ea823](https://github.com/Dicklesworthstone/frankentui/commit/689ea82380024189f379ced823150a6dc256ccf9))

---

## [v0.2.1] -- 2026-03-07 (GitHub Release)

> GitHub Release: <https://github.com/Dicklesworthstone/frankentui/releases/tag/v0.2.1>
> Published to crates.io as `ftui 0.2.1`.
> Compare: <https://github.com/Dicklesworthstone/frankentui/compare/v0.2.0...v0.2.1>

This release spans 373 commits from the v0.2.0 tag (2026-02-15) through the v0.2.1 tag (2026-03-07). It introduced the doctor diagnostic crate, the OpenTUI migration pipeline, massive widget and testing expansion, formal observability infrastructure, and extensive correctness hardening.

### doctor_frankentui -- New Crate

- New `doctor_frankentui` crate for automated health-check, capture pipeline, suite reporting, and diagnostic coverage gating. ([dd31dd92](https://github.com/Dicklesworthstone/frankentui/commit/dd31dd92c57caab47b4e225edc2bac28a7e9ff6a), [b952e774](https://github.com/Dicklesworthstone/frankentui/commit/b952e774dd3ff77c479591c85e5bfbc7682fd37b))
- Streaming VHS fatal detection, defunct ttyd reaping, and smoke timeout cap. ([76262862](https://github.com/Dicklesworthstone/frankentui/commit/76262862e2e825612fc7274154b5f114e24544ec))
- Mapping atlas v2 with abstract interpretation and policy-config. ([78b29e86](https://github.com/Dicklesworthstone/frankentui/commit/78b29e865575ee96045f27c378ae231e018dc861))
- Mapping atlas v3 with process spawn fallback and planner context. ([aab06892](https://github.com/Dicklesworthstone/frankentui/commit/aab06892e86003371ae0c11405dc2934425ad19f))
- OpenTUI import pipeline and semantic contract infrastructure. ([66fb6a7b](https://github.com/Dicklesworthstone/frankentui/commit/66fb6a7b3343531eb7af2ee7dcf2fdc93b7a40a2))
- OpenTUI import contracts, evidence manifest, and Bayesian confidence model. ([8f8efeba](https://github.com/Dicklesworthstone/frankentui/commit/8f8efeba7a479dec0ed826627116748c8eeadb42))
- Sandbox enforcement for untrusted project analysis. ([c9983c6e](https://github.com/Dicklesworthstone/frankentui/commit/c9983c6eeed527e73c06b9d7de4dee3bb1666813))
- Secret detection and artifact redaction pipeline. ([c96a316f](https://github.com/Dicklesworthstone/frankentui/commit/c96a316ffbc99f72ed9dd9a976d25c1aa8f8e63a))
- VHS, no-fake gate, and structured artifact map in CI pipeline. ([8668505e](https://github.com/Dicklesworthstone/frankentui/commit/8668505e18f7c15acd87ca6a689b46710eeec97a))

### OpenTUI Migration Pipeline

- TSX/JSX parser pipeline with typed AST and symbol table. ([c03aa363](https://github.com/Dicklesworthstone/frankentui/commit/c03aa3639a7bab44e2a3a5ebe1a9dc6ccddd0c22))
- Module graph construction and migration entrypoint detection. ([dc822cbb](https://github.com/Dicklesworthstone/frankentui/commit/dc822cbbf4ce608c6d9c00a5eb4a50fc2ce55fd4))
- UI composition semantics extraction from parsed TSX. ([67c2e0af](https://github.com/Dicklesworthstone/frankentui/commit/67c2e0afade07ab56c4a3825ee82dce00709814b))
- Canonical migration IR schema and invariants. ([1df76d03](https://github.com/Dicklesworthstone/frankentui/commit/1df76d038abc0ae7d4bd82c0db3ac0b9bcbe04ef))
- IR-to-FrankenTUI construct mapping atlas. ([87e8d645](https://github.com/Dicklesworthstone/frankentui/commit/87e8d645820a2fe99d4fbdc2aedebe853e491a6f))
- Translation planner with confidence-ranked strategies. ([2a2532a6](https://github.com/Dicklesworthstone/frankentui/commit/2a2532a6689cd5539c1be756dae15bcbebf02d73))
- State/event semantics translator for ftui-runtime Model/update/subscriptions. ([de8a9a9a](https://github.com/Dicklesworthstone/frankentui/commit/de8a9a9aea8950413c38c5540b7658f58c56357b))
- View/layout translator for ftui-layout and widget composition. ([17d99164](https://github.com/Dicklesworthstone/frankentui/commit/17d99164ca4c82d5b209712d101a5031113da70e))
- Style/theme translation with accessibility-safe upgrades. ([f524bf58](https://github.com/Dicklesworthstone/frankentui/commit/f524bf581e32a749dfb31122bfdbcfce944bd075))
- Effect/async translation to Cmd/subscription orchestration. ([5e5723cd](https://github.com/Dicklesworthstone/frankentui/commit/5e5723cd373e9eb27972a401cc195fa5a052419a))
- Code emission backend, module partitioner, and project scaffolder. ([f061d609](https://github.com/Dicklesworthstone/frankentui/commit/f061d609a2f699dfdec8fb96c01260e7a1186fc3))
- Codegen optimization passes and readability passes. ([7581cdaf](https://github.com/Dicklesworthstone/frankentui/commit/7581cdaff9f4a71b7702ccceec02a776f751d22f), [b6defa18](https://github.com/Dicklesworthstone/frankentui/commit/b6defa18bd2a98090ed87c11c256ba69cf2d5a54))
- Curated OpenTUI corpus acquisition pipeline and fixture taxonomy. ([d932a002](https://github.com/Dicklesworthstone/frankentui/commit/d932a002db380602467fcce081964877bccaa7fe), [a4366683](https://github.com/Dicklesworthstone/frankentui/commit/a4366683a4433062999705cd922ebf495fbbace4))
- Synthetic adversarial fixture generator. ([1338df49](https://github.com/Dicklesworthstone/frankentui/commit/1338df49ac3921b06c7c4b9b19923c52a80916d2))
- Reference execution harness for baseline behavior capture. ([e0a277ef](https://github.com/Dicklesworthstone/frankentui/commit/e0a277eff3f4cad229964750679a1612997a542b))
- Interaction trace capture and replay format. ([b2e473c6](https://github.com/Dicklesworthstone/frankentui/commit/b2e473c60d6a5f3440d180655e976b75c060f45d))

### Widget Expansion

- Galaxy-brain decision card widget with 4-level progressive disclosure transparency layer. ([03427231](https://github.com/Dicklesworthstone/frankentui/commit/034272314d7ad36b13e4e44705d93a9fccb9c5d5), [121a7b66](https://github.com/Dicklesworthstone/frankentui/commit/121a7b665deaae9793552274cb8819bea7b4c34a))
- Galaxy-brain transparency FrankenLab scenario in the demo. ([7c400e2f](https://github.com/Dicklesworthstone/frankentui/commit/7c400e2f74221be3a4f60f8c2d3951f1e2daf0c0))
- Adaptive radix tree widget with expanded E2E conformance coverage. ([de388072](https://github.com/Dicklesworthstone/frankentui/commit/de38807281cd9a1823e014639bae32c5f84dd188))
- Choreographic programming for multi-widget interactions. ([13b7dd24](https://github.com/Dicklesworthstone/frankentui/commit/13b7dd2448e63cc9e9322fb694384b8d79350427))
- Popover widget for anchored floating content. ([e54f496f](https://github.com/Dicklesworthstone/frankentui/commit/e54f496f8a0bc02c9a6999eac8eade5840ed7023))
- CSS-like InteractiveStyle, OverflowBehavior, and WrapMode::Optimal for migration parity. ([c6145e0e](https://github.com/Dicklesworthstone/frankentui/commit/c6145e0ec7b2a79d3fc36ce899749be1db53f820))
- Table widget stateful persistence layer with comprehensive rendering tests. ([e17a49de](https://github.com/Dicklesworthstone/frankentui/commit/e17a49dec2944b6f6e9cd7608d030338243821ca))
- Ctrl+Delete word-forward deletion in editor/textarea. ([a4be0712](https://github.com/Dicklesworthstone/frankentui/commit/a4be0712f06692c37e6b45748f07b11c87532102))
- Tree widget node expansion and collapse handling improvements. ([29f59a43](https://github.com/Dicklesworthstone/frankentui/commit/29f59a438a9b9111cf390d75bb418f1355f6d427))

### Observability and Policy Infrastructure

- Prometheus-compatible metrics registry. ([e0a7eda8](https://github.com/Dicklesworthstone/frankentui/commit/e0a7eda86f4907f9155c8ba576329e15ca1e8e80))
- `slo.yaml` with data-plane and decision-plane budgets. ([12d9e4b5](https://github.com/Dicklesworthstone/frankentui/commit/12d9e4b5becacac1e8daed51d492f2dab4d02a9c))
- `demo.yaml` with 5 reproducible 60-second demos and E2E execution claim verification. ([983ae440](https://github.com/Dicklesworthstone/frankentui/commit/983ae440d48b452056771c093e7b21260b816183), [993096f9](https://github.com/Dicklesworthstone/frankentui/commit/993096f9e5f8d779d6358cc412f283345fa31a0e))
- Policy-as-Data progressive delivery and fallback with controller integration tests. ([bfec6a9a](https://github.com/Dicklesworthstone/frankentui/commit/bfec6a9a4deaaba23565649825efc2810ee7fe62), [84273439](https://github.com/Dicklesworthstone/frankentui/commit/8427343910b04293e918e4a0c65fca72b885af17))
- SOS barrier certificates, CEGIS synthesis, e-graph optimizer, quotient filter, and drift visualization. ([b48eab99](https://github.com/Dicklesworthstone/frankentui/commit/b48eab9941f5dbedb86400c357f452e3a87c3758))
- JSONL diagnostic logging consolidated around shared event builder + sink abstraction (#32). ([43c7d5d2](https://github.com/Dicklesworthstone/frankentui/commit/43c7d5d217f61b538abca35a978f688a2d36d6c8))
- Reusable diagnostic hook/log substrate extracted into `ftui-widgets::diagnostics` (#33). ([04e0146d](https://github.com/Dicklesworthstone/frankentui/commit/04e0146d8e96ff3e385b28cdaa6c57000bb16a45))
- `CellAttrs::merged_flags()` for composing style flags. ([05b02705](https://github.com/Dicklesworthstone/frankentui/commit/05b027052dcc3aca9cc243b4cce99aa079ceb2f2))
- Stable slugs added to ScreenMeta with hotkey conflict fix (#31). ([cc0f1d22](https://github.com/Dicklesworthstone/frankentui/commit/cc0f1d22694e438c5c0e1013f4c300f2cf4ab731))

### Terminal and IME

- IME event model (`ImeEvent`, `ImePhase`) added to ftui-core. ([2e93c7f2](https://github.com/Dicklesworthstone/frankentui/commit/2e93c7f2))
- Native IME event pipeline for web, replacing composition-as-paste hack. ([e6cdfed4](https://github.com/Dicklesworthstone/frankentui/commit/e6cdfed4))
- IME composition wired into TextInput; host-focus lifecycle added to FocusManager. ([681aecd0](https://github.com/Dicklesworthstone/frankentui/commit/681aecd0))
- Mouse protocol compatibility hardening, WezTerm mux detection, and legacy mouse parser. ([0af52aa4](https://github.com/Dicklesworthstone/frankentui/commit/0af52aa4))
- C1 control code support for CSI/SS3/OSC sequences. ([e582aad9](https://github.com/Dicklesworthstone/frankentui/commit/e582aad9))

### Text and Typography

- Incremental Knuth-Plass line-break optimizer. ([70f567c9](https://github.com/Dicklesworthstone/frankentui/commit/70f567c9a97cdfb73728de38bfdf316a798645c3))
- Leading/baseline-grid and paragraph spacing system. ([3a564479](https://github.com/Dicklesworthstone/frankentui/commit/3a56447962a7568cb1e3079c4b8ec625f336899d))
- Microtypographic justification controls. ([e27612b3](https://github.com/Dicklesworthstone/frankentui/commit/e27612b3b0d9faab548c00fd74f42255bd131b6a))
- Layout policy presets and deterministic fallback contract. ([33b28404](https://github.com/Dicklesworthstone/frankentui/commit/33b2840410ab8726713d3f5327f266d5803bf6ce))
- Text shaping and script segmentation modules. ([6b86d038](https://github.com/Dicklesworthstone/frankentui/commit/6b86d0386bcff294fbdf0aae3e86166cbdf99410))
- Ligature policy system. ([e6d1ba73](https://github.com/Dicklesworthstone/frankentui/commit/e6d1ba737fd5abf9280a01da77274d79fdc7e6fd))
- Tier budget system with Emergency layout tier. ([f3e0f7bc](https://github.com/Dicklesworthstone/frankentui/commit/f3e0f7bca15c399a215196812928705dab0b5882))
- BiDi text handling, table rendering, and layout dep graph improvements. ([0662230f](https://github.com/Dicklesworthstone/frankentui/commit/0662230f9baf35191d82fc61382266a725471b13))

### Layout and Panes

- Cache-oblivious van Emde Boas tree layout for widget traversal. ([03b4a8eb](https://github.com/Dicklesworthstone/frankentui/commit/03b4a8eb08078348b5532e6a467fe8fc47423d23))
- Dependency graph, incremental engine, and enhanced pane physics. ([cdf6590e](https://github.com/Dicklesworthstone/frankentui/commit/cdf6590ec6e58b3bab5a086f294a7a6a9ad2cfdc))
- SmallVec<[T; 8]> in layout solver replacing Vec allocations. ([e960693d](https://github.com/Dicklesworthstone/frankentui/commit/e960693db11851874dbf8a40b904f914e443a6cb))
- FrameArena API extended and adopted in TextInput/Dashboard. ([e2e4ba37](https://github.com/Dicklesworthstone/frankentui/commit/e2e4ba3761004ef629cd6cb822567365505bdfcc))

### Runtime

- Graceful signal shutdown, schema compat layer, policy discovery, and buffer safety with compile-fail tests. ([76176973](https://github.com/Dicklesworthstone/frankentui/commit/76176973546ad8d4d742d068e5dec81321b584eb))
- Bayesian decision core with expected-loss minimization. ([d36dbeae](https://github.com/Dicklesworthstone/frankentui/commit/d36dbeaed8e1b76c2cd4bbed01e8c2100e611655))
- Predictive screen tick system with persistence. ([1cd5a999](https://github.com/Dicklesworthstone/frankentui/commit/1cd5a999e0a945195c21d29a0598178f6667d02f))
- Immediate-drain config, stats, and spin-storm prevention. ([18facd3f](https://github.com/Dicklesworthstone/frankentui/commit/18facd3f2a2c51b8b06ff589e6b905e6a63f5a96))
- Configurable degradation floor to prevent content-suppressing quality levels. ([e507218d](https://github.com/Dicklesworthstone/frankentui/commit/e507218de717092746abd48d8c4ee78438dba270))
- Backend capability probing and fallback resolution for terminal features. ([d75c8283](https://github.com/Dicklesworthstone/frankentui/commit/d75c8283d0edca1c0f36b68b29c3ea7009a8e217))
- Conformal frame guard, degradation cascade, and integration tests. ([12f6a98a](https://github.com/Dicklesworthstone/frankentui/commit/12f6a98a6569f6f79053d81eb60c3a31aa90c2d3))

### Deterministic Replay

- EventRecorder and EventReplayer for deterministic replay. ([e37be06c](https://github.com/Dicklesworthstone/frankentui/commit/e37be06c113701f579c3a2dda06fa2cb5997da00))
- Golden frame comparison for replay verification. ([674c3cc1](https://github.com/Dicklesworthstone/frankentui/commit/674c3cc1662b92a83ce1f56b4c9cb7fc44a323c2))
- Evidence ledger capture during replay. ([b52cdfef](https://github.com/Dicklesworthstone/frankentui/commit/b52cdfef5bc854b1d80c4e065cc173b852c3843b))
- E2E test suite for Recipe D Deterministic Debugging. ([181e8a9c](https://github.com/Dicklesworthstone/frankentui/commit/181e8a9c5e012969dfa89559ec0d51d3dff69696))

### Security

- Threat model, HTTPS-only defaults, and resource bound hardening. ([3125afab](https://github.com/Dicklesworthstone/frankentui/commit/3125afabb2873afffbdb7f4e1dc2aa20f79c2475))
- Clipboard policy API, link audit trail, and wide character handling. ([cddcc986](https://github.com/Dicklesworthstone/frankentui/commit/cddcc986803967aa96845945cdb8f8c4e6a34b2f))

### Shared Capabilities and Rendering

- SharedCapabilities with ArcSwap-backed concurrent access. ([d70e44cd](https://github.com/Dicklesworthstone/frankentui/commit/d70e44cdd6c43daa9343e83a1f891055dc2247a3))
- SharedResolvedTheme with ArcSwap-backed concurrent access. ([f39037b5](https://github.com/Dicklesworthstone/frankentui/commit/f39037b58737ce99d58100f6b4f22fb229cabd01))
- Frame guardrails for memory budget and queue depth. ([e3a2d4fb](https://github.com/Dicklesworthstone/frankentui/commit/e3a2d4fb16956f22cce5862b3536235e4f195818))
- Deterministic fit-to-container and font metric lifecycle. ([0d5d1c47](https://github.com/Dicklesworthstone/frankentui/commit/0d5d1c471a0e2313d5be786a03959a92c8741346))
- Selection copy/rect extraction, AHashMap migration, and render pipeline optimization. ([af044fa7](https://github.com/Dicklesworthstone/frankentui/commit/af044fa7fcf40b6ec06353171ea12c4645f89847))
- SelectionGestureController for pointer/keyboard gestures. ([792a4543](https://github.com/Dicklesworthstone/frankentui/commit/792a4543bbec86423c15705f721283f4c43f7334))

### Web/WASM

- Interactive pane studio with drag/dock/timeline and engine-backed selection copy. ([720f201d](https://github.com/Dicklesworthstone/frankentui/commit/720f201d))
- Event subscriptions, OSC 9;4 progress signals, and pane gesture extraction for frankenterm-web. ([c2320f0a](https://github.com/Dicklesworthstone/frankentui/commit/c2320f0a))
- Attach state machine hardened against mid-backoff transport closes. ([02bfdf26](https://github.com/Dicklesworthstone/frankentui/commit/02bfdf26))
- WASM gesture handling, text search, layout alignment, and virtualized list rendering. ([f42e6866](https://github.com/Dicklesworthstone/frankentui/commit/f42e6866))

### Testing and CI

- Terminal emulator compatibility matrix CI with docs. ([717f2c91](https://github.com/Dicklesworthstone/frankentui/commit/717f2c91ec0c06d9c5994058acb8a89944cd32a1))
- Shadow-mode Ratatui comparison across 10 scenarios. ([ecb5602b](https://github.com/Dicklesworthstone/frankentui/commit/ecb5602bff824a03272e45853aa92c06c3c52db9))
- Conformance matrix E2E across 5 terminal emulator profiles. ([815119a3](https://github.com/Dicklesworthstone/frankentui/commit/815119a395c5e35a162b3540c608dba02a901f9d))
- Full widget gallery E2E test across 5 terminal configs. ([06cf6078](https://github.com/Dicklesworthstone/frankentui/commit/06cf60783c9cbd20bd628d1029d5829fd4c139d8))
- Session-type mode transition E2E with JSONL logging. ([3bdd9d21](https://github.com/Dicklesworthstone/frankentui/commit/3bdd9d21832bb0f37d554b60ce2f267d907b1949))
- Roaring vs Vec<bool> dirty tracking equivalence test and benchmark. ([6e7e3a05](https://github.com/Dicklesworthstone/frankentui/commit/6e7e3a0574ab7f54d0d6254d440096264341689a), [28b90ded](https://github.com/Dicklesworthstone/frankentui/commit/28b90ded11fc76fae5e823c31c69f95410bb7cdd))
- Layout and widget fuzz targets; Rect::union commutativity bug fixed. ([d6582ce0](https://github.com/Dicklesworthstone/frankentui/commit/d6582ce00a3f822e83bdd7ef63543fc20ce2c559))
- E2E fuzz campaign validation script with JSONL logging. ([1ffffbbc](https://github.com/Dicklesworthstone/frankentui/commit/1ffffbbca2cab2975254bad6c6f0d0f0e8381d70))
- Tree widget Left/Right key navigation tests. ([ce1bf1a4](https://github.com/Dicklesworthstone/frankentui/commit/ce1bf1a4c1e0a40d920609e714057b73bb24d8a3))
- List widget selection and filter interaction tests. ([9c26cfa6](https://github.com/Dicklesworthstone/frankentui/commit/9c26cfa6bb97bcdc742cdaa0101e29a77c988357))
- Tabs widget switching, overflow, and interaction tests. ([3dee6926](https://github.com/Dicklesworthstone/frankentui/commit/3dee692685ff90e0c99974b26f2793b03bdd39a9))
- Accessibility roles and keyboard navigation tests. ([bd593f94](https://github.com/Dicklesworthstone/frankentui/commit/bd593f9406807eb86481647c17d8b618ea197a59))
- Standalone widget crate isolation tests. ([456a885d](https://github.com/Dicklesworthstone/frankentui/commit/456a885de08c360c25a3fe1269e66f217e1a5167))
- 216 test snapshots regenerated after correctness and theme fixes. ([45f5c59e](https://github.com/Dicklesworthstone/frankentui/commit/45f5c59ec0033ffd6a950d12a5169e99eecdf74b))

### Correctness and Hardening

- NaN-safe f64 clamping across animation, budget, and widget subsystems. ([da8cb314](https://github.com/Dicklesworthstone/frankentui/commit/da8cb314aef08cd43d0ffe09dd299d16928c7643), [b346386e](https://github.com/Dicklesworthstone/frankentui/commit/b346386e38afbacb3c953be69b43f09348dac817), [927694e9](https://github.com/Dicklesworthstone/frankentui/commit/927694e989f26354953174d7e3d77ec5cbb75ab5))
- Grapheme pool generation check for stale ID detection. ([6cc8e092](https://github.com/Dicklesworthstone/frankentui/commit/6cc8e092d519af6dbe59b85b4d1b9e743e0086ab))
- RAII ReentrancyGuard for two-way binding sync flag. ([911e8e17](https://github.com/Dicklesworthstone/frankentui/commit/911e8e17aad223b236182653db9e7ba4fc16b984))
- Focus trap deadlock, sentinel leak, and shutdown hang prevention. ([80f39dc9](https://github.com/Dicklesworthstone/frankentui/commit/80f39dc92ae68f7b9ae9beb9b9cdbf9b2f5fcdc0))
- Tour panic on empty layout areas guarded with zero-size `RelativeRect::resolve`. ([97621dda](https://github.com/Dicklesworthstone/frankentui/commit/97621dda22682fcf832e06ff55942d01d8764a23))
- Hyperlink registry reset to initial state on terminal reset. ([7f1dd716](https://github.com/Dicklesworthstone/frankentui/commit/7f1dd71641428102d3a12f38419b6b07b6fce92a))
- Stale hyperlink metadata cleared when painting over cells. ([87fc5e94](https://github.com/Dicklesworthstone/frankentui/commit/87fc5e942f7792b00d1bc0687d228fda46dfc466))
- Narrow-layout confirm dialog with translucent background compositing. ([5bd1a5ba](https://github.com/Dicklesworthstone/frankentui/commit/5bd1a5ba53c600cdb7d511b034f4e997133462c2))
- Confirm dialog button rendering clamped for safety. ([e5361579](https://github.com/Dicklesworthstone/frankentui/commit/e5361579007d9bc8b6bb5a0dc52c5ac8825d6e92))
- JSON escaping correctness, O(1) diagnostic log eviction, and form style preservation. ([7b198264](https://github.com/Dicklesworthstone/frankentui/commit/7b198264f5d5ef2b60013154617d9f774fc04594))
- Style-merging fix applied to remaining `apply_style` copies. ([e3d730d8](https://github.com/Dicklesworthstone/frankentui/commit/e3d730d857c21f7fe717aabbd8fd6b82dd5c92b0))
- File picker root confinement hardened against symlink escapes. ([c819b81b](https://github.com/Dicklesworthstone/frankentui/commit/c819b81b6b8e6bfde4c35557ec9f13e68a3d1456))
- Mermaid journey/timeline section parsing `unwrap()` calls removed. ([b412f4b1](https://github.com/Dicklesworthstone/frankentui/commit/b412f4b1377299feaeb455885842530345b9be7d))
- Dirty state preserved when render/present is skipped by budget. ([adab58cd](https://github.com/Dicklesworthstone/frankentui/commit/adab58cd960be15d4146be60e0b03d8a5e83da71))
- First resize event handled in coalescer without panicking. ([0cafef6a](https://github.com/Dicklesworthstone/frankentui/commit/0cafef6a5257888b621dbd6f6cf528ce684f1f75))
- `contains()` replaced with `binary_search()` in filtered list operations. ([5a89911d](https://github.com/Dicklesworthstone/frankentui/commit/5a89911d73d4aac331922e8ad79b7e63a8fd88d0))
- Bounds-checked `get_mut` replaced with `set_fast` across widget rendering. ([00758f43](https://github.com/Dicklesworthstone/frankentui/commit/00758f43abcae71a03ca7b72aaed02625c410f35))
- LOUDS tree `degree()` computation corrected. ([0b3e4817](https://github.com/Dicklesworthstone/frankentui/commit/0b3e4817f25597d5a4fc1bea3593e872a8a328f8))
- Atomic Replace undo, table filtered selection, progress bar overflow, and observable borrow safety. ([a3e59693](https://github.com/Dicklesworthstone/frankentui/commit/a3e59693a9c855d7bddcbe859e0e3ba9722eee10))
- Flex gap alignment, fairness guard, input selection, list caching, and textarea allocation fixes. ([c99ab370](https://github.com/Dicklesworthstone/frankentui/commit/c99ab370278eb3b01b606da0420f2583c0f547bd))
- LogViewer filtered scroll edge cases and `ensure_visible` underflow prevention. ([ab4456fc](https://github.com/Dicklesworthstone/frankentui/commit/ab4456fcd1d5017cbecf26fcf0a8dccd3a89e9b8))
- Dynamic viewport in VirtualizedSearch and cursor-aware palette editing. ([a17c2d41](https://github.com/Dicklesworthstone/frankentui/commit/a17c2d41fa8d4181520c7301626b6a3eb840c700))
- Widget state change reporting corrected; modal backdrop hit testing wired. ([756078b4](https://github.com/Dicklesworthstone/frankentui/commit/756078b4898ac344f1d69f2fcadcf709ce321c23))

### Licensing

- MIT + OpenAI/Anthropic rider adopted across workspace metadata. ([ca794209](https://github.com/Dicklesworthstone/frankentui/commit/ca7942093265555ca6ce83ff4e4515e6143242b1))
- Crates bumped to 0.2.1 for rider metadata. ([e76c723f](https://github.com/Dicklesworthstone/frankentui/commit/e76c723f076a3a1cededa7a676690145c6b29591))
- Licensing and provenance guardrails for imported projects. ([8a3da803](https://github.com/Dicklesworthstone/frankentui/commit/8a3da803ef10415f6fd8bf1b1decdc8523d49047))

---

## [v0.2.0] -- 2026-02-15 (Tag Only)

> Git tag only (no GitHub Release).
> Tag: <https://github.com/Dicklesworthstone/frankentui/releases/tag/v0.2.0>
> First tagged release of FrankenTUI.

This is the first tagged release. It spans from the initial commit (2026-01-31) through the version bump on 2026-02-15, covering the full foundation plus major expansion across all crates during the v0.1.1 crates.io publish cycle.

### FrankenTerm Terminal Engine -- New Crates

- New `frankenterm-core` crate with grid operations, cursor movement, DEC/ANSI modes, and scrollback buffer with resize/reflow. ([db25bf42](https://github.com/Dicklesworthstone/frankentui/commit/db25bf42), [2da057aa](https://github.com/Dicklesworthstone/frankentui/commit/2da057aa), [d5b4c41c](https://github.com/Dicklesworthstone/frankentui/commit/d5b4c41c))
- Incremental patch API with dirty tracking. ([7200ec6d](https://github.com/Dicklesworthstone/frankentui/commit/7200ec6d))
- Cell flags, hyperlink ID, charset translation, and wide-char support. ([7905b327](https://github.com/Dicklesworthstone/frankentui/commit/7905b327), [c151cb3d](https://github.com/Dicklesworthstone/frankentui/commit/c151cb3d))
- VT conformance: HTS/TBC/CBT/ECH/DECKPAM/DECKPNM, DECALN, REP, DECOM, DECSCUSR, DECSTR, and ICH/DCH/IL/DL parser support. ([80380315](https://github.com/Dicklesworthstone/frankentui/commit/80380315), [b7a1361b](https://github.com/Dicklesworthstone/frankentui/commit/b7a1361b), [78529934](https://github.com/Dicklesworthstone/frankentui/commit/78529934))
- New `frankenterm-web` crate with scrollback viewport virtualization, smooth scroll, and plaintext URL auto-link detection. ([c9988ed6](https://github.com/Dicklesworthstone/frankentui/commit/c9988ed6), [3e2c44c8](https://github.com/Dicklesworthstone/frankentui/commit/3e2c44c8), [1af32744](https://github.com/Dicklesworthstone/frankentui/commit/1af32744))
- WebSocket binary envelope codec. ([e26146c7](https://github.com/Dicklesworthstone/frankentui/commit/e26146c7d935bcf962e0d75f17292fa3359c4bfc))
- Window-based flow control and bounded backpressure for WebSocket bridge. ([ab9245f6](https://github.com/Dicklesworthstone/frankentui/commit/ab9245f62e506beb39d61d381a97c5e78e679773))
- Rescale repro corpus for DPR/zoom/container failures. ([95f62ca5](https://github.com/Dicklesworthstone/frankentui/commit/95f62ca5e15f22dcb328941898952e011c7bfa45))
- Unified resize signal arbiter with coalescing. ([ded026ab](https://github.com/Dicklesworthstone/frankentui/commit/ded026abe3a896e34bdd1117308f27a44abffab7))
- Coordinate guard for stale-geometry detection during rescale. ([9d967bdf](https://github.com/Dicklesworthstone/frankentui/commit/9d967bdf56758508fce6406f18d64312b4c32da2))

### Backend Abstraction Layer -- New Crates

- New `ftui-backend` trait crate and `ftui-tty` native TTY implementation with Unix raw mode and terminal feature toggles. ([5a314523](https://github.com/Dicklesworthstone/frankentui/commit/5a314523), [ac43f32e](https://github.com/Dicklesworthstone/frankentui/commit/ac43f32e))
- Mouse protocol compatibility hardening, WezTerm mux detection, and terminal session resilience. ([43d7cb7c](https://github.com/Dicklesworthstone/frankentui/commit/43d7cb7ca56a90d172c2c6635af2d1273201d4e3))

### WASM Showcase -- New Crate

- New `ftui-showcase-wasm` crate implementing a WASM showcase runner. ([b1f1303f](https://github.com/Dicklesworthstone/frankentui/commit/b1f1303f))
- Showcase demo HTML for in-browser demonstration. ([68612f63](https://github.com/Dicklesworthstone/frankentui/commit/68612f63))
- wasm32 CI gate and core WASM hygiene (ADR-008). ([d91f1bdc](https://github.com/Dicklesworthstone/frankentui/commit/d91f1bdc))
- 7-phase rendering pipeline optimization for WASM. ([10e0ba55](https://github.com/Dicklesworthstone/frankentui/commit/10e0ba556b43eaeeac55a506d43a576795896f00))

### Kernel Architecture (ftui-core)

- Rust workspace initialized with multi-crate structure. ([aadc5679](https://github.com/Dicklesworthstone/frankentui/commit/aadc567948ed52075e1e73af4d21f1971f76c86c))
- Terminal session lifecycle, color downgrade, style system, and terminal model. ([ced1e5e7](https://github.com/Dicklesworthstone/frankentui/commit/ced1e5e7e083eb848636263e3e2f4f2599418bd1))
- Buffer API with Cell/CellContent/GraphemeId foundations. ([0da0ba05](https://github.com/Dicklesworthstone/frankentui/commit/0da0ba0587baddec39dca7d1b2325e6b7e0e87d4))
- GraphemePool with reference-counted interning. ([e6cb5b83](https://github.com/Dicklesworthstone/frankentui/commit/e6cb5b8386f1fdfc684d2c7be0ff209339302c9e))
- One-writer discipline and inline-mode safety guidance. ([d0cd7b5e](https://github.com/Dicklesworthstone/frankentui/commit/d0cd7b5e01fec64033fd51287d67a5f722a6a685))
- Inline mode validation helpers and safety improvements. ([3ff7e41a](https://github.com/Dicklesworthstone/frankentui/commit/3ff7e41a2796501fdb4a743988100c1914a9e454))
- Cx capability context for cancellation and deadline propagation. ([fdd633a1](https://github.com/Dicklesworthstone/frankentui/commit/fdd633a1539855c90e871b29284569ef2135ea81))
- Cx threaded through all Ring 1 terminal I/O operations. ([a144dcac](https://github.com/Dicklesworthstone/frankentui/commit/a144dcac8687b7a96cd0008eb83836cee9c396b0))
- S3-FIFO scan-resistant cache. ([79d91f28](https://github.com/Dicklesworthstone/frankentui/commit/79d91f286dc7b6b8dd70a5e405b48dec9881feba))
- Composable animation primitives. ([67eb0314](https://github.com/Dicklesworthstone/frankentui/commit/67eb0314))
- Tracing instrumentation for input parser and event_type_label. ([196e6a3b](https://github.com/Dicklesworthstone/frankentui/commit/196e6a3badb3d075e29b6acac4a2b1b7bf1726d4))
- SharedCapabilities with ArcSwap-backed concurrent access. ([d70e44cd](https://github.com/Dicklesworthstone/frankentui/commit/d70e44cdd6c43daa9343e83a1f891055dc2247a3))
- Canonical terminal engine module. ([4e8b4bc4](https://github.com/Dicklesworthstone/frankentui/commit/4e8b4bc46c6f1171d2e99f6370e1d853aa0a99e9))

### Render Pipeline (ftui-render)

- BufferDiff with row-major scan. ([9abb1bf9](https://github.com/Dicklesworthstone/frankentui/commit/9abb1bf9ffd868b74257198f67829b1874ab0c90))
- Presenter with state-tracked ANSI emission. ([aa58e858](https://github.com/Dicklesworthstone/frankentui/commit/aa58e85813486827c6f43cb3d21377e735c8d608))
- Geometry primitives and drawing module. ([14a19353](https://github.com/Dicklesworthstone/frankentui/commit/14a193535f4172ee29a33006392a17cb5dd28c28))
- CountingWriter for output bytes tracking. ([07a836e9](https://github.com/Dicklesworthstone/frankentui/commit/07a836e9))
- C1 controls stripped in sanitizer with proptest Rust 2024 compat. ([e2d6e656](https://github.com/Dicklesworthstone/frankentui/commit/e2d6e656))
- Export adapters (HTML, SVG, Text) for Buffer rendering. ([5aa907fc](https://github.com/Dicklesworthstone/frankentui/commit/5aa907fc))
- Bump-allocated frame arena. ([de824689](https://github.com/Dicklesworthstone/frankentui/commit/de824689fd05bff1ca8c19823cbe68492b53ac99))
- Frame guardrails for memory budget and queue depth. ([e3a2d4fb](https://github.com/Dicklesworthstone/frankentui/commit/e3a2d4fb16956f22cce5862b3536235e4f195818))
- Presenter orphan detection, O(1) diff lookups, and DiffViewport elimination. ([7e27656e](https://github.com/Dicklesworthstone/frankentui/commit/7e27656e))
- Deterministic fit-to-container and font metric lifecycle. ([0d5d1c47](https://github.com/Dicklesworthstone/frankentui/commit/0d5d1c471a0e2313d5be786a03959a92c8741346))
- Hyperlink support and text rendering infrastructure. ([22532c8c](https://github.com/Dicklesworthstone/frankentui/commit/22532c8c))
- Golden checksums migrated from SipHash to BLAKE3. ([e6582158](https://github.com/Dicklesworthstone/frankentui/commit/e6582158c6edcb8e1807e06dfa7a80bb009575e6))

### Layout (ftui-layout)

- Flex layout solver. ([0baccfdd](https://github.com/Dicklesworthstone/frankentui/commit/0baccfddd7ad1897bdab11dea39ea4c78063abd2))
- LayoutDebugger with tracing. ([94d9d8bb](https://github.com/Dicklesworthstone/frankentui/commit/94d9d8bb))
- Iterative solver to prevent wasted space with Max constraints. ([324f019f](https://github.com/Dicklesworthstone/frankentui/commit/324f019f))
- Pane operations and transaction journal. ([25c20240](https://github.com/Dicklesworthstone/frankentui/commit/25c2024077aa37786a4216bfdcc1d0943b124483))
- Pane invariant diagnostics and safe repair. ([beb2bccf](https://github.com/Dicklesworthstone/frankentui/commit/beb2bccfb9814dc8094e4209c66438a1e22fc1c9))
- Deterministic pane drag/resize state machine. ([e411f7ae](https://github.com/Dicklesworthstone/frankentui/commit/e411f7ae9f3e56f33302bbce84301556bfb7d30a))
- Pane coordinate normalization. ([e438c8bb](https://github.com/Dicklesworthstone/frankentui/commit/e438c8bb1ee5ca5e1b8c393e332bca13f6c7520c))
- Pane interaction tuning policies. ([40f745b3](https://github.com/Dicklesworthstone/frankentui/commit/40f745b39796077ea90bcfe3127f1961d02567a1))
- Persisted workspace schema with validation and migration. ([e61597da](https://github.com/Dicklesworthstone/frankentui/commit/e61597daf8cb0263b804c34e170cfe45741cea4f))
- Cell attribute extensions and pane split/resize infrastructure. ([1518a6df](https://github.com/Dicklesworthstone/frankentui/commit/1518a6df1ea27dafa01470c36d639bef3ddaf9cf))

### Runtime (ftui-runtime)

- Elm/Bubbletea-style Program runtime with Model/Cmd pattern. ([75e21361](https://github.com/Dicklesworthstone/frankentui/commit/75e21361))
- Stdio capture utility for accidental println! protection. ([81d16a63](https://github.com/Dicklesworthstone/frankentui/commit/81d16a63))
- Pane terminal hit-testing and routing primitives. ([80f2128d](https://github.com/Dicklesworthstone/frankentui/commit/80f2128df2d0d1a365f3e26c893da0fdcea656c4))
- Input fairness module with adaptive scheduling and SLA tracking.
- Inline-mode active widget gauge, scrollback preservation tracing, and unit tests. ([4472647e](https://github.com/Dicklesworthstone/frankentui/commit/4472647e0d8185da8857ef4c71c0a481578e216e))
- Terminal writer with additional rendering paths. ([549aa034](https://github.com/Dicklesworthstone/frankentui/commit/549aa034674eb01c039b42f195a13efcdbca8cfd))
- Snapshot undo store, diff strategy enrichment, and evidence ledger tests. ([0eb0da3e](https://github.com/Dicklesworthstone/frankentui/commit/0eb0da3e6a9453d531ecb4c86de24f2090e84801))

### Widget System (ftui-widgets)

- Panel widget with border, title, and padding. ([85d17ace](https://github.com/Dicklesworthstone/frankentui/commit/85d17ace))
- StatusLine widget. ([a58bf932](https://github.com/Dicklesworthstone/frankentui/commit/a58bf932))
- Hit testing and cursor control for interactive widgets. ([ad8cb457](https://github.com/Dicklesworthstone/frankentui/commit/ad8cb457))
- Budget-aware degradation across all widgets. ([21bd6676](https://github.com/Dicklesworthstone/frankentui/commit/21bd6676))
- Focused input state and CellContent support. ([b9a7fefa](https://github.com/Dicklesworthstone/frankentui/commit/b9a7fefa))
- Mouse event handling for Tree, List, and Table widgets. ([65c68a58](https://github.com/Dicklesworthstone/frankentui/commit/65c68a58))
- TextArea, Help, Tree, JsonView, Emoji, Stopwatch, Timer, Pretty widgets and Live display system.
- Undo/redo editor core, Unicode BiDi support, SyntaxHighlighter API.
- Reactive runtime: Observable/Computed values, two-way bindings, BatchScope, undo/redo history, MacroPlayback.
- TerminalEmulator widget, key-sequence interpreter, focus management expansion.
- Guided Tour system, Form Validation screen, Virtualized Search screen.
- Fenwick-tree variable-height virtualization.
- Focus indicator widget. ([57f727e3](https://github.com/Dicklesworthstone/frankentui/commit/57f727e34c03705168a5d17fd8e99b3aa9bba9d5))

### Text (ftui-text)

- ASCII width fast-path optimization. ([86613c83](https://github.com/Dicklesworthstone/frankentui/commit/86613c83a82dcd1e029216cdffa73ddb54106d78))
- Unicode width corpus tests and grapheme helpers. ([bb8b02ab](https://github.com/Dicklesworthstone/frankentui/commit/bb8b02ab))
- Rope text storage and View helpers. ([4fa45b3a](https://github.com/Dicklesworthstone/frankentui/commit/4fa45b3a))
- GFM Markdown rendering with LaTeX and streaming support.
- Deterministic hyphenation engine with TeX pattern matching. ([be560559](https://github.com/Dicklesworthstone/frankentui/commit/be5605590e50ade9b2cf28950420173b2bf74340))
- Formal paragraph objective for Knuth-Plass line breaking. ([7d681683](https://github.com/Dicklesworthstone/frankentui/commit/7d6816838c3b5a435bd4619160e98b19c41fadf8))
- S3-FIFO width cache as drop-in replacement for LRU. ([7bd7a992](https://github.com/Dicklesworthstone/frankentui/commit/7bd7a9921a79bc11bebde603dd98628265d879fd))

### Style (ftui-style)

- Theme system with CSS-like cascading.
- SharedResolvedTheme with ArcSwap-backed concurrent access. ([f39037b5](https://github.com/Dicklesworthstone/frankentui/commit/f39037b58737ce99d58100f6b4f22fb229cabd01))
- Theme Studio with live palette editing and WCAG contrast fixes.

### Extras (ftui-extras)

- Console abstraction for styled output. ([39be7b6c](https://github.com/Dicklesworthstone/frankentui/commit/39be7b6c))
- Asciicast v2 session recording. ([491d4732](https://github.com/Dicklesworthstone/frankentui/commit/491d4732))
- Full Mermaid diagram rendering pipeline: parser, layout, renderer with support for flowchart, sequence, Gantt, class, ER, and C4 diagram families. ([ad85efc9](https://github.com/Dicklesworthstone/frankentui/commit/ad85efc9), [46600614](https://github.com/Dicklesworthstone/frankentui/commit/46600614))
- Visual diagram diffing (`render_diff`). ([46600614](https://github.com/Dicklesworthstone/frankentui/commit/46600614))
- Mermaid showcase controls and metrics panel. ([a71ac2ae](https://github.com/Dicklesworthstone/frankentui/commit/a71ac2ae))
- Visual-effects primitives: text-effects, gradients, reveal, particle dissolve, metaballs, plasma, wireframe.

### Demo Showcase (ftui-demo-showcase)

- Kanban Board screen. ([46a178ce](https://github.com/Dicklesworthstone/frankentui/commit/46a178ce9b60f296e11d2b7aad7884ef4eff70a9))
- DragDrop/Quake screens and dashboard splitters. ([a97b6e58](https://github.com/Dicklesworthstone/frankentui/commit/a97b6e58))
- Live Markdown Editor integration. ([4a926ddc](https://github.com/Dicklesworthstone/frankentui/commit/4a926ddc))
- Toggleable FPS render mode in visual effects showcase. ([5762768b](https://github.com/Dicklesworthstone/frankentui/commit/5762768b))
- Mermaid harness telemetry, dense flow sample, and tour landing UI. ([069a80dd](https://github.com/Dicklesworthstone/frankentui/commit/069a80dd))
- Performance HUD as a full interactive screen with real-time metrics.

### PTY (ftui-pty)

- PTY signal handling, feature flags, and backpressure fixes. ([748393b7](https://github.com/Dicklesworthstone/frankentui/commit/748393b7))

### Harness (ftui-harness)

- Input storm module for stress-testing terminal input handling. ([a00b05e2](https://github.com/Dicklesworthstone/frankentui/commit/a00b05e22eb38f9a9e9d96312503b77afbd8bd01))
- HDD minimization harness and roaring bitmap renderer. ([57f727e3](https://github.com/Dicklesworthstone/frankentui/commit/57f727e34c03705168a5d17fd8e99b3aa9bba9d5))

### Web/WASM (ftui-web)

- Pane pointer-capture adapter. ([3ed99188](https://github.com/Dicklesworthstone/frankentui/commit/3ed99188164b306a82b9e5f8d3000356c89fdead))
- Stable JS API contract and versioning policy for frankenterm-web. ([a31d8d7d](https://github.com/Dicklesworthstone/frankentui/commit/a31d8d7de879acaef482afeeb53fa6bff555345a))
- Remote-mode VT feed pipeline, scrollback viewport, and width policy. ([6601aaff](https://github.com/Dicklesworthstone/frankentui/commit/6601aaff744bcaaa8dea0d346a0f6414d53e6ba1))
- Browser attach state machine and semantic pane input events. ([cad6f396](https://github.com/Dicklesworthstone/frankentui/commit/cad6f39677a8a0e0d607d00e2ace294c5bca5167))
- MouseCapturePolicy tri-state (Auto/On/Off) replacing boolean mouse flag. ([145d60fd](https://github.com/Dicklesworthstone/frankentui/commit/145d60fdf2827bd8779b47ccf9427c68d3c8f4bf))

### Bayesian Intelligence Layer

- BOCPD integration into resize coalescer, diff strategy selector, and dirty-row diff optimization.
- VOI telemetry and conformal rank confidence.
- Evidence sink builder and allocation budget tracking.
- Evidence ledger with ring buffer for diff strategy. ([328171ae](https://github.com/Dicklesworthstone/frankentui/commit/328171aed60381b581dc6fa2a2d1b6ee8f46048c))
- Unified Bayesian evidence ledger schema. ([c95bb88e](https://github.com/Dicklesworthstone/frankentui/commit/c95bb88ec97210b0491a3d94af79265b6a9af766))
- Command palette scoring optimization with evidence descriptions.
- Baseline p50/p95/p99 capture for hot paths. ([009619c6](https://github.com/Dicklesworthstone/frankentui/commit/009619c606c57e9d5f579552d46a2e76ee43c8a7))
- Visual cell diff on checksum mismatch. ([8fb8e693](https://github.com/Dicklesworthstone/frankentui/commit/8fb8e69363609a432c29125600d59c65ed5b866a))
- Golden checksum and baseline gates in demo-showcase CI. ([fb506d0d](https://github.com/Dicklesworthstone/frankentui/commit/fb506d0d5a839d12c827b917d102893a25afe0fb))

### Security

- Threat model, HTTPS-only defaults, and resource bound hardening. ([3125afab](https://github.com/Dicklesworthstone/frankentui/commit/3125afabb2873afffbdb7f4e1dc2aa20f79c2475))
- Clipboard policy API, link audit trail, and wide character handling. ([cddcc986](https://github.com/Dicklesworthstone/frankentui/commit/cddcc986803967aa96845945cdb8f8c4e6a34b2f))

### Mouse and Input

- Mouse event handling for Tree, List, and Table widgets. ([65c68a58](https://github.com/Dicklesworthstone/frankentui/commit/65c68a58))
- `Cmd::SetMouseCapture` for runtime mouse capture control. ([9f2c4fa7](https://github.com/Dicklesworthstone/frankentui/commit/9f2c4fa7))
- Hit region registration for chrome overlays and status bar. ([4293bd1d](https://github.com/Dicklesworthstone/frankentui/commit/4293bd1d))
- Mouse dispatcher rewritten to activate on press with release suppression. ([67a37631](https://github.com/Dicklesworthstone/frankentui/commit/67a37631))
- Tab/BackTab prevented from switching screens during text input. ([f6d6fd48](https://github.com/Dicklesworthstone/frankentui/commit/f6d6fd48fa2e128562db3727b309eef443579e56))
- Force-cancel safety valve and RAII interaction guard. ([7251045](https://github.com/Dicklesworthstone/frankentui/commit/7251045296a16996794ce9cca6d4d084d65a20e5))
- Multiplexer capability matrix with fallback policy. ([657c2299](https://github.com/Dicklesworthstone/frankentui/commit/657c2299e6fc607fd03b76d2a3f160f0958df8d4))
- SelectionGestureController for pointer/keyboard gestures. ([792a4543](https://github.com/Dicklesworthstone/frankentui/commit/792a4543bbec86423c15705f721283f4c43f7334))
- Selection copy/rect extraction and AHashMap migration. ([af044fa7](https://github.com/Dicklesworthstone/frankentui/commit/af044fa7fcf40b6ec06353171ea12c4645f89847))

### Testing Infrastructure

- Over 1,500 new tests across the workspace including edge-case, property, conformance, and snapshot tests.
- ~120 tests across widgets, render, runtime, layout. ([38db5b3e](https://github.com/Dicklesworthstone/frankentui/commit/38db5b3e))
- ~55 tests for PTY, text, style, logging modules. ([268e24a0](https://github.com/Dicklesworthstone/frankentui/commit/268e24a0))
- 100+ VT conformance fixture tests for frankenterm-core. ([dfee0d76](https://github.com/Dicklesworthstone/frankentui/commit/dfee0d76), [023d479f](https://github.com/Dicklesworthstone/frankentui/commit/023d479f))
- Comprehensive inline mode tests. ([f52cf29b](https://github.com/Dicklesworthstone/frankentui/commit/f52cf29b4291d46913c2c7330509e9dadc55faf6))
- Property tests for S3-FIFO cache invariants. ([805bf8ee](https://github.com/Dicklesworthstone/frankentui/commit/805bf8ee98d0ef1f4dc4a94c8fcd4d95296ecce6))
- S3-FIFO vs W-TinyLFU vs LRU cache benchmarks. ([59657c48](https://github.com/Dicklesworthstone/frankentui/commit/59657c488f2dd481b326ea6e3000839153b4be54))
- Getting-started guide for library consumers. ([8d4bd83d](https://github.com/Dicklesworthstone/frankentui/commit/8d4bd83d))
- Domain-specific error model with graceful degradation. ([afe5f29d](https://github.com/Dicklesworthstone/frankentui/commit/afe5f29dfc5379b1886f3bc0a0a96d08a0533bd9))
- CI/CD pipeline and Dependabot configuration. ([bee75b14](https://github.com/Dicklesworthstone/frankentui/commit/bee75b14))

### Other Foundations (from initial development, 2026-01-31)

- Saturating arithmetic sweep across render, text, widgets, layout, and demo screens for overflow safety.
- Responsive layout primitives: Breakpoints, Responsive<T>, ResponsiveLayout with visibility helpers.
- Animation stack: Timeline scheduler, AnimationGroup lifecycle, spring physics, stagger utilities.
- Internationalization foundation: RTL layout mirroring and BiDi tests.
- Diff strategy decision contract with integration tests. ([7554ccbc](https://github.com/Dicklesworthstone/frankentui/commit/7554ccbc83b44eb26ad1e3cc4287364ab77940f4))

---

## v0.1.1 -- 2026-02-05 (crates.io Only)

> Published to crates.io. No git tag. No GitHub Release.
> Publish commit: [f2e8bca5](https://github.com/Dicklesworthstone/frankentui/commit/f2e8bca5f17550d17f0e9a786fde64b71c809691)
> Version bump: [44a12de1](https://github.com/Dicklesworthstone/frankentui/commit/44a12de186d7d8e3b7bc177a0fa81d13789ba3b9)

First crates.io publish of all 13 original workspace crates, bumped from 0.1.0 to 0.1.1 with pinned external dependencies for build reproducibility. Broke the `ftui-extras` / `ftui-harness` publish cycle by inlining ANSI snapshot helpers.

This version contains all foundational work listed under v0.2.0 above (the kernel, render pipeline, layout solver, runtime, widget system, text subsystem, extras, and PTY crate). The v0.2.0 tag includes the v0.1.1 content plus the feature expansion that followed.

---

## v0.1.0 -- 2026-01-31 (Internal Only)

> Initial version. No publish. No tag.

- Initial commit with FrankenTUI plan documents and architectural design (V5/V6.1 hybrid architecture). ([7a23b45a](https://github.com/Dicklesworthstone/frankentui/commit/7a23b45a1efa7b114ea55c51835cc093abf97605))
- Comprehensive bead graph with dependency structure and implementation roadmap covering 15 feature areas with 46 subtasks.
- Reference library sync script and build infrastructure. ([c37c599f](https://github.com/Dicklesworthstone/frankentui/commit/c37c599f300e1230795f882593439f3164e2cdaa))
- Workspace structure established with crate skeleton. ([aadc5679](https://github.com/Dicklesworthstone/frankentui/commit/aadc567948ed52075e1e73af4d21f1971f76c86c))

---

[Unreleased]: https://github.com/Dicklesworthstone/frankentui/compare/v0.2.1...main
[v0.2.1]: https://github.com/Dicklesworthstone/frankentui/compare/v0.2.0...v0.2.1
[v0.2.0]: https://github.com/Dicklesworthstone/frankentui/releases/tag/v0.2.0
