#!/usr/bin/env python3
"""Validate claims-ledger structure only; does not prove claims or extract coverage.

Read-only and stdlib-only. --schema-check is deliberately distinct from the
future full --check gate: a structurally valid pending row is not proof.
--self-test uses in-memory fixtures and performs no cleanup or compilation.
"""

import argparse
from collections import Counter
from datetime import date
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
        if row['status'] == 'proven' and verified == '-':
            problems.append('proven row requires verification date')
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


def experimental_sections(readme_text):
    """Yield (heading, body) for each README block marked experimental.

    Blocks are delimited by any heading, `##` or `###`, not just `##`. "Flake
    Detection & Sequential FDR Control" carries the marker twice, once per
    subsection; splitting only on `##` would let a single "Where it runs" line
    cover both and leave one of them unqualified.
    """
    lines = readme_text.splitlines()
    starts = [i for i, line in enumerate(lines)
              if line.startswith('## ') or line.startswith('### ')]
    for index, start in enumerate(starts):
        end = starts[index + 1] if index + 1 < len(starts) else len(lines)
        body = '\n'.join(lines[start:end])
        if EXPERIMENTAL_MARKER in body:
            yield lines[start].lstrip('#').strip(), body


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


def check_experimental(readme_path, crates_dir):
    """Every experimental section must say where its module runs, truthfully.

    On 2026-09-19 all eleven such sections described modules that no crate
    imports, in working present tense ("the runtime can enter safe mode"),
    while the Experimental modules table two thousand lines away correctly
    said "no production consumer". Readers believe the section they are
    reading. This keeps the two from drifting apart again.

    Scoped to experimental sections on purpose. The same detector was run over
    every other README section that names a module and produced 47 findings,
    all false: showcase screens are registered through an enum in `app.rs`, and
    `telemetry` reaches production as the re-exported `TelemetryConfig` rather
    than as `telemetry::`. Experimental modules are the case where path-based
    detection is sound, because being feature-gated and un-re-exported is
    exactly what forces a consumer to name the module path.
    """
    errors = []
    sections = list(experimental_sections(readme_path.read_text(encoding='utf-8')))
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
    modules = experimental_modules(readme_path.read_text(encoding='utf-8'))
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


def self_test():
    header = '| ' + ' | '.join(COLUMNS) + ' |\n|' + '---|' * 9 + '\n'
    row = '| C01 | A \\| B | README.md :: A | api | CODE | bd-example | bead:bd-example | pending-code | - |\n'
    valid, errors = validate(header + row)
    assert not errors and valid[0]['claim'] == 'A | B'
    fixtures = [('', 'missing header'), (header, 'empty table'),
                (header + row + row, 'duplicate'),
                (header + row.replace('pending-code', 'working'), 'invalid enum'),
                (header + row.replace('bd-example | bead', '- | bead'), 'missing owner'),
                (header + row.replace('README.md :: A', 'README.md:42'), 'missing anchor'),
                (header + row.replace('bead:bd-example', 'unknown:x'), 'invalid proof'),
                (header + row.replace('bead:bd-example', 'test:crate:name'), 'test delimiter'),
                (header + row.replace(' | - |', ' | 2026-02-30 |'), 'invalid date'),
                (header + row.replace('pending-code', 'proven'), 'undated proof'),
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

    print(json.dumps({'scope': 'schema-self-test', 'passed': len(fixtures) + 18}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument('--schema-check', action='store_true')
    action.add_argument('--self-test', action='store_true')
    action.add_argument('--experimental-check', action='store_true')
    action.add_argument('--proof-refs', action='store_true')
    parser.add_argument('--ledger', type=Path, default=Path(__file__).resolve().parents[1] / 'docs/claims-ledger.md')
    parser.add_argument('--readme', type=Path, default=Path(__file__).resolve().parents[1] / 'README.md')
    parser.add_argument('--crates', type=Path, default=Path(__file__).resolve().parents[1] / 'crates')
    args = parser.parse_args()
    if args.self_test:
        self_test()
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
            sections, errors = check_experimental(args.readme, args.crates)
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
