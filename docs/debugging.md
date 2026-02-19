# Debugging relay

## Fast triage

```sh
relay sync --plan --verbose
relay sync --apply --verbose
relay history --limit 20
```

Use `--plan` first to see what relay intends to change.

## Debug log file

Enable detailed logs:

```sh
relay --debug sync --apply --verbose
```

Default debug log path:

- `~/.config/relay/logs/relay-debug.log`

Override path:

```sh
relay --debug --debug-log-file /tmp/relay.log watch
```

## Background service (native)

Install/update service definition:

```sh
relay daemon install --debounce-ms 300 --quiet
```

Start/stop/restart:

```sh
relay daemon start
relay daemon stop
relay daemon restart
```

Show status:

```sh
relay daemon status
relay status
```

Quick path (install + start):

```sh
relay watch -d -q
```

Service manager by OS:

- macOS: launchd (`~/Library/LaunchAgents/dev.jdblackstar.relay.watch.plist`)
- Linux: systemd user unit (`~/.config/systemd/user/relay-watch.service`)

## Rollback workflow

1. Identify event:
   - `relay history --limit 20`
2. Roll back:
   - `relay rollback <event-id>`
3. If files were edited after the event and rollback refuses:
   - `relay rollback <event-id> --force`

Rollback restores the paths written by the target event. If a watch-triggered
event used one tool file as the source of truth and only wrote mirrored copies,
rollback restores those mirrored targets and may not modify the source file.

## Common checks

- Verify configured paths:
  - `relay init` (review prompts) or inspect `~/.config/relay/config.toml`
- Verify watch paths exist before starting watch.
- If syncing seems idle, run `relay sync --verbose` and inspect debug logs.

## Local Sandbox Testing

Use the repo-local sandbox when testing changes without touching your real home
directories:

```sh
cd /Users/josh/code/relay
./scripts/setup-test-env.sh staging
source /Users/josh/code/relay/.local/test-envs/staging/env.sh
```

Rebuild the binary used by that sandbox after code changes:

```sh
cd /Users/josh/code/relay
cargo build
relay --version
```

`env.sh` puts `/Users/josh/code/relay/target/debug` first on `PATH`, so the
sandbox uses the rebuilt local binary.

## Return To Regular Install

Fastest option: open a new terminal tab/window and run `relay`.

If you want to switch back in the same shell:

```sh
unset RELAY_HOME CODEX_HOME CLAUDE_HOME OPENCODE_HOME CURSOR_HOME RELAY_REPO_ROOT
hash -r
which relay
```

`which relay` should point to your normal installed location (not
`/Users/josh/code/relay/target/debug/relay`).
