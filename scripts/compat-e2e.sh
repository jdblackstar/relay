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
    "$CODEX_HOME/skills/codex-compat" \
    "$CODEX_HOME/rules" \
    "$CLAUDE_HOME/commands" \
    "$CLAUDE_HOME/skills/claude-compat" \
    "$CURSOR_HOME/commands" \
    "$OPENCODE_HOME/command" \
    "$OPENCODE_HOME/skill/opencode-compat"

cat <<'CONTENTS' > "$CLAUDE_HOME/commands/claude-compat.md"
Claude compatibility command.
CONTENTS

cat <<'CONTENTS' > "$CURSOR_HOME/commands/cursor-compat.md"
Cursor compatibility command.
CONTENTS

cat <<'CONTENTS' > "$OPENCODE_HOME/command/opencode-compat.md"
OpenCode compatibility command.
CONTENTS

cat <<'CONTENTS' > "$CODEX_HOME/skills/codex-compat/SKILL.md"
Codex compatibility skill body.
CONTENTS

cat <<'CONTENTS' > "$CLAUDE_HOME/skills/claude-compat/SKILL.md"
---
name: claude-compat
description: Claude compatibility skill.
---
Claude compatibility skill body.
CONTENTS

cat <<'CONTENTS' > "$OPENCODE_HOME/skill/opencode-compat/SKILL.md"
---
name: opencode-compat
description: OpenCode compatibility skill.
---
OpenCode compatibility skill body.
CONTENTS

cat <<'CONTENTS' > "$CODEX_HOME/rules/default.rules"
rule("compat")
CONTENTS

cat <<'CONTENTS' > "$CODEX_HOME/AGENTS.md"
Codex compatibility agents.
CONTENTS

cat <<'CONTENTS' > "$OPENCODE_HOME/AGENTS.md"
OpenCode compatibility agents.
CONTENTS

cargo build
./target/debug/relay sync --verbose

test -f "$HOME/.config/relay/commands/claude-compat.md"
test -f "$HOME/.config/relay/commands/cursor-compat.md"
test -f "$HOME/.config/relay/commands/opencode-compat.md"
test -d "$HOME/.agents/skills/codex-compat"
test -d "$HOME/.agents/skills/claude-compat"
test -d "$HOME/.agents/skills/opencode-compat"
test ! -d "$CODEX_HOME/skills/claude-compat"
test ! -d "$OPENCODE_HOME/skill/claude-compat"
test -f "$HOME/.config/relay/agents/codex/AGENTS.md"
test -f "$HOME/.config/relay/agents/opencode/AGENTS.md"
test -f "$HOME/.config/relay/rules/codex/default.rules"

echo "compat e2e ok"
