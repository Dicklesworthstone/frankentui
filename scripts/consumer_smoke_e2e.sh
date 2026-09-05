#!/usr/bin/env bash
# Isolated, unchanged documentation examples; no stale-binary reuse.
set -eo pipefail
if [ -z "$TMPDIR" ] && [ -d /dev/shm ]; then export TMPDIR=/dev/shm; fi
exec python3 - "$0" "$@" <<'PY'
import argparse
import copy
import fcntl
import hashlib
import json
import os
import pty
import re
import select
import shlex
import shutil
import signal
import struct
import subprocess
import sys
import tarfile
import tempfile
import termios
import time
from pathlib import Path
from urllib.parse import unquote, urlsplit

import tomllib

ROOT = Path(sys.argv[1]).resolve().parent.parent
REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"
EXAMPLES = ("minimal_inline", "getting_started")
ANSI = re.compile(rb"\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07]*(?:\x07|\x1b\\)")


def digest(path):
    value = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for chunk in iter(lambda: stream.read(65536), b""):
            value.update(chunk)
    return value.hexdigest()


def require(condition, message):
    if not condition:
        raise ValueError(message)


def feature_closure(package, mode):
    requested = {"default"} if mode == "default" else {"runtime", "backend"} if mode == "slim" else {"runtime"}
    definitions = package["features"]
    require(requested <= set(definitions), f"facade lacks requested features: {sorted(requested - set(definitions))}")
    found, pending = set(), list(requested)
    while pending:
        feature = pending.pop()
        if feature not in found:
            found.add(feature)
            pending.extend(value for value in definitions[feature] if value in definitions)
    return found


def validate_identity(metadata, lock, mode, version, features, workspace, internal_paths):
    packages = metadata["packages"]
    root = metadata["resolve"]["root"]
    require(metadata["workspace_members"] == [root], "consumer is not a standalone one-member workspace")
    consumer = next(p for p in packages if p["id"] == root)
    require(Path(consumer["manifest_path"]).resolve() == workspace / "Cargo.toml", "unexpected consumer manifest")
    require({d["name"] for d in consumer["dependencies"]} == {"ftui"}, "unexpected direct consumer dependencies")
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    facades = [p for p in packages if p["name"] == "ftui"]
    require(len(facades) == 1, "expected exactly one resolved ftui package")
    facade = facades[0]
    require(facade["version"] == version, "resolved ftui version differs from requested version")
    expected = feature_closure(facade, features)
    require(set(nodes[facade["id"]]["features"]) == expected, "hidden or missing facade features")
    records = {(p["name"], p["version"], p.get("source")): p for p in lock["package"]}
    require(len(records) == len(lock["package"]), "duplicate lock package identity")
    identities = []
    for package in packages:
        if package["id"] == root:
            continue
        key = (package["name"], package["version"], package.get("source"))
        require(key in records, f"package absent from lock: {key}")
        record = records[key]
        local = mode != "registry" and package["name"] in internal_paths
        if local:
            require(package.get("source") is None, f"internal registry substitution: {key}")
            require(package["version"] == version, f"mixed internal versions: {key}")
            require(Path(package["manifest_path"]).resolve() == internal_paths[package["name"]], f"hidden path substitution: {key}")
            require(record.get("checksum") is None, f"local package has registry checksum: {key}")
        else:
            require(package.get("source") == REGISTRY, f"non-registry or substituted dependency: {key}")
            require(re.fullmatch(r"[0-9a-f]{64}", str(record.get("checksum", ""))) is not None, f"missing registry checksum: {key}")
        identities.append({"name": key[0], "version": key[1], "source": key[2],
                           "checksum": record.get("checksum"), "manifest_path": package["manifest_path"]})
    return {"packages": identities, "ftui_features": sorted(expected),
            "ftui_package_id": facade["id"], "workspace_members": metadata["workspace_members"]}


def config_data(path):
    data = tomllib.loads(path.read_text())
    require(not any(key in data for key in ("patch", "replace", "paths")), f"dependency override in Cargo config: {path}")
    sources = data.get("source", {})
    require(isinstance(sources, dict) and all(isinstance(value, dict) for value in sources.values()), f"invalid source config: {path}")
    require(not any("replace-with" in value for value in sources.values()), f"source replacement in Cargo config: {path}")
    return {"path": str(path), "sha256": digest(path)}


def cleanup_modes(data, normal):
    transitions = {}
    for match in re.finditer(rb"\x1b\[\?([0-9;]+)([hl])", data):
        for number in match[1].split(b";"):
            transitions.setdefault(int(number), []).append([match.start(), match[2].decode()])
    errors = []
    for number in (47, 1047, 1049):
        if any(value == "h" for _, value in transitions.get(number, [])):
            errors.append(f"inline journey entered alternate screen mode {number}")
    for number in (9, 1000, 1001, 1002, 1003, 1004, 1005, 1006, 1007, 1015, 1016, 2004, 2026):
        events = transitions.get(number, [])
        if any(value == "h" for _, value in events) and events[-1][1] != "l":
            errors.append(f"mode {number} not disabled after last enable")
    cursor = transitions.get(25, [])
    if (normal or cursor) and (not cursor or cursor[-1][1] != "h"):
        errors.append("cursor not shown after final hide")
    if normal and b"\x1b[?25h" not in data[-400:]:
        errors.append("cursor show absent from final 400 bytes")
    paste = transitions.get(2004, [])
    if sum(value == "h" for _, value in paste) != sum(value == "l" for _, value in paste):
        errors.append("unbalanced bracketed-paste enable/disable counts")
    sync = transitions.get(2026, [])
    if sum(value == "h" for _, value in sync) != sum(value == "l" for _, value in sync):
        errors.append("unbalanced synchronized-output brackets")
    regions = list(re.finditer(rb"\x1b\[([0-9;]*)r", data))
    if regions and regions[-1][1] not in (b"", b"0;0"):
        errors.append("scroll region not reset after final region change")
    return errors, transitions


def selftest():
    # Synthetic validator fixtures only; no download, build or live PTY proof.
    work = Path("/synthetic-consumer")
    facade = {"id": "ftui", "name": "ftui", "version": "0.6.1", "source": REGISTRY,
              "manifest_path": "/registry/ftui/Cargo.toml",
              "features": {"default": ["runtime"], "runtime": []}}
    meta = {"workspace_members": ["consumer"], "packages": [
        {"id": "consumer", "name": "ftui_consumer_smoke", "manifest_path": str(work / "Cargo.toml"),
         "dependencies": [{"name": "ftui"}]}, facade],
        "resolve": {"root": "consumer", "nodes": [{"id": "ftui", "features": ["default", "runtime"]}]}}
    lock = {"package": [{"name": "ftui", "version": "0.6.1", "source": REGISTRY, "checksum": "a" * 64}]}
    validate_identity(meta, lock, "registry", "0.6.1", "default", work, {})
    cases = [
        ("workspace-unification", lambda m, l: m["workspace_members"].append("other")),
        ("wrong-version", lambda m, l: m["packages"][1].update(version="0.6.0")),
        ("path-substitution", lambda m, l: m["packages"][1].update(source=None)),
        ("git-substitution", lambda m, l: m["packages"][1].update(source="git+https://example.invalid/repo")),
        ("checksum-missing", lambda m, l: l["package"][0].pop("checksum")),
        ("duplicate-lock", lambda m, l: l["package"].append(copy.deepcopy(l["package"][0]))),
        ("hidden-feature", lambda m, l: m["resolve"]["nodes"][0]["features"].append("extras")),
        ("missing-feature", lambda m, l: m["resolve"]["nodes"][0]["features"].remove("runtime")),
        ("extra-direct-dependency", lambda m, l: m["packages"][0]["dependencies"].append({"name": "ftui-runtime"})),
    ]
    for name, mutate in cases:
        altered, changed_lock = copy.deepcopy(meta), copy.deepcopy(lock)
        mutate(altered, changed_lock)
        try:
            validate_identity(altered, changed_lock, "registry", "0.6.1", "default", work, {})
        except ValueError:
            continue
        raise ValueError(f"identity selftest failed to reject {name}")
    local_meta, local_lock = copy.deepcopy(meta), copy.deepcopy(lock)
    local_meta["packages"][1].update(source=None, manifest_path="/candidate/ftui/Cargo.toml")
    local_lock["package"][0].pop("source")
    local_lock["package"][0].pop("checksum")
    local_meta["packages"].append({"id": "core", "name": "ftui-core", "version": "0.6.1", "source": None,
                                   "manifest_path": "/candidate/ftui-core/Cargo.toml"})
    local_lock["package"].append({"name": "ftui-core", "version": "0.6.1"})
    paths = {name: Path("/candidate") / name / "Cargo.toml" for name in ("ftui", "ftui-core")}
    for mode in ("source", "packaged"):
        validate_identity(local_meta, local_lock, mode, "0.6.1", "default", work, paths)
        for name in ("wrong-path", "mixed-internal-version"):
            altered, changed_lock = copy.deepcopy(local_meta), copy.deepcopy(local_lock)
            if name == "wrong-path":
                altered["packages"][1]["manifest_path"] = "/hidden/ftui/Cargo.toml"
            else:
                altered["packages"][2]["version"] = "0.6.0"
                changed_lock["package"][1]["version"] = "0.6.0"
            try:
                validate_identity(altered, changed_lock, mode, "0.6.1", "default", work, paths)
            except ValueError:
                continue
            raise ValueError(f"identity selftest failed to reject {mode} {name}")
    require(not cleanup_modes(b"\x1b[?25l\x1b[?2004h\x1b[?2004l\x1b[?25h", True)[0], "valid teardown rejected")
    require(bool(cleanup_modes(b"\x1b[?1006l\x1b[?1006h\x1b[?25h", True)[0]), "reversed teardown accepted")
    require(bool(cleanup_modes(b"\x1b[?25h\x1b[?25l", True)[0]), "hidden final cursor accepted")
    require(bool(cleanup_modes(b"\x1b[?2004h\x1b[?2004h\x1b[?2004l\x1b[?25h", True)[0]), "unbalanced paste accepted")
    require(bool(cleanup_modes(b"\x1b[?25h" + b"x" * 401, True)[0]), "old cursor show accepted as exit cleanup")
    return {"outcome": "SELFTEST_PASS", "identity_cases": 16, "cleanup_cases": 5,
            "scope": "synthetic validator cases; no download, build or live PTY proof"}


parser = argparse.ArgumentParser(description="Run unchanged facade examples as isolated source, packaged or registry consumers.")
parser.add_argument("--mode", choices=("source", "packaged", "registry"), default="source")
parser.add_argument("--version", help="exact facade version; required for packaged/registry")
parser.add_argument("--features", choices=("default", "slim", "no-backend"), default="default")
parser.add_argument("--runner", choices=("rch", "local"), default="rch")
parser.add_argument("--cargo", default=os.environ.get("CARGO", "cargo"), help="Cargo executable or explicit wrapper")
parser.add_argument("--toolchain", help="installed toolchain (defaults to repository pin; never installed automatically)")
parser.add_argument("--cargo-home", help="explicit Cargo home/cache; configuration is inspected and recorded")
parser.add_argument("--cargo-config", action="append", default=[], help="explicit config file, repeatable; dependency overrides rejected")
parser.add_argument("--offline", action="store_true")
parser.add_argument("--target-dir", default=os.environ.get("CARGO_TARGET_DIR"))
parser.add_argument("--scratch-parent", default="/data/projects", help="parent of fresh workspace; must be admitted by RCH")
parser.add_argument("--archives", help="directory containing actual .crate archives (packaged mode only)")
parser.add_argument("--ssh-config", default=os.environ.get("CONSUMER_SMOKE_SSH_CONFIG"), help="SSH config for fetching actual RCH executable")
parser.add_argument("--out", help="new artifact directory (defaults to fresh directory under TMPDIR)")
parser.add_argument("--selftest", action="store_true")
args = parser.parse_args(sys.argv[2:])
if args.selftest:
    print(json.dumps(selftest(), indent=2))
    raise SystemExit(0)

out = Path(args.out).expanduser().resolve() if args.out else Path(tempfile.mkdtemp(prefix="ftui-consumer-evidence-"))
if args.out:
    out.mkdir(parents=True, exist_ok=False)
summary = {"mode": args.mode, "feature_mode": args.features, "outcome": "FAIL", "journeys": [],
           "scope": "isolated native consumer; packaged acceptance is not registry publication acceptance"}
shutil.copyfile(ROOT / "scripts/consumer_smoke_e2e.sh", out / "consumer_smoke_e2e.sh")
events = out / "consumer_smoke.jsonl"


def emit(event, **fields):
    with events.open("a") as stream:
        stream.write(json.dumps({"event": event, **fields}) + "\n")


def command(argv, label, cwd, env):
    stdout, stderr = out / (label + ".stdout"), out / (label + ".stderr")
    started = time.monotonic()
    with stdout.open("xb") as output, stderr.open("xb") as errors:
        try:
            result = subprocess.run(argv, cwd=cwd, env=env, stdout=output, stderr=errors, timeout=1900, check=False)
            code = result.returncode
        except subprocess.TimeoutExpired:
            code = 124
    emit("command", command=argv, cwd=str(cwd), exit_code=code,
         seconds=time.monotonic() - started, stdout=stdout.name, stderr=stderr.name,
         stdout_sha256=digest(stdout), stderr_sha256=digest(stderr))
    if code:
        print(f"{label}: exit {code}; retained {stderr}", file=sys.stderr)
    return code, stdout, stderr


def materialize(executable, worker, name, env):
    target = out / (name + ".executed")
    if not worker:
        require(Path(executable).is_file(), f"Cargo executable absent: {executable}")
        shutil.copy2(executable, target)
        require(digest(target) == digest(executable), "local executable changed during capture")
        return target, {"source": executable, "worker": None, "sha256": digest(target)}
    # Read-only transport: fetch the actual worker file, never a local namesake.
    fetch = (
        "import hashlib,json,pathlib,shutil,subprocess,sys,tomllib\n"
        "p=pathlib.Path(sys.argv[1]);h=hashlib.sha256()\n"
        "home=pathlib.Path(sys.argv[2]);work=pathlib.Path(sys.argv[3]);configs=[]\n"
        "paths=[home/'config',home/'config.toml']\n"
        "for d in [work,*work.parents]:paths.extend([d/'.cargo/config',d/'.cargo/config.toml'])\n"
        "for c in dict.fromkeys(paths):\n"
        " if c.is_file():\n"
        "  raw=c.read_bytes();data=tomllib.loads(raw.decode());sources=data.get('source',{})\n"
        "  invalid=not isinstance(sources,dict) or any(not isinstance(v,dict) for v in sources.values())\n"
        "  override=any(k in data for k in ('patch','replace','paths')) or invalid\n"
        "  if not invalid:override=override or any('replace-with' in v for v in sources.values())\n"
        "  configs.append({'path':str(c),'sha256':hashlib.sha256(raw).hexdigest(),'dependency_override':override})\n"
        "compiler=subprocess.run(['rustup','run',sys.argv[4],'rustc','-Vv'],check=True,stdout=subprocess.PIPE,stderr=sys.stderr,text=True)\n"
        "with p.open('rb') as f:\n"
        " for c in iter(lambda:f.read(65536),b''):h.update(c)\n"
        "print(json.dumps({'sha256':h.hexdigest(),'size':p.stat().st_size,'cargo_home':str(home),"
        "'cargo_configs':configs,'compiler':compiler.stdout}),flush=True)\n"
        "with p.open('rb') as f:shutil.copyfileobj(f,sys.stdout.buffer)\n"
    )
    ssh = ["ssh", "-o", "BatchMode=yes"]
    if args.ssh_config:
        ssh += ["-F", str(Path(args.ssh_config).expanduser().resolve())]
    remote = "python3 -c " + shlex.quote(fetch) + " " + " ".join(shlex.quote(value) for value in
        (executable, env["CARGO_HOME"], str(remote_workspace), toolchain))
    code, transport, _ = command([*ssh, worker, remote], name + "-fetch", workspace, env)
    require(code == 0, "actual RCH executable fetch failed")
    with transport.open("rb") as source:
        identity = json.loads(source.readline())
        with target.open("xb") as destination:
            shutil.copyfileobj(source, destination)
    require(target.stat().st_size == identity["size"] and digest(target) == identity["sha256"], "worker/fetched executable identity mismatch")
    require(not any(config["dependency_override"] for config in identity["cargo_configs"]), "worker Cargo configuration contains dependency override")
    require(identity["compiler"] == summary["compiler"], "worker installed compiler identity differs from local resolver compiler")
    target.chmod(0o755)
    return target, {"source": executable, "worker": worker, **identity}


def journey(binary, name):
    env = {k: v for k, v in os.environ.items() if k not in (
        "TMUX", "TMUX_PANE", "STY", "ZELLIJ", "WEZTERM_PANE", "WEZTERM_UNIX_SOCKET",
        "WEZTERM_EXECUTABLE", "TERM_PROGRAM", "TERM_PROGRAM_VERSION", "KITTY_WINDOW_ID")}
    env.update(TERM="xterm-256color", COLORTERM="truecolor")
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
    before = termios.tcgetattr(slave)
    pid = os.fork()
    if pid == 0:
        try:
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
            for fd in (0, 1, 2):
                os.dup2(slave, fd)
            os.close(master)
            os.close(slave)
            os.execve(str(binary), [str(binary)], env)
        except OSError as exc:
            os.write(2, str(exc).encode())
            os._exit(127)
    data = bytearray()
    start = time.monotonic()
    deadline = start + 10
    status, sent, timed_out = None, [], False
    try:
        while time.monotonic() < deadline:
            ready, _, _ = select.select([master], [], [], 0.03)
            if ready:
                try:
                    data.extend(os.read(master, 65536))
                except OSError:
                    pass
            plain = ANSI.sub(b"", bytes(data))
            if args.features != "no-backend":
                if name == "minimal_inline" and not sent and b"Ticks:" in plain:
                    os.write(master, b"q")
                    sent.append({"input": "q", "after_seconds": time.monotonic() - start})
                elif name == "getting_started":
                    if not sent and b"Started." in plain and b"Tick..." in plain:
                        os.write(master, b"x")
                        sent.append({"input": "x", "after_seconds": time.monotonic() - start})
                    elif len(sent) == 1 and b"Key: Char('x')" in plain:
                        os.write(master, b"\x03")
                        sent.append({"input": "Ctrl+C", "after_seconds": time.monotonic() - start})
            waited, observed = os.waitpid(pid, os.WNOHANG)
            if waited == pid:
                status = observed
                break
            if len(data) > 16 * 1024 * 1024:
                break
        if status is None:
            timed_out = True
            os.killpg(pid, signal.SIGKILL)
            _, status = os.waitpid(pid, 0)
        drain_until = time.monotonic() + 0.15
        while time.monotonic() < drain_until:
            ready, _, _ = select.select([master], [], [], 0.02)
            if ready:
                try:
                    data.extend(os.read(master, 65536))
                except OSError:
                    break
        after = termios.tcgetattr(slave)
    finally:
        if status is None:
            try:
                os.killpg(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.waitpid(pid, 0)
        os.close(master)
        os.close(slave)
    raw = out / (name + ".pty.bin")
    raw.write_bytes(data)
    plain = ANSI.sub(b"", bytes(data))
    code = os.waitstatus_to_exitcode(status)
    normal = args.features != "no-backend"
    errors, transitions = cleanup_modes(bytes(data), normal)
    if before != after:
        errors.append("actual termios changed")
    if timed_out:
        errors.append("journey exceeded monotonic deadline or output bound")
    if normal:
        if code != 0:
            errors.append(f"expected exit0 after input, observed {code}")
        if name == "minimal_inline" and (b"Ticks:" not in plain or len(sent) != 1):
            errors.append("minimal journey did not render and quit after q")
        if name == "getting_started" and not (b"Started." in plain and b"Tick..." in plain and b"Key: Char('x')" in plain and len(sent) == 2):
            errors.append("streaming journey missing startup, tick, x key or Ctrl+C quit")
    elif code != 1 or b"Unsupported" not in plain:
        errors.append("runtime-only consumer did not report expected Unsupported")
    encode_termios = lambda value: [v if not isinstance(v, list) else [x.hex() if isinstance(x, bytes) else x for x in v] for v in value]
    return {"name": name, "outcome": "PASS" if normal and not errors else "EXPECTED_UNSUPPORTED" if not normal and not errors else "FAIL",
            "normal_journey_passed": normal and not errors, "exit_code": code, "timed_out": timed_out,
            "seconds": time.monotonic() - start, "inputs": sent, "termios_before": encode_termios(before),
            "termios_after": encode_termios(after), "dec_transitions": transitions, "errors": errors,
            "pty": raw.name, "pty_sha256": digest(raw), "bytes": len(data)}


try:
    require(not os.environ.get("CONSUMER_SMOKE_SKIP_BUILD"), "skip-build is unsupported: actual build required")
    manifest = tomllib.loads((ROOT / "Cargo.toml").read_text())
    pin = tomllib.loads((ROOT / "rust-toolchain.toml").read_text())["toolchain"]["channel"]
    version = args.version or manifest["workspace"]["package"]["version"]
    require(args.mode == "source" or args.version is not None, "--version required for packaged/registry")
    require(re.fullmatch(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", version) is not None, "invalid exact version")
    require((args.mode == "packaged") == bool(args.archives), "--archives required only for packaged mode")
    require(not args.ssh_config or Path(args.ssh_config).expanduser().is_file(), "SSH config missing")
    toolchain = args.toolchain or pin
    env = dict(os.environ)
    env["RCH_REQUIRE_REMOTE"] = "1"
    env.setdefault("RCH_BUILD_TIMEOUT_SEC", "1800")
    env.setdefault("RCH_TEST_TIMEOUT_SEC", "1800")
    env.setdefault("RCH_SYNC_TIMEOUT_MS", "180000")
    cargo_home = Path(args.cargo_home or env.get("CARGO_HOME", str(Path.home() / ".cargo"))).expanduser().resolve()
    env["CARGO_HOME"] = str(cargo_home)
    if args.target_dir:
        env["CARGO_TARGET_DIR"] = str(Path(args.target_dir).expanduser().resolve())
    env["RCH_ENV_ALLOWLIST"] = ",".join(sorted(set(filter(None, env.get("RCH_ENV_ALLOWLIST", "").split(","))) | {"CARGO_HOME"}))
    parent = Path(args.scratch_parent).expanduser().resolve()
    require(parent.is_dir(), "scratch parent must exist")
    workspace = Path(tempfile.mkdtemp(prefix="ftui-consumer-", dir=parent))
    summary.update(version=version, toolchain=toolchain, repository_pin=pin, runner=args.runner,
                   workspace=str(workspace), cargo_home=str(cargo_home), offline=args.offline,
                   cargo_home_scope="local resolver and worker build use the named path on separate hosts; their caches are independent",
                   target_dir=env.get("CARGO_TARGET_DIR"), producer_sha256=digest(out / "consumer_smoke_e2e.sh"))
    repository = {}
    for label, arguments in (("commit", ["rev-parse", "HEAD"]), ("tree", ["rev-parse", "HEAD^{tree}"]),
                             ("status", ["status", "--porcelain=v1", "--untracked-files=normal"])):
        code, recorded, _ = command(["git", *arguments], "repository-" + label, ROOT, env)
        require(code == 0, "repository identity unavailable")
        repository[label] = recorded.read_text().strip()
    repository["dirty"] = bool(repository["status"])
    repository["source_scope"] = "current checkout bytes; commit alone does not identify dirty source"
    summary["repository"] = repository
    configs = []
    for directory in [workspace, *workspace.parents]:
        configs.extend(p for p in (directory / ".cargo/config", directory / ".cargo/config.toml") if p.is_file())
    configs.extend(p for p in (cargo_home / "config", cargo_home / "config.toml") if p.is_file())
    configs.extend(Path(p).expanduser().resolve() for p in args.cargo_config)
    summary["cargo_configs"] = [config_data(path) for path in dict.fromkeys(configs)]
    (workspace / "src/bin").mkdir(parents=True)
    (workspace / "rust-toolchain.toml").write_text(f'[toolchain]\nchannel = {json.dumps(toolchain)}\n')
    internal = {}
    for member in manifest["workspace"]["members"]:
        path = ROOT / member / "Cargo.toml"
        internal[tomllib.loads(path.read_text())["package"]["name"]] = path.resolve()
    patches = {}
    if args.mode == "packaged":
        archives = sorted(Path(args.archives).expanduser().resolve().glob("*.crate"))
        require(archives, "no candidate .crate archives found")
        extracted = workspace / "candidate"
        extracted.mkdir()
        (out / "archives").mkdir()
        summary["archives"] = []
        for archive in archives:
            captured = out / "archives" / archive.name
            shutil.copyfile(archive, captured)
            require(digest(captured) == digest(archive), "candidate archive changed during capture")
            with tarfile.open(captured, "r:gz") as tar:
                members = tar.getmembers()
                seen = set()
                require(sum(item.size for item in members) <= 2 * 1024**3, "candidate archive exceeds extraction bound")
                for item in members:
                    path = Path(item.name)
                    require(not path.is_absolute() and ".." not in path.parts and "\\" not in item.name and path.as_posix() == item.name.rstrip("/"), "unsafe candidate member path")
                    require(item.name not in seen and (item.isdir() or item.isfile()) and item.size >= 0, "duplicate, link or special candidate member")
                    seen.add(item.name)
                roots = {Path(item.name).parts[0] for item in members}
                require(len(roots) == 1, "candidate archive must have one package root")
                package_root = extracted / next(iter(roots))
                require(not package_root.exists(), "duplicate candidate package root")
                tar.extractall(extracted, filter="data")
            package = tomllib.loads((package_root / "Cargo.toml").read_text())["package"]
            name = package["name"]
            require(name in internal and package["version"] == version and name not in patches, "unexpected, duplicate or wrong-version candidate package")
            require(package_root.name == f"{name}-{version}", "candidate archive root differs from package identity")
            patches[name] = package_root / "Cargo.toml"
            vcs_path = package_root / ".cargo_vcs_info.json"
            vcs = json.loads(vcs_path.read_text()) if vcs_path.is_file() else None
            summary["archives"].append({"name": name, "version": version, "source_archive": str(archive),
                                        "archive": str(captured.relative_to(out)), "sha256": digest(captured),
                                        "cargo_vcs_info": vcs})
        require("ftui" in patches, "candidate archives omit ftui")
        internal = {name: patches.get(name) for name in internal}
    dependency = f"version = {json.dumps('=' + version)}"
    if args.mode == "source":
        dependency += f", path = {json.dumps(os.path.relpath(ROOT / 'crates/ftui', workspace))}"
    if args.features != "default":
        selected = ["runtime", "backend"] if args.features == "slim" else ["runtime"]
        dependency += ", default-features = false, features = " + json.dumps(selected)
    text = '[package]\nname = "ftui_consumer_smoke"\nversion = "0.0.0"\nedition = "2024"\n\n[workspace]\n\n[dependencies]\nftui = { ' + dependency + " }\n"
    if patches:
        text += "\n[patch.crates-io]\n" + "".join(f"{name} = {{ path = {json.dumps(os.path.relpath(path.parent, workspace))} }}\n" for name, path in sorted(patches.items()))
    (workspace / "Cargo.toml").write_text(text)
    shutil.copyfile(workspace / "Cargo.toml", out / "Cargo.toml")
    summary["examples"] = {}
    for name in EXAMPLES:
        source = ROOT / "crates/ftui/examples" / (name + ".rs")
        destination = workspace / "src/bin" / (name + ".rs")
        shutil.copyfile(source, destination)
        require(digest(source) == digest(destination), "documentation example copy changed")
        summary["examples"][name] = digest(destination)
    readme = (ROOT / "README.md").read_text()
    fence = chr(96) * 3
    block = re.search(r"(?ms)^## Minimal API Example.*?^" + fence + r"rust\n(.*?)^" + fence + r"\s*$", readme)
    require(block is not None and block[1].encode() == (workspace / "src/bin/minimal_inline.rs").read_bytes(), "README minimal example differs from unchanged source")
    guide = (ROOT / "docs/getting-started.md").read_text()
    block = re.search(r"(?ms)^## Minimal Inline App.*?^" + fence + r"rust\n(.*?)^" + fence + r"\s*$", guide)
    require(block is not None and block[1].encode() == (workspace / "src/bin/getting_started.rs").read_bytes(), "getting-started guide differs from unchanged source")
    for name in EXAMPLES:
        shutil.copyfile(workspace / "src/bin" / (name + ".rs"), out / (name + ".rs"))
    cargo = [args.cargo, "+" + toolchain]
    options = ["--offline"] if args.offline else []
    for number, config in enumerate(args.cargo_config):
        staged = workspace / f"consumer-config-{number}.toml"
        shutil.copyfile(Path(config).expanduser(), staged)
        options += ["--config", staged.name]
    code, compiler_out, _ = command(["rustup", "run", toolchain, "rustc", "-Vv"], "compiler", workspace, env)
    require(code == 0, "requested installed toolchain unavailable")
    summary["compiler"] = compiler_out.read_text()
    host = re.search(r"(?m)^host: (\S+)$", summary["compiler"])
    require(host is not None, "compiler did not identify its host triple")
    summary["compiler_host"] = host[1]
    summary["target"] = host[1]
    summary["target_selection"] = "explicit native --target; overrides inherited build.target"
    code, metadata_out, _ = command([*cargo, "metadata", "--format-version", "1", *options], "metadata", workspace, env)
    require(code == 0, "isolated Cargo metadata failed")
    metadata = json.loads(metadata_out.read_text())
    lock = tomllib.loads((workspace / "Cargo.lock").read_text())
    identity = validate_identity(metadata, lock, args.mode, version, args.features, workspace, internal)
    summary["identity"] = identity
    shutil.copyfile(workspace / "Cargo.lock", out / "Cargo.lock")
    summary["lock_sha256"] = digest(out / "Cargo.lock")
    emit("identity", **identity)
    for name in EXAMPLES:
        result = {"name": name, "outcome": "FAIL"}
        try:
            build = [*cargo, "build", "--locked", "--bin", name, "--target", summary["target"], "--message-format=json-render-diagnostics", *options]
            if args.target_dir:
                build += ["--target-dir", env["CARGO_TARGET_DIR"]]
            argv = ["rch", "exec", "--source-content-receipt", "--", "env", "CARGO_HOME=" + env["CARGO_HOME"], *build] if args.runner == "rch" else build
            code, build_out, build_err = command(argv, name + "-build", workspace, env)
            result["build_exit"] = code
            require(code == 0, f"{name} build failed with exit {code}")
            log = build_err.read_text(errors="replace") + "\n" + build_out.read_text(errors="replace")
            messages = []
            for line in log.splitlines():
                try:
                    messages.append(json.loads(line))
                except ValueError:
                    continue
            binaries = [m["executable"] for m in messages if m.get("reason") == "compiler-artifact"
                        and m.get("target", {}).get("name") == name and m.get("executable")]
            require(len(set(binaries)) == 1, "Cargo JSON did not identify one executable")
            workers = re.findall(r"\[RCH\] remote ([A-Za-z0-9_.-]+)", log)
            if not workers:
                workers = re.findall(r"Selected worker: ([A-Za-z0-9_.-]+)", log)
            worker = workers[-1] if workers else None
            require(args.runner != "rch" or worker is not None, "RCH did not identify the executing remote worker")
            mappings = []
            if args.runner == "rch":
                receipts = [json.loads(line.split("[RCH] source content receipt: ", 1)[1])
                            for line in log.splitlines() if "[RCH] source content receipt: " in line]
                require(len(receipts) == 1, "RCH did not provide exactly one actual source-content receipt")
                receipt = receipts[0]
                require(receipt["worker_id"] == worker and receipt["command_exit_code"] == 0, "RCH receipt worker/exit mismatch")
                mappings = sorted(receipt["roots"], key=lambda root: len(root["local_root"]), reverse=True)
                result["source_receipt_root"] = receipt["receipt_root"]

            def mapped_path(path, roots=mappings):
                path = Path(path).resolve()
                if args.runner == "local":
                    return path
                for root in roots:
                    if path.is_relative_to(root["local_root"]):
                        return Path(root["remote_root"]) / path.relative_to(root["local_root"])
                raise ValueError(f"local package absent from RCH source receipt: {path}")

            def package_key(package_id, remap=False):
                if not package_id.startswith("path+file://"):
                    return package_id
                parsed = urlsplit(package_id.removeprefix("path+"))
                path = Path(unquote(parsed.path))
                return (str(mapped_path(path) if remap else path), parsed.fragment)

            allowed = {package_key(p["id"], True) for p in metadata["packages"]}
            artifacts = [m for m in messages if m.get("reason") == "compiler-artifact"]
            require(all(package_key(m["package_id"]) in allowed for m in artifacts), "compiled dependency identity differs from isolated metadata and RCH mapping")
            facade_key = package_key(identity["ftui_package_id"], True)
            observed_features = [set(m["features"]) for m in artifacts if package_key(m["package_id"]) == facade_key]
            require(observed_features and all(f == set(identity["ftui_features"]) for f in observed_features), "actual compiled facade features differ from isolated metadata")
            result["compiled_ftui_features"] = sorted(observed_features[0])
            executable = binaries[0]
            remote_workspace = mapped_path(workspace)
            if not Path(executable).is_absolute():
                executable = str(remote_workspace / executable)
            binary, binary_identity = materialize(executable, worker, name, env)
            result.update(journey(binary, name))
            result["binary"] = binary.name
            result["binary_identity"] = binary_identity
        except (ValueError, OSError, KeyError, StopIteration) as exc:
            result["errors"] = [str(exc)]
        summary["journeys"].append(result)
        emit("journey", **result)
    require(len(summary["journeys"]) == 2 and all(j["outcome"] != "FAIL" for j in summary["journeys"]), "one or more consumer journeys failed")
    require(digest(workspace / "Cargo.lock") == summary["lock_sha256"], "consumer lock changed during builds")
    summary["outcome"] = "EXPECTED_UNSUPPORTED" if args.features == "no-backend" else "PASS"
except (ValueError, OSError, KeyError, StopIteration, tarfile.TarError) as exc:
    summary["error"] = str(exc)
finally:
    (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps({"outcome": summary["outcome"], "mode": args.mode, "features": args.features,
                      "summary": str(out / "summary.json"), "error": summary.get("error")}))
raise SystemExit(0 if summary["outcome"] != "FAIL" else 1)
PY
