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
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    try:
        import check_module_reachability
    except ImportError:  # pragma: no cover - the gate is normally present
        return text
    return check_module_reachability.strip_test_regions(text)


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
    # The other direction: a module the table calls unconsumed must stay that
    # way, or the table is the thing that has gone stale.
    #
    # "Production" here excludes the other experimental modules. Several of them
    # import each other -- resize_sla uses conformal_alert, policy_config uses
    # degradation_cascade -- which is a quarantined cluster wiring itself
    # together, not the runtime picking any of it up. Counting those as
    # consumers would make this check fire on day one and teach everyone to
    # ignore it.
    modules = experimental_modules(readme_path.read_text(encoding='utf-8'))
    quarantined_files = {f'{module}.rs' for module in modules}
    consumed = []
    for module in sorted(modules):
        pattern = re.compile(rf'use +(?:crate|ftui_runtime)::{re.escape(module)}\b'
                             rf'|(?:crate|ftui_runtime)::{re.escape(module)}::')
        for source in crates_dir.glob('*/src/**/*.rs'):
            if source.name in quarantined_files or source.name == 'lib.rs':
                continue
            try:
                text = strip_test_regions(
                    source.read_text(encoding='utf-8', errors='replace'))
                if pattern.search(text):
                    consumed.append(f'{module} <- {source.relative_to(crates_dir.parent)}')
                    break
            except OSError:
                continue
    for hit in consumed:
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

    print(json.dumps({'scope': 'schema-self-test', 'passed': len(fixtures) + 4}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument('--schema-check', action='store_true')
    action.add_argument('--self-test', action='store_true')
    action.add_argument('--experimental-check', action='store_true')
    parser.add_argument('--ledger', type=Path, default=Path(__file__).resolve().parents[1] / 'docs/claims-ledger.md')
    parser.add_argument('--readme', type=Path, default=Path(__file__).resolve().parents[1] / 'README.md')
    parser.add_argument('--crates', type=Path, default=Path(__file__).resolve().parents[1] / 'crates')
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return 0
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
