#!/usr/bin/env bash
# Shared interactive sandbox setup. Expects relay repo env vars to be exported.

set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
SANDBOX_ENV_NAME="${SANDBOX_ENV_NAME:-sandbox}"
SANDBOX_RESET="${SANDBOX_RESET:-0}"
SANDBOX_SKIP_SYNC="${SANDBOX_SKIP_SYNC:-0}"
SANDBOX_SKIP_BUILD="${SANDBOX_SKIP_BUILD:-0}"

BASE_DIR="${REPO_ROOT}/.local/test-envs/${SANDBOX_ENV_NAME}"
ENV_FILE="${BASE_DIR}/env.sh"
SHELL_INIT="${BASE_DIR}/sandbox-shell.sh"

if [[ "$SANDBOX_RESET" == "1" && -d "$BASE_DIR" ]]; then
    rm -rf "$BASE_DIR"
fi

"${REPO_ROOT}/scripts/setup-test-env.sh" "$SANDBOX_ENV_NAME" >/dev/null

# shellcheck disable=SC1090
source "$ENV_FILE"

"${REPO_ROOT}/scripts/seed-sandbox.sh"

if [[ "$SANDBOX_SKIP_BUILD" != "1" ]]; then
    (cd "$REPO_ROOT" && cargo build)
    RELAY_BIN="${CARGO_TARGET_DIR:-${REPO_ROOT}/target}/debug/relay"
    export PATH="$(dirname "$RELAY_BIN"):$PATH"
fi

if [[ "$SANDBOX_SKIP_SYNC" != "1" ]]; then
    relay sync --verbose
fi

cat >"$SHELL_INIT" <<'EOF'
# Relay sandbox helpers (isolated HOME; safe to experiment).

relay_sandbox_help() {
    cat <<'HELP'
Relay sandbox shell (isolated HOME)

  relay sync --verbose          Run sync against fake tool dirs
  relay sync --plan --verbose   Preview changes only
  relay watch --quiet           Watch fake tool dirs (try editing a command file)

Sandbox helpers:
  relay_tree                    Show central + tool file layout
  relay_codex_layout            Inspect Codex skills and wrappers
  relay_reseed                  Reset fixture files (does not delete sync output)
  relay_reset                   Wipe this sandbox and re-enter setup

Paths:
  RELAY_HOME   central config + store
  CODEX_HOME   skills, rules, and AGENTS
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

relay_reseed() {
    "${RELAY_REPO_ROOT}/scripts/seed-sandbox.sh"
    echo "fixtures reseeded"
}

relay_reset() {
    echo "wipe ${RELAY_HOME} parent and restart sandbox shell"
    rm -rf "$(dirname "${RELAY_HOME}")"
    exec "${RELAY_REPO_ROOT}/scripts/sandbox.sh"
}

relay_sandbox_help
EOF

echo
echo "Relay sandbox environment ready."
echo "  env:     ${BASE_DIR}"
echo "  config:  ${RELAY_HOME}/.config/relay/config.toml"
echo "  relay:   $(command -v relay)"
echo
echo "Entering shell. Run relay_sandbox_help for commands."
echo

export PS1="(relay-sandbox) \$ "
RCFILE="${BASE_DIR}/bashrc"
cat >"$RCFILE" <<EOF
source "$ENV_FILE"
source "$SHELL_INIT"
EOF
exec bash --rcfile "$RCFILE" -i
