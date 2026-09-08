#!/usr/bin/env python3
"""Exercise the soak comparator CLI with retained positive/adversarial fixtures.

Usage: python3 -B tests/e2e/scripts/test_doctor_determinism.py NEW_ARTIFACT_DIR
This verifies the gate itself; real capture proof comes from a separate soak.
All fixtures, command output, and reports remain in NEW_ARTIFACT_DIR.
"""

from __future__ import annotations

import hashlib
import json
import subprocess
import sys
from pathlib import Path


REPO = Path(__file__).resolve().parents[3]
SCRIPT = REPO / "scripts/doctor_frankentui_determinism_soak.sh"


def write_json(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, sort_keys=True) + "\n", encoding="utf-8")


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def fixture(root: Path, mutation: str) -> Path:
    root.mkdir()
    rows = []
    for workflow in ("happy", "failure"):
        for iteration in (1, 2):
            run = root / f"{workflow}_run_{iteration}"
            meta_dir = run / "meta"
            meta_dir.mkdir(parents=True)
            capture = run / "capture"
            capture.mkdir()
            altered = workflow == "happy" and iteration == 2
            trace = f"trace-{workflow}-{iteration}"
            started = f"2026-09-08T12:00:0{iteration}Z"
            finished = f"2026-09-08T12:00:1{iteration}Z"
            ledger = capture / "evidence_ledger.jsonl"
            record = {
                "timestamp": started, "trace_id": trace, "decision_id": "decision-0001",
                "action": "capture_finalize", "evidence_terms": ["final_status=ok"],
                "fallback_active": False, "fallback_reason": None, "policy_id": "doctor_frankentui/v1",
            }
            if altered and mutation == "ledger_decision":
                record["action"] = "capture_abandoned"
            if altered and mutation == "ledger_trace":
                record["trace_id"] = "unrelated-trace"
            if altered and mutation == "ledger_timestamp":
                record["timestamp"] = "yesterday"
            write_json(ledger, record)
            if altered and mutation == "ledger_nonobject":
                with ledger.open("a", encoding="utf-8") as stream:
                    stream.write("[]\n")
            capture_meta = {
                "profile": "analytics-empty", "status": "ok", "run_dir": str(capture),
                "trace_id": trace, "started_at": started, "finished_at": finished,
                "duration_seconds": iteration, "video_duration_seconds": 8 + iteration / 10,
                "video_exists": True, "evidence_ledger": str(ledger),
                "binary": f"echo demo {run}/project", "vhs_exit_code": 0,
            }
            if altered and mutation == "negative_duration":
                capture_meta["duration_seconds"] = -1
            if altered and mutation == "manifest_command":
                capture_meta["binary"] = "echo unrelated"
            write_json(capture / "run_meta.json", capture_meta)
            entry = {key: capture_meta[key] for key in ("profile", "status", "run_dir", "trace_id", "evidence_ledger")}
            manifest = {
                "suite_name": "example", "suite_dir": str(run), "started_at": started,
                "finished_at": finished, "success_count": 1, "failure_count": 0,
                "trace_ids": [trace], "run_index": [entry], "runs": [capture_meta],
                "report_failed": False,
            }
            if altered and mutation == "manifest_trace":
                manifest["trace_ids"] = ["unrelated-trace"]
            if altered and mutation == "index_trace":
                entry["trace_id"] = "unrelated-trace"
            if altered and mutation == "manifest_unknown_field":
                manifest["future_semantic_field"] = "changed"
            manifest_path = run / "suite_manifest.json"
            write_json(manifest_path, manifest)
            stdout = run / "suite.stdout.log"
            write_json(stdout, {
                "command": "suite", "status": "passed", "manifest_path": str(manifest_path),
                "trace_ids": ["unrelated-trace"] if altered and mutation == "stdout_trace" else [trace],
                "success_count": 2 if altered and mutation == "stdout_semantics" else 1,
            })
            stderr = run / "suite.stderr.log"
            message = "permission denied" if altered and mutation == "diagnostic" else "required path does not exist"
            stderr.write_text(f"{message}: {run}/project\n", encoding="utf-8")
            command = run / "command.txt"
            verb = "report" if altered and mutation == "command_file" else "capture"
            command.write_text(f"{verb} {run}/project\n", encoding="utf-8")
            plain_stdout = run / "plain.stdout.log"
            plain_stdout.write_text(f"manifest={run}/suite_manifest.json\n", encoding="utf-8")
            empty_stderr = run / "empty.stderr.log"
            empty_stderr.write_text("", encoding="utf-8")
            subprocess_stderr = run / "capture.stderr.log"
            pid = 1000 + iteration
            observed_pid = pid + 1 if altered and mutation == "process_identity" else pid
            diagnostic = "capture damaged" if altered and mutation == "stream_diagnostic" else "capture healthy"
            subprocess_stderr.write_text(
                f"[doctor:subprocess] vhs spawned pid={pid} timeout=300s\n"
                f"[WARN] {diagnostic}\n"
                f"[doctor:subprocess] vhs exited pid={observed_pid} code=0 elapsed={iteration}ms polls=60\n",
                encoding="utf-8",
            )
            port_path = run / "server.port"
            port = str(9000 + iteration)
            port_path.write_text("0" if altered and mutation == "invalid_port" else port, encoding="utf-8")
            endpoint = f"http://127.0.0.1:{port}/mcp/"
            seed_stdout = run / "seed.stdout.log"
            seed_stdout.write_text(
                f"waiting for {endpoint if not (altered and mutation == 'endpoint') else 'http://127.0.0.1:1/mcp/'}\n",
                encoding="utf-8",
            )
            seed_log = run / "seed_rpc.log"
            reason = "permission denied" if altered and mutation == "seed_reason" else "missing project"
            attempt = 2 if altered and mutation == "seed_attempt" else 1
            remaining = 6000 if altered and mutation == "seed_timing" else 5000 - iteration
            seed_log.write_text(
                f"[{started}] event=seed_start endpoint={endpoint} timeout_seconds=5\n"
                f"[{finished}] event=seed_stage_failed stage=ensure_project elapsed_ms={iteration} remaining_ms={remaining} reason={reason}\n"
                f"[{finished}] event=rpc_retry_scheduled method=ensure_project attempt={attempt} backoff_ms=100 reason={reason}\n",
                encoding="utf-8",
            )
            doctor_summary = {"command": "doctor", "status": "ok", "generated_at": started, "capture_timeout_seconds": 45}
            summary_path = run / "doctor_summary.json"
            write_json(summary_path, doctor_summary)
            doctor_stdout = run / "doctor.stdout.log"
            write_json(doctor_stdout, dict(doctor_summary, doctor_summary_path=str(summary_path)))
            snapshot = run / "snapshot.png"
            # Positive byte-size contract only: these are not claimed as PNG captures.
            snapshot.write_bytes(b"capture-bytes" * iteration)
            if altered and mutation == "empty_snapshot":
                snapshot.write_bytes(b"")
            artifacts = [ledger, manifest_path, stdout, stderr, command, plain_stdout, snapshot,
                         empty_stderr, port_path, seed_stdout, seed_log, doctor_stdout]
            events = []
            run_id = f"run-{workflow}-{iteration}"

            def event(kind: str, artifact: Path | None = None) -> None:
                seq = len(events) + 1
                payload = {
                    "schema_version": "1.0.0", "timestamp_utc": started,
                    "run_id": run_id, "correlation_id": f"{run_id}-corr-{seq}",
                    "case_id": "case" if kind not in ("run_start", "run_end") else "__run__",
                    "step_id": "capture" if kind not in ("run_start", "run_end") and workflow == "happy" else None,
                    "event_type": kind, "command": f"capture {run}/project", "env_hash": "a" * 64,
                    "duration_ms": iteration, "exit_code": 0,
                    "stdout_sha256": None, "stderr_sha256": None,
                    "artifact_hashes": {str(artifact): digest(artifact)} if artifact else {},
                    "expected": {"exit_code": 0, "regex_match": True, "json_valid": True},
                    "actual": {"exit_code": 0, "regex_match": True, "json_valid": True},
                }
                if artifact:
                    payload["actual"]["size_bytes"] = artifact.stat().st_size
                if kind in ("step_end", "case_end"):
                    payload["actual"]["stdout_log"] = str(plain_stdout)
                    payload["actual"]["stderr_log"] = str(subprocess_stderr)
                    payload["stdout_sha256"] = digest(plain_stdout)
                    payload["stderr_sha256"] = "0" * 64 if altered and mutation == "stream_hash" else digest(subprocess_stderr)
                if altered and artifact == snapshot:
                    if mutation == "snapshot_size":
                        payload["actual"]["size_bytes"] = 0
                    if mutation == "snapshot_type":
                        payload["actual"]["size_bytes"] = True
                    if mutation == "snapshot_actual":
                        payload["actual"]["json_valid"] = False
                    if mutation == "volatile_hash":
                        payload["artifact_hashes"][str(snapshot)] = "0" * 64
                events.append(payload)

            event("run_start")
            event("step_start" if workflow == "happy" else "case_start")
            for artifact in artifacts:
                event("artifact", artifact)
            event("step_end" if workflow == "happy" else "case_end")
            event("run_end")
            if altered:
                if mutation == "event_command":
                    events[1]["command"] = "unrelated"
                if mutation == "exit_code":
                    events[1]["exit_code"] = 1
                if mutation == "event_run_id":
                    events[1]["run_id"] = "another-run"
                if mutation == "correlation":
                    events[1]["correlation_id"] = events[0]["correlation_id"]
                if mutation == "raw_hash":
                    events[2]["artifact_hashes"][str(ledger)] = "0" * 64
                if mutation == "metadata_trace":
                    changed_meta = dict(capture_meta, trace_id="another-trace")
                    write_json(capture / "run_meta.json", changed_meta)
            event_path = meta_dir / "events.jsonl"
            event_path.write_text("".join(json.dumps(value) + "\n" for value in events), encoding="utf-8")
            skipped = mutation == "all_skipped" or (mutation == "skipped_workflow" and workflow == "failure")
            write_json(meta_dir / "summary.json", {"status": "skipped" if skipped else "passed"})
            write_json(meta_dir / "events_validation_report.json", {"status": "skipped" if skipped else "passed"})
            rows.append("\t".join([
                workflow, str(iteration), str(run), "0", str(meta_dir / "summary.json"),
                str(event_path), str(meta_dir / "events_validation_report.json"), str(stdout), str(stderr),
            ]))
    if mutation == "missing_iteration":
        rows = rows[:-1]
    if mutation == "missing_workflow":
        rows = rows[:2]
    if mutation == "duplicate_run":
        rows.append(rows[0])
    if mutation == "unexpected_workflow":
        rows.append(rows[0].replace("happy\t", "other\t", 1))
    if mutation == "malformed_index":
        rows.append("happy\tnot-an-integer")
    index = root / "meta/run_index.tsv"
    index.parent.mkdir()
    index.write_text("\n".join(rows) + "\n", encoding="utf-8")
    return root


def main() -> None:
    output = Path(sys.argv[1]).absolute()
    output.mkdir()  # Refuse to overwrite an earlier test run. Never clean up artifacts.
    cases = [
        "equivalent", "ledger_decision", "ledger_trace", "ledger_timestamp", "ledger_nonobject",
        "negative_duration", "manifest_command", "manifest_trace", "index_trace",
        "manifest_unknown_field", "stdout_trace", "stdout_semantics", "diagnostic", "command_file",
        "empty_snapshot", "snapshot_size", "snapshot_type", "snapshot_actual", "volatile_hash",
        "event_command", "exit_code", "event_run_id", "correlation", "raw_hash", "metadata_trace",
        "missing_iteration", "missing_workflow", "duplicate_run", "unexpected_workflow",
        "malformed_index", "skipped_workflow",
        "invalid_port", "endpoint", "seed_reason", "seed_attempt", "seed_timing",
        "process_identity", "stream_diagnostic", "stream_hash", "all_skipped",
    ]
    results = []
    for case in cases:
        run = fixture(output / case, case)
        before = {str(path): digest(path) for path in run.rglob("*") if path.is_file()}
        report_dir = output / f"{case}-report"
        result = subprocess.run(
            ["bash", str(SCRIPT), "--compare-only", str(run), str(report_dir), "2"],
            capture_output=True, text=True, timeout=30, check=False,
        )
        (output / f"{case}.stdout.log").write_text(result.stdout, encoding="utf-8")
        (output / f"{case}.stderr.log").write_text(result.stderr, encoding="utf-8")
        after = {str(path): digest(path) for path in run.rglob("*") if path.is_file()}
        assert before == after, f"{case}: retained inputs were modified"
        report = json.loads((report_dir / "determinism_report.json").read_text())
        expected = "passed" if case == "equivalent" else ("skipped" if case == "all_skipped" else "failed")
        assert report["status"] == expected, f"{case}: {report}"
        assert result.returncode == (0 if case == "equivalent" else 1), f"{case}: {result}"
        results.append({"case": case, "expected_status": expected, "exit_code": result.returncode})
        print(f"PASS {case}", flush=True)
    # Retained reports must survive accidental reuse of the same output directory.
    existing = output / "equivalent-report/determinism_report.json"
    before = digest(existing)
    result = subprocess.run(
        ["bash", str(SCRIPT), "--compare-only", str(output / "equivalent"), str(existing.parent), "2"],
        capture_output=True, text=True, timeout=30, check=False,
    )
    assert result.returncode == 2 and digest(existing) == before, result
    for iterations in ("0", "1", "invalid"):
        result = subprocess.run(
            ["bash", str(SCRIPT), "--compare-only", str(output / "equivalent"), str(output / "invalid-report"), iterations],
            capture_output=True, text=True, timeout=30, check=False,
        )
        assert result.returncode == 2 and not (output / "invalid-report").exists(), result
    alias = output / "equivalent-alias"
    alias.symlink_to(output / "equivalent", target_is_directory=True)
    result = subprocess.run(
        ["bash", str(SCRIPT), "--compare-only", str(alias), str(output / "alias-report"), "2"],
        capture_output=True, text=True, timeout=30, check=False,
    )
    assert result.returncode == 0, result
    write_json(output / "report.json", {"status": "passed", "cases": results, "report_reuse_rejected": True, "invalid_iteration_requests_rejected": 3, "symlink_alias_passed": True})
    print(f"PASS {len(cases)} comparison controls plus report protection, iteration guards, and symlink replay")


if __name__ == "__main__":
    main()
