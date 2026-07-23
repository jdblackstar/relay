# relay

Minimal CLI to keep slash commands, skills, and agent/rule files aligned
across tools. Skills use a shared user-owned store; Relay provides migration,
compatibility adapters, reconciliation, history, and rollback around it.

## Defaults

Commands:

- Central store: `~/.config/relay/commands`
- Claude commands: `$CLAUDE_HOME/commands` (default `~/.claude/commands`)
- Cursor commands: `$CURSOR_HOME/commands` (default `~/.cursor/commands`)
- OpenCode commands: `$OPENCODE_HOME/commands` (default `~/.config/opencode/commands`)
- Codex command skill wrappers: `~/.agents/skills/<name>/SKILL.md` by default

Skills:

- Canonical store: `~/.agents/skills`
- Claude compatibility adapter: `$CLAUDE_HOME/skills` (default `~/.claude/skills`)
- Codex and OpenCode: read the canonical store directly by default
- Older Relay, Codex, and OpenCode directories: import-only migration sources

Agents:

- Central store: `~/.config/relay/agents`
- Codex AGENTS: `$CODEX_HOME/AGENTS.md` (default `~/.codex/AGENTS.md`)
- OpenCode AGENTS: `$OPENCODE_HOME/AGENTS.md` (default `~/.config/opencode/AGENTS.md`)

Rules:

- Central store: `~/.config/relay/rules`
- Codex rules: `$CODEX_HOME/rules/default.rules` (default `~/.codex/rules/default.rules`)

Commands are markdown files (e.g. `review.md`). For Codex, relay generates a skill
wrapper in the configured Codex skills directory (the shared store by default), so Codex can discover the
workflow through skills. Relay does not sync old `$CODEX_HOME/prompts` command
files. Generated command skill wrappers include a `.relay-command` marker and
are ignored as skill sources. If a real skill and generated command wrapper
share a Codex skill name, the real skill owns that directory and relay skips the
wrapper. Skills are stored as directories named after the skill, with a
`SKILL.md` inside (e.g. `review/SKILL.md`). Relay does not create redundant
Codex or OpenCode copies when those clients use the shared store.
Claude and OpenCode also read project commands from `.claude/commands/` and
`.opencode/commands/`, plus project skills from `.claude/skills/<name>/SKILL.md`
and `.opencode/skills/<name>/SKILL.md`; relay currently syncs global locations
only.

### XDG Notes

- Relay follows `XDG_CONFIG_HOME` for config-style paths.
- If `XDG_CONFIG_HOME` is not set, relay uses `$HOME/.config`.
- `XDG_HOME` is not a standard XDG variable.
- Existing configs that use OpenCode's former default `command` directory write
  to `commands` while continuing to read and watch the legacy path during
  migration. Custom paths are left unchanged. After a successful sync, update
  that config value to `commands` to stop watching the legacy path.
- In `config.toml` and path env vars, use concrete paths or supported forms:
  `~`, `$HOME`, `${HOME}`, `$XDG_CONFIG_HOME`, `${XDG_CONFIG_HOME}`, and
  `${XDG_CONFIG_HOME:-$HOME/.config}`.

## Install 

```sh
brew install jdblackstar/tap/relay
```

Or download a release archive from GitHub and place the `relay` binary on your PATH.

## Commands

```sh
relay [--debug] [--debug-log-file <path>] init
relay [--debug] [--debug-log-file <path>] sync [-p|--plan|-a|--apply] [-v|--verbose|-q|--quiet] [--fail-on-conflict] [-c|--confirm-versions]
relay [--debug] [--debug-log-file <path>] sync skill [-p|--plan|-a|--apply] [-v|--verbose|-q|--quiet] [--fail-on-conflict] [-c|--confirm-versions] <path>...
relay [--debug] [--debug-log-file <path>] capabilities --json
relay [--debug] [--debug-log-file <path>] watch [-b|--debounce-ms 300] [-q|--quiet] [-d|--daemon] [-c|--confirm-versions]
relay [--debug] [--debug-log-file <path>] status
relay [--debug] [--debug-log-file <path>] daemon install [-b|--debounce-ms 300] [-q|--quiet] [-c|--confirm-versions]
relay [--debug] [--debug-log-file <path>] daemon start|stop|restart|status|uninstall
relay [--debug] [--debug-log-file <path>] history [-n|--limit 20]
relay [--debug] [--debug-log-file <path>] rollback <event-id> [-f|--force]
relay [--debug] [--debug-log-file <path>] rollback [-l|--latest] [-f|--force]
```

`relay init` is interactive and writes config to
`$XDG_CONFIG_HOME/relay/config.toml` when `XDG_CONFIG_HOME` is set, otherwise
`~/.config/relay/config.toml`.
`relay watch` is event-driven with a small debounce and keeps copies aligned.
`relay watch --daemon` installs/updates and starts a native background service:
- macOS: `launchd` user agent
- Linux: `systemd --user` service

`relay daemon` exposes explicit lifecycle control for that service.
`relay status` is shorthand for `relay daemon status` and also prints skill
store roles, paths, counts, tombstones, and import collisions.
Init detects installed tool directories and lets you pick which ones to sync.
Use Space to toggle selections and Enter to confirm.
`relay sync --plan` previews changes without writing files.
`relay sync --fail-on-conflict` stops before writing if relay finds competing edits.
`relay sync skill PATH...` narrows discovery and reconciliation to the selected
skill packages. Paths are positional operands suitable for shell completion.
`relay history` lists recorded sync/watch/rollback events.
Watch-triggered history entries include source context in `origin` when
available (example: `watch:codex:review.md`).
`relay rollback` restores paths from a previous history event.
`--debug` enables file logging for deeper troubleshooting.

## Safety Model

- `relay sync --plan`: preview writes without changing files.
- `relay sync --apply`: execute writes and record a history event.
- `relay sync --fail-on-conflict`: abort before apply writes when sync finds conflicts.
- `relay sync skill PATH...`: reconcile only explicitly selected skills; commands,
  agents, rules, and unrelated skills are not reconciled or changed.
- `relay watch`: auto-apply writes on file events and record history events.
- `relay watch --daemon`: run watch as native background service.
- `relay rollback`: restore paths from a recorded event.
- `relay rollback` validates current file state before restoring; use `--force`
  only when you intentionally want to override newer edits.
- `relay rollback` restores the paths written by the chosen event (for example,
  mirrored targets from a watch sync), which may not include the original
  source file that triggered the sync.

## Shared skill architecture

`~/.agents/skills` is the canonical, user-owned skill directory. A skill in
that directory wins over a same-named skill in an import-only legacy/native
directory. Relay reports those collisions in verbose sync output and in
`relay status`; it does not overwrite the canonical skill.

Claude remains a read/write compatibility adapter. Relay mirrors canonical
skills there, but an intentional Claude-side edit is reconciled back when the
canonical copy has not changed. If both changed since the prior reconciliation,
Relay reports a conflict and keeps the canonical version. Explicitly configured
nonstandard Codex/OpenCode skill paths are also compatibility adapters; their
standard legacy paths are import-only.

Relay records canonical and adapter content hashes in
`~/.config/relay/runtime/skills-state.toml`. When an observed canonical skill is
deleted, Relay writes a tombstone and removes only adapter copies whose content
still matches the last Relay-owned mirror. A modified adapter is preserved for
manual resolution. Tombstones prevent stale legacy/native copies from
resurrecting a deleted skill. Recreating the canonical skill clears its
tombstone.

On upgrade, an exact old default `central_skills_dir` of
`~/.config/relay/skills` is interpreted as the legacy store and the effective
canonical path becomes `~/.agents/skills`. Relay imports valid user-authored
skills from the old Relay, Codex, and OpenCode locations when no canonical skill
or tombstone owns the name. Hidden entries, generated command wrappers,
symlinked skill roots, and directories carrying `.system`, `.plugin`,
`.managed`, or `.relay-managed` markers are excluded from migration imports.
Custom `central_skills_dir` values remain authoritative.

### Scoped skill sync

`relay sync skill PATH...` limits reconciliation to each discovered
package name. Existing canonical-store ownership, compatibility adapters,
state, conflicts, history, locking, plan/apply, and atomic publication behavior
still apply. A selected package outside configured Relay locations can populate
a missing canonical skill. If it differs from an existing canonical package,
Relay reports a conflict and refuses all scoped writes. Scoped mode does not run
command, agent, or rule sync, does not reconcile unrelated skill names, and
never interprets a missing operand as a deletion.

A directory containing `SKILL.md` is one complete package. A directory without
`SKILL.md` is searched recursively, including below discovered skill roots. A
direct path to `SKILL.md` is also accepted. Collection discovery skips `.git`,
`node_modules`, and directory symlinks. Relay rejects overlapping ancestor and
descendant skill roots, invalid frontmatter, declared names that differ from
their parent directory, and duplicate declared names before apply can write.

Once selected, a package is validated and copied independently of collection
discovery exclusions. Relay copies all regular files and directories, including
hidden content, `.git`, and `node_modules`. Any file or directory symlink inside
a selected package is a validation error rather than being silently omitted.
Selecting a missing path is an error; targeted removal is not supported.

Machine callers can rely on exit status: success exits 0, while invalid input,
collision, lock, conflict-abort, or write failures exit nonzero. Callers do not
need to parse human output.

Probe support with `relay capabilities --json`:

```json
{"schema_version":1,"capabilities":{"skills.sync.scoped":1}}
```

The schema and capability versions are independent integers. Relay versions
without this command should be treated as not supporting scoped skill sync.

## Debugging

- Fast check: `relay sync --plan --verbose`
- Debug logging: `relay --debug sync --apply --verbose`
- Default log file: `~/.config/relay/logs/relay-debug.log`
- Custom log file: `relay --debug --debug-log-file /tmp/relay.log watch`
- Service status: `relay status` (or `relay daemon status`)
- Detailed guide: `docs/debugging.md`

## Local test environment

Create an isolated sandbox in this repo (ignored by git):

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

## E2E test (apple/container)

For isolated end-to-end verification using Apple's `container`, see
`docs/e2e-container.md`.

## Weekly Compatibility PRs

To automate weekly tool upgrades + validation and open a PR from a local
machine, use:

```sh
./scripts/weekly-compat-pr.sh
```

On failure, it can also open a GitHub issue with detected versions and logs.
Compatibility snapshot uses a moving `[tested_latest]` and a manual
`[min_supported]` floor in `docs/compat/verified-versions.toml`.
Setup and launchd scheduling guide: `docs/weekly-compat-pr.md`.

## Notes

- Commands retain two-way newest-wins synchronization across configured tool
  directories and `~/.config/relay/commands`.
- Skills use canonical-store reconciliation and compatibility adapters; they
  are not blindly copied to every tool directory.
- Skills are synced as directories, not single files, and must include `SKILL.md`.
- Claude skills require frontmatter `name:` and `description:` in `SKILL.md`.
- Shared-compatible clients consume canonical skill directories directly.
- Codex command files are also mirrored as generated skill wrappers unless a
  real Codex skill already owns the same name.
- AGENTS and rules are synced as files per tool into the central store.
- OpenCode does not have a separate rules file; it uses `AGENTS.md` instead.
- Frontmatter body is ignored for change detection except `name:` and
  `description:` when both are present in valid frontmatter.
- Relay syncs `name:` and `description:` across tools; other frontmatter fields
  remain tool-specific.
- If frontmatter is missing or malformed, relay skips frontmatter sync and logs
  a warning.
- Relay follows symlinks for command files and skill folders. Symlinks inside
  skill folders are ignored to avoid loops.
- Competing command/agent/rule edits still use newest-wins. Competing
  canonical/adapter skill edits keep the canonical version.
- Applied writes are recorded under `~/.config/relay/history` (events + blobs)
  for transparency and rollback.
- If `~/.dotfiles` is detected during init, relay can optionally move existing
  `~/.config/relay` data into `~/.dotfiles/config/relay` and symlink
  `~/.config/relay` to the dotfiles location.
- Version checks for `codex` and `claude` are best-effort and informational.
- Old Codex prompt files under `$CODEX_HOME/prompts` are ignored.
- Only selected tools are synced and watched.

## Adding Tools

Relay keeps tool setup in one place so new tools are easy to add.

1. Add default paths and env overrides in `src/config.rs`.
2. Add a new entry in `TOOL_DEFINITIONS` in `src/tools.rs`.
   - Abilities are just paths:
     - `commands_dir`: command files (markdown files)
     - `skills_dir`: skill folders that contain `SKILL.md`
     - `agents_file`: a single `AGENTS.md` file
     - `rules_file`: a single rules file (Codex uses Starlark)
3. Update `PROVIDERS.md` and this README.

If a tool does not support an ability, set it to `None`.

## Tool Layout

Each tool has its own subdirectories or files:

- Commands: `commands` (Codex uses generated skill wrappers)
- Skills: `skills`
- Agents: `AGENTS.md`
- Rules: `rules/default.rules` (Codex only)

Relay also keeps a central store in `~/.config/relay` with:

- `commands/`
- `agents/`
- `rules/`

Skills live separately in the standard `~/.agents/skills` store.

## Limitations

- Windows is not supported yet.
- `relay watch` does not auto-detect new tool install directories; rerun
  `relay sync` or restart `relay watch` after installing tools.
- Only global locations are synced; project-level commands/skills are not
  imported yet.
- Frontmatter compatibility is best-effort; relay does not rewrite or validate
  provider-specific frontmatter yet. A future `relay import`/`relay lint` will
  help normalize and validate per tool.
- Per-tool skill blacklists cannot hide a canonical skill from a client that
  reads `~/.agents/skills` directly. Relay never deletes the canonical copy to
  implement such a blacklist.
- Migration filtering cannot identify every third-party manager that writes a
  plain, unmarked, non-symlinked skill into a native directory. Such managers
  should add a supported marker or users should remove that import path from
  their configuration.
- Cursor commands are synced; Relay does not maintain a Cursor-specific skill
  copy because shared-compatible clients can consume the canonical store.

## Verified Versions

You can pin known-good tool versions in config. If set, relay will color the
detected version:

- green: same major + minor (patch differences ok)
- yellow: same major, different minor
- red: different major

Example:

```toml
verified_versions = { claude = "2.0.76", codex = "0.77.0" }
```

## Releases

Relay uses `cargo-release` for consistent tags and version bumps.

```sh
cargo install cargo-release
cargo release patch --execute   # or minor/major
```

This updates `Cargo.toml`, commits, tags `vX.Y.Z`, and pushes. GitHub Actions
publishes the release artifacts for macOS + Linux.

## Contributing

Quick release guide:

- `cargo release patch --execute` for bug fixes
- `cargo release minor --execute` for new features
- `cargo release major --execute` for breaking changes

The workflow updates `Cargo.toml`, creates a `vX.Y.Z` tag, and pushes. CI
builds and uploads release artifacts automatically.

Contributors should run:

```sh
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

See `CONTRIBUTING.md` and `SECURITY.md` for contribution and disclosure policy.

## Roadmap

- `relay import` for project-level resources from `.relay/` into your current
  project (selectively importing skills, commands, agents, and rules).
- Import is intentionally deferred for now while ecosystem conventions are
  shifting toward `.agents/`-style layouts.
