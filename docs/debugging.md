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

## Rollback workflow

1. Identify event:
   - `relay history --limit 20`
2. Roll back:
   - `relay rollback <event-id>`
3. If files were edited after the event and rollback refuses:
   - `relay rollback <event-id> --force`

## Common checks

- Verify configured paths:
  - `relay init` (review prompts) or inspect `~/.config/relay/config.toml`
- Verify watch paths exist before starting watch.
- If syncing seems idle, run `relay sync --verbose` and inspect debug logs.
