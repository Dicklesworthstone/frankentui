#!/usr/bin/env python3
"""Static workflow & DSR verification contract checker for doctor_frankentui (G22 / .28.4).

Verifies that:
1. Both `.github/workflows/ci.yml` (`doctor-frankentui-verification` job) and
   `.github/workflows/doctor_frankentui_extended.yml` use `./.github/actions/install-vhs`
   and do not contain ad-hoc `sudo install -m 0755 /tmp/vhs `.
2. Both workflows invoke all four canonical gate scripts:
   - `doctor_frankentui_happy_e2e.sh`
   - `doctor_frankentui_failure_e2e.sh`
   - `doctor_frankentui_determinism_soak.sh`
   - `doctor_frankentui_coverage.sh`
3. Both workflows upload `/tmp/doctor_frankentui_ci` artifacts with `if: always()`.
4. `.config/dsr-quality.yaml` defines `frankentui-doctor-verification` invoking the
   complete native DSR verification stack (`doctor_frankentui_core_time_budget.sh`,
   `doctor_frankentui_happy_e2e.sh`, `doctor_frankentui_failure_e2e.sh`,
   `doctor_frankentui_replay_triage.py`, `doctor_frankentui_determinism_soak.sh`,
   `doctor_frankentui_coverage.sh`, and `doctor_frankentui_validate_jsonl.py`).
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CI_YML = ROOT / ".github" / "workflows" / "ci.yml"
EXTENDED_YML = ROOT / ".github" / "workflows" / "doctor_frankentui_extended.yml"
DSR_QUALITY_YAML = ROOT / ".config" / "dsr-quality.yaml"

FOUR_GATES = (
    "doctor_frankentui_happy_e2e.sh",
    "doctor_frankentui_failure_e2e.sh",
    "doctor_frankentui_determinism_soak.sh",
    "doctor_frankentui_coverage.sh",
)

DSR_REQUIRED_SCRIPTS = (
    "doctor_frankentui_core_time_budget.sh",
    "doctor_frankentui_happy_e2e.sh",
    "doctor_frankentui_failure_e2e.sh",
    "doctor_frankentui_replay_triage.py",
    "doctor_frankentui_determinism_soak.sh",
    "doctor_frankentui_coverage.sh",
    "doctor_frankentui_validate_jsonl.py",
)


def extract_ci_doctor_job(ci_text: str) -> str:
    """Extract the `doctor-frankentui-verification:` job block from ci.yml."""
    marker = "  doctor-frankentui-verification:\n"
    idx = ci_text.find(marker)
    if idx < 0:
        return ""
    rest = ci_text[idx + len(marker) :]
    lines = [marker.rstrip("\n")]
    for line in rest.splitlines():
        if re.match(r"^  [a-zA-Z0-9_-]+:\s*$", line):
            break
        lines.append(line)
    return "\n".join(lines)


def check_workflow_block(
    workflow_label: str,
    block_text: str,
    artifact_prefix: str,
) -> list[dict[str, str]]:
    results: list[dict[str, str]] = []

    has_shared_vhs = "uses: ./.github/actions/install-vhs" in block_text
    has_manual_vhs = "sudo install -m 0755 /tmp/vhs " in block_text
    if has_shared_vhs and not has_manual_vhs:
        results.append(
            {
                "check": "doctor_jobs_use_shared_vhs_action",
                "workflow": workflow_label,
                "status": "pass",
                "detail": "uses ./.github/actions/install-vhs without manual /tmp/vhs install",
            }
        )
    else:
        detail = []
        if not has_shared_vhs:
            detail.append("missing uses: ./.github/actions/install-vhs")
        if has_manual_vhs:
            detail.append("contains forbidden sudo install -m 0755 /tmp/vhs")
        results.append(
            {
                "check": "doctor_jobs_use_shared_vhs_action",
                "workflow": workflow_label,
                "status": "fail",
                "detail": "; ".join(detail),
            }
        )

    missing_gates = [gate for gate in FOUR_GATES if gate not in block_text]
    if not missing_gates:
        results.append(
            {
                "check": "doctor_jobs_invoke_all_four_gates",
                "workflow": workflow_label,
                "status": "pass",
                "detail": "invokes " + ", ".join(FOUR_GATES),
            }
        )
    else:
        results.append(
            {
                "check": "doctor_jobs_invoke_all_four_gates",
                "workflow": workflow_label,
                "status": "fail",
                "detail": "missing gate scripts: " + ", ".join(missing_gates),
            }
        )

    upload_pattern = re.compile(
        r"if:\s*always\(\)\s*\n\s*uses:\s*actions/upload-artifact@[^\n]+\n(?:.*?\n)*?\s*path:\s*"
        + re.escape(artifact_prefix),
        re.MULTILINE,
    )
    if upload_pattern.search(block_text):
        results.append(
            {
                "check": "doctor_jobs_upload_artifact_root_always",
                "workflow": workflow_label,
                "status": "pass",
                "detail": f"uploads {artifact_prefix} with if: always()",
            }
        )
    else:
        results.append(
            {
                "check": "doctor_jobs_upload_artifact_root_always",
                "workflow": workflow_label,
                "status": "fail",
                "detail": f"missing upload-artifact step with if: always() and path: {artifact_prefix}",
            }
        )

    return results


def check_dsr_quality(dsr_text: str, workflow_label: str = ".config/dsr-quality.yaml") -> list[dict[str, str]]:
    results: list[dict[str, str]] = []
    marker = "  frankentui-doctor-verification:\n"
    idx = dsr_text.find(marker)
    if idx < 0:
        results.append(
            {
                "check": "dsr_doctor_verification_tool_contract",
                "workflow": workflow_label,
                "status": "fail",
                "detail": "missing tool frankentui-doctor-verification",
            }
        )
    else:
        rest = dsr_text[idx + len(marker) :]
        lines = [marker.rstrip("\n")]
        for line in rest.splitlines():
            if re.match(r"^  [a-zA-Z0-9_-]+:\s*$", line):
                break
            lines.append(line)
        block = "\n".join(lines)
        missing = [script for script in DSR_REQUIRED_SCRIPTS if script not in block]
        if not missing:
            results.append(
                {
                    "check": "dsr_doctor_verification_tool_contract",
                    "workflow": workflow_label,
                    "status": "pass",
                    "detail": "invokes " + ", ".join(DSR_REQUIRED_SCRIPTS),
                }
            )
        else:
            results.append(
                {
                    "check": "dsr_doctor_verification_tool_contract",
                    "workflow": workflow_label,
                    "status": "fail",
                    "detail": "missing DSR doctor checks: " + ", ".join(missing),
                }
            )

    e2e_marker = "  frankentui-render-pane-e2e:\n"
    e2e_idx = dsr_text.find(e2e_marker)
    if e2e_idx < 0:
        results.append(
            {
                "check": "dsr_render_pane_e2e_tool_contract",
                "workflow": workflow_label,
                "status": "fail",
                "detail": "missing tool frankentui-render-pane-e2e",
            }
        )
    else:
        rest = dsr_text[e2e_idx + len(e2e_marker) :]
        lines = [e2e_marker.rstrip("\n")]
        for line in rest.splitlines():
            if re.match(r"^  [a-zA-Z0-9_-]+:\s*$", line):
                break
            lines.append(line)
        block = "\n".join(lines)
        required_e2e = ("scripts/e2e_test.sh --quick", "scripts/pane_e2e.sh --mode smoke")
        missing_e2e = [cmd for cmd in required_e2e if cmd not in block]
        if not missing_e2e:
            results.append(
                {
                    "check": "dsr_render_pane_e2e_tool_contract",
                    "workflow": workflow_label,
                    "status": "pass",
                    "detail": "invokes " + ", ".join(required_e2e),
                }
            )
        else:
            results.append(
                {
                    "check": "dsr_render_pane_e2e_tool_contract",
                    "workflow": workflow_label,
                    "status": "fail",
                    "detail": "missing DSR E2E checks: " + ", ".join(missing_e2e),
                }
            )

    return results


def evaluate_all(ci_text: str, extended_text: str, dsr_text: str) -> list[dict[str, str]]:
    results: list[dict[str, str]] = []
    ci_job = extract_ci_doctor_job(ci_text)
    results.extend(
        check_workflow_block(
            ".github/workflows/ci.yml#doctor-frankentui-verification",
            ci_job,
            "/tmp/doctor_frankentui_ci/",
        )
    )
    results.extend(
        check_workflow_block(
            ".github/workflows/doctor_frankentui_extended.yml",
            extended_text,
            "/tmp/doctor_frankentui_ci_extended/",
        )
    )
    results.extend(check_dsr_quality(dsr_text))
    return results


def run_self_test() -> int:
    ci_text = CI_YML.read_text(encoding="utf-8")
    extended_text = EXTENDED_YML.read_text(encoding="utf-8")
    dsr_text = DSR_QUALITY_YAML.read_text(encoding="utf-8")

    baseline = evaluate_all(ci_text, extended_text, dsr_text)
    if any(r["status"] != "pass" for r in baseline):
        print("[self-test] baseline repository workflow check failed:", file=sys.stderr)
        for r in baseline:
            if r["status"] != "pass":
                print(" ", json.dumps(r), file=sys.stderr)
        return 1

    # Mutation 1: remove shared VHS action from ci.yml
    mut_ci_vhs = ci_text.replace("uses: ./.github/actions/install-vhs", "uses: ./.github/actions/noop")
    res1 = evaluate_all(mut_ci_vhs, extended_text, dsr_text)
    assert any(
        r["check"] == "doctor_jobs_use_shared_vhs_action" and r["status"] == "fail" for r in res1
    ), "expected failure when shared VHS action is removed"

    # Mutation 2: inject forbidden manual /tmp/vhs install into extended workflow
    mut_ext_manual = extended_text + "\n          sudo install -m 0755 /tmp/vhs /usr/local/bin/vhs\n"
    res2 = evaluate_all(ci_text, mut_ext_manual, dsr_text)
    assert any(
        r["check"] == "doctor_jobs_use_shared_vhs_action" and r["status"] == "fail" for r in res2
    ), "expected failure when manual /tmp/vhs install is present"

    # Mutation 3: drop one of the four gates from extended workflow
    mut_ext_gate = extended_text.replace("doctor_frankentui_coverage.sh", "doctor_frankentui_removed.sh")
    res3 = evaluate_all(ci_text, mut_ext_gate, dsr_text)
    assert any(
        r["check"] == "doctor_jobs_invoke_all_four_gates" and r["status"] == "fail" for r in res3
    ), "expected failure when a required gate script is removed"

    # Mutation 4: remove artifact upload path
    mut_ci_upload = ci_text.replace("path: /tmp/doctor_frankentui_ci/", "path: /tmp/other_dir/")
    res4 = evaluate_all(mut_ci_upload, extended_text, dsr_text)
    assert any(
        r["check"] == "doctor_jobs_upload_artifact_root_always" and r["status"] == "fail" for r in res4
    ), "expected failure when upload-artifact path is altered"

    # Mutation 5: remove a required script from dsr-quality.yaml
    mut_dsr = dsr_text.replace("doctor_frankentui_validate_jsonl.py", "removed_validator.py")
    res5 = evaluate_all(ci_text, extended_text, mut_dsr)
    assert any(
        r["check"] == "dsr_doctor_verification_tool_contract" and r["status"] == "fail" for r in res5
    ), "expected failure when DSR doctor verification script is removed"

    print(
        json.dumps(
            {
                "check": "check_workflows_self_test",
                "workflow": "self-test",
                "status": "pass",
                "detail": "5 seeded mutations rejected and baseline passed",
            }
        )
    )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true", help="Run seeded mutation self-tests")
    parser.add_argument("--quiet", action="store_true", help="Print only failing checks")
    args = parser.parse_args()

    if args.self_test:
        return run_self_test()

    results = evaluate_all(
        CI_YML.read_text(encoding="utf-8"),
        EXTENDED_YML.read_text(encoding="utf-8"),
        DSR_QUALITY_YAML.read_text(encoding="utf-8"),
    )
    failed = False
    for record in results:
        if record["status"] != "pass":
            failed = True
        if not args.quiet or record["status"] != "pass":
            print(json.dumps(record, sort_keys=True))

    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
