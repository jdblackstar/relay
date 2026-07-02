#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
USE_CONTAINER="${USE_CONTAINER:-0}"
SANDBOX_RESET="${SANDBOX_RESET:-0}"
SANDBOX_SKIP_SYNC="${SANDBOX_SKIP_SYNC:-0}"
SANDBOX_SKIP_BUILD="${SANDBOX_SKIP_BUILD:-0}"

usage() {
    cat <<EOF
Usage: $(basename "$0") [options]

Interactive relay sandbox with isolated fake tool homes.

Options:
  --container     Run inside Apple's container CLI (Linux VM, like CI e2e)
  --reset         Delete existing sandbox before setup
  --no-sync       Skip initial 'relay sync --verbose'
  --no-build      Skip 'cargo build'
  -h, --help      Show this help

Local example:
  ./scripts/sandbox.sh

Container example:
  container system start
  ./scripts/sandbox.sh --container
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --container)
            USE_CONTAINER=1
            ;;
        --reset)
            SANDBOX_RESET=1
            ;;
        --no-sync)
            SANDBOX_SKIP_SYNC=1
            ;;
        --no-build)
            SANDBOX_SKIP_BUILD=1
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

    IMAGE_NAME="${IMAGE_NAME:-relay-sandbox}"
    CONTAINER_NAME="${CONTAINER_NAME:-relay-sandbox-dev}"

    container build --progress plain -t "$IMAGE_NAME" -f "${REPO_ROOT}/Containerfile.e2e" "$REPO_ROOT"
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
        -e SANDBOX_RESET="$SANDBOX_RESET" \
        -e SANDBOX_SKIP_SYNC="$SANDBOX_SKIP_SYNC" \
        -e SANDBOX_SKIP_BUILD="$SANDBOX_SKIP_BUILD" \
        "$IMAGE_NAME" \
        /bin/bash -lc "/workspace/relay/scripts/sandbox-core.sh"
fi

export REPO_ROOT
export SANDBOX_RESET
export SANDBOX_SKIP_SYNC
export SANDBOX_SKIP_BUILD
exec "${REPO_ROOT}/scripts/sandbox-core.sh"
