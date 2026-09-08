#!/usr/bin/env bash
# Pinned Linux x86_64 capture tools for native DSR hosts. Retain all downloads
# and extraction directories; never replace an existing different installation.
set -euo pipefail

VHS_VERSION=0.10.0
VHS_SHA256=b552c3870aca101dcafe533cfef32dceb7b783400ad32642e728775c9f125407
TTYD_VERSION=1.7.7
TTYD_SHA256=8a217c968aba172e0dbf3f34447218dc015bc4d5e59bf51db2f2cd12b7be4f55
USER_AGENT='OpenAI File Downloader, XaiImageApiFetch/1.0'
PREFIX="${XDG_DATA_HOME:-${HOME}/.local/share}/doctor-frankentui/capture-tools/vhs-${VHS_VERSION}-ttyd-${TTYD_VERSION}"
CACHE_DIR=""

usage() {
  echo "Usage: bash $0 [--prefix DIR] [--cache-dir DIR]"
  echo "Installs checksummed VHS ${VHS_VERSION} and ttyd ${TTYD_VERSION} on Linux x86_64."
  echo "Retains downloads and extracted files; ffmpeg, ffprobe and Chrome are separate prerequisites."
}

while (( $# )); do
  case "$1" in
    --prefix|--cache-dir)
      if (( $# < 2 )) || [[ -z "$2" || "$2" == --* ]]; then
        echo "missing directory for $1" >&2
        exit 2
      fi
      if [[ "$1" == --prefix ]]; then PREFIX="$2"; else CACHE_DIR="$2"; fi
      shift 2
      ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done

if [[ "$(uname -s)" != Linux || "$(uname -m)" != x86_64 ]]; then
  echo "capture-tool installer supports Linux x86_64 only" >&2
  exit 2
fi
for tool in curl sha256sum tar mktemp install cmp; do
  command -v "$tool" >/dev/null || { echo "missing required command: $tool" >&2; exit 2; }
done
CACHE_DIR="${CACHE_DIR:-${PREFIX}/downloads}"
mkdir -p -- "$CACHE_DIR" "$PREFIX/bin"

download_verified() {
  local url=$1 path=$2 expected=$3
  if [[ ! -e "$path" && ! -L "$path" ]]; then
    curl --user-agent "$USER_AGENT" --fail --location --show-error \
      --connect-timeout 15 --max-time 180 --output "$path" "$url"
  fi
  # A partial or corrupted cached download fails here and is retained for review.
  printf '%s  %s\n' "$expected" "$path" | sha256sum --check --strict -
}

install_retained() {
  local source=$1 destination=$2
  if [[ -e "$destination" || -L "$destination" ]]; then
    if [[ ! -f "$destination" || -L "$destination" ]] || ! cmp -s -- "$source" "$destination"; then
      echo "refusing to replace existing installation: $destination" >&2
      exit 1
    fi
    if [[ ! -x "$destination" ]]; then
      echo "existing installation is not executable: $destination" >&2
      exit 1
    fi
  else
    install -m 0755 -- "$source" "$destination"
  fi
}

VHS_ARCHIVE="${CACHE_DIR}/vhs_${VHS_VERSION}_Linux_x86_64.tar.gz"
TTYD_BINARY="${CACHE_DIR}/ttyd.${TTYD_VERSION}.x86_64"
download_verified "https://github.com/charmbracelet/vhs/releases/download/v${VHS_VERSION}/vhs_${VHS_VERSION}_Linux_x86_64.tar.gz" "$VHS_ARCHIVE" "$VHS_SHA256"
download_verified "https://github.com/tsl0922/ttyd/releases/download/${TTYD_VERSION}/ttyd.x86_64" "$TTYD_BINARY" "$TTYD_SHA256"

EXTRACT_DIR=$(mktemp -d "${CACHE_DIR}/extract.XXXXXXXX")
tar -xzf "$VHS_ARCHIVE" -C "$EXTRACT_DIR"
VHS_BINARY="${EXTRACT_DIR}/vhs_${VHS_VERSION}_Linux_x86_64/vhs"
if [[ ! -f "$VHS_BINARY" ]]; then
  echo "pinned archive is missing its expected binary: $VHS_BINARY" >&2
  exit 1
fi
install_retained "$VHS_BINARY" "${PREFIX}/bin/vhs"
install_retained "$TTYD_BINARY" "${PREFIX}/bin/ttyd"
"${PREFIX}/bin/vhs" --version
"${PREFIX}/bin/ttyd" --version
sha256sum "${PREFIX}/bin/vhs" "${PREFIX}/bin/ttyd"
printf 'capture_tools_bin=%s\nretained_extract_dir=%s\n' "${PREFIX}/bin" "$EXTRACT_DIR"
