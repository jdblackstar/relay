#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
IMAGE_NAME="${IMAGE_NAME:-relay-smoke}"
CONTAINER_NAME="${CONTAINER_NAME:-relay-smoke}"
KEEP_IMAGE="${KEEP_IMAGE:-0}"

if ! command -v container >/dev/null 2>&1; then
    echo "error: container CLI not found; install apple/container first" >&2
    exit 1
fi

cleanup() {
    container rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true
    if [[ "$KEEP_IMAGE" != "1" ]]; then
        container image delete "$IMAGE_NAME" >/dev/null 2>&1 || true
    fi
}
trap cleanup EXIT

container build --progress plain -t "$IMAGE_NAME" -f "${REPO_ROOT}/Containerfile.smoke" "$REPO_ROOT"

# Remove any existing container with the same name
container rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true

run_args=(
    --name "$CONTAINER_NAME" --rm
    -v "${REPO_ROOT}:/workspace/relay"
    -e HOME=/root
    -e CODEX_HOME=/root/.codex
    -e CLAUDE_HOME=/root/.claude
    -e CURSOR_HOME=/root/.cursor
    -e OPENCODE_HOME=/root/.config/opencode
)

container run --progress none "${run_args[@]}" "$IMAGE_NAME" /bin/bash -lc "/workspace/relay/scripts/smoke-inside.sh"
