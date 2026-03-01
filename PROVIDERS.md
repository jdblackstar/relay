# Providers

This is a living reference of how each tool exposes rules, commands, skills, and AGENTS.md.
Keep this up to date as tooling changes.

Each entry shows whether relay syncs that ability (`true`/`false`), followed by
the path layout.

## Rules (auto-loaded at conversation start)

- Codex: `true`. `$CODEX_HOME/rules/default.rules` (default `~/.codex/rules/default.rules`). Not Markdown; uses Starlark.
- Claude: `false`.
- OpenCode: no separate rules file; uses `AGENTS.md` (see below).
- Cursor: not supported by relay (project-scoped rules only; skills incomplete).

## Commands (slash prompts)

- Codex: `true`. `/prompt:name` maps to `$CODEX_HOME/prompts/name.md`. No project-level prompts.
- Claude: `true`. `/name` maps to `~/.claude/commands/name.md` and project `.claude/commands/`.
- OpenCode: `true`. Global `~/.config/opencode/command/name.md` and project `.opencode/command/name.md`.
- Cursor: not supported by relay (project-scoped rules only; skills incomplete).

## Skills (model-discoverable instructions)

- Codex: `true`. `$CODEX_HOME/skills` (default `~/.codex/skills`). Higher level overrides lower (user > project > plugin).
- Claude: `true`. `~/.claude/skills/<name>/SKILL.md` and project `.claude/skills/<name>/SKILL.md`.
  Requires `SKILL.md` with frontmatter `name:` and `description:`. Higher level overrides lower (user > project > plugin).
- OpenCode: `true`. Project `.opencode/skill/<name>/SKILL.md` and global `~/.config/opencode/skill/<name>/SKILL.md`.
  Also loads Claude-compatible skills from `.claude/skills/<name>/SKILL.md` (project + global).
- Cursor: not supported by relay (project-scoped rules only; skills incomplete).

## AGENTS.md

- Codex: `true`. Project `AGENTS.md` and global `~/.codex/AGENTS.md`.
- Claude: `false`.
- OpenCode: `true`. Project `AGENTS.md` and global `~/.config/opencode/AGENTS.md`. OpenCode combines project + global.

## Adding a provider (relay checklist)

1. Add default paths + env override in `src/config.rs`.
2. Add a new entry in `TOOL_DEFINITIONS` in `src/tools.rs`.
3. Wire sync behavior in `src/sync.rs` and watch paths in `src/watch.rs`.
4. Update README defaults and this document.
