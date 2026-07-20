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
RELAY_BIN="${CARGO_TARGET_DIR:-${REPO_ROOT}/target}/debug/relay"

if ! command -v cargo >/dev/null 2>&1; then
    echo "error: cargo not found on PATH" >&2
    exit 127
fi

if command -v rustup >/dev/null 2>&1 \
    && ! rustup show active-toolchain >/dev/null 2>&1 \
    && [[ -z "${RUSTUP_TOOLCHAIN:-}" ]]; then
    export RUSTUP_TOOLCHAIN=stable
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

assert_codex_command_skill_wrapper() {
    local name="$1"
    local expected_body="$2"
    local skill_path="$HOME/.agents/skills/${name}/SKILL.md"
    local marker_path="$HOME/.agents/skills/${name}/.relay-command"

    if [[ ! -f "$skill_path" ]]; then
        echo "error: missing codex command skill wrapper at $skill_path" >&2
        exit 1
    fi
    if [[ ! -f "$marker_path" ]]; then
        echo "error: missing relay command marker at $marker_path" >&2
        exit 1
    fi
    if ! grep -q "$expected_body" "$skill_path"; then
        echo "error: codex skill wrapper missing expected body at $skill_path" >&2
        exit 1
    fi
}

for bin in codex claude opencode; do
    if ! command -v "$bin" >/dev/null 2>&1; then
        echo "note: ${bin} not found on PATH (install if needed)"
    fi
done

"${REPO_ROOT}/scripts/seed-sandbox.sh"

cargo build
"$RELAY_BIN" sync --verbose

test -f "$HOME/.config/relay/commands/claude-sandbox.md"
test -f "$HOME/.config/relay/commands/opencode-sandbox.md"
test -d "$HOME/.agents/skills/codex-sandbox"
test -d "$HOME/.agents/skills/claude-sandbox"
test -d "$HOME/.agents/skills/opencode-sandbox"
test ! -d "$CODEX_HOME/skills/claude-sandbox"
test ! -d "$OPENCODE_HOME/skill/claude-sandbox"
test -f "$HOME/.config/relay/agents/codex/AGENTS.md"
test -f "$HOME/.config/relay/agents/opencode/AGENTS.md"
test -f "$HOME/.config/relay/rules/codex/default.rules"

assert_codex_command_skill_wrapper "cursor-sandbox" "Cursor sandbox command."

watch_log="$(mktemp)"
"$RELAY_BIN" watch --quiet --debounce-ms "$WATCH_DEBOUNCE_MS" >"$watch_log" 2>&1 &
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

# The first central write happens before the rest of a sync finishes. Wait for
# the final targets too so the test does not mutate the probe mid-reconciliation.
wait_for_file "$OPENCODE_HOME/command/watch-probe.md" "$watch_log"
wait_for_file "$HOME/.agents/skills/watch-probe/SKILL.md" "$watch_log"

# Write test file with retry logic
write_test_file() {
    local attempts=3
    local i
    for ((i = 0; i < attempts; i++)); do
        cat <<'EOF' > "$CLAUDE_HOME/commands/watch-e2e.md"
Claude watch e2e command.
EOF
        if wait_for_file "$HOME/.config/relay/commands/watch-e2e.md" "$watch_log"; then
            return 0
        fi
        if [[ $i -lt $((attempts - 1)) ]]; then
            sleep 0.2
        fi
    done
    return 1
}

if ! write_test_file; then
    echo "error: failed to write watch e2e file after retries" >&2
    kill "$watch_pid" >/dev/null 2>&1 || true
    wait "$watch_pid" >/dev/null 2>&1 || true
    rm -f "$watch_log"
    exit 1
fi

wait_for_file "$HOME/.agents/skills/watch-e2e/SKILL.md" "$watch_log"
wait_for_file "$OPENCODE_HOME/command/watch-e2e.md" "$watch_log"

if ! grep -q "Claude watch e2e command." "$HOME/.config/relay/commands/watch-e2e.md"; then
    echo "error: watch e2e content missing in central" >&2
    exit 1
fi

assert_codex_command_skill_wrapper "watch-e2e" "Claude watch e2e command."

if ! grep -q "Claude watch e2e command." "$OPENCODE_HOME/command/watch-e2e.md"; then
    echo "error: watch e2e content missing in opencode" >&2
    exit 1
fi

kill "$watch_pid" >/dev/null 2>&1 || true
wait "$watch_pid" >/dev/null 2>&1 || true
rm -f \
    "$watch_log" \
    "$probe_file" \
    "$HOME/.config/relay/commands/watch-probe.md" \
    "$CURSOR_HOME/commands/watch-probe.md" \
    "$OPENCODE_HOME/command/watch-probe.md"
rm -rf "$HOME/.agents/skills/watch-probe"

echo "e2e test ok"
