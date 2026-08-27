#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

#[derive(Debug, PartialEq, Eq)]
enum Entry {
    Dir(u32),
    File(u32, Vec<u8>),
    Link(PathBuf),
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0
    }

    fn usize(&mut self, upper: usize) -> usize {
        self.next() as usize % upper
    }

    fn text(&mut self, len: usize) -> String {
        (0..len)
            .map(|_| (b'a' + self.usize(26) as u8) as char)
            .collect()
    }
}

fn relay(home: &Path, args: &[&str]) -> io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_relay"))
        .args(args)
        .env("HOME", home)
        .env("RELAY_HOME", home)
        .env_remove("RELAY_CONFIG_DIR")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("CODEX_HOME")
        .env_remove("CLAUDE_HOME")
        .env_remove("CURSOR_HOME")
        .env_remove("OPENCODE_HOME")
        .output()
}

fn text(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).expect("process output should be UTF-8")
}

fn initialize(home: &Path, custom_store: bool) -> io::Result<()> {
    let relay = home.join(".config/relay");
    let store = if custom_store {
        home.join("custom/skills")
    } else {
        relay.join("skills")
    };
    fs::create_dir_all(&relay)?;
    fs::write(
        relay.join("config.toml"),
        format!("central_skills_dir = \"{}\"\n", store.display()),
    )
}

fn skill(root: &Path, name: &str, body: &str) -> io::Result<()> {
    let root = root.join(name);
    fs::create_dir_all(root.join("assets/deep"))?;
    fs::write(
        root.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: test\n---\n{body}\n"),
    )?;
    fs::write(root.join("assets/deep/data"), body)
}

fn assert_link(path: &Path, target: &Path) -> io::Result<()> {
    assert!(fs::symlink_metadata(path)?.file_type().is_symlink());
    assert_eq!(fs::read_link(path)?, target);
    Ok(())
}

fn snapshot(root: &Path) -> io::Result<BTreeMap<PathBuf, Entry>> {
    fn walk(root: &Path, path: &Path, out: &mut BTreeMap<PathBuf, Entry>) -> io::Result<()> {
        let mut paths = fs::read_dir(path)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()?;
        paths.sort();
        for path in paths {
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            let metadata = fs::symlink_metadata(&path)?;
            let mode = metadata.permissions().mode();
            if metadata.file_type().is_symlink() {
                out.insert(relative, Entry::Link(fs::read_link(path)?));
            } else if metadata.is_dir() {
                out.insert(relative, Entry::Dir(mode));
                walk(root, &path, out)?;
            } else {
                out.insert(relative, Entry::File(mode, fs::read(path)?));
            }
        }
        Ok(())
    }

    let mut entries = BTreeMap::new();
    walk(root, root, &mut entries)?;
    Ok(entries)
}

#[test]
fn migrate_modes_and_plan_are_write_free() -> io::Result<()> {
    let tmp = TempDir::new()?;
    let home = tmp.path();
    let output = relay(home, &["migrate", "--plan", "--apply"])?;
    assert_eq!(output.status.code(), Some(2));
    assert!(!home.join(".config").exists());

    let output = relay(home, &["migrate", "--apply"])?;
    assert!(!output.status.success());
    assert!(!home.join(".config").exists());

    initialize(home, false)?;
    fs::create_dir(home.join(".dotfiles"))?;
    skill(&home.join(".agents/skills"), "current", "Current")?;
    skill(&home.join(".config/relay/skills"), "legacy", "Legacy")?;
    fs::create_dir_all(home.join(".claude/commands"))?;
    fs::write(home.join(".claude/commands/keep"), "Keep")?;
    let before = snapshot(home)?;
    let output = relay(home, &["migrate", "--dotfiles"])?;
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(snapshot(home)?, before);
    let stdout = text(&output.stdout);
    for expected in [
        "would import 'legacy'",
        "would move",
        ".dotfiles/agents",
        ".dotfiles/claude",
    ] {
        assert!(stdout.contains(expected), "missing {expected}: {stdout}");
    }
    Ok(())
}

#[test]
fn migrate_moves_complete_roots_and_is_idempotent() -> io::Result<()> {
    let tmp = TempDir::new()?;
    let home = tmp.path();
    initialize(home, false)?;
    fs::create_dir(home.join(".dotfiles"))?;
    skill(&home.join(".agents/skills"), "current", "Current")?;
    skill(&home.join(".config/relay/skills"), "legacy", "Legacy")?;
    fs::create_dir_all(home.join(".claude/commands/nested"))?;
    fs::write(home.join(".claude/settings.json"), "{\"keep\":true}")?;
    fs::write(home.join(".claude/commands/nested/keep"), "Keep")?;

    let output = relay(home, &["migrate", "--apply", "--dotfiles"])?;
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_link(&home.join(".agents"), &home.join(".dotfiles/agents"))?;
    assert_link(&home.join(".claude"), &home.join(".dotfiles/claude"))?;
    for name in ["current", "legacy"] {
        assert!(home
            .join(".agents/skills")
            .join(name)
            .join("SKILL.md")
            .exists());
        assert!(home
            .join(".claude/skills")
            .join(name)
            .join("SKILL.md")
            .exists());
    }
    assert_eq!(
        fs::read_to_string(home.join(".claude/settings.json"))?,
        "{\"keep\":true}"
    );
    assert_eq!(
        fs::read_to_string(home.join(".claude/commands/nested/keep"))?,
        "Keep"
    );

    for args in [
        &["migrate", "--dotfiles"][..],
        &["migrate", "--apply", "--dotfiles"],
    ] {
        let output = relay(home, args)?;
        assert!(output.status.success(), "{}", text(&output.stderr));
        assert!(text(&output.stdout).contains("no changes"));
    }
    Ok(())
}

#[test]
fn migrate_without_dotfiles_only_syncs_skills() -> io::Result<()> {
    let tmp = TempDir::new()?;
    let home = tmp.path();
    initialize(home, false)?;
    skill(&home.join(".config/relay/skills"), "legacy", "Legacy")?;
    let output = relay(home, &["migrate", "--apply"])?;
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert!(home.join(".agents/skills/legacy/SKILL.md").exists());
    assert!(home.join(".claude/skills/legacy/SKILL.md").exists());
    assert!(fs::symlink_metadata(home.join(".agents"))?.is_dir());
    assert!(fs::symlink_metadata(home.join(".claude"))?.is_dir());
    Ok(())
}

fn prepare_root(home: &Path, name: &str, mode: usize) -> io::Result<PathBuf> {
    let source = home.join(format!(".{name}"));
    let target = home.join(".dotfiles").join(name);
    let root = if mode == 0 { &source } else { &target };
    fs::create_dir_all(root)?;
    fs::write(root.join("marker"), name)?;
    if mode == 2 {
        symlink(&target, &source)?;
    }
    Ok(if mode == 1 { target } else { source })
}

#[test]
fn randomized_fake_homes_migrate_without_data_loss() -> io::Result<()> {
    let mut rng = Rng(0x6d69_6772_6174_6521);
    for case in 0..64 {
        let tmp = TempDir::new()?;
        let home = tmp.path();
        initialize(home, false)?;
        fs::create_dir(home.join(".dotfiles"))?;
        let agents = prepare_root(home, "agents", case % 3)?;
        let claude = prepare_root(home, "claude", case / 3 % 3)?;
        let linked = home.join("linked");
        fs::write(&linked, format!("case-{case}"))?;
        symlink(&linked, claude.join("preserved-link"))?;

        let mut names = Vec::new();
        for (prefix, root, count) in [
            ("current", agents.join("skills"), 1 + rng.usize(5)),
            (
                "legacy",
                home.join(".config/relay/skills"),
                1 + rng.usize(5),
            ),
        ] {
            for index in 0..count {
                let name = format!("{prefix}-{case}-{index}");
                let len = 1 + rng.usize(1024);
                let body = rng.text(len);
                skill(&root, &name, &body)?;
                names.push((name, body));
            }
        }

        let before = snapshot(home)?;
        assert!(relay(home, &["migrate", "--dotfiles"])?.status.success());
        assert_eq!(snapshot(home)?, before, "plan wrote in case {case}");
        let output = relay(home, &["migrate", "--apply", "--dotfiles"])?;
        assert!(
            output.status.success(),
            "case {case}: {}",
            text(&output.stderr)
        );
        assert_link(&home.join(".agents"), &home.join(".dotfiles/agents"))?;
        assert_link(&home.join(".claude"), &home.join(".dotfiles/claude"))?;
        assert_eq!(fs::read_link(home.join(".claude/preserved-link"))?, linked);
        assert_eq!(fs::read_to_string(home.join(".agents/marker"))?, "agents");
        assert_eq!(fs::read_to_string(home.join(".claude/marker"))?, "claude");
        for (name, body) in names {
            for root in [".agents", ".claude"] {
                assert_eq!(
                    fs::read_to_string(
                        home.join(root)
                            .join("skills")
                            .join(&name)
                            .join("assets/deep/data")
                    )?,
                    body
                );
            }
        }
    }
    Ok(())
}

#[test]
fn randomized_refusals_do_not_change_fake_homes() -> io::Result<()> {
    let mut rng = Rng(0x7265_6675_7361_6c21);
    for case in 0..64 {
        let tmp = TempDir::new()?;
        let home = tmp.path();
        let failure = case % 6;
        initialize(home, failure == 5)?;
        fs::create_dir(home.join(".dotfiles"))?;
        match failure {
            0 => {
                fs::create_dir_all(home.join(".agents/skills"))?;
                fs::create_dir_all(home.join(".dotfiles/agents"))?;
            }
            1 => {
                fs::create_dir(home.join("wrong"))?;
                symlink(home.join("wrong"), home.join(".agents"))?;
            }
            2 => {
                fs::remove_dir(home.join(".dotfiles"))?;
                fs::create_dir(home.join("wrong"))?;
                symlink(home.join("wrong"), home.join(".dotfiles"))?;
            }
            3 => {
                skill(&home.join(".config/relay/skills"), "conflict", "relay")?;
                skill(&home.join(".codex/skills"), "conflict", "codex")?;
            }
            4 => fs::write(home.join(".agents"), "file")?,
            5 => fs::create_dir_all(home.join("custom/skills"))?,
            _ => unreachable!(),
        }
        fs::create_dir_all(home.join(".claude/nested"))?;
        let len = 1 + rng.usize(256);
        fs::write(home.join(".claude/nested/keep"), rng.text(len))?;
        let before = snapshot(home)?;
        let output = relay(home, &["migrate", "--apply", "--dotfiles"])?;
        assert!(!output.status.success(), "case {case} succeeded");
        assert_eq!(snapshot(home)?, before, "refusal wrote in case {case}");
    }
    Ok(())
}
