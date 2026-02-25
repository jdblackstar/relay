#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

export HOME="${HOME:-$RELAY_HOME}"
export CODEX_HOME="${CODEX_HOME:-$HOME/.codex}"
export CLAUDE_HOME="${CLAUDE_HOME:-$HOME/.claude}"
export CURSOR_HOME="${CURSOR_HOME:-$HOME/.cursor}"
export OPENCODE_HOME="${OPENCODE_HOME:-$HOME/.config/opencode}"

if ! command -v cargo >/dev/null 2>&1; then
    echo "error: cargo not found on PATH" >&2
    exit 127
fi

for bin in codex claude cursor opencode; do
    if ! command -v "$bin" >/dev/null 2>&1; then
        echo "note: ${bin} not found on PATH (install if needed)"
    fi
done

mkdir -p \
    "$CODEX_HOME/prompts" \
    "$CODEX_HOME/skills/codex-smoke" \
    "$CODEX_HOME/rules" \
    "$CLAUDE_HOME/commands" \
    "$CLAUDE_HOME/skills/claude-smoke" \
    "$CURSOR_HOME/commands" \
    "$OPENCODE_HOME/command" \
    "$OPENCODE_HOME/skill/opencode-smoke"

cat <<'CONTENTS' > "$CODEX_HOME/prompts/codex-smoke.md"
Codex smoke command.
CONTENTS

cat <<'CONTENTS' > "$CLAUDE_HOME/commands/claude-smoke.md"
Claude smoke command.
CONTENTS

cat <<'CONTENTS' > "$CURSOR_HOME/commands/cursor-smoke.md"
Cursor smoke command.
CONTENTS

cat <<'CONTENTS' > "$OPENCODE_HOME/command/opencode-smoke.md"
OpenCode smoke command.
CONTENTS

cat <<'CONTENTS' > "$CODEX_HOME/skills/codex-smoke/SKILL.md"
Codex smoke skill body.
CONTENTS

cat <<'CONTENTS' > "$CLAUDE_HOME/skills/claude-smoke/SKILL.md"
---
name: claude-smoke
description: Claude smoke skill.
---
Claude smoke skill body.
CONTENTS

cat <<'CONTENTS' > "$OPENCODE_HOME/skill/opencode-smoke/SKILL.md"
---
name: opencode-smoke
description: OpenCode smoke skill.
---
OpenCode smoke skill body.
CONTENTS

cat <<'CONTENTS' > "$CODEX_HOME/rules/default.rules"
rule("smoke")
CONTENTS

cat <<'CONTENTS' > "$CODEX_HOME/AGENTS.md"
Codex smoke agents.
CONTENTS

cat <<'CONTENTS' > "$OPENCODE_HOME/AGENTS.md"
OpenCode smoke agents.
CONTENTS

cargo build
./target/debug/relay sync --verbose

test -f "$HOME/.config/relay/commands/codex-smoke.md"
test -f "$HOME/.config/relay/commands/claude-smoke.md"
test -f "$HOME/.config/relay/commands/cursor-smoke.md"
test -f "$HOME/.config/relay/commands/opencode-smoke.md"
test -d "$HOME/.config/relay/skills/codex-smoke"
test -d "$HOME/.config/relay/skills/claude-smoke"
test -d "$HOME/.config/relay/skills/opencode-smoke"
test -f "$HOME/.config/relay/agents/codex/AGENTS.md"
test -f "$HOME/.config/relay/agents/opencode/AGENTS.md"
test -f "$HOME/.config/relay/rules/codex/default.rules"

echo "compat smoke ok"
