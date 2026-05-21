#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
USE_CONTAINER="${USE_CONTAINER:-0}"
SMOKE_RESET="${SMOKE_RESET:-0}"
SMOKE_SKIP_SYNC="${SMOKE_SKIP_SYNC:-0}"
SMOKE_SKIP_BUILD="${SMOKE_SKIP_BUILD:-0}"

usage() {
    cat <<EOF
Usage: $(basename "$0") [options]

Interactive relay smoke environment with isolated fake tool homes.

Options:
  --container     Run inside Apple's container CLI (Linux VM, like CI smoke)
  --reset         Delete existing smoke env before setup
  --no-sync       Skip initial 'relay sync --verbose'
  --no-build      Skip 'cargo build'
  -h, --help      Show this help

Local example:
  ./scripts/smoke-interactive.sh

Container example:
  container system start
  ./scripts/smoke-interactive.sh --container
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --container)
            USE_CONTAINER=1
            ;;
        --reset)
            SMOKE_RESET=1
            ;;
        --no-sync)
            SMOKE_SKIP_SYNC=1
            ;;
        --no-build)
            SMOKE_SKIP_BUILD=1
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            echo "error: unknown option: $1" >&2
            usage >&2
            exit 1
            ;;
    esac
    shift
done

if [[ "$USE_CONTAINER" == "1" ]]; then
    if ! command -v container >/dev/null 2>&1; then
        echo "error: container CLI not found; run 'container system start' first" >&2
        exit 1
    fi

    IMAGE_NAME="${IMAGE_NAME:-relay-smoke}"
    CONTAINER_NAME="${CONTAINER_NAME:-relay-smoke-dev}"

    container build --progress plain -t "$IMAGE_NAME" -f "${REPO_ROOT}/Containerfile.smoke" "$REPO_ROOT"
    container rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true

    exec container run -it --name "$CONTAINER_NAME" --rm \
        -v "${REPO_ROOT}:/workspace/relay" \
        -e HOME=/root \
        -e CODEX_HOME=/root/.codex \
        -e CLAUDE_HOME=/root/.claude \
        -e CURSOR_HOME=/root/.cursor \
        -e OPENCODE_HOME=/root/.config/opencode \
        -e CARGO_TARGET_DIR=/tmp/relay-target \
        -e REPO_ROOT=/workspace/relay \
        -e SMOKE_RESET="$SMOKE_RESET" \
        -e SMOKE_SKIP_SYNC="$SMOKE_SKIP_SYNC" \
        -e SMOKE_SKIP_BUILD="$SMOKE_SKIP_BUILD" \
        "$IMAGE_NAME" \
        /bin/bash -lc "/workspace/relay/scripts/smoke-interactive-core.sh"
fi

export REPO_ROOT
export SMOKE_RESET
export SMOKE_SKIP_SYNC
export SMOKE_SKIP_BUILD
exec "${REPO_ROOT}/scripts/smoke-interactive-core.sh"
