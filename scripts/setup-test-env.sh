#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_NAME="${1:-staging}"
BASE_DIR="${REPO_ROOT}/.local/test-envs/${ENV_NAME}"
HOME_DIR="${BASE_DIR}/home"
ENV_FILE="${BASE_DIR}/env.sh"
CONFIG_FILE="${HOME_DIR}/.config/relay/config.toml"
BIN_DIR="${BASE_DIR}/bin"

mkdir -p \
  "${BIN_DIR}" \
  "${HOME_DIR}/.config/relay/commands" \
  "${HOME_DIR}/.config/relay/skills" \
  "${HOME_DIR}/.config/relay/agents" \
  "${HOME_DIR}/.config/relay/rules" \
  "${HOME_DIR}/.agents/skills" \
  "${HOME_DIR}/.claude/commands" \
  "${HOME_DIR}/.claude/skills" \
  "${HOME_DIR}/.codex/skills" \
  "${HOME_DIR}/.codex/rules" \
  "${HOME_DIR}/.cursor/commands" \
  "${HOME_DIR}/.config/opencode/commands" \
  "${HOME_DIR}/.config/opencode/skills"

cat > "${BIN_DIR}/codex" <<'EOF'
#!/usr/bin/env bash
echo "codex-cli 0.135.0"
EOF

cat > "${BIN_DIR}/claude" <<'EOF'
#!/usr/bin/env bash
echo "2.1.119 (Claude Code)"
EOF

cat > "${BIN_DIR}/cursor" <<'EOF'
#!/usr/bin/env bash
echo "2.5.20"
EOF

cat > "${BIN_DIR}/opencode" <<'EOF'
#!/usr/bin/env bash
echo "1.2.10"
EOF

chmod +x "${BIN_DIR}/codex" "${BIN_DIR}/claude" "${BIN_DIR}/cursor" "${BIN_DIR}/opencode"

cat > "${CONFIG_FILE}" <<EOF
enabled_tools = ["claude", "codex", "cursor", "opencode"]
verified_versions = {}
blacklist = {}
central_dir = "${HOME_DIR}/.config/relay/commands"
central_skills_dir = "${HOME_DIR}/.agents/skills"
central_agents_dir = "${HOME_DIR}/.config/relay/agents"
central_rules_dir = "${HOME_DIR}/.config/relay/rules"
claude_dir = "${HOME_DIR}/.claude/commands"
claude_skills_dir = "${HOME_DIR}/.claude/skills"
cursor_dir = "${HOME_DIR}/.cursor/commands"
opencode_commands_dir = "${HOME_DIR}/.config/opencode/commands"
opencode_skills_dir = "${HOME_DIR}/.agents/skills"
opencode_agents_file = "${HOME_DIR}/.config/opencode/AGENTS.md"
codex_skills_dir = "${HOME_DIR}/.agents/skills"
codex_rules_file = "${HOME_DIR}/.codex/rules/default.rules"
codex_agents_file = "${HOME_DIR}/.codex/AGENTS.md"
EOF

cat > "${ENV_FILE}" <<EOF
export RELAY_REPO_ROOT="${REPO_ROOT}"
export RELAY_HOME="${HOME_DIR}"
export HOME="${HOME_DIR}"
export CODEX_HOME="${HOME_DIR}/.codex"
export CLAUDE_HOME="${HOME_DIR}/.claude"
export OPENCODE_HOME="${HOME_DIR}/.config/opencode"
export CURSOR_HOME="${HOME_DIR}/.cursor"
export PATH="${REPO_ROOT}/target/debug:${BIN_DIR}:\$PATH"

if command -v rustup >/dev/null 2>&1 \
  && ! rustup show active-toolchain >/dev/null 2>&1 \
  && [[ -z "\${RUSTUP_TOOLCHAIN:-}" ]]; then
  export RUSTUP_TOOLCHAIN=stable
fi

if ! command -v relay >/dev/null 2>&1; then
  relay() {
    cargo run --manifest-path "${REPO_ROOT}/Cargo.toml" -- "\$@"
  }
fi
EOF

echo "Test environment created:"
echo "  ${BASE_DIR}"
echo "  config: ${CONFIG_FILE}"
echo
echo "Activate it with:"
echo "  source ${ENV_FILE}"
echo
echo "Then run examples:"
echo "  relay sync --plan --verbose"
echo "  relay sync --apply --verbose"
echo "  relay watch --quiet"
echo
echo "Or start the interactive sandbox (fixtures + helpers):"
echo "  ./scripts/sandbox.sh"
