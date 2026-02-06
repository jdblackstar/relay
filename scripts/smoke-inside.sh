#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-/workspace/relay}"
cd "$REPO_ROOT"

export HOME="${HOME:-/root}"
export CODEX_HOME="${CODEX_HOME:-$HOME/.codex}"
export CLAUDE_HOME="${CLAUDE_HOME:-$HOME/.claude}"
export CURSOR_HOME="${CURSOR_HOME:-$HOME/.cursor}"
export OPENCODE_HOME="${OPENCODE_HOME:-$HOME/.config/opencode}"
export PATH="/usr/local/cargo/bin:/usr/local/rustup/bin:${PATH}"
WATCH_DEBOUNCE_MS="${WATCH_DEBOUNCE_MS:-200}"

if ! command -v cargo >/dev/null 2>&1; then
    echo "error: cargo not found on PATH" >&2
    exit 127
fi

wait_for_file() {
    local path="$1"
    local log_path="$2"
    local attempts=50
    local i
    for ((i = 0; i < attempts; i++)); do
        if [[ -f "$path" ]]; then
            return 0
        fi
        sleep 0.1
    done
    echo "error: timed out waiting for $path" >&2
    if [[ -n "$log_path" && -f "$log_path" ]]; then
        echo "watch log:" >&2
        sed -n '1,200p' "$log_path" >&2
    fi
    return 1
}

for bin in codex claude opencode; do
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
    "$OPENCODE_HOME/command" \
    "$OPENCODE_HOME/skill/opencode-smoke"

cat <<'EOF' > "$CODEX_HOME/prompts/codex-smoke.md"
Codex smoke command.
EOF

cat <<'EOF' > "$CLAUDE_HOME/commands/claude-smoke.md"
Claude smoke command.
EOF

cat <<'EOF' > "$OPENCODE_HOME/command/opencode-smoke.md"
OpenCode smoke command.
EOF

cat <<'EOF' > "$CODEX_HOME/skills/codex-smoke/SKILL.md"
Codex smoke skill body.
EOF

cat <<'EOF' > "$CLAUDE_HOME/skills/claude-smoke/SKILL.md"
---
name: claude-smoke
description: Claude smoke skill.
---
Claude smoke skill body.
EOF

cat <<'EOF' > "$OPENCODE_HOME/skill/opencode-smoke/SKILL.md"
---
name: opencode-smoke
description: OpenCode smoke skill.
---
OpenCode smoke skill body.
EOF

cat <<'EOF' > "$CODEX_HOME/rules/default.rules"
rule("smoke")
EOF

cat <<'EOF' > "$CODEX_HOME/AGENTS.md"
Codex smoke agents.
EOF

cat <<'EOF' > "$OPENCODE_HOME/AGENTS.md"
OpenCode smoke agents.
EOF

cargo build
./target/debug/relay sync --verbose

test -f "$HOME/.config/relay/commands/codex-smoke.md"
test -f "$HOME/.config/relay/commands/claude-smoke.md"
test -f "$HOME/.config/relay/commands/opencode-smoke.md"
test -d "$HOME/.config/relay/skills/codex-smoke"
test -d "$HOME/.config/relay/skills/claude-smoke"
test -d "$HOME/.config/relay/skills/opencode-smoke"
test -f "$HOME/.config/relay/agents/codex/AGENTS.md"
test -f "$HOME/.config/relay/agents/opencode/AGENTS.md"
test -f "$HOME/.config/relay/rules/codex/default.rules"

watch_log="$(mktemp)"
./target/debug/relay watch --quiet --debounce-ms "$WATCH_DEBOUNCE_MS" >"$watch_log" 2>&1 &
watch_pid=$!

# Wait for watch to be ready by writing a probe file and waiting for it to be processed
probe_file="$CLAUDE_HOME/commands/watch-probe.md"
write_probe_file() {
    local attempts=5
    local i
    for ((i = 0; i < attempts; i++)); do
        cat <<EOF > "$probe_file"
watch probe ${i}
EOF
        if wait_for_file "$HOME/.config/relay/commands/watch-probe.md" "$watch_log"; then
            return 0
        fi
        sleep 0.1
    done
    return 1
}

# Wait for probe to be processed (indicates watch is ready)
if ! write_probe_file; then
    echo "error: watch not ready (probe file not processed)" >&2
    kill "$watch_pid" >/dev/null 2>&1 || true
    wait "$watch_pid" >/dev/null 2>&1 || true
    rm -f "$watch_log" "$probe_file"
    exit 1
fi

# Clean up probe file
rm -f "$probe_file" "$HOME/.config/relay/commands/watch-probe.md"

# Write test file with retry logic
write_test_file() {
    local attempts=3
    local i
    for ((i = 0; i < attempts; i++)); do
        cat <<'EOF' > "$CLAUDE_HOME/commands/watch-smoke.md"
Claude watch smoke command.
EOF
        if wait_for_file "$HOME/.config/relay/commands/watch-smoke.md" "$watch_log"; then
            return 0
        fi
        if [[ $i -lt $((attempts - 1)) ]]; then
            sleep 0.2
        fi
    done
    return 1
}

if ! write_test_file; then
    echo "error: failed to write watch smoke file after retries" >&2
    kill "$watch_pid" >/dev/null 2>&1 || true
    wait "$watch_pid" >/dev/null 2>&1 || true
    rm -f "$watch_log"
    exit 1
fi

wait_for_file "$CODEX_HOME/prompts/watch-smoke.md" "$watch_log"
wait_for_file "$OPENCODE_HOME/command/watch-smoke.md" "$watch_log"

if ! grep -q "Claude watch smoke command." "$HOME/.config/relay/commands/watch-smoke.md"; then
    echo "error: watch smoke content missing in central" >&2
    exit 1
fi

if ! grep -q "Claude watch smoke command." "$CODEX_HOME/prompts/watch-smoke.md"; then
    echo "error: watch smoke content missing in codex" >&2
    exit 1
fi

if ! grep -q "Claude watch smoke command." "$OPENCODE_HOME/command/watch-smoke.md"; then
    echo "error: watch smoke content missing in opencode" >&2
    exit 1
fi

kill "$watch_pid" >/dev/null 2>&1 || true
wait "$watch_pid" >/dev/null 2>&1 || true
rm -f "$watch_log"

echo "smoke test ok"
