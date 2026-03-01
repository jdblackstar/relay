# Changelog automation

When you merge a branch into `main`, the changelog is updated automatically with a deterministic entry. When you cut a release, the accumulated entries become the release notes (optionally polished by an agent).

## On merge to main

A GitHub Action (`.github/workflows/changelog.yml`) runs on every push to `main`.

- **Skips bot commits** so the workflow's own "chore: changelog" push doesn't loop.
- **Detects the PR number** from either:
  - Merge commits: `Merge pull request #123 from ...`
  - Squash-merges: `Some title (#123)`
- **Fetches the PR title** via `gh pr view` and uses it as the changelog bullet.
- **Categorizes by label.** PR labels map to Keep a Changelog sections:
  - `bug`, `fix`, `bugfix` → `### Fixed`
  - `enhancement`, `refactor`, `breaking` → `### Changed`
  - `deprecation` → `### Deprecated`
  - `removal` → `### Removed`
  - `security` → `### Security`
  - Anything else (or no label) → `### Added`
- Commits and pushes the updated `CHANGELOG.md` to `main`.

If a push to `main` has no PR number (e.g. a direct push), the workflow skips.

## On release (tag push)

When you push a tag like `v0.3.0`, the release workflow does two things:

1. **Publish job** — builds artifacts and creates a GitHub Release. The release body is the raw `[Unreleased]` section from `CHANGELOG.md`.

2. **Changelog cut job** — checks out `main`, renames `[Unreleased]` to `## [0.3.0] - YYYY-MM-DD`, inserts a new empty `[Unreleased]` section, updates the compare links, and pushes to `main`.

### Agent-polished release notes (opt-in)

The release workflow has a commented-out block that uses an agent CLI (e.g. Claude Code) to rewrite the raw bullets into polished prose before publishing. To enable it:

1. Uncomment the "Install agent CLI" and "Polish release notes" steps in `release.yml`.
2. Add `ANTHROPIC_API_KEY` (or your agent's key) to repo secrets.
3. Change the Publish step's `body:` from `steps.raw.outputs.body` to `steps.polished.outputs.body`.

This is where the agent adds real value — rewriting accumulated bullets once per release, not on every merge.

## Format

`CHANGELOG.md` follows [Keep a Changelog](https://keepachangelog.com/). You can always edit it by hand to tweak wording or move items between sections.

## Label tips

Add labels to your PRs before merging to get automatic categorization:

| Label | Section |
|-------|---------|
| `bug`, `fix`, `bugfix` | Fixed |
| `enhancement`, `refactor`, `breaking` | Changed |
| `deprecation` | Deprecated |
| `removal` | Removed |
| `security` | Security |
| *(anything else)* | Added |
