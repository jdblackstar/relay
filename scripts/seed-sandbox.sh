#!/usr/bin/env bash
# Seed sandbox fixture files into an isolated relay tool layout.
# Requires HOME, CODEX_HOME, CLAUDE_HOME, CURSOR_HOME, and OPENCODE_HOME.

set -euo pipefail

: "${HOME:?HOME must be set}"
: "${CODEX_HOME:?CODEX_HOME must be set}"
: "${CLAUDE_HOME:?CLAUDE_HOME must be set}"
: "${CURSOR_HOME:?CURSOR_HOME must be set}"
: "${OPENCODE_HOME:?OPENCODE_HOME must be set}"

mkdir -p \
    "$CODEX_HOME/skills/codex-sandbox" \
    "$CODEX_HOME/rules" \
    "$CLAUDE_HOME/commands" \
    "$CLAUDE_HOME/skills/claude-sandbox" \
    "$CURSOR_HOME/commands" \
    "$OPENCODE_HOME/command" \
    "$OPENCODE_HOME/skill/opencode-sandbox"

cat <<'EOF' >"$CLAUDE_HOME/commands/claude-sandbox.md"
Claude sandbox command.
EOF

cat <<'EOF' >"$CURSOR_HOME/commands/cursor-sandbox.md"
Cursor sandbox command.
EOF

cat <<'EOF' >"$OPENCODE_HOME/command/opencode-sandbox.md"
OpenCode sandbox command.
EOF

cat <<'EOF' >"$CODEX_HOME/skills/codex-sandbox/SKILL.md"
Codex sandbox skill body.
EOF

cat <<'EOF' >"$CLAUDE_HOME/skills/claude-sandbox/SKILL.md"
---
name: claude-sandbox
description: Claude sandbox skill.
---
Claude sandbox skill body.
EOF

cat <<'EOF' >"$OPENCODE_HOME/skill/opencode-sandbox/SKILL.md"
---
name: opencode-sandbox
description: OpenCode sandbox skill.
---
OpenCode sandbox skill body.
EOF

cat <<'EOF' >"$CODEX_HOME/rules/default.rules"
rule("sandbox")
EOF

cat <<'EOF' >"$CODEX_HOME/AGENTS.md"
Codex sandbox agents.
EOF

cat <<'EOF' >"$OPENCODE_HOME/AGENTS.md"
OpenCode sandbox agents.
EOF
