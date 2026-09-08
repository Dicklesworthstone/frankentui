# Changelog research

## Scope and sources

This update applies `changelog-md-workmanship` to the unpublished 0.7.0 changes
in `v0.6.0..fc04ef3f`, with a separate audit of the historical version timeline.
Research date: 2026-09-08. Sources are local Git history and tags, live GitHub
Release metadata, and the checked-in `.beads/issues.jsonl` records. The existing
historical capability sections are retained unless evidence identifies a
specific correction.

The release candidate is frozen at
[`798efa0b`](https://github.com/Dicklesworthstone/frankentui/commit/798efa0bb746601cea78b75ad8bc859f738a6456).
Later documentation and tracker commits do not change the provenance of its
prepared binaries. Version 0.7.0 remains **Unreleased** until publication.

## Research chunks

1. Version spine: reconcile local tags, published GitHub Releases, and the
   older 0.1.x package-only/internal milestones.
2. First half of the post-0.6.0 commits: identify capability changes, regressions,
   feature gates, and representative implementation commits.
3. Remaining post-0.6.0 commits: include release preparation fixes, rendering
   performance, and verification improvements without claiming pending gates
   or publication have completed.
4. Map implemented capabilities to precise tracker records; validate commit
   ancestry, dates, links, and the final Markdown.

## Version spine findings

The GitHub Releases API lists six published, non-draft releases: 0.2.1, 0.3.0,
0.3.1, 0.4.1, 0.5.0, and 0.6.0. Local tags also include 0.2.0 and 0.4.0;
neither has a published GitHub Release. There is no 0.7.0 tag or Release.
The 0.4.0 tag timestamp is 2026-04-24 21:23:53 -0400, so the timeline retains
its recorded local date instead of silently changing it to the next UTC day.

Distilled changes: expanded the timeline to match the document's full scope,
replaced the fabricated 0.2.0 Release URL with its tag URL, and supplied missing
Markdown reference definitions for version headings. The 0.1.x milestones
remain distinct from tagged releases.

## Post-0.6.0 findings and validation

The scope contains 194 non-merge commits, reviewed in two chronological
97-commit windows with selected implementation diffs and current API checks.

The first 97 commits were read chronologically. Their implementation and API
evidence adds default backends, keymaps and Help feedback, accessibility hooks,
variable-height virtualization, history/progress/border widgets, BOCPD defaults,
startup DECSTBM fallback, capability/queue evidence, grapheme caching, and SAT
tile-row skipping. These changes are distilled into Unreleased with individual
implementation links. Pure tracker, formatting, and superseded workflow changes
are not represented as new runtime capabilities.

The SAT report compares the tiled path plus prefilter against a flat diff; it
does not isolate the prefilter's speedup. The changelog therefore describes work
elimination and counters without borrowing its timing ratios as general claims.

The second 97 commits run from
[`61914fa7`](https://github.com/Dicklesworthstone/frankentui/commit/61914fa7)
through the researched revision. Reviewed changes add experimental feature
gates, exact grapheme cache keys, explicit unavailable conformal bounds,
accessibility privacy, pane persistence/constraints, streaming log sanitization,
touch routing, narrow-cell fill optimization, and DSR/PTY verification repairs.
These are distilled into the feature, fix, migration, and verification sections.
No aggregate test pass, completed browser gate, or published artifact is claimed.

Six workstream links point to exact lines in the researched tracker revision.
G01 facade, G09 runtime accessibility, and G14 keymap core are closed records;
G17 widgets, G47 panes, and the release record remain open or in progress.

The live crates.io API confirms 0.1.1 publication on 2026-02-05 and no 0.1.0
or 0.7.0 package. The 0.2.1 package was published on 2026-02-19, before the
2026-03-07 GitHub Release; this distinction is now explicit. The 0.2.0 package
exists and is currently yanked, independently of its tag-only GitHub status.

A full-document commit-link ancestry audit exposed 16 nonexistent hashes in
the preserved 0.2.0 section and three real commits after that tag. The invalid
citations were replaced with verified implementation commits, except for an
unsupported test-count line that was removed. Misplaced mouse/showcase changes
were moved into 0.2.1. Twelve predecessor references in 0.2.1 were consolidated
under 0.2.0, preserving the capabilities at the version where they landed.
The 0.3.0 range has
193 non-merge commits, correcting the previous 190 count; the other stated
0.2.1–0.5.0 range counts match Git.

Historical wording now reflects the implementation: stdio capture requires
the `ftui_println!`/`ftui_eprintln!` macros; Console and animation landed in
multi-feature commits whose subjects mention other components; the asciicast
recorder belongs to the harness, LayoutDebugger to widgets, and export adapters
and PTY backpressure to extras. Unsupported aggregate test counts were removed.
The 0.1.1 entry no longer attributes every subsequent 0.2.0 feature to its earlier
package snapshot.

## Validation

- The independent review checked the revised claims and migration notes against
  code, including the fallible conformal constructor and optional residuals.
- All 409 distinct commit URLs resolve locally. All 424 uses belong to the
  stated version's ancestry, and references in incremental release sections
  are newer than their predecessor tag.
- All six tracker anchors resolve to the named record and recorded status at
  the pinned revision. Version-heading reference links are defined.
- The skill's structural validator and `git diff --check` pass.
- The full live HTTP scan checked 431 distinct inline links: 428 succeeded;
  three crates.io frontend routes returned 404. Those three links now use the
  authoritative per-version registry API, each independently checked with the
  same HEAD method and User-Agent and returning HTTP 200. All final inline
  links are therefore covered by successful HTTP checks.

The validation commands were
`validate-changelog-md.py CHANGELOG.md --verify-links --max-links 1000 --timeout 3`,
the structural-only invocation after wording corrections, read-only Git
ancestry and tracker-line checks, and `git diff --check`. This documentation
update required no Rust builds, GitHub Actions, RCH, or UBS execution.
