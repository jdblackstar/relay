# Contributing

Thanks for contributing to relay.

## Development setup

```sh
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

## Typical change flow

1. Make the code change.
2. Add or update tests.
3. Run `cargo fmt`, `cargo test`, and strict clippy.
4. Update docs (`README.md` and/or `PROVIDERS.md`) when behavior changes.

## Debugging

- Use `relay sync --verbose` for per-action output.
- Use `relay --debug <command>` to write debug logs to:
  - default: `~/.config/relay/logs/relay-debug.log`
  - override: `relay --debug --debug-log-file /tmp/relay.log <command>`
- Use history + rollback:
  - `relay history --limit 20`
  - `relay rollback <event-id>`

## Release notes

- CI should be green before cutting a release.
- Release tags use `vX.Y.Z`.
- Artifacts and checksums are published by GitHub Actions.
