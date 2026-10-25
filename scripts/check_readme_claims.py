#!/usr/bin/env python3
"""Validate claims-ledger structure only; does not prove claims or extract coverage.

Read-only and stdlib-only. --schema-check is deliberately distinct from the
future full --check gate: a structurally valid pending row is not proof.
--self-test uses in-memory fixtures and performs no cleanup or compilation.
"""

import argparse
from collections import Counter
from datetime import date
import difflib
import json
from pathlib import Path
import re
import sys


COLUMNS = ('id', 'claim', 'location', 'kind', 'decision', 'owner', 'proof',
           'status', 'last_verified')
KINDS = set('count api constant default event file algorithm example ci env status'.split())
DECISIONS = {'CODE', 'DOC', 'DOC+quarantine', 'regenerate', 'n/a'}
STATUSES = {'proven', 'pending-code', 'pending-doc', 'retracted', 'allowlisted'}
PROOFS = {'test', 'path', 'ident', 'cmd', 'job', 'count', 'manual', 'bead'}

PROOF_GRAMMAR = """\
`test:<crate>::<name>` — a named test in the retained test inventory.
`path:<repo-relative>` — a repository path exists; proves existence only.
`ident:<Rust identifier or string>` — production source contains the identifier.
`cmd:<shell command that must exit 0>` — an explicitly permitted, bounded command.
`job:<workflow>:<job>` — historical syntax only; not an executable proof under DSR-only policy.
`count:<python expression over repo files>` — a restricted counting expression.
`manual:<YYYY-MM-DD>:<who>` — a dated manual verification, expires after 90 days.
`bead:<slug>` — an unresolved implementation obligation, not a passing proof."""

ALLOWLIST_LINE_RE = re.compile(
    r'^([^:\s#][^:]*):(.+?)\s+#\s+(bd-[a-z0-9.-]+)\s+(.+)$'
)
MIN_PROVEN_RATIO = 0.60
MANUAL_EXPIRY_DAYS = 90


def cells(line):
    """Split Markdown table cells, honoring backslash-escaped pipes."""
    parts = []
    current = []
    escaped = False
    for char in line.strip():
        if escaped:
            current.append(char if char == '|' else '\\' + char)
            escaped = False
        elif char == '\\':
            escaped = True
        elif char == '|':
            parts.append(''.join(current).strip())
            current = []
        else:
            current.append(char)
    if escaped:
        current.append('\\')
    parts.append(''.join(current).strip())
    if parts[0] or parts[-1]:
        raise ValueError('table row must start and end with an unescaped pipe')
    return parts[1:-1]


def iso_date(value):
    if not re.fullmatch(r'\d{4}-\d{2}-\d{2}', value):
        return False
    try:
        date.fromisoformat(value)
        return True
    except ValueError:
        return False


def validate(text):
    rows = []
    errors = []
    seen = set()
    state = 'before'
    for number, line in enumerate(text.splitlines(), 1):
        stripped = line.strip()
        if not stripped.startswith('|'):
            if state == 'separator':
                errors.append(f'line {number}: missing table separator')
                state = 'after'
            elif state == 'rows':
                state = 'after'
            continue
        try:
            parts = cells(line)
        except ValueError as error:
            errors.append(f'line {number}: {error}')
            continue
        if tuple(parts) == COLUMNS:
            if state != 'before':
                errors.append(f'line {number}: duplicate claims table')
            state = 'separator'
            continue
        if state == 'separator':
            if len(parts) != len(COLUMNS) or not all(re.fullmatch(r':?-{3,}:?', p) for p in parts):
                errors.append(f'line {number}: invalid table separator')
            state = 'rows'
            continue
        if state != 'rows':
            errors.append(f'line {number}: unexpected table row outside claims table')
            continue
        if len(parts) != len(COLUMNS):
            errors.append(f'line {number}: expected nine columns, got {len(parts)}')
            continue
        row = dict(zip(COLUMNS, parts))
        problems = []
        if not all(parts):
            problems.append('empty cell')
        ident = row['id']
        if not re.fullmatch(r'[CVSN][0-9]{2,}', ident) or int(ident[1:] or '0') == 0:
            problems.append('invalid claim ID')
        if ident in seen:
            problems.append('duplicate claim ID')
        seen.add(ident)
        for column, allowed in [('kind', KINDS), ('decision', DECISIONS), ('status', STATUSES)]:
            if row[column] not in allowed:
                problems.append(f'invalid {column}')
        path, delimiter, anchor = row['location'].partition(' :: ')
        if not delimiter or not path.strip() or not anchor.strip():
            problems.append('location requires filename :: text anchor')
        if row['decision'] == 'CODE' and row['owner'] == '-':
            problems.append('CODE row requires an owner')
        for proof in row['proof'].split('; '):
            kind, delimiter, payload = proof.partition(':')
            if not delimiter or kind not in PROOFS or not payload.strip():
                problems.append('invalid proof syntax')
            elif kind == 'test' and not re.fullmatch(r'[^:\s]+::[^\s:][^\s]*', payload):
                problems.append('invalid test proof')
            elif kind == 'job' and (':' not in payload or not all(payload.split(':', 1))):
                problems.append('invalid job proof')
            elif kind == 'manual':
                day, separator, who = payload.partition(':')
                if not separator or not who.strip() or not iso_date(day):
                    problems.append('invalid manual proof')
        verified = row['last_verified']
        if verified != '-' and not iso_date(verified):
            problems.append('invalid verification date')
        if row['status'] == 'proven':
            if verified == '-':
                problems.append('proven row requires verification date')
            proof_kinds = {
                p.partition(':')[0]
                for p in row['proof'].split('; ')
                if ':' in p
            }
            if proof_kinds and proof_kinds.issubset({'path', 'ident'}):
                problems.append(
                    'proven row cannot rely solely on path:/ident: proofs (proof-class inflation)'
                )
            if 'bead' in proof_kinds:
                problems.append('proven row cannot cite an unresolved bead: proof')
        if problems:
            errors.append(f'line {number} ({ident}): ' + '; '.join(problems))
        rows.append(row)
    if state == 'before':
        errors.append('missing canonical claims table header')
    if not rows:
        errors.append('claims table contains no rows')
    return rows, errors


EXPERIMENTAL_MARKER = '**Status: experimental**'
WHERE_IT_RUNS = '**Where it runs'

REQUIRED_WHERE_IT_RUNS_SECTIONS = {
    'Damped Spring Dynamics (Animation System)',
    'Easing Curves + Stagger Distributions',
    'Sine Pulse Sequences (Attention Cues)',
    'Degradation Cascade',
    'BOCPD for Resize Regimes',
    'Run\u2011Length Posterior + Hazard Function (BOCPD Core)',
    'E\u2011Processes + GRAPA (Anytime\u2011Valid Monitoring)',
    'GRAPA (Adaptive Betting Fraction)',
    'Count\u2011Min Sketch (Approximate Counts)',
    'PAC\u2011Bayes Calibration (Error Tightening)',
    'Scheduling Math (Smith\u2019s Rule + Aging)',
    'E-Process: Anytime-Valid Statistical Testing',
    'Queueing-Theoretic Scheduler (Deep Dive)',
    'State Persistence',
    'Frame Arena Allocator',
}

QUALIFIED_MATH_ROWS = {
    '**Conformal Rank Confidence**': ('opt-in', 'helper', 'utility'),
    '**BOCPD**': ('opt-in',),
    '**Run\u2011Length Posterior**': ('opt-in',),
    '**GRAPA**': ('experimental',),
    '**Damped Spring**': ('standalone',),
    '**Easing Curves**': ('standalone',),
    '**Staggered Cascades**': ('standalone',),
    '**Sine Pulse**': ('standalone',),
    '**Count\u2011Min Sketch**': ('benchmark', 'experimental'),
    '**W\u2011TinyLFU Admission**': ('benchmark',),
    '**PAC\u2011Bayes**': ('experimental',),
    '**Smith\u2019s Rule + Aging**': ('opt-in',),
}


def strip_test_regions(text):
    """Blank `#[cfg(test)]` items, borrowed from the reachability gate.

    A test importing an experimental module is not a production consumer --
    `unified_evidence.rs` calls `evidence_bridges::from_diff_strategy` only from
    its own test module, and counting that would report the opposite of the
    truth. Rather than reimplement brace matching that already exists and is
    tested next door, reuse it; if the import fails, fall back to the raw text,
    which errs toward reporting a consumer rather than hiding one.
    """
    if _reachability_gate is None:
        return text
    return _reachability_gate.strip_test_regions(text)


def _load_reachability_gate():
    """Import the sibling gate once, without mutating `sys.path` repeatedly."""
    scripts_dir = str(Path(__file__).resolve().parent)
    added = scripts_dir not in sys.path
    if added:
        sys.path.insert(0, scripts_dir)
    try:
        import check_module_reachability
        return check_module_reachability
    except ImportError:  # pragma: no cover - the gate is normally present
        if added:
            sys.path.remove(scripts_dir)
        return None


_reachability_gate = _load_reachability_gate()


def all_readme_sections(readme_text):
    """Yield (heading, body) for every `## ` or `### ` block in README."""
    lines = readme_text.splitlines()
    starts = [i for i, line in enumerate(lines)
              if line.startswith('## ') or line.startswith('### ')]
    for index, start in enumerate(starts):
        end = starts[index + 1] if index + 1 < len(starts) else len(lines)
        body = '\n'.join(lines[start:end])
        yield lines[start].lstrip('#').strip(), body


def experimental_sections(readme_text):
    """Yield (heading, body) for each README block marked experimental.

    Blocks are delimited by any heading, `##` or `###`, not just `##`. "Flake
    Detection & Sequential FDR Control" carries the marker twice, once per
    subsection; splitting only on `##` would let a single "Where it runs" line
    cover both and leave one of them unqualified.
    """
    for heading, body in all_readme_sections(readme_text):
        if EXPERIMENTAL_MARKER in body:
            yield heading, body


def read_sources(modules, crates_dir):
    """Yield (display path, source text) for crate sources worth scanning.

    One pass over the tree, not one per module: stripping `#[cfg(test)]` means
    brace-matching a whole file, so re-reading 600 files for each of 27 modules
    ran 10x longer than the reachability gate this sits beside. A cheap
    substring prefilter on the raw text decides what is worth stripping, and
    almost nothing is. The prefilter cannot hide a real consumer -- every
    pattern in `find_consumers` contains the module name, and stripping only
    removes text.

    A module's own file never counts as its consumer, and neither does a
    `lib.rs`, whose `pub mod` declaration is not use.

    **Known gap.** `ftui-runtime/src/lib.rs` re-exports many of these modules'
    types (`pub use flat_combine::{CombinerStats, FlatCombiner};` and friends),
    every one behind `#[cfg(feature = "experimental")]`. A future consumer
    writing `use ftui_runtime::FlatCombiner;` reaches the module without ever
    naming its path, and this check would miss it. Matching the re-exported type
    names instead was rejected: `lens` alone exports `Identity`, `Fst` and
    `Snd`, which collide with ordinary names and would make the check noisy
    enough to ignore. The likely wiring path -- `use crate::<module>::` from
    inside the owning crate -- is covered, and `make reachability` would also
    stop reporting the module as unreached.
    """
    quarantined_files = {f'{module}.rs' for module in modules}
    prefilter = re.compile('|'.join(re.escape(m) for m in sorted(modules))) \
        if modules else None
    if prefilter is None:
        return
    for source in sorted(crates_dir.glob('*/src/**/*.rs')):
        if source.name in quarantined_files or source.name == 'lib.rs':
            continue
        try:
            raw = source.read_text(encoding='utf-8', errors='replace')
        except OSError:
            continue
        if prefilter.search(raw):
            yield source.relative_to(crates_dir.parent).as_posix(), raw


def find_consumers(modules, sources):
    """Report `module <- path` for each module some source actually uses.

    `sources` is an iterable of (path, raw text) so this stays pure and
    testable without touching the filesystem.

    "Production" excludes the other experimental modules, which `read_sources`
    filters out: several of them import each other -- `resize_sla` uses
    `conformal_alert`, `policy_config` uses `degradation_cascade` -- and that is
    a quarantined cluster wiring itself together, not the runtime picking any of
    it up. Counting those would have fired on seven modules on day one and
    taught everyone to ignore the check.
    """
    # Any `ftui_*` crate prefix, not just ftui_runtime: the table also owns
    # `ftui-render::roaring_bitmap`, and a cross-crate consumer writes
    # `ftui_render::roaring_bitmap::…`, which a runtime-only pattern misses.
    crate_path = r'(?:crate|ftui_[a-z][a-z0-9_]*)'
    patterns = {
        module: re.compile(rf'use +{crate_path}::{re.escape(module)}\b'
                           rf'|{crate_path}::{re.escape(module)}::')
        for module in modules
    }
    consumed = {}
    for path, raw in sources:
        text = strip_test_regions(raw)
        for module, pattern in patterns.items():
            if module not in consumed and pattern.search(text):
                consumed[module] = f'{module} <- {path}'
    return [consumed[module] for module in sorted(consumed)]


TEST_FN_RE = re.compile(r'\bfn\s+([a-z_][a-z0-9_]*)\s*[(<]')


def collect_test_fn_names(crates_dir):
    """Map each `fn name` under crates/ to the (crate, file stem) pairs defining it.

    Bare names are not enough to police a proof: `default_config_values` is
    defined in five different modules (gesture, hover_stabilizer, key_sequence,
    pty_capture, voi_sampling), so a proof naming the wrong one would pass a
    name-only check. Recording crate and file stem lets `check_proof_refs`
    verify as much of `test:<crate>::<module>::<name>` as is unambiguous.

    Inline `#[cfg(test)] mod tests` means a proof's middle segment is usually
    the *file* stem rather than a real module path, which is why the stem is
    what gets matched.
    """
    index: dict[str, set[tuple[str, str]]] = {}
    for source in crates_dir.glob('*/**/*.rs'):
        try:
            text = source.read_text(encoding='utf-8', errors='replace')
        except OSError:
            continue
        try:
            crate = source.relative_to(crates_dir).parts[0]
        except ValueError:  # pragma: no cover - source is always under crates/
            continue
        for name in TEST_FN_RE.findall(text):
            index.setdefault(name, set()).add((crate, source.stem))
    return index


def check_proof_refs(rows, crates_dir, root):
    """Every `test:` and `path:` proof must point at something that exists.

    This does **not** prove a claim -- it checks that the evidence cited is
    real. On 2026-09-19 three rows cited test names that did not exist
    (`history_records_on_enter`, `render_plasma_frame_deterministic`,
    `russian_rules`), each a near-miss for a real test written from memory
    rather than looked up. A fabricated proof is worse than the `bead:`
    placeholder it replaced, because it reads as settled.

    Exactly what is caught, so nobody over-trusts a green run:

    1. a `test:` proof whose final segment names no `fn` anywhere in crates/;
    2. one whose first segment names a real crate that does not define it;
    3. one whose path segments name no file that defines it;
    4. a `path:` proof pointing at a file that does not exist.

    Not caught: a bogus *inner* module segment when a sibling segment is right
    (`progress::indeterminate_tests::x` passes because `progress.rs` defines
    `x`), and a real test that pins the wrong thing. Distinguishing a fake
    inner module from a real one needs the module tree, and proofs legitimately
    appear as `crate::file::name`, `crate::file::tests::name` and
    `crate::dir::file::tests::name`.
    """
    errors = []
    index = collect_test_fn_names(crates_dir)
    if not index:
        return errors
    crates = {crate for sites in index.values() for crate, _ in sites}
    for row in rows:
        for proof in row['proof'].split('; '):
            kind, _, payload = proof.partition(':')
            if kind == 'test':
                segments = [s.strip() for s in payload.split('::') if s.strip()]
                if not segments:
                    continue
                name = segments[-1]
                sites = index.get(name)
                if not sites:
                    errors.append(
                        f"{row['id']}: cites test `{payload}` but no `fn {name}` "
                        f"exists under crates/. Check the name -- a proof that "
                        f"cannot be run is worse than no proof."
                    )
                    continue
                # Only police a crate segment that names a real crate, so a
                # proof written as bare `test:some_name` stays acceptable.
                crate = segments[0] if len(segments) > 1 else None
                if crate in crates and not any(c == crate for c, _ in sites):
                    found = ', '.join(sorted({c for c, _ in sites}))
                    errors.append(
                        f"{row['id']}: cites `{payload}`, but `fn {name}` is not "
                        f"in {crate} -- it is in {found}."
                    )
                    continue
                # Likewise the module path, when the name is ambiguous enough
                # for the wrong one to matter. Any middle segment may be the
                # file stem: proofs are written both as `crate::file::name` and
                # as the true Rust path `crate::file::tests::name`, and a
                # nested module adds `crate::dir::file::tests::name`. Requiring
                # *some* middle segment to match the defining file accepts all
                # three without pretending to resolve real module paths.
                middle = set(segments[1:-1])
                stems = {s for c, s in sites if crate not in crates or c == crate}
                if middle and stems and not (middle & stems):
                    errors.append(
                        f"{row['id']}: cites `{payload}`, but `fn {name}` is "
                        f"defined in {', '.join(sorted(stems))}.rs, which none "
                        f"of its path segments name."
                    )
            elif kind == 'path':
                target = payload.strip()
                if target and not (root / target).exists():
                    errors.append(
                        f"{row['id']}: cites path `{target}`, which does not exist."
                    )
    return errors


def check_math_at_a_glance(readme_text):
    """Verify `### Math At a Glance` qualifies non-default/experimental/benchmark rows."""
    errors = []
    in_table = False
    for line in readme_text.splitlines():
        if line.startswith('## ') or line.startswith('### '):
            in_table = line.strip() == '### Math At a Glance'
            continue
        if in_table and line.startswith('|'):
            try:
                parts = cells(line)
            except ValueError:
                continue
            if len(parts) < 2:
                continue
            tech, where_used = parts[0], parts[1].lower()
            required_terms = QUALIFIED_MATH_ROWS.get(tech)
            if required_terms and not any(t in where_used for t in required_terms):
                errors.append(
                    f'README "Math At a Glance" row {tech} must qualify '
                    f'"Where It’s Used" with one of {required_terms} '
                    f'(currently: "{parts[1]}").'
                )
    return errors


def check_constants_consistency(readme_text, rows=None):
    """Verify repeated numeric/formula constants across README sections and ledger backing."""
    errors = []
    # 1. Resize coalescer delays: Steady = 16ms, Burst = 40ms (never 200ms/20ms as delays)
    if re.search(r'Steady\s*\(\s*200\s*ms\s+delay', readme_text) or re.search(
        r'Burst\s*\(\s*20\s*ms\s+delay', readme_text
    ):
        errors.append(
            'README confuses BOCPD observation means (200ms/20ms) with resize '
            'coalescing delays (Steady = 16ms, Burst = 40ms).'
        )
    # 2. E-Process formula: must not regress to retracted multiplicative wealth W_t = W_{t-1}(1 + \lambda_t(X_t - \mu_0))
    for heading, body in all_readme_sections(readme_text):
        if heading in (
            'E\u2011Processes + GRAPA (Anytime\u2011Valid Monitoring)',
            'E-Process: Anytime-Valid Statistical Testing',
        ):
            if 'W_{t-1}' in body and '(X_t' in body:
                errors.append(
                    f'README section "{heading}" uses retracted multiplicative '
                    f'wealth formula W_t instead of exponential E_t.'
                )
    # 3. Verify ledger backing for repeated constant groups when rows are provided
    if rows is not None:
        by_id = {r['id']: r for r in rows}
        required_constant_rows = (
            ('V36', 'Resize coalescing regimes (16ms/40ms/100ms, 200ms/20ms)'),
            ('V13', 'Mondrian conformal defaults (alpha=0.05, min_samples=20, window=256)'),
            ('V37', 'Budget PID gains (Kp=0.5, Ki=0.05, Kd=0.2, warmup_frames=10)'),
            ('C35', 'VOI inline-auto defaults (1.0, 9.0, 1000ms, 100ms, 0.08)'),
        )
        for cid, desc in required_constant_rows:
            row = by_id.get(cid)
            if not row or row['status'] != 'proven':
                errors.append(
                    f'Repeated constant group "{desc}" requires {cid} to be proven.'
                )
            elif not any(
                p.startswith(('test:', 'count:')) for p in row['proof'].split('; ')
            ):
                errors.append(
                    f'Repeated constant group "{desc}" ({cid}) requires a test: or count: proof.'
                )
    return errors


def check_experimental(readme_path, crates_dir, rows=None):
    """Every experimental, opt-in, or standalone section must say where its module runs."""
    errors = []
    readme_text = readme_path.read_text(encoding='utf-8')
    sections = list(experimental_sections(readme_text))
    if not sections:
        errors.append('no README section carries the experimental marker; '
                      'the marker text may have changed')
    for heading, body in sections:
        if WHERE_IT_RUNS not in body:
            errors.append(
                f'README section "{heading}" is marked experimental but has no '
                f'"Where it runs" line. Say whether anything imports the module '
                f'it describes; an unqualified description reads as a working '
                f'feature.')
    modules = experimental_modules(readme_text)
    exempt_headings = {'Experimental modules', 'Math At a Glance'}
    mod_mention_re = (
        re.compile(
            r'(?:`|::)(' + '|'.join(re.escape(m) for m in sorted(modules)) + r')\b'
        )
        if modules
        else None
    )
    for heading, body in all_readme_sections(readme_text):
        if heading in exempt_headings:
            continue
        if heading in REQUIRED_WHERE_IT_RUNS_SECTIONS and WHERE_IT_RUNS not in body:
            errors.append(
                f'README section "{heading}" describes an experimental, opt-in, '
                f'or standalone subsystem but has no "{WHERE_IT_RUNS}" line.'
            )
        elif (
            mod_mention_re is not None
            and EXPERIMENTAL_MARKER not in body
            and WHERE_IT_RUNS not in body
        ):
            hit = mod_mention_re.search(body)
            if hit:
                errors.append(
                    f'README section "{heading}" references experimental module '
                    f'`{hit.group(1)}` without "{EXPERIMENTAL_MARKER}" or '
                    f'"{WHERE_IT_RUNS}".'
                )
    errors.extend(check_math_at_a_glance(readme_text))
    errors.extend(check_constants_consistency(readme_text, rows))
    for hit in find_consumers(modules, read_sources(modules, crates_dir)):
        errors.append(
            f'experimental module now has a production consumer ({hit}); the '
            f'README says these have none, so update the table and that '
            f'module\'s "Where it runs" line')
    return sections, errors


def experimental_modules(readme_text):
    """Module names from the Experimental modules table's second column."""
    modules = set()
    in_table = False
    for line in readme_text.splitlines():
        if line.startswith('## '):
            in_table = line.strip() == '## Experimental modules'
            continue
        if in_table and line.startswith('| `'):
            parts = [c.strip() for c in line.strip('|').split('|')]
            if len(parts) >= 2 and parts[1].startswith('`'):
                modules.add(parts[1].strip('`'))
    return modules


def check_proof_grammar_sync(ledger_text):
    """Verify PROOF_GRAMMAR matches the `## Proof grammar` section of docs/claims-ledger.md."""
    errors = []
    if PROOF_GRAMMAR not in ledger_text:
        errors.append(
            'PROOF_GRAMMAR in scripts/check_readme_claims.py is out of sync with '
            '`## Proof grammar` in docs/claims-ledger.md.'
        )
    return errors


def load_bead_statuses(issues_path):
    """Read `.beads/issues.jsonl` and return `{bead_id: status}`."""
    statuses = {}
    if not issues_path or not issues_path.exists():
        return statuses
    try:
        for line in issues_path.read_text(encoding='utf-8', errors='replace').splitlines():
            stripped = line.strip()
            if not stripped:
                continue
            try:
                obj = json.loads(stripped)
            except ValueError:
                continue
            bid = obj.get('id')
            status = obj.get('status')
            if bid and status:
                statuses[bid] = status
    except OSError:
        pass
    return statuses


def parse_allowlist(text, open_beads=None):
    """Parse `docs/claims-allowlist.txt` and validate `<doc>:<pattern> # <bead-id> <reason>`.

    If `open_beads` (set of open/in_progress bead IDs) is supplied, any entry
    whose bead is not in `open_beads` produces an error.
    """
    entries = []
    errors = []
    for line_no, raw in enumerate(text.splitlines(), 1):
        stripped = raw.strip()
        if not stripped or stripped.startswith('#'):
            continue
        match = ALLOWLIST_LINE_RE.match(stripped)
        if not match:
            errors.append(
                f'allowlist line {line_no}: expected `<doc>:<line-or-pattern> # <bead-id> <reason>`, '
                f'got `{stripped}`'
            )
            continue
        doc, pattern, bead, reason = (
            match.group(1).strip(),
            match.group(2).strip(),
            match.group(3).strip(),
            match.group(4).strip(),
        )
        if open_beads is not None and bead not in open_beads:
            errors.append(
                f'allowlist line {line_no}: `{doc}:{pattern}` cites closed or unknown bead `{bead}`'
            )
        entries.append(
            {
                'line': line_no,
                'doc': doc,
                'pattern': pattern,
                'bead': bead,
                'reason': reason,
            }
        )
    return entries, errors


def check_owners(rows, bead_statuses):
    """Verify every ledger row's `owner` exists, and pending rows point at open beads."""
    errors = []
    if not bead_statuses:
        return errors
    open_statuses = {'open', 'in_progress'}
    for row in rows:
        owner = row.get('owner', '-').strip()
        ident = row.get('id', '?')
        status = row.get('status', '')
        if owner == '-':
            if row.get('decision') == 'CODE' or status in {'pending-code', 'pending-doc'}:
                errors.append(f'{ident}: row with status `{status}` requires a bead owner')
            continue
        if owner not in bead_statuses:
            errors.append(f'{ident}: owner bead `{owner}` does not exist in .beads/issues.jsonl')
            continue
        if status in {'pending-code', 'pending-doc'} and bead_statuses[owner] not in open_statuses:
            errors.append(
                f'{ident}: status is `{status}` but owner bead `{owner}` is `{bead_statuses[owner]}` '
                f'(must be open or in_progress)'
            )
    return errors


def check_all_proofs(rows, crates_dir, root, today=None):
    """Run full proof verification (`test:`, `path:`, `ident:`, `manual:`, `bead:`)."""
    errors = list(check_proof_refs(rows, crates_dir, root))
    today = today or date.today()

    # Collect searchable corpus for `ident:` proofs lazily on demand.
    ident_Needles = {
        proof.partition(':')[2].strip()
        for row in rows
        for proof in row['proof'].split('; ')
        if proof.startswith('ident:') and proof.partition(':')[2].strip()
    }
    found_idents = set()
    if ident_Needles and root.exists():
        search_files = list(crates_dir.glob('*/**/*.rs')) if crates_dir.exists() else []
        for extra in ('README.md', 'AGENTS.md'):
            candidate = root / extra
            if candidate.exists():
                search_files.append(candidate)
        docs_dir = root / 'docs'
        if docs_dir.exists():
            search_files.extend(docs_dir.glob('**/*.md'))
        remaining = set(ident_Needles)
        for path in search_files:
            if not remaining:
                break
            try:
                content = path.read_text(encoding='utf-8', errors='replace')
            except OSError:
                continue
            matched = {needle for needle in remaining if needle in content}
            found_idents.update(matched)
            remaining -= matched

    for row in rows:
        ident = row['id']
        status = row['status']
        proofs = [p.strip() for p in row['proof'].split('; ') if p.strip()]
        non_manual_kinds = {
            p.partition(':')[0]
            for p in proofs
            if ':' in p and p.partition(':')[0] not in {'manual', 'bead'}
        }
        for proof in proofs:
            kind, _, payload = proof.partition(':')
            payload = payload.strip()
            if kind == 'ident' and ident_Needles and root.exists():
                if payload not in found_idents:
                    errors.append(
                        f'{ident}: cites `ident:{payload}`, which was not found in crates/ or docs.'
                    )
            elif kind == 'manual':
                day_str, _, _ = payload.partition(':')
                if iso_date(day_str) and status == 'proven' and not non_manual_kinds:
                    age_days = (today - date.fromisoformat(day_str)).days
                    if age_days > MANUAL_EXPIRY_DAYS:
                        errors.append(
                            f'{ident}: proven row relies solely on expired manual proof '
                            f'`{proof}` ({age_days} days old > {MANUAL_EXPIRY_DAYS}).'
                        )
            elif kind == 'bead' and status == 'proven':
                errors.append(
                    f'{ident}: proven row cannot cite unresolved `{proof}`.'
                )
    return errors


def check_doc_locations(rows, root, docs_list=None, allowlist_entries=None):
    """Verify ledger location files exist and S/N anchors resolve in scanned docs."""
    errors = []
    doc_cache = {}
    allowlisted_patterns = {}
    for entry in allowlist_entries or []:
        allowlisted_patterns.setdefault(entry['doc'], set()).add(entry['pattern'])

    for row in rows:
        ident = row['id']
        loc_file, _, anchor = row['location'].partition(' :: ')
        loc_file = loc_file.strip().split(':')[0]
        anchor = anchor.strip()
        if docs_list and loc_file not in docs_list:
            continue
        target = root / loc_file
        if loc_file not in doc_cache:
            if not target.exists():
                errors.append(f'{ident}: location file `{loc_file}` does not exist.')
                doc_cache[loc_file] = None
                continue
            try:
                doc_cache[loc_file] = target.read_text(encoding='utf-8', errors='replace')
            except OSError:
                doc_cache[loc_file] = None
                continue
        content = doc_cache[loc_file]
        if content is None:
            continue
        # S and N rows track current headings/capabilities in live docs (unless retracted or allowlisted).
        if ident.startswith(('S', 'N')) and row['status'] == 'proven':
            if anchor not in content and anchor not in allowlisted_patterns.get(loc_file, set()):
                errors.append(
                    f'{ident}: anchor `{anchor}` not found in `{loc_file}`.'
                )
    return errors


def extract_markdown_rust_block(md_text, section_heading):
    """Extract the first ```rust fenced block under `## <section_heading>`."""
    in_section = False
    in_rust = False
    code_lines = []
    heading_re = re.compile(rf'^##+\s+{re.escape(section_heading)}(?:\b.*)?$')
    for line in md_text.splitlines():
        if heading_re.match(line.strip()):
            in_section = True
            continue
        if in_section:
            if line.startswith('## ') and not in_rust:
                break
            if not in_rust and line.strip().startswith('```rust'):
                in_rust = True
                continue
            if in_rust:
                if line.strip() == '```':
                    return '\n'.join(code_lines)
                code_lines.append(line)
    return None


def check_example_identity(md_path, section_heading, rs_path):
    """Compare the first ```rust block under `section_heading` in `md_path` against `rs_path`."""
    md_text = Path(md_path).read_text(encoding='utf-8')
    rs_text = Path(rs_path).read_text(encoding='utf-8')
    block = extract_markdown_rust_block(md_text, section_heading)
    if block is None:
        return False, [f'No ```rust block found under heading "{section_heading}" in {md_path}']
    norm_md = [line.rstrip() for line in block.strip().splitlines()]
    norm_rs = [line.rstrip() for line in rs_text.strip().splitlines()]
    if norm_md == norm_rs:
        return True, []
    diff = list(
        difflib.unified_diff(
            norm_rs,
            norm_md,
            fromfile=str(rs_path),
            tofile=f'{md_path}::{section_heading}',
            lineterm='',
        )
    )
    return False, diff


WIDGET_IMPL_RE = re.compile(
    r'\bimpl(?:\s*<[^>]*>)?\s+(?:[\w:]+::)?(StatefulWidget|Widget)\s+for\s+([A-Za-z_][A-Za-z0-9_]*)'
)


def count_widgets(crates_dir):
    """Count Widget and StatefulWidget implementations in ftui-widgets and workspace-wide."""
    ftui_widgets_impls = 0
    ftui_widgets_types = set()
    workspace_impls = 0
    workspace_types = set()
    for rs_file in sorted(Path(crates_dir).glob('*/src/**/*.rs')):
        if rs_file.name.endswith('_test.rs') or rs_file.name == 'tests.rs':
            continue
        try:
            raw = rs_file.read_text(encoding='utf-8', errors='replace')
        except OSError:
            continue
        prod = strip_test_regions(raw)
        crate_name = rs_file.relative_to(crates_dir).parts[0]
        for match in WIDGET_IMPL_RE.finditer(prod):
            type_name = match.group(2)
            workspace_impls += 1
            workspace_types.add((crate_name, type_name))
            if crate_name == 'ftui-widgets':
                ftui_widgets_impls += 1
                ftui_widgets_types.add(type_name)
    return {
        'ftui_widgets_impls': ftui_widgets_impls,
        'ftui_widgets_types': len(ftui_widgets_types),
        'workspace_impls': workspace_impls,
        'workspace_types': len(workspace_types),
    }


def count_lines(crates_dir):
    """Count Rust source lines and files under `crates/`."""
    total_lines = 0
    total_files = 0
    for rs_file in sorted(Path(crates_dir).glob('*/**/*.rs')):
        try:
            text = rs_file.read_text(encoding='utf-8', errors='replace')
        except OSError:
            continue
        total_files += 1
        total_lines += len(text.splitlines())
    return {'files': total_files, 'lines': total_lines}


def check_telemetry_literals(readme_path, crates_dir):
    """Verify evidence-sink event names in README.md exist in crates/."""
    readme_text = Path(readme_path).read_text(encoding='utf-8')
    schema_path = Path(crates_dir) / 'ftui-runtime/src/telemetry_schema.rs'
    corpus = ''
    if schema_path.exists():
        corpus += schema_path.read_text(encoding='utf-8', errors='replace')
    for extra in Path(crates_dir).glob('*/src/**/*.rs'):
        try:
            corpus += '\n' + extra.read_text(encoding='utf-8', errors='replace')
        except OSError:
            continue
    errors = []
    in_evidence = False
    for line in readme_text.splitlines():
        if line.startswith('## ') or line.startswith('### '):
            in_evidence = 'Evidence Sink' in line
            continue
        if in_evidence and line.startswith('| `'):
            parts = [c.strip() for c in line.strip('|').split('|')]
            if parts and parts[0].startswith('`') and parts[0].endswith('`'):
                event_name = parts[0].strip('`')
                if f'"{event_name}"' not in corpus:
                    errors.append(
                        f'README Evidence Sink event `{event_name}` not found as string literal in crates/'
                    )
    return errors


def check_bench_block_hash(root):
    """Verify README benchmark block matches docs/perf artifacts via render_perf_readme.py."""
    scripts_dir = str(Path(__file__).resolve().parent)
    added = scripts_dir not in sys.path
    if added:
        sys.path.insert(0, scripts_dir)
    try:
        import render_perf_readme
        root_p = Path(root)
        perf_dir = root_p / 'docs/perf'
        rc = render_perf_readme.run_tool(
            perf_dir / 'regression_report_2026-10-19.jsonl',
            perf_dir / 'bytes_report_2026-10-19.json',
            perf_dir / 'alloc_report_2026-10-19.json',
            perf_dir / 'baseline_2026-10-19.md',
            root_p / 'README.md',
            perf_dir / 'README_BLOCK.sha256',
            check_only=True,
        )
        if rc != 0:
            return ['README.md benchmark block or docs/perf baseline drifted']
        return []
    except Exception as exc:  # pragma: no cover
        return [f'bench block check failed: {exc}']
    finally:
        if added and scripts_dir in sys.path:
            sys.path.remove(scripts_dir)


def check_compat_matrix_fresh(root):
    """Verify every E2E script path cited in docs/compat-matrix.md exists."""
    matrix_path = Path(root) / 'docs/compat-matrix.md'
    if not matrix_path.exists():
        matrix_path = Path(root) / 'docs/compatibility-matrix.md'
    if not matrix_path.exists():
        return ['docs/compat-matrix.md does not exist']
    text = matrix_path.read_text(encoding='utf-8')
    errors = []
    for match in re.finditer(r'\b(tests/e2e/scripts/[A-Za-z0-9_.-]+\.(?:sh|py))\b', text):
        rel = match.group(1)
        if not (Path(root) / rel).exists():
            errors.append(f'{matrix_path.name} cites missing E2E script `{rel}`')
    return errors


def run_full_check(
    ledger_path,
    readme_path,
    crates_dir,
    allowlist_path=None,
    issues_path=None,
    docs_list=None,
    today=None,
):
    """Run the complete G06 claims gate (`--check`)."""
    root = Path(crates_dir).resolve().parent
    ledger_text = Path(ledger_path).read_text(encoding='utf-8')
    rows, errors = validate(ledger_text)
    errors.extend(check_proof_grammar_sync(ledger_text))
    if not errors:
        errors.extend(check_all_proofs(rows, Path(crates_dir), root, today=today))
    bead_statuses = load_bead_statuses(Path(issues_path)) if issues_path else {}
    if bead_statuses:
        errors.extend(check_owners(rows, bead_statuses))
    open_beads = (
        {bid for bid, st in bead_statuses.items() if st in {'open', 'in_progress'}}
        if bead_statuses
        else None
    )
    allowlist_entries = []
    if allowlist_path and Path(allowlist_path).exists():
        allow_text = Path(allowlist_path).read_text(encoding='utf-8')
        allowlist_entries, allow_errors = parse_allowlist(allow_text, open_beads=open_beads)
        errors.extend(allow_errors)
    if Path(readme_path).exists():
        _, exp_errors = check_experimental(Path(readme_path), Path(crates_dir), rows)
        errors.extend(exp_errors)
    errors.extend(check_doc_locations(rows, root, docs_list=docs_list, allowlist_entries=allowlist_entries))

    statuses = dict(Counter(r['status'] for r in rows))
    total = len(rows)
    proven = statuses.get('proven', 0)
    proven_ratio = (proven / total) if total else 0.0
    if total and proven_ratio < MIN_PROVEN_RATIO:
        errors.append(
            f'proven ratio {proven}/{total} ({proven_ratio:.1%}) is below minimum '
            f'{MIN_PROVEN_RATIO:.0%} threshold'
        )
    summary = {
        'scope': 'full-check',
        'ledger': str(ledger_path),
        'rows': total,
        'statuses': statuses,
        'proven_ratio': round(proven_ratio, 4),
        'allowlisted': len(allowlist_entries),
        'coverage_verified': True,
        'proofs_verified': not bool(errors),
        'errors': errors,
    }
    return summary, errors


def self_test():
    header = '| ' + ' | '.join(COLUMNS) + ' |\n|' + '---|' * 9 + '\n'
    row = '| C01 | A \\| B | README.md :: A | api | CODE | bd-example | bead:bd-example | pending-code | - |\n'
    valid, errors = validate(header + row)
    assert not errors and valid[0]['claim'] == 'A | B'
    path_only_proven = (
        '| S25 | E-Graph | README.md :: E-Graph | status | DOC | bd-example '
        '| path:docs/perf/egraph_vs_flex_2026-09-18.md | proven | 2026-09-18 |\n'
    )
    ident_only_proven = (
        '| S25 | E-Graph | README.md :: E-Graph | status | DOC | bd-example '
        '| ident:solve_layout | proven | 2026-09-18 |\n'
    )
    fixtures = [('', 'missing header'), (header, 'empty table'),
                (header + row + row, 'duplicate'),
                (header + row.replace('pending-code', 'working'), 'invalid enum'),
                (header + row.replace('bd-example | bead', '- | bead'), 'missing owner'),
                (header + row.replace('README.md :: A', 'README.md:42'), 'missing anchor'),
                (header + row.replace('bead:bd-example', 'unknown:x'), 'invalid proof'),
                (header + row.replace('bead:bd-example', 'test:crate:name'), 'test delimiter'),
                (header + row.replace(' | - |', ' | 2026-02-30 |'), 'invalid date'),
                (header + row.replace('pending-code', 'proven'), 'undated proof'),
                (header + path_only_proven, 'path-only proven inflation'),
                (header + ident_only_proven, 'ident-only proven inflation'),
                (header + row.replace(' | api', ' | extra | api'), 'extra column'),
                (header + row + '\n' + row, 'stray row')]
    for text, label in fixtures:
        assert validate(text)[1], label

    # experimental_sections: splits on ### as well as ##, so a section with two
    # marked subsections cannot be covered by one "Where it runs" line.
    marked = '\n**Status: experimental**\n\nprose\n'
    readme = ('## Plain\n\nprose\n'
              '## Feature\n' + marked +
              '### Sub A\n' + marked + '**Where it runs: nowhere.**\n'
              '### Sub B\n' + marked)
    found = [heading for heading, _ in experimental_sections(readme)]
    assert found == ['Feature', 'Sub A', 'Sub B'], found
    missing = [h for h, body in experimental_sections(readme)
               if WHERE_IT_RUNS not in body]
    assert missing == ['Feature', 'Sub B'], missing

    # check_math_at_a_glance and check_constants_consistency
    bad_math = (
        '### Math At a Glance\n'
        '| Technique | Where It’s Used | Core Formula | Impact |\n'
        '|---|---|---|---|\n'
        '| **GRAPA** | Adaptive e-process | formula | fast |\n'
    )
    assert check_math_at_a_glance(bad_math), 'unqualified GRAPA row must fail'
    good_math = bad_math.replace('Adaptive e-process', 'Adaptive e-process (experimental)')
    assert not check_math_at_a_glance(good_math), check_math_at_a_glance(good_math)
    assert check_constants_consistency('Steady (200ms delay)\nBurst (20ms delay)')
    assert not check_constants_consistency('Steady (16ms delay)\nBurst (40ms delay)')

    # experimental_modules: second column of the Experimental modules table
    # only, and not tables that happen to follow other headings.
    table = ('## Experimental modules\n'
             '| Crate | Module | What | Status |\n'
             '| `ftui-runtime` | `ivm` | words | `experimental` |\n'
             '| `ftui-render` | `roaring_bitmap` | words | `experimental` |\n'
             '## Something else\n'
             '| `ftui-core` | `not_experimental` | words | `stable` |\n')
    assert experimental_modules(table) == {'ivm', 'roaring_bitmap'}, \
        experimental_modules(table)

    # find_consumers: a consumer check that cannot fire is worse than none, and
    # the prefilter added for speed is exactly the kind of change that could
    # silently disable it. In memory, so --self-test still writes nothing and
    # deletes nothing.
    mods = {'ivm', 'roaring_bitmap'}
    assert find_consumers(mods, [('a.rs', 'fn f() {}\n')]) == []
    assert find_consumers(mods, [('a.rs', 'use crate::ivm::ViewId;\n')]) == \
        ['ivm <- a.rs']
    assert find_consumers(mods, [('a.rs', 'let x = ftui_runtime::ivm::fx_hash(1);\n')]) == \
        ['ivm <- a.rs']
    # Cross-crate, non-runtime: the table owns ftui-render::roaring_bitmap too.
    assert find_consumers(mods, [
        ('a.rs', 'use ftui_render::roaring_bitmap::RoaringBitmap;\n')
    ]) == ['roaring_bitmap <- a.rs']
    # A use inside #[cfg(test)] is not a production consumer.
    assert find_consumers(mods, [
        ('a.rs', 'fn f() {}\n#[cfg(test)]\nmod t {\n    use crate::ivm::ViewId;\n}\n')
    ]) == []
    # A name that merely contains the module name is not a use of it.
    assert find_consumers(mods, [('a.rs', 'use crate::ivm_helpers::X;\n')]) == []
    # First consumer wins, and every consumed module is reported once.
    assert find_consumers(mods, [('a.rs', 'use crate::ivm::A;\n'),
                                 ('b.rs', 'use crate::ivm::B;\n'),
                                 ('c.rs', 'use crate::roaring_bitmap::C;\n')]) == \
        ['ivm <- a.rs', 'roaring_bitmap <- c.rs']

    # check_proof_refs: a fabricated `test:` proof reads as settled evidence,
    # so the check that catches it must itself be covered. In memory via a
    # stub crates dir is not possible here (it globs the filesystem), so these
    # exercise the pure decision against a known name set.
    import types
    fake = types.SimpleNamespace(glob=lambda _pattern: iter(()))
    # With no sources discovered the check stays silent rather than condemning
    # every row -- a missing crates/ must not produce 52 false accusations.
    assert check_proof_refs([{'id': 'C01', 'proof': 'test:x::nope'}],
                            fake, Path('.')) == []

    # The path rules, against a stub index. Both `crate::file::name` and the
    # true Rust path `crate::file::tests::name` are legitimate in the ledger,
    # and an earlier version of this check condemned fourteen correct proofs by
    # assuming the file stem was always the second-to-last segment.
    # Two crates, so `ftui-widgets` counts as *known* -- the crate rule only
    # polices a segment that names a crate the index has actually seen, so a
    # one-crate fixture would silently skip the case it means to cover.
    real = {'alpha': {('ftui-core', 'gesture')},
            'beta': {('ftui-widgets', 'input')}}
    original = globals()['collect_test_fn_names']
    globals()['collect_test_fn_names'] = lambda _d: real
    try:
        ok = ['test:ftui-core::gesture::alpha',
              'test:ftui-core::gesture::tests::alpha',
              'test:ftui-core::sub::gesture::tests::alpha',
              'test:alpha']
        for proof in ok:
            assert check_proof_refs([{'id': 'C01', 'proof': proof}],
                                    fake, Path('.')) == [], proof
        bad = ['test:ftui-core::gesture::missing',      # no such fn
               'test:ftui-widgets::gesture::alpha',     # real crate, not this one
               'test:ftui-core::hover::tests::alpha']   # no segment names gesture
        for proof in bad:
            assert check_proof_refs([{'id': 'C01', 'proof': proof}],
                                    fake, Path('.')) != [], proof
    finally:
        globals()['collect_test_fn_names'] = original

    print(json.dumps({'scope': 'schema-self-test', 'passed': len(fixtures) + 22}))


def main():
    root_default = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument('--schema-check', action='store_true')
    action.add_argument('--self-test', action='store_true')
    action.add_argument('--experimental-check', action='store_true')
    action.add_argument('--proof-refs', action='store_true')
    action.add_argument('--check', action='store_true')
    action.add_argument('--report', action='store_true')
    action.add_argument('--verify-owners', action='store_true')
    action.add_argument('--example-identity', nargs=3, metavar=('MD', 'SECTION', 'RS'))
    action.add_argument('--count-widgets', action='store_true')
    action.add_argument('--count-lines', action='store_true')
    action.add_argument('--telemetry-literals', action='store_true')
    action.add_argument('--bench-block-hash', action='store_true')
    action.add_argument('--compat-matrix-fresh', action='store_true')
    parser.add_argument('--ledger', type=Path, default=root_default / 'docs/claims-ledger.md')
    parser.add_argument('--readme', type=Path, default=root_default / 'README.md')
    parser.add_argument('--crates', type=Path, default=root_default / 'crates')
    parser.add_argument('--allowlist', type=Path, default=root_default / 'docs/claims-allowlist.txt')
    parser.add_argument('--issues', type=Path, default=root_default / '.beads/issues.jsonl')
    parser.add_argument('--docs', type=str, default=None, help='Comma-separated doc paths to check')
    parser.add_argument('--json', action='store_true', help='Emit JSON report output')
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return 0
    if args.example_identity:
        md_path, section, rs_path = args.example_identity
        ok, diff = check_example_identity(md_path, section, rs_path)
        print(json.dumps({'scope': 'example-identity', 'ok': ok, 'diff': diff}))
        for line in diff:
            print(line, file=sys.stderr)
        return 0 if ok else 1
    if args.count_widgets:
        print(json.dumps({'scope': 'count-widgets', **count_widgets(args.crates)}))
        return 0
    if args.count_lines:
        print(json.dumps({'scope': 'count-lines', **count_lines(args.crates)}))
        return 0
    if args.telemetry_literals:
        errors = check_telemetry_literals(args.readme, args.crates)
        print(json.dumps({'scope': 'telemetry-literals', 'errors': errors}))
        for err in errors:
            print(f'[claims] {err}', file=sys.stderr)
        return int(bool(errors))
    if args.bench_block_hash:
        errors = check_bench_block_hash(args.crates.parent)
        print(json.dumps({'scope': 'bench-block-hash', 'errors': errors}))
        for err in errors:
            print(f'[claims] {err}', file=sys.stderr)
        return int(bool(errors))
    if args.compat_matrix_fresh:
        errors = check_compat_matrix_fresh(args.crates.parent)
        print(json.dumps({'scope': 'compat-matrix-fresh', 'errors': errors}))
        for err in errors:
            print(f'[claims] {err}', file=sys.stderr)
        return int(bool(errors))
    if args.verify_owners:
        rows, schema_errors = validate(args.ledger.read_text(encoding='utf-8'))
        if schema_errors:
            for err in schema_errors:
                print(f'[claims] {err}', file=sys.stderr)
            return 1
        bead_statuses = load_bead_statuses(args.issues)
        errors = check_owners(rows, bead_statuses)
        print(json.dumps({'scope': 'verify-owners', 'rows': len(rows), 'errors': errors}))
        for err in errors:
            print(f'[claims] {err}', file=sys.stderr)
        return int(bool(errors))
    if args.check or args.report:
        docs_list = (
            [d.strip() for d in args.docs.split(',') if d.strip()]
            if args.docs
            else None
        )
        try:
            summary, errors = run_full_check(
                args.ledger,
                args.readme,
                args.crates,
                allowlist_path=args.allowlist,
                issues_path=args.issues,
                docs_list=docs_list,
            )
        except (OSError, UnicodeError) as error:
            print(f'[claims] {error}', file=sys.stderr)
            return 1
        if args.report:
            summary['scope'] = 'report'
        print(json.dumps(summary))
        if args.check:
            for error in errors:
                print(f'[claims] {error}', file=sys.stderr)
            return int(bool(errors))
        return 0
    if args.proof_refs:
        try:
            rows, schema_errors = validate(args.ledger.read_text(encoding='utf-8'))
        except (OSError, UnicodeError) as error:
            print(f'[claims] {error}', file=sys.stderr)
            return 1
        if schema_errors:
            print('[claims] fix --schema-check errors first', file=sys.stderr)
            return 1
        errors = check_proof_refs(rows, args.crates, args.crates.parent)
        cited = sum(
            1 for r in rows for p in r['proof'].split('; ')
            if p.startswith(('test:', 'path:'))
        )
        print(json.dumps({'scope': 'proof-refs', 'cited': cited,
                          'errors': errors}))
        for error in errors:
            print(f'[claims] {error}', file=sys.stderr)
        return int(bool(errors))
    if args.experimental_check:
        try:
            rows, _ = validate(args.ledger.read_text(encoding='utf-8'))
            sections, errors = check_experimental(args.readme, args.crates, rows)
        except (OSError, UnicodeError) as error:
            print(f'[claims] {error}', file=sys.stderr)
            return 1
        print(json.dumps({'scope': 'experimental-sections',
                          'sections': [h for h, _ in sections],
                          'errors': errors}))
        for error in errors:
            print(f'[claims] {error}', file=sys.stderr)
        return int(bool(errors))
    try:
        rows, errors = validate(args.ledger.read_text(encoding='utf-8'))
    except (OSError, UnicodeError) as error:
        print(f'[claims] {error}', file=sys.stderr)
        return 1
    print(json.dumps({'scope': 'schema-only', 'ledger': str(args.ledger),
                      'rows': len(rows), 'statuses': dict(Counter(r['status'] for r in rows)),
                      'errors': errors, 'coverage_verified': False, 'proofs_verified': False}))
    return int(bool(errors))


if __name__ == '__main__':
    sys.exit(main())
