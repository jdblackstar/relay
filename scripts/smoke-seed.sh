#!/usr/bin/env bash
# Seed smoke fixture files into an isolated relay tool layout.
# Requires HOME, CODEX_HOME, CLAUDE_HOME, CURSOR_HOME, and OPENCODE_HOME.

set -euo pipefail

: "${HOME:?HOME must be set}"
: "${CODEX_HOME:?CODEX_HOME must be set}"
: "${CLAUDE_HOME:?CLAUDE_HOME must be set}"
: "${CURSOR_HOME:?CURSOR_HOME must be set}"
: "${OPENCODE_HOME:?OPENCODE_HOME must be set}"

mkdir -p \
    "$CODEX_HOME/prompts" \
    "$CODEX_HOME/skills/codex-smoke" \
    "$CODEX_HOME/rules" \
    "$CLAUDE_HOME/commands" \
    "$CLAUDE_HOME/skills/claude-smoke" \
    "$CURSOR_HOME/commands" \
    "$OPENCODE_HOME/command" \
    "$OPENCODE_HOME/skill/opencode-smoke"

cat <<'EOF' >"$CODEX_HOME/prompts/codex-smoke.md"
Codex smoke command.
EOF

cat <<'EOF' >"$CLAUDE_HOME/commands/claude-smoke.md"
Claude smoke command.
EOF

cat <<'EOF' >"$CURSOR_HOME/commands/cursor-smoke.md"
Cursor smoke command.
EOF

cat <<'EOF' >"$OPENCODE_HOME/command/opencode-smoke.md"
OpenCode smoke command.
EOF

cat <<'EOF' >"$CODEX_HOME/skills/codex-smoke/SKILL.md"
Codex smoke skill body.
EOF

cat <<'EOF' >"$CLAUDE_HOME/skills/claude-smoke/SKILL.md"
---
name: claude-smoke
description: Claude smoke skill.
---
Claude smoke skill body.
EOF

cat <<'EOF' >"$OPENCODE_HOME/skill/opencode-smoke/SKILL.md"
---
name: opencode-smoke
description: OpenCode smoke skill.
---
OpenCode smoke skill body.
EOF

cat <<'EOF' >"$CODEX_HOME/rules/default.rules"
rule("smoke")
EOF

cat <<'EOF' >"$CODEX_HOME/AGENTS.md"
Codex smoke agents.
EOF

cat <<'EOF' >"$OPENCODE_HOME/AGENTS.md"
OpenCode smoke agents.
EOF
