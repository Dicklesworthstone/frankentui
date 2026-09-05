#!/usr/bin/env bash
# Print the workspace's dated nightly, rejecting invalid TOML and floating pins.
# Python 3.11+ is installed by the rust-pin composite action before this runs.
set -euo pipefail

if [[ $# -gt 1 ]]; then
    echo "usage: $0 [rust-toolchain.toml]" >&2
    exit 1
fi

"${PYTHON_BIN:-python3}" - "${1:-rust-toolchain.toml}" <<'PY'
import datetime
import pathlib
import re
import sys
import tomllib

path = pathlib.Path(sys.argv[1])
try:
    with path.open("rb") as source:
        document = tomllib.load(source)
    toolchain = document.get("toolchain", {})
    if not isinstance(toolchain, dict):
        raise ValueError("toolchain must be a TOML table")
    channel = toolchain.get("channel")
    if channel == "nightly":
        raise ValueError("floating channel not allowed")
    if not isinstance(channel, str) or not re.fullmatch(r"nightly-[0-9]{4}-[0-9]{2}-[0-9]{2}", channel):
        raise ValueError("toolchain.channel must be a dated nightly (nightly-YYYY-MM-DD)")
    try:
        datetime.date.fromisoformat(channel.removeprefix("nightly-"))
    except ValueError as error:
        raise ValueError("invalid nightly calendar date") from error
except (OSError, ValueError) as error:
    print(f"{path}: {error}", file=sys.stderr)
    sys.exit(1)
print(channel)
PY
