#![cfg_attr(any(test, coverage), allow(dead_code))]

use crate::config::{resolve_home_dir, Config};
use crate::daemon::{self, ServiceState};
use crate::process_lock::ProcessLock;
use crate::report;
use crate::sync::{self, ExecutionMode, LogMode, SyncOutcome};
use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::{symlink, MetadataExt};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinkAction {
    None,
    Move,
    Link,
    Create,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LinkPlan {
    source: PathBuf,
    target: PathBuf,
    action: LinkAction,
}

pub(crate) fn run(cfg: &Config, apply: bool, dotfiles: bool) -> io::Result<()> {
    let skill_plan = plan_skills(cfg, LogMode::Actions)?;
    reject_conflicts(&skill_plan)?;
    let links = dotfiles.then(|| plan_dotfiles(cfg)).transpose()?;
    if let Some(links) = links.as_deref() {
        print_link_plan(links);
        println!("migrate: review ~/.dotfiles/claude for machine-local data before you commit it");
    }

    if !apply {
        report::print_plan_summary(&skill_plan.report);
        if !dotfiles {
            print_dotfiles_hint()?;
        }
        return Ok(());
    }

    ensure_watcher_stopped(cfg)?;
    let _lock = ProcessLock::acquire("migrate")?;
    reject_conflicts(&plan_skills(cfg, LogMode::Quiet)?)?;
    let apply_plan = dotfiles.then(|| plan_dotfiles(cfg)).transpose()?;
    let outcome = apply_migration(cfg, apply_plan.as_deref())?;
    report::print_sync_summary(&outcome.report);
    if let Some(event_id) = outcome.history_event_id.as_deref() {
        println!("history: recorded event {event_id}");
    }
    Ok(())
}

fn plan_skills(cfg: &Config, log_mode: LogMode) -> io::Result<SyncOutcome> {
    sync::sync_skills_only_with_mode(cfg, log_mode, ExecutionMode::Plan, "migrate:plan")
}

fn reject_conflicts(outcome: &SyncOutcome) -> io::Result<()> {
    if !outcome.has_conflicts() {
        return Ok(());
    }
    report::print_migration_conflict_summary(&outcome.conflicts);
    Err(io::Error::other(format!(
        "migration aborted due to {} conflict{}",
        outcome.conflicts.len(),
        if outcome.conflicts.len() == 1 {
            ""
        } else {
            "s"
        }
    )))
}

fn ensure_watcher_stopped(cfg: &Config) -> io::Result<()> {
    let status = daemon::watch_service_status(cfg)?;
    if status.state != ServiceState::Running {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "the Relay watcher is running as {}; stop it with `relay daemon stop` before migration",
        status.service_name
    )))
}

fn print_dotfiles_hint() -> io::Result<()> {
    let Some(home) = resolve_home_dir()? else {
        return Ok(());
    };
    if home.join(".dotfiles").is_dir() {
        println!("migrate: detected ~/.dotfiles; add --dotfiles to plan directory links");
    }
    Ok(())
}

fn plan_dotfiles(cfg: &Config) -> io::Result<Vec<LinkPlan>> {
    let home = resolve_home_dir()?.ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "could not resolve home directory")
    })?;
    require_path(
        "shared skill store",
        &cfg.central_skills_dir,
        &home.join(".agents/skills"),
    )?;
    require_path(
        "Claude skill store",
        &cfg.claude_skills_dir,
        &home.join(".claude/skills"),
    )?;

    let dotfiles = home.join(".dotfiles");
    require_real_dir(&dotfiles, "--dotfiles requires a real directory")?;
    [
        (home.join(".agents"), dotfiles.join("agents")),
        (home.join(".claude"), dotfiles.join("claude")),
    ]
    .into_iter()
    .map(|(source, target)| plan_link(&source, &target, &dotfiles))
    .collect()
}

fn require_path(label: &str, actual: &Path, expected: &Path) -> io::Result<()> {
    if actual == expected {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "--dotfiles requires the {label} at {}; configured path is {}",
            expected.display(),
            actual.display()
        ),
    ))
}

fn require_real_dir(path: &Path, context: &str) -> io::Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!("{context} at {}: {err}", path.display()),
        )
    })?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        return Ok(metadata);
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{context}, not a file or symlink: {}", path.display()),
    ))
}

fn plan_link(source: &Path, target: &Path, dotfiles: &Path) -> io::Result<LinkPlan> {
    let source_meta = metadata_if_exists(source)?;
    let target_meta = metadata_if_exists(target)?;
    let action = match (source_meta.as_ref(), target_meta.as_ref()) {
        (Some(source_meta), Some(target_meta)) if source_meta.file_type().is_symlink() => {
            if fs::canonicalize(resolve_link(source)?)? != fs::canonicalize(target)? {
                return refusal("replace unexpected symlink", source, target);
            }
            require_metadata_dir(target_meta, target)?;
            LinkAction::None
        }
        (Some(source_meta), None) if source_meta.is_dir() => {
            #[cfg(unix)]
            if source_meta.dev() != fs::metadata(dotfiles)?.dev() {
                return refusal("move across filesystems", source, target);
            }
            LinkAction::Move
        }
        (None, Some(target_meta)) => {
            require_metadata_dir(target_meta, target)?;
            LinkAction::Link
        }
        (None, None) => LinkAction::Create,
        (Some(source_meta), _) if !source_meta.is_dir() => {
            return refusal("use a source that is not a directory", source, target);
        }
        (Some(_), Some(_)) => return refusal("merge existing directories", source, target),
        _ => return refusal("use this directory state", source, target),
    };
    Ok(LinkPlan {
        source: source.to_path_buf(),
        target: target.to_path_buf(),
        action,
    })
}

fn metadata_if_exists(path: &Path) -> io::Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

fn require_metadata_dir(metadata: &fs::Metadata, path: &Path) -> io::Result<()> {
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "dotfiles target is not a real directory: {}",
            path.display()
        ),
    ))
}

fn refusal<T>(action: &str, source: &Path, target: &Path) -> io::Result<T> {
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "refusing to {action}: {} and {}",
            source.display(),
            target.display()
        ),
    ))
}

fn resolve_link(path: &Path) -> io::Result<PathBuf> {
    let target = fs::read_link(path)?;
    Ok(if target.is_absolute() {
        target
    } else {
        path.parent()
            .map_or(target.clone(), |parent| parent.join(target))
    })
}

fn print_link_plan(plans: &[LinkPlan]) {
    for plan in plans {
        match plan.action {
            LinkAction::None => println!(
                "migrate: already linked {} -> {}",
                plan.source.display(),
                plan.target.display()
            ),
            LinkAction::Move => println!(
                "migrate: would move {} to {} and link it back",
                plan.source.display(),
                plan.target.display()
            ),
            LinkAction::Link => println!(
                "migrate: would link {} -> {}",
                plan.source.display(),
                plan.target.display()
            ),
            LinkAction::Create => println!(
                "migrate: would create {} and link {} -> {}",
                plan.target.display(),
                plan.source.display(),
                plan.target.display()
            ),
        }
    }
}

fn apply_migration(cfg: &Config, plans: Option<&[LinkPlan]>) -> io::Result<SyncOutcome> {
    let applied = plans.map(apply_links).transpose()?.unwrap_or_default();
    match sync::sync_skills_only_with_mode(cfg, LogMode::Actions, ExecutionMode::Apply, "migrate") {
        Ok(outcome) => Ok(outcome),
        Err(err) => rollback_after_error(err, &applied),
    }
}

fn apply_links(plans: &[LinkPlan]) -> io::Result<Vec<LinkPlan>> {
    let mut applied = Vec::new();
    for plan in plans {
        let dotfiles = plan.target.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "dotfiles target has no parent")
        })?;
        let result = (|| {
            if plan_link(&plan.source, &plan.target, dotfiles)?.action != plan.action {
                return Err(io::Error::other(format!(
                    "directory state changed after the plan: {}",
                    plan.source.display()
                )));
            }
            apply_link(plan)
        })();
        if let Err(err) = result {
            return rollback_after_error(err, &applied);
        }
        if plan.action != LinkAction::None {
            applied.push(plan.clone());
        }
    }
    Ok(applied)
}

fn rollback_after_error<T>(err: io::Error, applied: &[LinkPlan]) -> io::Result<T> {
    match rollback_links(applied) {
        Ok(()) => Err(err),
        Err(rollback_err) => Err(io::Error::new(
            err.kind(),
            format!(
                "migration failed ({err}) and failed to restore directory links ({rollback_err})"
            ),
        )),
    }
}

fn rollback_links(plans: &[LinkPlan]) -> io::Result<()> {
    let mut failures = Vec::new();
    let mut failure_kind = None;
    for plan in plans.iter().rev() {
        if let Err(err) = rollback_link(plan) {
            failure_kind.get_or_insert(err.kind());
            failures.push(format!("{}: {err}", plan.source.display()));
        }
    }
    if failures.is_empty() {
        return Ok(());
    }
    Err(io::Error::new(
        failure_kind.unwrap_or(io::ErrorKind::Other),
        failures.join("; "),
    ))
}

#[cfg(unix)]
fn rollback_link(plan: &LinkPlan) -> io::Result<()> {
    if plan.action == LinkAction::None {
        return Ok(());
    }
    let dotfiles = plan.target.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "dotfiles target has no parent")
    })?;
    if plan_link(&plan.source, &plan.target, dotfiles)?.action != LinkAction::None {
        return Err(io::Error::other("directory state changed before rollback"));
    }
    if plan.action == LinkAction::Create
        && fs::read_dir(&plan.target)?.next().transpose()?.is_some()
    {
        return Err(io::Error::other(format!(
            "created directory is not empty: {}",
            plan.target.display()
        )));
    }

    fs::remove_file(&plan.source)?;
    let result = match plan.action {
        LinkAction::Move => fs::rename(&plan.target, &plan.source),
        LinkAction::Create => fs::remove_dir(&plan.target),
        LinkAction::Link | LinkAction::None => Ok(()),
    };
    if let Err(rollback_err) = result {
        return match symlink(&plan.target, &plan.source) {
            Ok(()) => Err(rollback_err),
            Err(restore_err) => Err(io::Error::new(
                rollback_err.kind(),
                format!("rollback failed ({rollback_err}); link restore failed ({restore_err})"),
            )),
        };
    }
    Ok(())
}

#[cfg(not(unix))]
fn rollback_link(_plan: &LinkPlan) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "directory symlinks require Unix",
    ))
}

#[cfg(unix)]
fn apply_link(plan: &LinkPlan) -> io::Result<()> {
    match plan.action {
        LinkAction::None => Ok(()),
        LinkAction::Move => {
            fs::rename(&plan.source, &plan.target)?;
            if let Err(link_err) = symlink(&plan.target, &plan.source) {
                fs::rename(&plan.target, &plan.source).map_err(|restore_err| {
                    io::Error::new(
                        link_err.kind(),
                        format!("link failed ({link_err}); restore failed ({restore_err})"),
                    )
                })?;
                return Err(link_err);
            }
            Ok(())
        }
        LinkAction::Link => symlink(&plan.target, &plan.source),
        LinkAction::Create => {
            fs::create_dir(&plan.target)?;
            symlink(&plan.target, &plan.source).inspect_err(|_| {
                let _ = fs::remove_dir(&plan.target);
            })
        }
    }
}

#[cfg(not(unix))]
fn apply_link(_plan: &LinkPlan) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "directory symlinks require Unix",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{TOOL_CLAUDE, TOOL_CODEX};
    use crate::sync::test_support::{doc, write_skill};
    use tempfile::TempDir;

    #[test]
    #[cfg(unix)]
    fn equivalent_symlink_target_is_already_linked() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let home = tmp.path();
        let dotfiles = home.join(".dotfiles");
        let source = home.join(".agents");
        let target = dotfiles.join("agents");
        fs::create_dir_all(&target)?;
        symlink(".dotfiles/../.dotfiles/agents", &source)?;

        let plan = plan_link(&source, &target, &dotfiles)?;

        assert_eq!(plan.action, LinkAction::None);
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn later_link_failure_restores_earlier_move() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let home = tmp.path();
        let dotfiles = home.join(".dotfiles");
        fs::create_dir(&dotfiles)?;
        let agents_source = home.join(".agents");
        let agents_target = dotfiles.join("agents");
        let claude_source = home.join(".claude");
        let claude_target = dotfiles.join("claude");
        fs::create_dir(&agents_source)?;
        fs::write(agents_source.join("keep"), "agents")?;
        fs::create_dir(&claude_source)?;
        fs::write(claude_source.join("keep"), "claude")?;
        let plans = vec![
            plan_link(&agents_source, &agents_target, &dotfiles)?,
            plan_link(&claude_source, &claude_target, &dotfiles)?,
        ];
        fs::create_dir(&claude_target)?;

        let err = apply_links(&plans).unwrap_err();

        assert!(err
            .to_string()
            .contains("refusing to merge existing directories"));
        assert!(!fs::symlink_metadata(&agents_source)?
            .file_type()
            .is_symlink());
        assert_eq!(fs::read_to_string(agents_source.join("keep"))?, "agents");
        assert!(!agents_target.exists());
        assert_eq!(fs::read_to_string(claude_source.join("keep"))?, "claude");
        assert!(claude_target.is_dir());
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn skill_sync_failure_restores_relocated_roots() -> io::Result<()> {
        let _env = crate::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let tmp = TempDir::new()?;
        let home = tmp.path().join("home");
        fs::create_dir(&home)?;
        std::env::set_var("RELAY_HOME", &home);
        let result = (|| {
            let mut cfg = Config::default_paths()?;
            cfg.enabled_tools = vec![TOOL_CLAUDE.to_string(), TOOL_CODEX.to_string()];
            cfg.central_skills_dir = home.join(".agents/skills");
            cfg.claude_skills_dir = home.join(".claude/skills");
            cfg.codex_skills_dir = home.join(".codex/relay-skills");
            fs::create_dir(home.join(".dotfiles"))?;
            fs::create_dir(home.join(".codex"))?;
            write_skill(
                &cfg.central_skills_dir,
                "current",
                &doc("current", "Current"),
            )?;
            fs::create_dir_all(home.join(".claude/commands"))?;
            fs::write(home.join(".claude/settings.json"), "keep")?;
            let plans = plan_dotfiles(&cfg)?;
            let fault_target = cfg.codex_skills_dir.join("current");
            std::env::set_var("RELAY_TEST_FAIL_SKILL_TARGET", &fault_target);

            let migration = apply_migration(&cfg, Some(&plans));

            std::env::remove_var("RELAY_TEST_FAIL_SKILL_TARGET");
            let err = migration.unwrap_err();
            assert!(err
                .to_string()
                .contains("injected late skill target failure"));
            for source in [home.join(".agents"), home.join(".claude")] {
                assert!(!fs::symlink_metadata(source)?.file_type().is_symlink());
            }
            assert!(!home.join(".dotfiles/agents").exists());
            assert!(!home.join(".dotfiles/claude").exists());
            assert!(cfg.central_skills_dir.join("current/SKILL.md").exists());
            assert!(!cfg.claude_skills_dir.join("current").exists());
            assert!(!cfg.codex_skills_dir.join("current").exists());
            assert_eq!(
                fs::read_to_string(home.join(".claude/settings.json"))?,
                "keep"
            );
            assert!(!cfg.skill_state_path()?.exists());
            Ok(())
        })();
        std::env::remove_var("RELAY_TEST_FAIL_SKILL_TARGET");
        std::env::remove_var("RELAY_HOME");
        result
    }
}
