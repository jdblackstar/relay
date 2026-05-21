#!/usr/bin/env bash
# Shared interactive smoke setup. Expects relay repo env vars to be exported.

set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
SMOKE_ENV_NAME="${SMOKE_ENV_NAME:-smoke}"
SMOKE_RESET="${SMOKE_RESET:-0}"
SMOKE_SKIP_SYNC="${SMOKE_SKIP_SYNC:-0}"
SMOKE_SKIP_BUILD="${SMOKE_SKIP_BUILD:-0}"

BASE_DIR="${REPO_ROOT}/.local/test-envs/${SMOKE_ENV_NAME}"
ENV_FILE="${BASE_DIR}/env.sh"
SHELL_INIT="${BASE_DIR}/smoke-shell.sh"

if [[ "$SMOKE_RESET" == "1" && -d "$BASE_DIR" ]]; then
    rm -rf "$BASE_DIR"
fi

"${REPO_ROOT}/scripts/setup-test-env.sh" "$SMOKE_ENV_NAME" >/dev/null

# shellcheck disable=SC1090
source "$ENV_FILE"

"${REPO_ROOT}/scripts/smoke-seed.sh"

if [[ "$SMOKE_SKIP_BUILD" != "1" ]]; then
    (cd "$REPO_ROOT" && cargo build)
    RELAY_BIN="${CARGO_TARGET_DIR:-${REPO_ROOT}/target}/debug/relay"
    export PATH="$(dirname "$RELAY_BIN"):$PATH"
fi

if [[ "$SMOKE_SKIP_SYNC" != "1" ]]; then
    relay sync --verbose
fi

cat >"$SHELL_INIT" <<'EOF'
# Relay smoke interactive helpers (isolated HOME — safe to experiment).

relay_smoke_help() {
    cat <<'HELP'
Relay smoke shell (isolated HOME)

  relay sync --verbose          Run sync against fake tool dirs
  relay sync --plan --verbose   Preview changes only
  relay watch --quiet           Watch fake tool dirs (try editing a command file)

  relay_tree                    Show central + tool file layout
  relay_codex_layout            Inspect Codex prompts vs skills vs wrappers
  relay_wrappers                List relay-generated command skill wrappers
  relay_reseed                  Reset fixture files (does not delete sync output)
  relay_reset                   Wipe this smoke env and re-enter setup

Paths:
  RELAY_HOME   central config + store
  CODEX_HOME   prompts + skills (legacy commands + skill wrappers)
HELP
}

relay_tree() {
    echo "=== central (${RELAY_HOME}/.config/relay) ==="
    find "${RELAY_HOME}/.config/relay" -type f 2>/dev/null | sort || true
    echo
    echo "=== codex (${CODEX_HOME}) ==="
    find "$CODEX_HOME" -type f 2>/dev/null | sort || true
    echo
    echo "=== claude (${CLAUDE_HOME}) ==="
    find "$CLAUDE_HOME" -type f 2>/dev/null | sort || true
    echo
    echo "=== opencode (${OPENCODE_HOME}) ==="
    find "$OPENCODE_HOME" -type f 2>/dev/null | sort || true
}

relay_codex_layout() {
    echo "Codex legacy prompts (${CODEX_HOME}/prompts):"
    ls -la "${CODEX_HOME}/prompts" 2>/dev/null || echo "  (missing)"
    echo
    echo "Codex skills (${CODEX_HOME}/skills):"
    if [[ -d "${CODEX_HOME}/skills" ]]; then
        find "${CODEX_HOME}/skills" -mindepth 1 -maxdepth 2 -type f | sort
    else
        echo "  (missing)"
    fi
    echo
    echo "Relay-generated command wrappers (.relay-command):"
    find "${CODEX_HOME}/skills" -name '.relay-command' 2>/dev/null | sort || echo "  (none)"
}

relay_wrappers() {
    relay_codex_layout
}

relay_reseed() {
    "${RELAY_REPO_ROOT}/scripts/smoke-seed.sh"
    echo "fixtures reseeded"
}

relay_reset() {
    echo "wipe ${RELAY_HOME} parent and restart smoke shell"
    rm -rf "$(dirname "${RELAY_HOME}")"
    exec "${RELAY_REPO_ROOT}/scripts/smoke-interactive.sh"
}

relay_smoke_help
EOF

echo
echo "Relay smoke interactive environment ready."
echo "  env:     ${BASE_DIR}"
echo "  relay:   $(command -v relay)"
echo
echo "Entering shell. Run relay_smoke_help for commands."
echo

export PS1="(relay-smoke) \$ "
RCFILE="${BASE_DIR}/bashrc"
cat >"$RCFILE" <<EOF
source "$ENV_FILE"
source "$SHELL_INIT"
EOF
exec bash --rcfile "$RCFILE" -i
