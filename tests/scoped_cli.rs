use std::fs;
use std::io;
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn relay_command(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_relay"));
    command
        .env("RELAY_HOME", home)
        .env_remove("RELAY_CONFIG_DIR")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("CODEX_HOME")
        .env_remove("CLAUDE_HOME")
        .env_remove("CURSOR_HOME")
        .env_remove("OPENCODE_HOME");
    command
}

fn relay(home: &Path, args: &[&str]) -> io::Result<Output> {
    relay_command(home).args(args).output()
}

fn write_skill(root: &Path, name: &str, body: &str) -> io::Result<PathBuf> {
    let path = root.join(name);
    fs::create_dir_all(&path)?;
    fs::write(
        path.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {name} description\n---\n{body}"),
    )?;
    Ok(path)
}

fn utf8(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).expect("process output should be UTF-8")
}

fn initialize(home: &Path) -> io::Result<()> {
    let root = home.join("relay-data");
    let config = format!(
        r#"enabled_tools = ["claude"]
central_dir = "{}"
central_skills_dir = "{}"
central_agents_dir = "{}"
central_rules_dir = "{}"
claude_dir = "{}"
claude_skills_dir = "{}"
"#,
        root.join("commands").display(),
        root.join("skills").display(),
        root.join("agents").display(),
        root.join("rules").display(),
        home.join("claude/commands").display(),
        home.join("claude/skills").display(),
    );
    let path = home.join(".config/relay/config.toml");
    fs::create_dir_all(path.parent().expect("config has parent"))?;
    fs::write(path, config)
}

#[cfg(unix)]
fn initialize_with_verified_claude(home: &Path, version: &str) -> io::Result<()> {
    initialize(home)?;
    let path = home.join(".config/relay/config.toml");
    let mut config = fs::read_to_string(&path)?;
    config.push_str(&format!("\n[verified_versions]\nclaude = \"{version}\"\n"));
    fs::write(path, config)
}

fn initialize_all_tools(home: &Path) -> io::Result<()> {
    let root = home.join("relay-data");
    let config = format!(
        r#"enabled_tools = ["claude", "codex", "cursor", "opencode"]
central_dir = "{}"
central_skills_dir = "{}"
central_agents_dir = "{}"
central_rules_dir = "{}"
claude_dir = "{}"
claude_skills_dir = "{}"
cursor_dir = "{}"
opencode_commands_dir = "{}"
opencode_skills_dir = "{}"
opencode_agents_file = "{}"
codex_skills_dir = "{}"
codex_rules_file = "{}"
codex_agents_file = "{}"
"#,
        root.join("commands").display(),
        root.join("skills").display(),
        root.join("agents").display(),
        root.join("rules").display(),
        home.join("claude/commands").display(),
        home.join("claude/skills").display(),
        home.join("cursor/commands").display(),
        home.join("opencode/commands").display(),
        home.join("opencode/skills").display(),
        home.join("opencode/AGENTS.md").display(),
        home.join("codex/skills").display(),
        home.join("codex/rules/default.rules").display(),
        home.join("codex/AGENTS.md").display(),
    );
    let path = home.join(".config/relay/config.toml");
    fs::create_dir_all(path.parent().expect("config has parent"))?;
    fs::write(path, config)
}

#[test]
fn capabilities_json_uses_real_entrypoint_without_initialization() -> io::Result<()> {
    let tmp = TempDir::new()?;

    let output = relay(tmp.path(), &["capabilities", "--json"])?;

    assert!(output.status.success());
    assert_eq!(
        utf8(&output.stdout),
        "{\"schema_version\":1,\"capabilities\":{\"skills.sync.scoped\":1}}\n"
    );
    assert_eq!(output.stderr, b"");
    assert!(!tmp.path().join(".config/relay").exists());
    Ok(())
}

#[test]
fn scoped_plan_supports_one_or_many_operands_without_initialization() -> io::Result<()> {
    for count in [1, 2] {
        let tmp = TempDir::new()?;
        let source = tmp.path().join("source");
        let first = write_skill(&source, "first", "First")?;
        let second = write_skill(&source, "second", "Second")?;
        let mut args = vec!["sync", "skill", "--plan"];
        args.push(first.to_str().expect("temporary path should be UTF-8"));
        if count == 2 {
            args.push(second.to_str().expect("temporary path should be UTF-8"));
        }

        let output = relay(tmp.path(), &args)?;

        assert!(output.status.success(), "{}", utf8(&output.stderr));
        assert_eq!(
            utf8(&output.stdout),
            format!(
                "plan: commands would_update=0; skills would_update={}; agents would_update=0; rules would_update=0\n",
                count * 2
            )
        );
        assert_eq!(
            utf8(&output.stderr),
            "hint: relay has not been set up yet; run `relay init` first\n\n"
        );
        assert!(!tmp.path().join(".agents/skills").exists());
        assert!(!tmp.path().join(".config/relay/runtime/relay.lock").exists());
    }
    Ok(())
}

#[test]
fn scoped_options_after_operands_plan_without_writes_and_quietly_apply_all_paths() -> io::Result<()>
{
    let tmp = TempDir::new()?;
    initialize(tmp.path())?;
    let source = tmp.path().join("source");
    let first = write_skill(&source, "first", "First")?;
    let second = write_skill(&source, "second", "Second")?;
    let first = first.to_str().expect("temporary path should be UTF-8");
    let second = second.to_str().expect("temporary path should be UTF-8");

    let plan = relay(tmp.path(), &["sync", "skill", first, "--plan"])?;

    assert!(plan.status.success(), "{}", utf8(&plan.stderr));
    assert!(utf8(&plan.stdout).contains("skills would_update=2"));
    for root in [
        tmp.path().join("relay-data/skills"),
        tmp.path().join("claude/skills"),
    ] {
        assert!(!root.join("first").exists());
        assert!(!root.join("second").exists());
    }
    assert!(!tmp
        .path()
        .join(".config/relay/runtime/skills-state.toml")
        .exists());
    assert!(!tmp.path().join("relay-data/history/events").exists());

    let apply = relay(tmp.path(), &["sync", "skill", first, "--quiet", second])?;

    assert!(apply.status.success(), "{}", utf8(&apply.stderr));
    assert_eq!(apply.stdout, b"");
    assert_eq!(apply.stderr, b"");
    for root in [
        tmp.path().join("relay-data/skills"),
        tmp.path().join("claude/skills"),
    ] {
        for name in ["first", "second"] {
            assert!(root.join(name).join("SKILL.md").exists());
        }
    }
    Ok(())
}

#[test]
fn scoped_skill_file_operand_plans_then_applies_only_that_package() -> io::Result<()> {
    let tmp = TempDir::new()?;
    initialize(tmp.path())?;
    let source = tmp.path().join("source");
    let selected = write_skill(&source, "selected", "Selected")?;
    write_skill(&source, "adjacent", "Adjacent")?;
    let operand = selected.join("SKILL.md");
    let operand = operand.to_str().expect("temporary path should be UTF-8");

    let plan = relay(tmp.path(), &["sync", "skill", "--plan", operand])?;

    assert!(plan.status.success(), "{}", utf8(&plan.stderr));
    assert!(utf8(&plan.stdout).contains("skills would_update=2"));
    for root in [
        tmp.path().join("relay-data/skills"),
        tmp.path().join("claude/skills"),
    ] {
        assert!(!root.join("selected").exists());
        assert!(!root.join("adjacent").exists());
    }
    assert!(!tmp
        .path()
        .join(".config/relay/runtime/skills-state.toml")
        .exists());

    let apply = relay(tmp.path(), &["sync", "skill", "--quiet", operand])?;

    assert!(apply.status.success(), "{}", utf8(&apply.stderr));
    assert_eq!(apply.stdout, b"");
    assert_eq!(apply.stderr, b"");
    for root in [
        tmp.path().join("relay-data/skills"),
        tmp.path().join("claude/skills"),
    ] {
        assert!(root.join("selected/SKILL.md").exists());
        assert!(!root.join("adjacent").exists());
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn scoped_collection_operand_recurses_but_excludes_metadata_and_dependencies() -> io::Result<()> {
    let tmp = TempDir::new()?;
    initialize(tmp.path())?;
    let collection = tmp.path().join("collection");
    write_skill(&collection, "top", "Top")?;
    write_skill(&collection.join("one/two"), "middle", "Middle")?;
    write_skill(&collection.join("one/two/three/four"), "deep", "Deep")?;
    write_skill(&collection.join(".local/skills"), "hidden", "Hidden")?;
    write_skill(&collection.join(".git/nested"), "repository", "Repository")?;
    write_skill(
        &collection.join("one/node_modules/package"),
        "dependency",
        "Dependency",
    )?;
    let linked_source = write_skill(&tmp.path().join("linked-source"), "linked", "Linked")?;
    std::os::unix::fs::symlink(&linked_source, collection.join("linked"))?;
    let operand = collection.to_str().expect("temporary path should be UTF-8");

    let plan = relay(tmp.path(), &["sync", "skill", "--plan", operand])?;

    assert!(plan.status.success(), "{}", utf8(&plan.stderr));
    assert!(utf8(&plan.stdout).contains("skills would_update=8"));
    for root in [
        tmp.path().join("relay-data/skills"),
        tmp.path().join("claude/skills"),
    ] {
        assert!(!root.exists());
    }
    assert!(!tmp
        .path()
        .join(".config/relay/runtime/skills-state.toml")
        .exists());

    let apply = relay(tmp.path(), &["sync", "skill", "--quiet", operand])?;

    assert!(apply.status.success(), "{}", utf8(&apply.stderr));
    assert_eq!(apply.stdout, b"");
    assert_eq!(apply.stderr, b"");
    for root in [
        tmp.path().join("relay-data/skills"),
        tmp.path().join("claude/skills"),
    ] {
        for name in ["top", "middle", "deep", "hidden"] {
            assert!(root.join(name).join("SKILL.md").exists());
        }
        for name in ["repository", "dependency", "linked"] {
            assert!(!root.join(name).exists());
        }
    }
    Ok(())
}

#[test]
fn scoped_verbose_plan_replays_actions_without_writing() -> io::Result<()> {
    let tmp = TempDir::new()?;
    let selected = write_skill(&tmp.path().join("source"), "verbose", "Verbose")?;

    let output = relay(
        tmp.path(),
        &[
            "sync",
            "skill",
            "--plan",
            "--verbose",
            selected.to_str().expect("temporary path should be UTF-8"),
        ],
    )?;

    assert!(output.status.success(), "{}", utf8(&output.stderr));
    let stdout = utf8(&output.stdout);
    assert!(stdout.contains(&format!(
        "skills: would update {}",
        tmp.path().join(".agents/skills/verbose").display()
    )));
    assert!(stdout.contains("skills: would import 'verbose' from selected input"));
    assert!(stdout.contains(&format!(
        "skills: would update {}",
        tmp.path().join(".claude/skills/verbose").display()
    )));
    assert!(stdout.ends_with(
        "plan: commands would_update=0; skills would_update=2; agents would_update=0; rules would_update=0\n"
    ));
    assert_eq!(
        utf8(&output.stderr),
        "hint: relay has not been set up yet; run `relay init` first\n\n"
    );
    assert!(!tmp.path().join(".agents/skills").exists());
    assert!(!tmp.path().join(".claude/skills").exists());
    assert!(!tmp
        .path()
        .join(".config/relay/runtime/skills-state.toml")
        .exists());
    assert!(!tmp.path().join(".config/relay/runtime/relay.lock").exists());
    Ok(())
}

#[test]
fn scoped_plan_rejects_invalid_path_and_quiet_suppresses_success_output() -> io::Result<()> {
    let tmp = TempDir::new()?;
    let missing = tmp.path().join("missing");
    let invalid = relay(
        tmp.path(),
        &[
            "sync",
            "skill",
            "--plan",
            missing.to_str().expect("temporary path should be UTF-8"),
        ],
    )?;
    assert!(!invalid.status.success());
    assert_eq!(invalid.stdout, b"");
    assert!(utf8(&invalid.stderr).contains("invalid skill path"));

    let selected = write_skill(&tmp.path().join("source"), "quiet", "Quiet")?;
    let quiet = relay(
        tmp.path(),
        &[
            "sync",
            "skill",
            "--plan",
            "--quiet",
            selected.to_str().expect("temporary path should be UTF-8"),
        ],
    )?;
    assert!(quiet.status.success(), "{}", utf8(&quiet.stderr));
    assert_eq!(quiet.stdout, b"");
    assert_eq!(quiet.stderr, b"");
    Ok(())
}

#[test]
fn scoped_conflicting_options_exit_two_without_filesystem_writes() -> io::Result<()> {
    for conflicting in [["--plan", "--apply"], ["--verbose", "--quiet"]] {
        let tmp = TempDir::new()?;
        initialize(tmp.path())?;
        let selected = write_skill(&tmp.path().join("source"), "selected", "Selected")?;
        let selected = selected.to_str().expect("temporary path should be UTF-8");
        let args = vec!["sync", "skill", conflicting[0], conflicting[1], selected];

        let output = relay(tmp.path(), &args)?;

        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert_eq!(output.stdout, b"", "{args:?}");
        assert!(
            utf8(&output.stderr).contains("cannot be used with"),
            "{args:?}"
        );
        assert!(!tmp.path().join("relay-data/skills/selected").exists());
        assert!(!tmp.path().join("claude/skills/selected").exists());
        assert!(!tmp
            .path()
            .join(".config/relay/runtime/skills-state.toml")
            .exists());
        assert!(!tmp.path().join("relay-data/history/events").exists());
        assert!(!tmp.path().join(".config/relay/runtime/relay.lock").exists());
    }
    Ok(())
}

#[test]
fn scoped_conflict_is_nonzero_and_does_not_write() -> io::Result<()> {
    let tmp = TempDir::new()?;
    let canonical = write_skill(&tmp.path().join(".agents/skills"), "conflict", "Canonical")?;
    let selected = write_skill(&tmp.path().join("source"), "conflict", "Selected")?;

    let output = relay(
        tmp.path(),
        &[
            "sync",
            "skill",
            "--plan",
            selected.to_str().expect("temporary path should be UTF-8"),
        ],
    )?;

    assert!(!output.status.success());
    assert!(utf8(&output.stdout).contains("conflicts: 1 detected"));
    assert!(utf8(&output.stderr).contains("scoped sync aborted due to canonical conflicts (1)"));
    assert!(fs::read_to_string(canonical.join("SKILL.md"))?.contains("Canonical"));
    assert!(!tmp.path().join(".claude/skills/conflict").exists());
    assert!(!tmp
        .path()
        .join(".config/relay/runtime/skills-state.toml")
        .exists());
    Ok(())
}

#[test]
fn scoped_apply_requires_initialization() -> io::Result<()> {
    let tmp = TempDir::new()?;
    let selected = write_skill(&tmp.path().join("source"), "selected", "Selected")?;

    let output = relay(
        tmp.path(),
        &[
            "sync",
            "skill",
            selected.to_str().expect("temporary path should be UTF-8"),
        ],
    )?;

    assert!(!output.status.success());
    assert_eq!(output.stdout, b"");
    assert!(utf8(&output.stderr).contains("relay is not initialized"));
    assert!(!tmp.path().join(".agents/skills").exists());
    assert!(!tmp.path().join(".claude/skills").exists());
    Ok(())
}

#[test]
fn scoped_apply_conflict_is_nonzero_and_write_free() -> io::Result<()> {
    let tmp = TempDir::new()?;
    initialize(tmp.path())?;
    let canonical = write_skill(
        &tmp.path().join("relay-data/skills"),
        "conflict",
        "Canonical",
    )?;
    let selected = write_skill(&tmp.path().join("source"), "conflict", "Selected")?;

    let output = relay(
        tmp.path(),
        &[
            "sync",
            "skill",
            selected.to_str().expect("temporary path should be UTF-8"),
        ],
    )?;

    assert!(!output.status.success());
    assert!(utf8(&output.stdout).contains("conflicts: 1 detected"));
    assert!(utf8(&output.stderr).contains("scoped sync aborted due to canonical conflicts (1)"));
    assert!(fs::read_to_string(canonical.join("SKILL.md"))?.contains("Canonical"));
    assert!(!tmp.path().join("claude/skills/conflict").exists());
    assert!(!tmp
        .path()
        .join(".config/relay/runtime/skills-state.toml")
        .exists());
    assert!(!tmp.path().join("relay-data/history/events").exists());
    Ok(())
}

#[test]
fn scoped_existing_destination_name_collisions_are_nonzero_and_write_free() -> io::Result<()> {
    for destination in ["canonical", "adapter"] {
        let tmp = TempDir::new()?;
        initialize(tmp.path())?;
        let root = if destination == "canonical" {
            tmp.path().join("relay-data/skills")
        } else {
            tmp.path().join("claude/skills")
        };
        let existing = root.join("Portable");
        fs::create_dir_all(existing.join("unselected-content"))?;
        fs::write(existing.join("unselected-content/sentinel"), "untouched")?;
        let selected = write_skill(&tmp.path().join("source"), "portable", "Selected")?;

        let output = relay(
            tmp.path(),
            &[
                "sync",
                "skill",
                selected.to_str().expect("temporary path should be UTF-8"),
            ],
        )?;

        assert!(!output.status.success(), "{destination}");
        assert_eq!(output.stdout, b"", "{destination}");
        assert!(
            utf8(&output.stderr).contains("refusing non-portable destination names"),
            "{destination}: {}",
            utf8(&output.stderr)
        );
        assert_eq!(
            fs::read_to_string(existing.join("unselected-content/sentinel"))?,
            "untouched"
        );
        assert_eq!(fs::read_dir(&root)?.count(), 1, "{destination}");
        let untouched_other_root = if destination == "canonical" {
            tmp.path().join("claude/skills/portable")
        } else {
            tmp.path().join("relay-data/skills/portable")
        };
        assert!(!untouched_other_root.exists(), "{destination}");
        assert!(!tmp
            .path()
            .join(".config/relay/runtime/skills-state.toml")
            .exists());
        assert!(!tmp.path().join("relay-data/history/events").exists());
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn scoped_unsafe_canonical_packages_abort_before_any_sync_write() -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    for selection in ["external", "canonical"] {
        for unsupported in ["symlink", "fifo"] {
            let tmp = TempDir::new()?;
            initialize(tmp.path())?;
            let source = tmp.path().join("source");
            let first = write_skill(&source, "a-first", "First")?;
            let canonical =
                write_skill(&tmp.path().join("relay-data/skills"), "z-unsafe", "Unsafe")?;
            let external = write_skill(&source, "z-unsafe", "Unsafe")?;
            let adapter = write_skill(
                &tmp.path().join("claude/skills"),
                "z-unsafe",
                "Adapter untouched",
            )?;
            let adapter_before = fs::read(adapter.join("SKILL.md"))?;
            let outside = tmp.path().join("outside.txt");
            fs::write(&outside, "outside untouched")?;
            let internal = canonical.join("references/unsupported");
            fs::create_dir_all(internal.parent().expect("internal entry has parent"))?;
            if unsupported == "symlink" {
                std::os::unix::fs::symlink(&outside, &internal)?;
            } else {
                let fifo =
                    CString::new(internal.as_os_str().as_bytes()).map_err(io::Error::other)?;
                // SAFETY: `fifo` is a live NUL-terminated path and `mkfifo` does not retain it.
                if unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) } != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            let unsafe_operand = if selection == "canonical" {
                &canonical
            } else {
                &external
            };

            let output = relay(
                tmp.path(),
                &[
                    "sync",
                    "skill",
                    first.to_str().expect("temporary path should be UTF-8"),
                    unsafe_operand
                        .to_str()
                        .expect("temporary path should be UTF-8"),
                ],
            )?;

            assert!(!output.status.success(), "{selection} {unsupported}");
            assert_eq!(output.stdout, b"", "{selection} {unsupported}");
            let expected = if unsupported == "symlink" {
                "symlinks are not supported inside selected skill packages"
            } else {
                "unsupported non-regular entry inside selected skill package"
            };
            assert!(
                utf8(&output.stderr).contains(expected),
                "{selection} {unsupported}: {}",
                utf8(&output.stderr)
            );
            assert!(!tmp.path().join("relay-data/skills/a-first").exists());
            assert!(!tmp.path().join("claude/skills/a-first").exists());
            assert_eq!(fs::read(adapter.join("SKILL.md"))?, adapter_before);
            assert_eq!(fs::read_to_string(&outside)?, "outside untouched");
            assert!(
                fs::symlink_metadata(&internal)?.file_type().is_symlink() || unsupported == "fifo"
            );
            assert!(!tmp
                .path()
                .join(".config/relay/runtime/skills-state.toml")
                .exists());
            assert!(!tmp.path().join("relay-data/history/events").exists());
        }
    }
    Ok(())
}

#[test]
fn scoped_invalid_frontmatter_real_binary_preflight_is_nonzero_and_write_free() -> io::Result<()> {
    let tmp = TempDir::new()?;
    initialize(tmp.path())?;
    let source = tmp.path().join("source");
    let valid = write_skill(&source, "a-valid", "Valid")?;
    let invalid = source.join("z-invalid");
    fs::create_dir_all(&invalid)?;
    fs::write(
        invalid.join("SKILL.md"),
        "---\nname: z-invalid\n---\nInvalid",
    )?;

    let output = relay(
        tmp.path(),
        &[
            "sync",
            "skill",
            valid.to_str().expect("temporary path should be UTF-8"),
            invalid.to_str().expect("temporary path should be UTF-8"),
        ],
    )?;

    assert!(!output.status.success());
    assert_eq!(output.stdout, b"");
    assert!(utf8(&output.stderr).contains("invalid skill frontmatter"));
    for root in [
        tmp.path().join("relay-data/skills"),
        tmp.path().join("claude/skills"),
    ] {
        assert!(!root.join("a-valid").exists());
        assert!(!root.join("z-invalid").exists());
    }
    assert!(!tmp
        .path()
        .join(".config/relay/runtime/skills-state.toml")
        .exists());
    assert!(!tmp.path().join("relay-data/history/events").exists());
    Ok(())
}

#[test]
fn scoped_quiet_fail_on_conflict_real_binary_exit_contract() -> io::Result<()> {
    let success_home = TempDir::new()?;
    initialize(success_home.path())?;
    let selected = write_skill(&success_home.path().join("source"), "selected", "Selected")?;
    let success = relay(
        success_home.path(),
        &[
            "sync",
            "skill",
            "--quiet",
            "--fail-on-conflict",
            selected.to_str().expect("temporary path should be UTF-8"),
        ],
    )?;
    assert!(success.status.success(), "{}", utf8(&success.stderr));
    assert_eq!(success.stdout, b"");
    assert_eq!(success.stderr, b"");
    assert!(success_home
        .path()
        .join("relay-data/skills/selected/SKILL.md")
        .exists());

    let conflict_home = TempDir::new()?;
    initialize(conflict_home.path())?;
    write_skill(
        &conflict_home.path().join("relay-data/skills"),
        "conflict",
        "Canonical",
    )?;
    let conflicting = write_skill(&conflict_home.path().join("source"), "conflict", "Selected")?;
    let conflict = relay(
        conflict_home.path(),
        &[
            "sync",
            "skill",
            "--quiet",
            "--fail-on-conflict",
            conflicting
                .to_str()
                .expect("temporary path should be UTF-8"),
        ],
    )?;
    assert!(!conflict.status.success());
    assert_eq!(conflict.stdout, b"");
    assert!(utf8(&conflict.stderr).contains("scoped sync aborted due to canonical conflicts (1)"));
    assert!(!conflict_home.path().join("claude/skills/conflict").exists());
    Ok(())
}

#[test]
fn scoped_apply_initializes_only_selected_skill_reconciliation() -> io::Result<()> {
    let tmp = TempDir::new()?;
    initialize(tmp.path())?;
    fs::create_dir_all(tmp.path().join("claude/commands"))?;
    fs::write(tmp.path().join("claude/commands/pending.md"), "pending")?;
    let selected = write_skill(&tmp.path().join("source"), "selected", "Selected")?;

    let output = relay(
        tmp.path(),
        &[
            "sync",
            "skill",
            "--quiet",
            selected.to_str().expect("temporary path should be UTF-8"),
        ],
    )?;

    assert!(output.status.success(), "{}", utf8(&output.stderr));
    assert_eq!(output.stdout, b"");
    assert_eq!(output.stderr, b"");
    assert!(tmp
        .path()
        .join("relay-data/skills/selected/SKILL.md")
        .exists());
    assert!(tmp.path().join("claude/skills/selected/SKILL.md").exists());
    assert!(!tmp.path().join("relay-data/commands/pending.md").exists());
    assert!(tmp.path().join(".config/relay/runtime/relay.lock").exists());
    Ok(())
}

#[test]
fn scoped_apply_multiple_operands_writes_only_selected_skills() -> io::Result<()> {
    let tmp = TempDir::new()?;
    initialize(tmp.path())?;
    let source = tmp.path().join("source");
    let first = write_skill(&source, "first", "First")?;
    let second = write_skill(&source, "second", "Second")?;
    write_skill(&source, "unselected", "Unselected")?;

    let output = relay(
        tmp.path(),
        &[
            "sync",
            "skill",
            "--quiet",
            first.to_str().expect("temporary path should be UTF-8"),
            second.to_str().expect("temporary path should be UTF-8"),
        ],
    )?;

    assert!(output.status.success(), "{}", utf8(&output.stderr));
    assert_eq!(output.stdout, b"");
    assert_eq!(output.stderr, b"");
    for name in ["first", "second"] {
        assert!(tmp
            .path()
            .join("relay-data/skills")
            .join(name)
            .join("SKILL.md")
            .exists());
        assert!(tmp
            .path()
            .join("claude/skills")
            .join(name)
            .join("SKILL.md")
            .exists());
    }
    assert!(!tmp.path().join("relay-data/skills/unselected").exists());
    assert!(!tmp.path().join("claude/skills/unselected").exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn scoped_confirm_versions_dispatches_only_selected_skills_noninteractively() -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new()?;
    initialize_with_verified_claude(tmp.path(), "1.2.3")?;
    let bin = tmp.path().join("bin");
    fs::create_dir_all(&bin)?;
    let fake_claude = bin.join("claude");
    fs::write(&fake_claude, "#!/bin/sh\nprintf 'claude 1.2.3\\n'\n")?;
    fs::set_permissions(&fake_claude, fs::Permissions::from_mode(0o755))?;
    fs::create_dir_all(tmp.path().join("claude/skills"))?;
    fs::create_dir_all(tmp.path().join("claude/commands"))?;
    fs::write(tmp.path().join("claude/commands/pending.md"), "pending")?;
    let selected = write_skill(&tmp.path().join("source"), "selected", "Selected")?;
    let inherited_path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(&inherited_path));
    let joined_path = std::env::join_paths(paths).map_err(io::Error::other)?;

    let output = relay_command(tmp.path())
        .env("PATH", joined_path)
        .args([
            "sync",
            "skill",
            "--confirm-versions",
            "--quiet",
            selected.to_str().expect("temporary path should be UTF-8"),
        ])
        .output()?;

    assert!(output.status.success(), "{}", utf8(&output.stderr));
    assert!(utf8(&output.stdout).contains("Detected claude version:"));
    assert!(tmp
        .path()
        .join("relay-data/skills/selected/SKILL.md")
        .exists());
    assert!(tmp.path().join("claude/skills/selected/SKILL.md").exists());
    assert!(!tmp.path().join("relay-data/commands/pending.md").exists());
    Ok(())
}

#[test]
fn plain_sync_still_reconciles_every_category_through_real_dispatch() -> io::Result<()> {
    let tmp = TempDir::new()?;
    initialize_all_tools(tmp.path())?;
    fs::create_dir_all(tmp.path().join("claude/commands"))?;
    fs::write(
        tmp.path().join("claude/commands/pending.md"),
        "Pending command",
    )?;
    write_skill(
        &tmp.path().join("relay-data/skills"),
        "canonical",
        "Canonical skill",
    )?;
    fs::create_dir_all(tmp.path().join("codex"))?;
    fs::write(tmp.path().join("codex/AGENTS.md"), "Pending agent")?;
    fs::create_dir_all(tmp.path().join("codex/rules"))?;
    fs::write(
        tmp.path().join("codex/rules/default.rules"),
        "rule(allow=true)",
    )?;

    let output = relay(tmp.path(), &["sync", "--quiet"])?;

    assert!(output.status.success(), "{}", utf8(&output.stderr));
    assert_eq!(output.stdout, b"");
    assert_eq!(output.stderr, b"");
    assert!(tmp.path().join("relay-data/commands/pending.md").exists());
    assert!(tmp.path().join("claude/skills/canonical/SKILL.md").exists());
    assert!(tmp
        .path()
        .join("relay-data/agents/codex/AGENTS.md")
        .exists());
    assert!(tmp
        .path()
        .join("relay-data/rules/codex/default.rules")
        .exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn scoped_apply_waits_for_process_lock_before_writing() -> io::Result<()> {
    let tmp = TempDir::new()?;
    initialize(tmp.path())?;
    let selected = write_skill(&tmp.path().join("source"), "blocked", "Blocked")?;
    let lock_path = tmp.path().join(".config/relay/runtime/relay.lock");
    fs::create_dir_all(lock_path.parent().expect("lock has parent"))?;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let debug_log = tmp.path().join("lock-debug.log");
    let mut child = relay_command(tmp.path())
        .args([
            "--debug",
            "--debug-log-file",
            debug_log.to_str().expect("temporary path should be UTF-8"),
            "sync",
            "skill",
            "--quiet",
            selected.to_str().expect("temporary path should be UTF-8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if fs::read_to_string(&debug_log)
            .unwrap_or_default()
            .contains("process lock waiting operation=sync:scoped")
        {
            break;
        }
        if let Some(status) = child.try_wait()? {
            return Err(io::Error::other(format!(
                "relay exited before waiting for the process lock: {status}"
            )));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "relay did not log process lock contention",
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }

    assert!(!tmp.path().join("relay-data/skills/blocked").exists());
    assert!(!tmp.path().join("claude/skills/blocked").exists());
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) } != 0 {
        return Err(io::Error::last_os_error());
    }

    let output = child.wait_with_output()?;
    assert!(output.status.success(), "{}", utf8(&output.stderr));
    assert!(tmp
        .path()
        .join("relay-data/skills/blocked/SKILL.md")
        .exists());
    assert!(tmp.path().join("claude/skills/blocked/SKILL.md").exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn scoped_apply_lock_open_failure_exits_without_writing() -> io::Result<()> {
    let tmp = TempDir::new()?;
    initialize(tmp.path())?;
    let selected = write_skill(&tmp.path().join("source"), "blocked", "Blocked")?;
    let lock_path = tmp.path().join(".config/relay/runtime/relay.lock");
    fs::create_dir_all(&lock_path)?;
    let unrelated = tmp.path().join("relay-data/unrelated.txt");
    fs::create_dir_all(unrelated.parent().expect("unrelated path has parent"))?;
    fs::write(&unrelated, "unchanged")?;

    let output = relay(
        tmp.path(),
        &[
            "sync",
            "skill",
            selected.to_str().expect("temporary path should be UTF-8"),
        ],
    )?;

    assert_eq!(output.status.code(), Some(1));
    assert!(!output.stderr.is_empty());
    assert!(!utf8(&output.stderr).contains("another relay process"));
    assert!(!tmp.path().join("relay-data/skills/blocked").exists());
    assert!(!tmp.path().join("claude/skills/blocked").exists());
    assert!(!tmp
        .path()
        .join(".config/relay/runtime/skills-state.toml")
        .exists());
    assert!(!tmp.path().join("relay-data/history").exists());
    assert_eq!(fs::read_to_string(unrelated)?, "unchanged");
    assert!(lock_path.is_dir());
    Ok(())
}
