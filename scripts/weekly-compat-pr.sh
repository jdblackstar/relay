#!/usr/bin/env bash
set -Eeuo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

log() {
    printf '[compat] %s\n' "$*"
}

die() {
    local msg="$*"
    FAIL_MESSAGE="$msg"
    if [[ -n "$CURRENT_TOOL" ]]; then
        add_failed_tool "$CURRENT_TOOL"
    fi
    printf '[compat] error: %s\n' "$msg" >&2
    exit 1
}

set_context() {
    CURRENT_PHASE="$1"
    CURRENT_TOOL="${2:-}"
    CURRENT_COMMAND="${3:-}"
}

add_failed_tool() {
    local tool="$1"
    if [[ -z "$tool" ]]; then
        return 0
    fi
    case " $FAILED_TOOLS " in
        *" $tool "*) ;;
        *) FAILED_TOOLS="${FAILED_TOOLS:+$FAILED_TOOLS }$tool" ;;
    esac
}

add_failed_tools() {
    local tool
    for tool in $1; do
        add_failed_tool "$tool"
    done
}

count_words() {
    local value="$1"
    local count=0
    local _item
    for _item in $value; do
        count=$((count + 1))
    done
    printf '%s' "$count"
}

tool_in_list() {
    local target="$1"
    local list="$2"
    local item
    for item in $list; do
        if [[ "$item" == "$target" ]]; then
            return 0
        fi
    done
    return 1
}

require_cmd() {
    local cmd="$1"
    command -v "$cmd" >/dev/null 2>&1 || die "required command not found: ${cmd}"
}

toml_escape() {
    printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'
}

markdown_escape() {
    printf '%s' "$1" | sed -e 's/|/\\|/g' -e 's/`/\\`/g'
}

trim_spaces() {
    printf '%s' "$1" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//'
}

tool_bin() {
    case "$1" in
        codex) echo "codex" ;;
        claude) echo "claude" ;;
        cursor) echo "cursor" ;;
        opencode) echo "opencode" ;;
        *) echo "$1" ;;
    esac
}

tool_display_name() {
    case "$1" in
        codex) echo "Codex CLI" ;;
        claude) echo "Claude Code" ;;
        cursor) echo "Cursor CLI" ;;
        opencode) echo "OpenCode" ;;
        *) echo "$1" ;;
    esac
}

upgrade_var_name() {
    case "$1" in
        codex) echo "COMPAT_UPGRADE_CODEX_CMD" ;;
        claude) echo "COMPAT_UPGRADE_CLAUDE_CMD" ;;
        cursor) echo "COMPAT_UPGRADE_CURSOR_CMD" ;;
        opencode) echo "COMPAT_UPGRADE_OPENCODE_CMD" ;;
        *) echo "" ;;
    esac
}

run_shell_cmd() {
    local phase="$1"
    local tool="$2"
    local cmd="$3"
    if [[ -z "$cmd" ]]; then
        return 0
    fi

    set_context "$phase" "$tool" "$cmd"
    if [[ -n "$tool" ]]; then
        log "${phase} (${tool}): ${cmd}"
    else
        log "${phase}: ${cmd}"
    fi
    if [[ "$COMPAT_DRY_RUN" == "1" ]]; then
        return 0
    fi
    bash -lc "$cmd"
}

extract_version_token() {
    local raw="$1"
    local token
    token="$(printf '%s\n' "$raw" | grep -Eo '[0-9]+([.][0-9]+)+' | head -n 1 || true)"
    if [[ -z "$token" ]]; then
        token="$(printf '%s\n' "$raw" | grep -Eo '[0-9]+' | head -n 1 || true)"
    fi
    printf '%s' "$token"
}

is_required_tool() {
    local target="$1"
    local required
    for required in $COMPAT_REQUIRED_TOOLS; do
        if [[ "$required" == "$target" ]]; then
            return 0
        fi
    done
    return 1
}

collect_versions() {
    : > "$VERSIONS_TSV"

    local tool
    for tool in $COMPAT_TOOLS; do
        local bin
        local raw
        local token

        bin="$(tool_bin "$tool")"
        if ! command -v "$bin" >/dev/null 2>&1; then
            printf '%s\t%s\t%s\n' "$tool" "" "missing" >> "$VERSIONS_TSV"
            continue
        fi

        raw="$($bin --version 2>/dev/null | head -n 1 | tr -d '\r' || true)"
        raw="${raw//$'\t'/ }"
        token="$(extract_version_token "$raw")"
        if [[ -z "$token" ]]; then
            token="$raw"
        fi

        printf '%s\t%s\t%s\n' "$tool" "$token" "$raw" >> "$VERSIONS_TSV"
    done
}

min_supported_for_tool() {
    local target="$1"
    local tool
    local version
    if [[ ! -f "$MIN_SUPPORTED_TSV" ]]; then
        printf ''
        return 0
    fi
    while IFS=$'\t' read -r tool version; do
        if [[ "$tool" == "$target" ]]; then
            printf '%s' "$version"
            return 0
        fi
    done < "$MIN_SUPPORTED_TSV"
    printf ''
}

has_min_supported_for_tool() {
    local target="$1"
    local tool
    local version
    if [[ ! -f "$MIN_SUPPORTED_TSV" ]]; then
        return 1
    fi
    while IFS=$'\t' read -r tool version; do
        if [[ "$tool" == "$target" ]]; then
            return 0
        fi
    done < "$MIN_SUPPORTED_TSV"
    return 1
}

load_min_supported_versions() {
    local tool
    local token
    local raw

    : > "$MIN_SUPPORTED_TSV"

    if [[ -f "$COMPAT_VERSION_FILE" ]]; then
        awk '
            BEGIN { in_min = 0 }
            /^\[min_supported\]/ { in_min = 1; next }
            /^\[/ { if (in_min) exit; next }
            in_min && /^[[:space:]]*[A-Za-z0-9_-]+[[:space:]]*=/ {
                line = $0
                sub(/[[:space:]]*#.*/, "", line)
                split(line, parts, "=")
                key = parts[1]
                gsub(/[[:space:]]/, "", key)
                value = parts[2]
                sub(/^[[:space:]]*"/, "", value)
                sub(/"[[:space:]]*$/, "", value)
                if (key != "" && value != "") {
                    print key "\t" value
                }
            }
        ' "$COMPAT_VERSION_FILE" > "$MIN_SUPPORTED_TSV"
    fi

    while IFS=$'\t' read -r tool token raw; do
        if [[ "$raw" == "missing" || -z "$token" ]]; then
            continue
        fi
        if ! has_min_supported_for_tool "$tool"; then
            printf '%s\t%s\n' "$tool" "$token" >> "$MIN_SUPPORTED_TSV"
        fi
    done < "$VERSIONS_TSV"
}

tool_version_for_title() {
    local target="$1"
    local tool
    local token
    local raw

    if [[ -f "$VERSIONS_TSV" ]]; then
        while IFS=$'\t' read -r tool token raw; do
            if [[ "$tool" == "$target" && "$raw" != "missing" && -n "$token" ]]; then
                printf '%s' "$token"
                return 0
            fi
        done < "$VERSIONS_TSV"
    fi

    local bin
    bin="$(tool_bin "$target")"
    if command -v "$bin" >/dev/null 2>&1; then
        raw="$($bin --version 2>/dev/null | head -n 1 | tr -d '\r' || true)"
        token="$(extract_version_token "$raw")"
        printf '%s' "$token"
        return 0
    fi

    printf ''
}

assert_required_tools_present() {
    local missing=""
    local tool
    local token
    local raw

    while IFS=$'\t' read -r tool token raw; do
        if is_required_tool "$tool"; then
            if [[ "$raw" == "missing" || -z "$token" ]]; then
                missing+=" ${tool}"
            fi
        fi
    done < "$VERSIONS_TSV"

    if [[ -n "$missing" ]]; then
        add_failed_tools "$missing"
        die "required tools missing or unreadable:${missing}. Adjust PATH or COMPAT_REQUIRED_TOOLS."
    fi
}

write_version_snapshot() {
    local out_path="$1"
    local tmp
    local tool
    local token
    local raw
    local version

    mkdir -p "$(dirname "$out_path")"
    tmp="$(mktemp)"

    {
        echo "# Generated by scripts/weekly-compat-pr.sh"
        echo
        echo "[tested_latest]"
        while IFS=$'\t' read -r tool token raw; do
            if [[ "$raw" != "missing" && -n "$token" ]]; then
                printf '%s = "%s"\n' "$tool" "$(toml_escape "$token")"
            fi
        done < "$VERSIONS_TSV"
        echo
        echo "[min_supported]"
        for tool in $COMPAT_TOOLS; do
            version="$(min_supported_for_tool "$tool")"
            if [[ -n "$version" ]]; then
                printf '%s = "%s"\n' "$tool" "$(toml_escape "$version")"
            fi
        done
        while IFS=$'\t' read -r tool version; do
            if tool_in_list "$tool" "$COMPAT_TOOLS"; then
                continue
            fi
            if [[ -n "$version" ]]; then
                printf '%s = "%s"\n' "$tool" "$(toml_escape "$version")"
            fi
        done < "$MIN_SUPPORTED_TSV"
        echo
        echo "[raw_versions]"
        while IFS=$'\t' read -r tool token raw; do
            printf '%s = "%s"\n' "$tool" "$(toml_escape "$raw")"
        done < "$VERSIONS_TSV"
    } > "$tmp"

    mv "$tmp" "$out_path"
}

write_test_config() {
    local cfg_path
    local tool
    local token
    local raw

    cfg_path="${RELAY_HOME}/.config/relay/config.toml"
    mkdir -p "$(dirname "$cfg_path")"

    {
        echo 'enabled_tools = ["claude", "codex", "cursor", "opencode"]'
        echo
        echo '[verified_versions]'
        while IFS=$'\t' read -r tool token raw; do
            if [[ "$raw" != "missing" && -n "$token" ]]; then
                printf '%s = "%s"\n' "$tool" "$(toml_escape "$token")"
            fi
        done < "$VERSIONS_TSV"
    } > "$cfg_path"
}

run_validation_suite() {
    local original_home
    original_home="$HOME"

    if [[ "$COMPAT_DRY_RUN" == "1" ]]; then
        log "dry-run: skipping validation suite"
        return 0
    fi

    set_context "validation:setup-test-env" "" "./scripts/setup-test-env.sh ${COMPAT_TEST_ENV}"
    log "setting up isolated test env: ${COMPAT_TEST_ENV}"
    rm -rf "./.local/test-envs/${COMPAT_TEST_ENV}"
    ./scripts/setup-test-env.sh "$COMPAT_TEST_ENV" >/dev/null

    # shellcheck disable=SC1091
    source "./.local/test-envs/${COMPAT_TEST_ENV}/env.sh"

    if [[ -d "${original_home}/.rustup" ]]; then
        export RUSTUP_HOME="${RUSTUP_HOME:-${original_home}/.rustup}"
    fi
    if [[ -d "${original_home}/.cargo" ]]; then
        export CARGO_HOME="${CARGO_HOME:-${original_home}/.cargo}"
        export PATH="${CARGO_HOME}/bin:${PATH}"
    fi
    export HOME="$RELAY_HOME"

    set_context "validation:write-test-config" "" "write_test_config"
    write_test_config

    set_context "validation:cargo-test" "" "cargo test"
    log "running cargo test"
    cargo test

    set_context "validation:clippy" "" "cargo clippy --all-targets --all-features -- -D warnings"
    log "running cargo clippy"
    cargo clippy --all-targets --all-features -- -D warnings

    set_context "validation:compat-smoke" "" "./scripts/compat-smoke.sh"
    log "running compatibility smoke checks"
    ./scripts/compat-smoke.sh
}

ensure_git_clean() {
    if [[ "$COMPAT_ALLOW_DIRTY" == "1" ]]; then
        return 0
    fi
    if ! git diff --quiet || ! git diff --cached --quiet; then
        die "tracked working tree is dirty; commit/stash changes or set COMPAT_ALLOW_DIRTY=1"
    fi
}

sync_base_branch() {
    if [[ "$COMPAT_SKIP_GIT_SYNC" == "1" ]]; then
        log "skipping fetch/pull (COMPAT_SKIP_GIT_SYNC=1)"
        return 0
    fi

    if [[ "$COMPAT_DRY_RUN" == "1" ]]; then
        log "dry-run: would switch to ${COMPAT_BASE_BRANCH} and pull from ${COMPAT_REMOTE}"
        return 0
    fi

    log "syncing ${COMPAT_BASE_BRANCH} from ${COMPAT_REMOTE}"
    set_context "git:fetch" "" "git fetch ${COMPAT_REMOTE} ${COMPAT_BASE_BRANCH}"
    git fetch "$COMPAT_REMOTE" "$COMPAT_BASE_BRANCH"
    set_context "git:switch" "" "git switch ${COMPAT_BASE_BRANCH}"
    git switch "$COMPAT_BASE_BRANCH"
    set_context "git:pull" "" "git pull --ff-only ${COMPAT_REMOTE} ${COMPAT_BASE_BRANCH}"
    git pull --ff-only "$COMPAT_REMOTE" "$COMPAT_BASE_BRANCH"
}

build_pr_body() {
    local out_path="$1"
    local run_date="$2"
    local tool
    local token
    local raw
    local version_cell
    local raw_cell

    {
        echo "## Weekly Compatibility Refresh"
        echo
        echo "Run date (UTC): ${run_date}"
        echo
        echo "Detected CLI versions:"
        echo
        echo "| Tool | Version | Raw --version output |"
        echo "| --- | --- | --- |"
        while IFS=$'\t' read -r tool token raw; do
            if [[ "$raw" == "missing" ]]; then
                version_cell="missing"
                raw_cell="command not found"
            else
                version_cell="$token"
                raw_cell="$raw"
            fi
            printf '| `%s` | `%s` | `%s` |\n' \
                "$(markdown_escape "$tool")" \
                "$(markdown_escape "$version_cell")" \
                "$(markdown_escape "$raw_cell")"
        done < "$VERSIONS_TSV"
        echo
        echo "Validation run:"
        echo "- \`cargo test\`"
        echo "- \`cargo clippy --all-targets --all-features -- -D warnings\`"
        echo "- \`./scripts/compat-smoke.sh\`"
        echo
        echo "Updated file:"
        echo "- \`${COMPAT_VERSION_FILE}\` (\`[tested_latest]\` refreshed; \`[min_supported]\` preserved)"
    } > "$out_path"
}

open_or_update_pr() {
    local branch_name="$1"
    local title="$2"
    local body_file="$3"
    local existing
    local created

    existing="$(gh pr list --head "$branch_name" --base "$COMPAT_BASE_BRANCH" --state open --json url --jq '.[0].url' 2>/dev/null || true)"

    if [[ -n "$existing" && "$existing" != "null" ]]; then
        gh pr edit "$existing" --title "$title" --body-file "$body_file" >/dev/null
        log "updated PR: ${existing}"
    else
        created="$(gh pr create --base "$COMPAT_BASE_BRANCH" --head "$branch_name" --title "$title" --body-file "$body_file")"
        log "created PR: ${created}"
    fi
}

build_failure_issue_title() {
    local failed_count
    local total_count
    failed_count="$(count_words "$FAILED_TOOLS")"
    total_count="$(count_words "$COMPAT_TOOLS")"

    if [[ "$failed_count" -eq 1 ]]; then
        local single_tool
        local display
        local version
        single_tool="$FAILED_TOOLS"
        display="$(tool_display_name "$single_tool")"
        version="$(tool_version_for_title "$single_tool")"
        if [[ -n "$version" ]]; then
            printf 'failing upgrade to %s %s' "$display" "$version"
        else
            printf 'failing upgrade to %s' "$display"
        fi
        return 0
    fi

    if [[ "$failed_count" -gt 0 && "$failed_count" -eq "$total_count" ]]; then
        printf 'failing upgrade to all providers'
        return 0
    fi

    printf 'failing upgrade to multiple providers'
}

write_versions_table_markdown() {
    local out_file="$1"
    local tool
    local token
    local raw
    local version_cell
    local raw_cell

    if [[ ! -s "$VERSIONS_TSV" ]]; then
        collect_versions || true
    fi

    {
        echo "| Tool | Version | Raw --version output |"
        echo "| --- | --- | --- |"
        while IFS=$'\t' read -r tool token raw; do
            if [[ "$raw" == "missing" ]]; then
                version_cell="missing"
                raw_cell="command not found"
            else
                version_cell="$token"
                raw_cell="$raw"
            fi
            printf '| `%s` | `%s` | `%s` |\n' \
                "$(markdown_escape "$tool")" \
                "$(markdown_escape "$version_cell")" \
                "$(markdown_escape "$raw_cell")"
        done < "$VERSIONS_TSV"
    } >> "$out_file"
}

write_failure_issue_body() {
    local out_file="$1"
    local exit_code="$2"
    local now_utc
    local git_sha
    local git_branch

    now_utc="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
    git_sha="$(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
    git_branch="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown)"

    {
        echo "## Weekly compatibility run failed"
        echo
        echo "- Date (UTC): ${now_utc}"
        echo "- Repo: ${REPO_ROOT}"
        echo "- Branch: ${git_branch}"
        echo "- Commit: ${git_sha}"
        echo "- Exit code: ${exit_code}"
        echo "- Phase: ${CURRENT_PHASE:-unknown}"
        if [[ -n "$CURRENT_TOOL" ]]; then
            echo "- Tool context: ${CURRENT_TOOL}"
        fi
        if [[ -n "$CURRENT_COMMAND" ]]; then
            echo "- Command: \`${CURRENT_COMMAND}\`"
        fi
        if [[ -n "$FAIL_MESSAGE" ]]; then
            echo "- Error: ${FAIL_MESSAGE}"
        fi
        if [[ -n "$COMPAT_LOG_HINT" ]]; then
            echo "- Log hint: \`${COMPAT_LOG_HINT}\`"
        fi
        echo
        if [[ -n "$FAILED_TOOLS" ]]; then
            echo "Failed providers: ${FAILED_TOOLS}"
        else
            echo "Failed providers: unknown (global failure)"
        fi
        echo
        echo "Detected versions:"
    } > "$out_file"

    write_versions_table_markdown "$out_file"

    if [[ -n "$COMPAT_LOG_HINT" && -f "$COMPAT_LOG_HINT" ]]; then
        {
            echo
            echo "Log tail (${COMPAT_LOG_SNIPPET_LINES} lines):"
            echo
            echo '```text'
            tail -n "$COMPAT_LOG_SNIPPET_LINES" "$COMPAT_LOG_HINT"
            echo '```'
        } >> "$out_file"
    fi
}

create_failure_issue() {
    local exit_code="$1"

    if [[ "$COMPAT_CREATE_ISSUE" != "1" ]]; then
        return 0
    fi

    if ! command -v gh >/dev/null 2>&1; then
        log "failure issue skipped: gh not found"
        return 0
    fi

    if [[ -z "$FAILED_TOOLS" && -n "$CURRENT_TOOL" ]]; then
        add_failed_tool "$CURRENT_TOOL"
    fi

    local title
    local body_file
    local issue_url
    local issue_cmd
    local old_ifs
    local label
    local assignee

    title="$(build_failure_issue_title)"
    body_file="$(mktemp)"
    ISSUE_BODY_FILE="$body_file"
    write_failure_issue_body "$body_file" "$exit_code"

    issue_cmd=(gh issue create --title "$title" --body-file "$body_file")
    if [[ -n "$COMPAT_ISSUE_REPO" ]]; then
        issue_cmd+=(--repo "$COMPAT_ISSUE_REPO")
    fi

    old_ifs="$IFS"
    IFS=','
    for label in $COMPAT_ISSUE_LABELS; do
        label="$(trim_spaces "$label")"
        if [[ -n "$label" ]]; then
            issue_cmd+=(--label "$label")
        fi
    done
    IFS="$old_ifs"

    old_ifs="$IFS"
    IFS=','
    for assignee in $COMPAT_ISSUE_ASSIGNEES; do
        assignee="$(trim_spaces "$assignee")"
        if [[ -n "$assignee" ]]; then
            issue_cmd+=(--assignee "$assignee")
        fi
    done
    IFS="$old_ifs"

    if issue_url="$("${issue_cmd[@]}" 2>&1)"; then
        log "created failure issue: ${issue_url}"
    else
        log "failed to create failure issue: ${issue_url}"
        return 1
    fi
}

on_err() {
    local rc="$1"
    local line="$2"
    local cmd="$3"

    if [[ -z "$FAIL_MESSAGE" ]]; then
        FAIL_MESSAGE="command failed with exit ${rc} at line ${line}"
    fi
    if [[ -z "$CURRENT_COMMAND" ]]; then
        CURRENT_COMMAND="$cmd"
    fi
    if [[ -n "$CURRENT_TOOL" ]]; then
        add_failed_tool "$CURRENT_TOOL"
    fi
}

cleanup() {
    rm -f "$VERSIONS_TSV"
    rm -f "$MIN_SUPPORTED_TSV"
    if [[ -n "$PR_BODY_FILE" ]]; then
        rm -f "$PR_BODY_FILE"
    fi
    if [[ -n "$ISSUE_BODY_FILE" ]]; then
        rm -f "$ISSUE_BODY_FILE"
    fi
}

on_exit() {
    local rc="$1"
    trap - ERR
    set +e

    if [[ "$rc" -ne 0 ]]; then
        create_failure_issue "$rc" || log "failed to create issue for failed run"
        log "weekly compatibility run failed"
    fi

    cleanup
    return 0
}

COMPAT_ENV_FILE="${COMPAT_ENV_FILE:-.local/compat-weekly.env}"
if [[ -f "$COMPAT_ENV_FILE" ]]; then
    # shellcheck disable=SC1090
    source "$COMPAT_ENV_FILE"
    log "loaded env overrides from ${COMPAT_ENV_FILE}"
fi

: "${COMPAT_TOOLS:=codex claude cursor opencode}"
: "${COMPAT_REQUIRED_TOOLS:=${COMPAT_TOOLS}}"
: "${COMPAT_BASE_BRANCH:=main}"
: "${COMPAT_REMOTE:=origin}"
: "${COMPAT_BRANCH_PREFIX:=automation/weekly-compat}"
: "${COMPAT_VERSION_FILE:=docs/compat/verified-versions.toml}"
: "${COMPAT_TEST_ENV:=compat-weekly}"
: "${COMPAT_ALLOW_DIRTY:=0}"
: "${COMPAT_SKIP_GIT_SYNC:=0}"
: "${COMPAT_PUSH_BRANCH:=1}"
: "${COMPAT_CREATE_PR:=1}"
: "${COMPAT_CREATE_ISSUE:=1}"
: "${COMPAT_ISSUE_LABELS:=compat,weekly-upgrade,breaking-change}"
: "${COMPAT_ISSUE_ASSIGNEES:=}"
: "${COMPAT_ISSUE_REPO:=}"
: "${COMPAT_LOG_HINT:=.local/logs/weekly-compat.err.log}"
: "${COMPAT_LOG_SNIPPET_LINES:=120}"
: "${COMPAT_DRY_RUN:=0}"
: "${COMPAT_PRE_UPGRADE_CMD:=}"
: "${COMPAT_POST_UPGRADE_CMD:=}"
: "${COMPAT_EXTRA_PATH:=/opt/homebrew/bin:/usr/local/bin}"

if [[ -n "$COMPAT_EXTRA_PATH" ]]; then
    export PATH="${COMPAT_EXTRA_PATH}:${PATH}"
fi

VERSIONS_TSV="$(mktemp)"
MIN_SUPPORTED_TSV="$(mktemp)"
PR_BODY_FILE=""
ISSUE_BODY_FILE=""
CURRENT_PHASE="startup"
CURRENT_TOOL=""
CURRENT_COMMAND=""
FAIL_MESSAGE=""
FAILED_TOOLS=""

trap 'on_err $? $LINENO "$BASH_COMMAND"' ERR
trap 'on_exit $?' EXIT

set_context "preflight:require git" "" "command -v git"
require_cmd git
set_context "preflight:require cargo" "" "command -v cargo"
require_cmd cargo
set_context "preflight:require sed" "" "command -v sed"
require_cmd sed

if [[ "$COMPAT_CREATE_PR" == "1" || "$COMPAT_CREATE_ISSUE" == "1" ]]; then
    set_context "preflight:require gh" "" "command -v gh"
    require_cmd gh
fi

set_context "git:ensure-clean" "" "git diff --quiet"
ensure_git_clean
set_context "git:sync-base" "" "sync base branch"
sync_base_branch

run_shell_cmd "upgrade:pre" "" "$COMPAT_PRE_UPGRADE_CMD"
for tool in $COMPAT_TOOLS; do
    var_name="$(upgrade_var_name "$tool")"
    cmd=""
    if [[ -n "$var_name" ]]; then
        cmd="${!var_name:-}"
    fi
    if [[ -z "$cmd" ]]; then
        log "no upgrade command configured for ${tool}"
        continue
    fi
    run_shell_cmd "upgrade:tool" "$tool" "$cmd"
done
run_shell_cmd "upgrade:post" "" "$COMPAT_POST_UPGRADE_CMD"

set_context "versions:collect" "" "collect_versions"
collect_versions
set_context "versions:assert-required" "" "assert_required_tools_present"
assert_required_tools_present
set_context "versions:load-min-supported" "" "load_min_supported_versions"
load_min_supported_versions
set_context "versions:write-snapshot" "" "write_version_snapshot ${COMPAT_VERSION_FILE}"
write_version_snapshot "$COMPAT_VERSION_FILE"
set_context "validation:run" "" "run_validation_suite"
run_validation_suite

if [[ "$COMPAT_DRY_RUN" == "1" ]]; then
    log "dry-run complete"
    exit 0
fi

set_context "git:diff-version-file" "" "git diff -- ${COMPAT_VERSION_FILE}"
if git diff --quiet -- "$COMPAT_VERSION_FILE"; then
    log "no version changes detected in ${COMPAT_VERSION_FILE}; skipping PR"
    exit 0
fi

RUN_DATE_UTC="$(date -u +"%Y-%m-%d")"
RUN_STAMP="$(date -u +"%Y%m%d")"
BRANCH_NAME="${COMPAT_BRANCH_PREFIX}-${RUN_STAMP}"
TITLE="chore: weekly compatibility refresh (${RUN_DATE_UTC})"

set_context "git:create-branch" "" "git switch -C ${BRANCH_NAME}"
log "creating branch ${BRANCH_NAME}"
git switch -C "$BRANCH_NAME"
set_context "git:add" "" "git add ${COMPAT_VERSION_FILE}"
git add "$COMPAT_VERSION_FILE"
set_context "git:commit" "" "git commit -m ${TITLE}"
git commit -m "$TITLE"

if [[ "$COMPAT_PUSH_BRANCH" == "1" ]]; then
    set_context "git:push" "" "git push --force-with-lease -u ${COMPAT_REMOTE} ${BRANCH_NAME}"
    git push --force-with-lease -u "$COMPAT_REMOTE" "$BRANCH_NAME"
else
    log "COMPAT_PUSH_BRANCH=0, leaving local commit only"
fi

if [[ "$COMPAT_CREATE_PR" == "1" ]]; then
    if [[ "$COMPAT_PUSH_BRANCH" != "1" ]]; then
        die "COMPAT_CREATE_PR=1 requires COMPAT_PUSH_BRANCH=1"
    fi
    PR_BODY_FILE="$(mktemp)"
    set_context "pr:build-body" "" "build_pr_body"
    build_pr_body "$PR_BODY_FILE" "$RUN_DATE_UTC"
    set_context "pr:open-or-update" "" "gh pr create/edit"
    open_or_update_pr "$BRANCH_NAME" "$TITLE" "$PR_BODY_FILE"
fi

log "weekly compatibility run complete"
