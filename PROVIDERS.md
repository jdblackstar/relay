# Providers

This is a living reference of how each tool exposes rules, commands, skills, and AGENTS.md.
Keep this up to date as tooling changes.

## Rules (auto-loaded at conversation start)

- Codex: `true`. `$CODEX_HOME/rules/default.rules` (default `~/.codex/rules/default.rules`). Not Markdown; uses Starlark.
- Claude: `false`.
- OpenCode: no separate rules file; uses `AGENTS.md` (see below).
- Cursor: not supported by relay (project-scoped rules only; skills incomplete).

## Commands (slash prompts)

- Codex: `true`. Relay writes generated command skill wrappers into the configured Codex skill store (`~/.agents/skills` by default). Generated wrappers include `.relay-command`, are ignored as skill sources, and are skipped when a real skill already owns the same name. Relay ignores old `$CODEX_HOME/prompts` command files.
- Claude: `true`. `/name` maps to `~/.claude/commands/name.md` and project `.claude/commands/`.
- OpenCode: `true`. Global `~/.config/opencode/command/name.md` and project `.opencode/command/name.md`.
- Cursor: not supported by relay (project-scoped rules only; skills incomplete).

## Skills (model-discoverable instructions)

- Codex: `true`. Relay's default user store is `~/.agents/skills`; legacy `$CODEX_HOME/skills` is import-only during migration. Higher level overrides lower (user > project > plugin).
- Claude: `true`. `~/.claude/skills/<name>/SKILL.md` and project `.claude/skills/<name>/SKILL.md`. Relay maintains the global directory as a read/write compatibility adapter for the canonical `~/.agents/skills` store.
  Requires `SKILL.md` with frontmatter `name:` and `description:`. Higher level overrides lower (user > project > plugin).
- OpenCode: `true`. Relay's default user store is `~/.agents/skills`; legacy global `~/.config/opencode/skill/<name>/SKILL.md` and `skills/` are import-only during migration. Project `.opencode/skill/<name>/SKILL.md` remains project-owned.
- Cursor: Relay does not maintain a tool-specific skill copy; shared-store discovery is preferred.

## AGENTS.md

- Codex: `true`. Project `AGENTS.md` and global `~/.codex/AGENTS.md`.
- Claude: `false`.
- OpenCode: `true`. Project `AGENTS.md` and global `~/.config/opencode/AGENTS.md`. OpenCode combines project + global.

## Adding a provider (relay checklist)

1. Add default paths + env override in `src/config.rs`.
2. Add the provider to `TOOL_SPECS` and `tool_paths` in `src/main.rs` (init + detection + notes).
3. Wire sync behavior in `src/sync.rs` and watch paths in `src/watch.rs`.
4. Update README defaults and this document.
