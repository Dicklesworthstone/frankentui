#!/usr/bin/env python3
"""Unit tests for scripts/check_readme_claims.py (G06 / bd-g00-root-epic-ewths.5.4).

In-memory and read-only against the repository tree; never creates or deletes
temporary files (Rule 1).
"""

from datetime import date
from pathlib import Path
import sys
import types
import unittest

SCRIPTS_DIR = Path(__file__).resolve().parents[1]
ROOT_DIR = SCRIPTS_DIR.parent
if str(SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIR))

import check_readme_claims as crc  # noqa: E402


HEADER = '| ' + ' | '.join(crc.COLUMNS) + ' |\n|' + '---|' * 9 + '\n'
BASE_ROW = (
    '| C01 | A \\| B | README.md :: A | api | CODE | bd-example '
    '| bead:bd-example | pending-code | - |\n'
)


class TestCheckReadmeClaims(unittest.TestCase):
    def test_valid_ledger_passes_schema(self):
        rows, errors = crc.validate(HEADER + BASE_ROW)
        self.assertEqual(errors, [])
        self.assertEqual(len(rows), 1)

    def test_escaped_pipe_in_claim_cell(self):
        rows, errors = crc.validate(HEADER + BASE_ROW)
        self.assertEqual(errors, [])
        self.assertEqual(rows[0]['claim'], 'A | B')

    def test_missing_header_fails(self):
        _, errors = crc.validate('')
        self.assertTrue(any('missing canonical claims table header' in e for e in errors))

    def test_empty_table_fails(self):
        _, errors = crc.validate(HEADER)
        self.assertTrue(any('claims table contains no rows' in e for e in errors))

    def test_duplicate_claim_id_fails(self):
        _, errors = crc.validate(HEADER + BASE_ROW + BASE_ROW)
        self.assertTrue(any('duplicate claim ID' in e for e in errors))

    def test_invalid_enum_status_fails(self):
        bad = BASE_ROW.replace('pending-code', 'working')
        _, errors = crc.validate(HEADER + bad)
        self.assertTrue(any('invalid status' in e for e in errors))

    def test_missing_owner_on_code_row_fails(self):
        bad = BASE_ROW.replace('bd-example | bead', '- | bead')
        _, errors = crc.validate(HEADER + bad)
        self.assertTrue(any('CODE row requires an owner' in e for e in errors))

    def test_missing_location_anchor_fails(self):
        bad = BASE_ROW.replace('README.md :: A', 'README.md:42')
        _, errors = crc.validate(HEADER + bad)
        self.assertTrue(any('location requires filename :: text anchor' in e for e in errors))

    def test_invalid_proof_kind_fails(self):
        bad = BASE_ROW.replace('bead:bd-example', 'unknown:x')
        _, errors = crc.validate(HEADER + bad)
        self.assertTrue(any('invalid proof syntax' in e for e in errors))

    def test_invalid_test_proof_delimiter_fails(self):
        bad = BASE_ROW.replace('bead:bd-example', 'test:crate:name')
        _, errors = crc.validate(HEADER + bad)
        self.assertTrue(any('invalid test proof' in e for e in errors))

    def test_invalid_verification_date_fails(self):
        bad = BASE_ROW.replace(' | - |', ' | 2026-02-30 |')
        _, errors = crc.validate(HEADER + bad)
        self.assertTrue(any('invalid verification date' in e for e in errors))

    def test_proven_without_date_fails(self):
        bad = BASE_ROW.replace('bead:bd-example | pending-code', 'test:c::t | proven')
        _, errors = crc.validate(HEADER + bad)
        self.assertTrue(any('proven row requires verification date' in e for e in errors))

    def test_path_only_proven_inflation_fails(self):
        row = (
            '| S25 | E-Graph | README.md :: E-Graph | status | DOC | bd-example '
            '| path:docs/perf/egraph_vs_flex_2026-09-18.md | proven | 2026-09-18 |\n'
        )
        _, errors = crc.validate(HEADER + row)
        self.assertTrue(any('proof-class inflation' in e for e in errors))

    def test_ident_only_proven_inflation_fails(self):
        row = (
            '| S25 | E-Graph | README.md :: E-Graph | status | DOC | bd-example '
            '| ident:solve_layout | proven | 2026-09-18 |\n'
        )
        _, errors = crc.validate(HEADER + row)
        self.assertTrue(any('proof-class inflation' in e for e in errors))

    def test_bead_proof_on_proven_row_fails(self):
        row = (
            '| C01 | Claim | README.md :: Claim | api | CODE | bd-example '
            '| bead:bd-example | proven | 2026-09-19 |\n'
        )
        _, errors = crc.validate(HEADER + row)
        self.assertTrue(any('unresolved bead: proof' in e for e in errors))

    def test_proof_grammar_sync_matches_real_ledger(self):
        ledger_text = (ROOT_DIR / 'docs/claims-ledger.md').read_text(encoding='utf-8')
        self.assertEqual(crc.check_proof_grammar_sync(ledger_text), [])

    def test_proof_grammar_drift_detected(self):
        self.assertNotEqual(crc.check_proof_grammar_sync('# Claims ledger\n'), [])

    def test_allowlist_valid_and_malformed_lines(self):
        text = (
            '# comment\n'
            'README.md:Some pattern # bd-open-1 temporary exemption\n'
            'malformed line without hash\n'
        )
        entries, errors = crc.parse_allowlist(text, open_beads={'bd-open-1'})
        self.assertEqual(len(entries), 1)
        self.assertEqual(entries[0]['doc'], 'README.md')
        self.assertEqual(entries[0]['bead'], 'bd-open-1')
        self.assertEqual(len(errors), 1)
        self.assertIn('allowlist line 3', errors[0])

    def test_allowlist_rejects_closed_or_unknown_bead(self):
        text = 'README.md:Pattern # bd-closed-99 stale entry\n'
        _, errors = crc.parse_allowlist(text, open_beads={'bd-open-1'})
        self.assertEqual(len(errors), 1)
        self.assertIn('closed or unknown bead', errors[0])

    def test_check_owners_rejects_closed_bead_on_pending_row(self):
        rows = [
            {
                'id': 'V99',
                'decision': 'CODE',
                'owner': 'bd-closed',
                'status': 'pending-code',
            },
            {
                'id': 'V98',
                'decision': 'CODE',
                'owner': 'bd-open',
                'status': 'pending-code',
            },
            {
                'id': 'V97',
                'decision': 'DOC',
                'owner': 'bd-closed',
                'status': 'retracted',
            },
        ]
        statuses = {'bd-closed': 'closed', 'bd-open': 'open'}
        errors = crc.check_owners(rows, statuses)
        self.assertEqual(len(errors), 1)
        self.assertIn('V99', errors[0])

    def test_manual_proof_90_day_expiry_on_proven_row(self):
        fake_crates = types.SimpleNamespace(
            glob=lambda _p: iter(()),
            exists=lambda: False,
        )
        fake_root = types.SimpleNamespace(exists=lambda: False)
        expired_row = {
            'id': 'C11',
            'status': 'proven',
            'proof': 'manual:2026-01-01:Alice',
        }
        retracted_row = {
            'id': 'C12',
            'status': 'retracted',
            'proof': 'manual:2026-01-01:Alice',
        }
        errs_proven = crc.check_all_proofs(
            [expired_row], fake_crates, fake_root, today=date(2026, 6, 1)
        )
        self.assertEqual(len(errs_proven), 1)
        self.assertIn('expired manual proof', errs_proven[0])
        errs_retracted = crc.check_all_proofs(
            [retracted_row], fake_crates, fake_root, today=date(2026, 6, 1)
        )
        self.assertEqual(errs_retracted, [])

    def test_experimental_sections_require_where_it_runs(self):
        marked = '\n**Status: experimental**\n\nprose\n'
        readme = (
            '## Plain\n\nprose\n'
            '## Feature\n'
            + marked
            + '### Sub A\n'
            + marked
            + '**Where it runs: nowhere.**\n'
            '### Sub B\n'
            + marked
        )
        missing = [
            h
            for h, body in crc.experimental_sections(readme)
            if crc.WHERE_IT_RUNS not in body
        ]
        self.assertEqual(missing, ['Feature', 'Sub B'])

    def test_find_consumers_ignores_cfg_test_and_detects_prod(self):
        mods = {'ivm', 'roaring_bitmap'}
        self.assertEqual(
            crc.find_consumers(
                mods,
                [('a.rs', 'fn f() {}\n#[cfg(test)]\nmod t { use crate::ivm::ViewId; }\n')],
            ),
            [],
        )
        self.assertEqual(
            crc.find_consumers(mods, [('a.rs', 'use crate::ivm::ViewId;\n')]),
            ['ivm <- a.rs'],
        )

    def test_example_identity_match_and_mismatch_diff(self):
        ok, diff = crc.check_example_identity(
            ROOT_DIR / 'docs/getting-started.md',
            'Minimal Inline App',
            ROOT_DIR / 'crates/ftui/examples/getting_started.rs',
        )
        self.assertTrue(ok, diff)
        self.assertEqual(diff, [])

        # Missing section produces a diagnostic diff line
        ok_bad, diff_bad = crc.check_example_identity(
            ROOT_DIR / 'docs/getting-started.md',
            'Nonexistent Section Heading',
            ROOT_DIR / 'crates/ftui/examples/getting_started.rs',
        )
        self.assertFalse(ok_bad)
        self.assertTrue(diff_bad)

    def test_count_widgets_and_lines_and_telemetry_on_repo(self):
        widgets = crc.count_widgets(ROOT_DIR / 'crates')
        self.assertGreaterEqual(widgets['ftui_widgets_types'], 55)
        self.assertGreaterEqual(widgets['workspace_impls'], 80)

        lines = crc.count_lines(ROOT_DIR / 'crates')
        self.assertGreater(lines['files'], 500)
        self.assertGreater(lines['lines'], 500_000)

        telem_errors = crc.check_telemetry_literals(
            ROOT_DIR / 'README.md', ROOT_DIR / 'crates'
        )
        self.assertEqual(telem_errors, [])

    def test_real_ledger_full_check_green(self):
        summary, errors = crc.run_full_check(
            ROOT_DIR / 'docs/claims-ledger.md',
            ROOT_DIR / 'README.md',
            ROOT_DIR / 'crates',
            allowlist_path=ROOT_DIR / 'docs/claims-allowlist.txt',
            issues_path=ROOT_DIR / '.beads/issues.jsonl',
        )
        self.assertEqual(errors, [])
        self.assertGreaterEqual(summary['proven_ratio'], crc.MIN_PROVEN_RATIO)


if __name__ == '__main__':
    unittest.main()
