# AGENTS.md

## Cursor Cloud specific instructions

**Relay** is a single-binary Rust CLI that syncs slash commands, skills, and agent/rule files across AI coding tools (Claude Code, Codex CLI, OpenCode, Cursor). No external services or databases are required.

### Standard commands

All standard dev commands are in `CONTRIBUTING.md` and the `README.md`:

- **Build:** `cargo build`
- **Test:** `cargo test` (117 unit/integration tests)
- **Lint:** `cargo clippy --all-targets --all-features -- -D warnings`
- **Format:** `cargo fmt --check`

### Sandboxed test environment

To manually exercise the relay binary without touching real home directories, use the test sandbox:

```sh
./scripts/setup-test-env.sh staging
source ./.local/test-envs/staging/env.sh
export HOME="$RELAY_HOME"
export XDG_CONFIG_HOME="$RELAY_HOME/.config"
```

You must also write a config file before running relay commands in the sandbox:

```sh
mkdir -p "$RELAY_HOME/.config/relay"
echo 'tools = ["claude", "codex", "opencode"]' > "$RELAY_HOME/.config/relay/config.toml"
```

Then run `relay sync --plan --verbose` or `relay sync --apply --verbose`. The sandbox directories live under `.local/` (git-ignored).

### Gotchas

- `relay init` is interactive (uses `dialoguer` prompts) and will block in non-TTY contexts. Use the sandbox config approach above instead.
- Version-check warnings for codex/claude/opencode are expected in the cloud VM since those tools are not installed — they are informational only and do not affect sync behavior.
- The `.cursor/hooks.json` file references an `entire` CLI that is not part of this repo; ignore hook errors.
