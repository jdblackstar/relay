# Weekly Compatibility PR Automation

This repo includes `scripts/weekly-compat-pr.sh` to automate a weekly
compatibility sweep from a local machine (for example, an always-on Mac Mini).

The script will:

1. Optionally run upgrade commands for `codex`, `claude`, `cursor`, `opencode`.
2. Detect tool versions and update `docs/compat/verified-versions.toml`.
   - Refreshes `[tested_latest]` every run.
   - Preserves `[min_supported]` unless you edit it manually.
3. Run validation in an isolated sandbox:
   - `cargo test`
   - `cargo clippy --all-targets --all-features -- -D warnings`
   - `./scripts/compat-smoke.sh`
4. If versions changed, create/update a branch and open a PR with `gh`.
5. If any step fails, create a GitHub issue with failure details.

## Prerequisites

- `git`, `cargo`, and `gh` installed.
- `gh auth login` completed for the repo.
- Tool CLIs installed and usable on the machine (`codex`, `claude`, `cursor`, `opencode`).
- A clean working tree (tracked files) when the job starts.

## Configure Upgrades

1. Copy the example config:

```bash
cp scripts/compat-weekly.env.example .local/compat-weekly.env
```

2. Edit `.local/compat-weekly.env` and set machine-specific upgrade commands.

Notes:
- Package/formula names differ by installer and can change.
- Leave a command blank to skip upgrading that tool.

## Run Manually

```bash
./scripts/weekly-compat-pr.sh
```

Useful flags (set via env in `.local/compat-weekly.env` or inline):

- `COMPAT_DRY_RUN=1`: print actions, skip commands that mutate.
- `COMPAT_CREATE_PR=0`: run checks + commit only (no PR creation).
- `COMPAT_PUSH_BRANCH=0`: keep commit local.
- `COMPAT_REQUIRED_TOOLS="codex claude"`: reduce hard requirements.
- `COMPAT_CREATE_ISSUE=0`: disable automatic failure issues.

## Failure Issue Titles

Failure issue titles are generated with provider granularity:

- `failing upgrade to Claude Code 3.0.0` (single provider)
- `failing upgrade to multiple providers` (2+ providers, not all)
- `failing upgrade to all providers` (all configured providers)

## Support Window Policy

`docs/compat/verified-versions.toml` has two key sections:

- `[tested_latest]`: moving weekly snapshot from automation.
- `[min_supported]`: stable support floor that you change intentionally.

Recommended flow:

1. Let weekly automation move `[tested_latest]`.
2. Change `[min_supported]` manually when you intentionally drop older versions.
3. Mention `[min_supported]` bumps in release notes/changelog.

Example dry-run:

```bash
COMPAT_DRY_RUN=1 ./scripts/weekly-compat-pr.sh
```

## Schedule On Monday (launchd)

Create `~/Library/LaunchAgents/dev.relay.weekly-compat.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
  <dict>
    <key>Label</key>
    <string>dev.relay.weekly-compat</string>

    <key>ProgramArguments</key>
    <array>
      <string>/bin/bash</string>
      <string>-lc</string>
      <string>cd /Users/josh/code/relay && ./scripts/weekly-compat-pr.sh</string>
    </array>

    <key>StartCalendarInterval</key>
    <dict>
      <key>Weekday</key><integer>2</integer>
      <key>Hour</key><integer>9</integer>
      <key>Minute</key><integer>0</integer>
    </dict>

    <key>StandardOutPath</key>
    <string>/Users/josh/code/relay/.local/logs/weekly-compat.out.log</string>
    <key>StandardErrorPath</key>
    <string>/Users/josh/code/relay/.local/logs/weekly-compat.err.log</string>
    <key>RunAtLoad</key>
    <false/>
  </dict>
</plist>
```

Load it:

```bash
mkdir -p .local/logs
launchctl unload ~/Library/LaunchAgents/dev.relay.weekly-compat.plist 2>/dev/null || true
launchctl load ~/Library/LaunchAgents/dev.relay.weekly-compat.plist
```

`Weekday=2` is Monday in launchd calendar format.
