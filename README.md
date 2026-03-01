# relay

If you use multiple AI coding tools (Claude, Codex, Cursor, OpenCode), you end
up with the same slash commands, skills, and agent/rule files scattered across
each tool's config directory. Relay keeps them in sync.

It mirrors content into every enabled tool directory and a central store at
`~/.config/relay`, using last-write-wins to resolve conflicts. You can run it
once with `relay sync`, or leave `relay watch` running to stay aligned
automatically.

## Install

```sh
brew install jdblackstar/tap/relay
```

Or use the install script:

```sh
curl -fsSL https://raw.githubusercontent.com/jdblackstar/relay/main/install.sh | sh
```

Or download a release archive from GitHub and place the `relay` binary on your
PATH.

## CLI Usage

```sh
relay [--debug] [--debug-log-file <path>] init
relay [--debug] [--debug-log-file <path>] sync [-p|--plan|-a|--apply] [-v|--verbose|-q|--quiet] [-c|--confirm-versions]
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
Init detects installed tool directories and lets you pick which ones to sync.
Use Space to toggle selections and Enter to confirm.

`relay sync --plan` previews changes without writing files.
`relay sync --apply` executes writes and records a history event.

`relay watch` is event-driven with a small debounce and keeps copies aligned.
`relay watch --daemon` installs/updates and starts a native background service:
- macOS: `launchd` user agent
- Linux: `systemd --user` service

`relay daemon` exposes explicit lifecycle control for that service.
`relay status` is shorthand for `relay daemon status`.

`relay history` lists recorded sync/watch/rollback events.
Watch-triggered history entries include source context in `origin` when
available (example: `watch:codex:review.md`).

`relay rollback` restores paths from a previous history event.
`--debug` enables file logging for deeper troubleshooting.

## Safety Model

- `relay sync --plan`: preview writes without changing files.
- `relay sync --apply`: execute writes and record a history event.
- `relay watch`: auto-apply writes on file events and record history events.
- `relay watch --daemon`: run watch as native background service.
- `relay rollback`: restore paths from a recorded event.
- `relay rollback` validates current file state before restoring; use `--force`
  only when you intentionally want to override newer edits.
- `relay rollback` restores the paths written by the chosen event (for example,
  mirrored targets from a watch sync), which may not include the original
  source file that triggered the sync.

## Synced Paths

Commands:

- Central store: `~/.config/relay/commands`
- Claude commands: `$CLAUDE_HOME/commands` (default `~/.claude/commands`)
- Cursor commands: `$CURSOR_HOME/commands` (default `~/.cursor/commands`)
- OpenCode commands: `$OPENCODE_HOME/command` (default `~/.config/opencode/command`)
- Codex prompts: `$CODEX_HOME/prompts` (default `~/.codex/prompts`)

Skills:

- Central store: `~/.config/relay/skills`
- Claude skills: `$CLAUDE_HOME/skills` (default `~/.claude/skills`)
- OpenCode skills: `$OPENCODE_HOME/skill` (default `~/.config/opencode/skill`)
- Codex skills: `$CODEX_HOME/skills` (default `~/.codex/skills`)

Agents:

- Central store: `~/.config/relay/agents`
- Codex AGENTS: `$CODEX_HOME/AGENTS.md` (default `~/.codex/AGENTS.md`)
- OpenCode AGENTS: `$OPENCODE_HOME/AGENTS.md` (default `~/.config/opencode/AGENTS.md`)

Rules:

- Central store: `~/.config/relay/rules`
- Codex rules: `$CODEX_HOME/rules/default.rules` (default `~/.codex/rules/default.rules`)

Commands are markdown files (e.g. `review.md`). Codex invokes them as
`/prompt:<name>`, but the file is stored as `<name>.md` in
`$CODEX_HOME/prompts`.

Skills are directories named after the skill, with a `SKILL.md` inside (e.g.
`review/SKILL.md`).

Claude and OpenCode also read project commands from `.claude/commands/` and
`.opencode/command/`, plus project skills from `.claude/skills/<name>/SKILL.md`
and `.opencode/skill/<name>/SKILL.md`; relay currently syncs global locations
only.

### XDG Notes

- Relay follows `XDG_CONFIG_HOME` for config-style paths.
- If `XDG_CONFIG_HOME` is not set, relay uses `$HOME/.config`.
- `XDG_HOME` is not a standard XDG variable.
- In `config.toml` and path env vars, use concrete paths or supported forms:
  `~`, `$HOME`, `${HOME}`, `$XDG_CONFIG_HOME`, `${XDG_CONFIG_HOME}`, and
  `${XDG_CONFIG_HOME:-$HOME/.config}`.

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

## Notes and Limitations

Sync behavior:

- Two-way sync is last-write-wins across tool directories; relay mirrors the
  winning content into every synced location, including the central folder.
- Edits made in `~/.config/relay` are treated the same as edits in tool
  directories and will propagate on the next sync/watch cycle.
- In watch mode, if two tools are edited within about 2 seconds and the
  content differs, relay prints a warning and still uses last-write-wins.
- Only selected tools are synced and watched.
- Applied writes are recorded under `~/.config/relay/history` (events + blobs)
  for transparency and rollback.

File types:

- Skills are synced as directories, not single files, and must include `SKILL.md`.
- Claude skills require frontmatter `name:` and `description:` in `SKILL.md`.
- Codex skills are synced as directories with `SKILL.md` (same layout as Claude/OpenCode).
- AGENTS and rules are synced as files per tool into the central store.
- OpenCode does not have a separate rules file; it uses `AGENTS.md` instead.
- Legacy Codex files prefixed with `prompt:` are supported; relay writes plain
  filenames for new copies.

Frontmatter:

- Frontmatter body is ignored for change detection except `name:` and
  `description:` when both are present in valid frontmatter.
- Relay syncs `name:` and `description:` across tools; other frontmatter fields
  remain tool-specific.
- If frontmatter is missing or malformed, relay skips frontmatter sync and logs
  a warning.

Symlinks:

- Relay follows symlinks for command files and skill folders. Symlinks inside
  skill folders are ignored to avoid loops.

Dotfiles integration:

- If `~/.dotfiles` is detected during init, relay can optionally move existing
  `~/.config/relay` data into `~/.dotfiles/config/relay` and symlink
  `~/.config/relay` to the dotfiles location.

Version checks:

- Version checks for `codex` and `claude` are best-effort and informational.

Current limitations:

- Windows is not supported yet.
- `relay watch` does not auto-detect new tool install directories; rerun
  `relay sync` or restart `relay watch` after installing tools.
- Only global locations are synced; project-level commands/skills are not
  imported yet.
- Frontmatter compatibility is best-effort; relay does not rewrite or validate
  provider-specific frontmatter yet. A future `relay import`/`relay lint` will
  help normalize and validate per tool.
- Cursor commands are synced; skills, agents, and rules are not supported
  because Cursor only offers project-scoped rules and partial skills.

## Roadmap

- `relay import` for project-level resources from `.relay/` into your current
  project (selectively importing skills, commands, and prompts).
- Import is intentionally deferred for now while ecosystem conventions are
  shifting toward `.agents/`-style layouts.

## See Also

- [Debugging guide](docs/debugging.md) — troubleshooting, debug logs, local sandbox testing
- [Apple container smoke test](docs/smoke-container.md) — isolated end-to-end smoke test
- [Weekly compatibility PRs](docs/weekly-compat-pr.md) — automated tool upgrade validation
- [Contributing](CONTRIBUTING.md) — development setup, adding tools, release process
- [Providers](PROVIDERS.md) — per-tool path and capability reference
- [Security](SECURITY.md) — vulnerability reporting
