#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_NAME="${1:-staging}"
BASE_DIR="${REPO_ROOT}/.local/test-envs/${ENV_NAME}"
HOME_DIR="${BASE_DIR}/home"
ENV_FILE="${BASE_DIR}/env.sh"

mkdir -p \
  "${HOME_DIR}/.config" \
  "${HOME_DIR}/.claude/commands" \
  "${HOME_DIR}/.claude/skills" \
  "${HOME_DIR}/.codex/prompts" \
  "${HOME_DIR}/.codex/skills" \
  "${HOME_DIR}/.codex/rules" \
  "${HOME_DIR}/.config/opencode/command" \
  "${HOME_DIR}/.config/opencode/skill"

cat > "${ENV_FILE}" <<EOF
export RELAY_REPO_ROOT="${REPO_ROOT}"
export RELAY_HOME="${HOME_DIR}"
export CODEX_HOME="${HOME_DIR}/.codex"
export CLAUDE_HOME="${HOME_DIR}/.claude"
export OPENCODE_HOME="${HOME_DIR}/.config/opencode"
export CURSOR_HOME="${HOME_DIR}/.cursor"
export PATH="${REPO_ROOT}/target/debug:\$PATH"

if ! command -v relay >/dev/null 2>&1; then
  relay() {
    cargo run --manifest-path "${REPO_ROOT}/Cargo.toml" -- "\$@"
  }
fi
EOF

echo "Test environment created:"
echo "  ${BASE_DIR}"
echo
echo "Activate it with:"
echo "  source ${ENV_FILE}"
echo
echo "Then run examples:"
echo "  relay sync --plan --verbose"
echo "  relay sync --apply --verbose"
echo "  relay watch --quiet"
