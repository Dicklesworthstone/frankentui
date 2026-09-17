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
    print(json.dumps({'scope': 'schema-self-test', 'passed': len(fixtures) + 1}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument('--schema-check', action='store_true')
    action.add_argument('--self-test', action='store_true')
    parser.add_argument('--ledger', type=Path, default=Path(__file__).resolve().parents[1] / 'docs/claims-ledger.md')
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return 0
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
