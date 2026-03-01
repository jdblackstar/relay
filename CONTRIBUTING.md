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

## Adding Tools

Relay keeps tool setup in one place so new tools are easy to add.

1. Add default paths and env overrides in `src/config.rs`.
2. Add a new entry in `TOOL_DEFINITIONS` in `src/tools.rs`.
   - Abilities are just paths:
     - `commands_dir`: command files (markdown files)
     - `skills_dir`: skill folders that contain `SKILL.md`
     - `agents_file`: a single `AGENTS.md` file
     - `rules_file`: a single rules file (Codex uses Starlark)
3. Update `PROVIDERS.md` and the README.

If a tool does not support an ability, set it to `None`.

## Tool Layout

Each tool has its own subdirectories or files:

- Commands: `commands` (Codex uses `prompts`)
- Skills: `skills`
- Agents: `AGENTS.md`
- Rules: `rules/default.rules` (Codex only)

Relay also keeps a central store in `~/.config/relay` with:

- `commands/`
- `skills/`
- `agents/`
- `rules/`

## Local test environment

Create an isolated sandbox in the repo (ignored by git):

```sh
./scripts/setup-test-env.sh staging
source ./.local/test-envs/staging/env.sh
```

Then run relay commands safely against the sandbox instead of your real home
directories.

After code changes, rebuild the sandboxed binary with `cargo build`.
To return to your regular install, open a new terminal or unset the sandbox
env vars (`RELAY_HOME`, `CODEX_HOME`, `CLAUDE_HOME`, `OPENCODE_HOME`,
`CURSOR_HOME`).
See `docs/debugging.md` for the full switch-over steps.

## Debugging

- Use `relay sync --verbose` for per-action output.
- Use `relay --debug <command>` to write debug logs to:
  - default: `~/.config/relay/logs/relay-debug.log`
  - override: `relay --debug --debug-log-file /tmp/relay.log <command>`
- Use history + rollback:
  - `relay history --limit 20`
  - `relay rollback <event-id>`

See `docs/debugging.md` for the detailed guide.

## Smoke test

For an isolated end-to-end smoke test using Apple's `container`, see
`docs/smoke-container.md`.

## Weekly Compatibility PRs

To automate weekly tool upgrades + validation and open a PR from a local
machine, use:

```sh
./scripts/weekly-compat-pr.sh
```

On failure, it can also open a GitHub issue with detected versions and logs.
Setup and launchd scheduling guide: `docs/weekly-compat-pr.md`.

## Releases

Relay uses `cargo-release` for consistent tags and version bumps.

```sh
cargo install cargo-release
cargo release patch --execute   # bug fixes
cargo release minor --execute   # new features
cargo release major --execute   # breaking changes
```

This updates `Cargo.toml`, commits, tags `vX.Y.Z`, and pushes. GitHub Actions
publishes the release artifacts for macOS + Linux.

## Release checklist

- CI should be green before cutting a release.
- Release tags use `vX.Y.Z`.
- Artifacts and checksums are published by GitHub Actions.
