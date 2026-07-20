use crate::config::Config;
use crate::sync::LogMode;
use crate::tools::{ToolDefinition, TOOL_DEFINITIONS};
use notify::RecursiveMode;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(not(any(test, coverage)))]
use crate::process_lock::ProcessLock;
#[cfg(not(any(test, coverage)))]
use crate::sync::{self, ExecutionMode};
#[cfg(not(any(test, coverage)))]
use notify::{Config as NotifyConfig, RecommendedWatcher, Watcher};
#[cfg(not(any(test, coverage)))]
use std::sync::mpsc::{self, RecvTimeoutError};
#[cfg(not(any(test, coverage)))]
use std::time::{Duration, Instant};

pub(crate) fn build_watch_list(cfg: &Config) -> Vec<(PathBuf, RecursiveMode)> {
    let mut paths = Vec::new();
    let mut push_unique = |path: PathBuf, mode: RecursiveMode| {
        if path.exists() && !paths.iter().any(|(existing, _)| existing == &path) {
            paths.push((path, mode));
        }
    };
    for (path, mode) in [
        (&cfg.central_dir, RecursiveMode::NonRecursive),
        (&cfg.central_skills_dir, RecursiveMode::Recursive),
        (&cfg.central_agents_dir, RecursiveMode::Recursive),
        (&cfg.central_rules_dir, RecursiveMode::Recursive),
    ] {
        push_unique(path.clone(), mode);
    }

    for definition in TOOL_DEFINITIONS.iter() {
        if !cfg.tool_enabled(definition.id) {
            continue;
        }
        for (path, mode) in tool_watch_paths(cfg, definition) {
            push_unique(path, mode);
        }
    }
    if let Ok(import_dirs) = cfg.legacy_skill_import_dirs() {
        for path in import_dirs {
            push_unique(path, RecursiveMode::Recursive);
        }
    }
    paths
}

fn watch_origin(cfg: &Config, paths: &[PathBuf]) -> Option<String> {
    for path in paths {
        if let Some(origin) = classify_origin(cfg, path) {
            return Some(origin);
        }
    }
    None
}

fn classify_origin(cfg: &Config, path: &Path) -> Option<String> {
    fn format_origin(path: &Path, root: &Path, label: &str) -> Option<String> {
        if !path.starts_with(root) {
            return None;
        }
        let rel = path.strip_prefix(root).ok()?;
        let rel = rel.to_string_lossy().trim_start_matches('/').to_string();
        if rel.is_empty() {
            Some(format!("watch:{label}"))
        } else {
            Some(format!("watch:{label}:{rel}"))
        }
    }

    let roots: [(&str, &Path); 10] = [
        ("central", &cfg.central_dir),
        ("central_skills", &cfg.central_skills_dir),
        ("central_agents", &cfg.central_agents_dir),
        ("central_rules", &cfg.central_rules_dir),
        ("claude", &cfg.claude_dir),
        ("claude_skills", &cfg.claude_skills_dir),
        ("codex_skills", &cfg.codex_skills_dir),
        ("opencode", &cfg.opencode_commands_dir),
        ("opencode_skills", &cfg.opencode_skills_dir),
        ("cursor", &cfg.cursor_dir),
    ];
    for (label, root) in roots {
        if let Some(origin) = format_origin(path, root, label) {
            return Some(origin);
        }
    }

    let files: [(&str, &Path); 3] = [
        ("opencode_agents", &cfg.opencode_agents_file),
        ("codex_agents", &cfg.codex_agents_file),
        ("codex_rules", &cfg.codex_rules_file),
    ];
    for (label, file) in files {
        if path == file {
            return Some(format!("watch:{label}"));
        }
    }

    if let Ok(import_dirs) = cfg.legacy_skill_import_dirs() {
        for root in import_dirs {
            if let Some(origin) = format_origin(path, &root, "skill_import") {
                return Some(origin);
            }
        }
    }

    None
}

fn tool_watch_paths(cfg: &Config, tool: &ToolDefinition) -> Vec<(PathBuf, RecursiveMode)> {
    let mut paths = Vec::new();
    if let Some(getter) = tool.commands_dir {
        paths.push((getter(cfg).clone(), RecursiveMode::NonRecursive));
    }
    if let Some(getter) = tool.skills_dir {
        paths.push((getter(cfg).clone(), RecursiveMode::Recursive));
    }
    if let Some(getter) = tool.agents_file {
        if let Some(parent) = getter(cfg).parent() {
            paths.push((parent.to_path_buf(), RecursiveMode::NonRecursive));
        }
    }
    if let Some(getter) = tool.rules_file {
        if let Some(parent) = getter(cfg).parent() {
            paths.push((parent.to_path_buf(), RecursiveMode::NonRecursive));
        }
    }
    paths
}

#[cfg(not(any(test, coverage)))]
pub(crate) fn watch(cfg: &Config, debounce_ms: u64, log_mode: LogMode) -> io::Result<()> {
    let (tx, rx) = mpsc::channel();
    let mut watcher = RecommendedWatcher::new(
        move |res| {
            let _ = tx.send(res);
        },
        NotifyConfig::default(),
    )
    .map_err(to_io)?;

    for (path, mode) in build_watch_list(cfg) {
        crate::logging::debug(&format!(
            "watch register path={} mode={mode:?}",
            path.display()
        ));
        watcher.watch(&path, mode).map_err(to_io)?;
    }

    loop {
        let mut changed_paths: Vec<PathBuf> = Vec::new();
        match rx.recv() {
            Ok(Ok(event)) => {
                crate::logging::debug(&format!("watch event: {event:?}"));
                changed_paths.extend(event.paths);
            }
            Ok(Err(err)) => {
                crate::logging::debug(&format!("watch error: {err}"));
                return Err(to_io(err));
            }
            Err(_) => {
                crate::logging::debug("watch channel disconnected");
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "file watcher channel disconnected",
                ));
            }
        }
        let start = Instant::now();
        loop {
            let elapsed = start.elapsed();
            if elapsed >= Duration::from_millis(debounce_ms) {
                break;
            }
            let remaining = Duration::from_millis(debounce_ms).saturating_sub(elapsed);
            match rx.recv_timeout(remaining) {
                Ok(Ok(event)) => {
                    crate::logging::debug(&format!("watch debounce event: {event:?}"));
                    changed_paths.extend(event.paths);
                }
                Ok(Err(err)) => {
                    crate::logging::debug(&format!("watch debounce error: {err}"));
                    return Err(to_io(err));
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    crate::logging::debug("watch channel disconnected during debounce");
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "file watcher channel disconnected",
                    ));
                }
            }
        }
        let Some(origin) = watch_origin(cfg, &changed_paths) else {
            crate::logging::debug("watch ignored unrelated event batch");
            continue;
        };
        crate::logging::debug(&format!("watch applying sync origin={origin}"));
        let _lock = ProcessLock::acquire(&origin)?;
        let _ = sync::sync_all_with_mode(cfg, log_mode, ExecutionMode::Apply, &origin)?;
    }
}

#[cfg(any(test, coverage))]
pub(crate) fn watch(cfg: &Config, _debounce_ms: u64, _log_mode: LogMode) -> io::Result<()> {
    let _ = build_watch_list(cfg);
    Ok(())
}

fn to_io(err: notify::Error) -> io::Error {
    io::Error::other(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, TOOL_CLAUDE, TOOL_CODEX, TOOL_OPENCODE};
    use std::fs;
    use tempfile::TempDir;

    fn make_config(tmp: &TempDir) -> Config {
        Config {
            enabled_tools: vec![
                TOOL_CLAUDE.to_string(),
                TOOL_CODEX.to_string(),
                TOOL_OPENCODE.to_string(),
            ],
            verified_versions: std::collections::HashMap::new(),
            blacklist: std::collections::HashMap::new(),
            central_dir: tmp.path().join("central"),
            central_skills_dir: tmp.path().join("central_skills"),
            central_agents_dir: tmp.path().join("central_agents"),
            central_rules_dir: tmp.path().join("central_rules"),
            claude_dir: tmp.path().join("claude_commands"),
            claude_skills_dir: tmp.path().join("claude_skills"),
            cursor_dir: tmp.path().join("cursor"),
            opencode_commands_dir: tmp.path().join("opencode_commands"),
            opencode_skills_dir: tmp.path().join("opencode_skills"),
            opencode_agents_file: tmp.path().join("opencode_agents/AGENTS.md"),
            codex_skills_dir: tmp.path().join("codex_skills"),
            codex_rules_file: tmp.path().join("codex_rules/default.rules"),
            codex_agents_file: tmp.path().join("codex_agents/AGENTS.md"),
        }
    }

    #[test]
    fn build_watch_list_includes_expected_paths() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let cfg = make_config(&tmp);
        fs::create_dir_all(&cfg.central_dir)?;
        fs::create_dir_all(&cfg.central_skills_dir)?;
        fs::create_dir_all(&cfg.central_agents_dir)?;
        fs::create_dir_all(&cfg.central_rules_dir)?;
        fs::create_dir_all(&cfg.claude_dir)?;
        fs::create_dir_all(&cfg.claude_skills_dir)?;
        fs::create_dir_all(&cfg.opencode_commands_dir)?;
        fs::create_dir_all(&cfg.opencode_skills_dir)?;
        fs::create_dir_all(cfg.opencode_agents_file.parent().unwrap())?;
        fs::create_dir_all(&cfg.codex_skills_dir)?;
        fs::create_dir_all(cfg.codex_rules_file.parent().unwrap())?;
        fs::create_dir_all(cfg.codex_agents_file.parent().unwrap())?;

        let paths = build_watch_list(&cfg);
        assert!(paths.contains(&(cfg.central_dir.clone(), RecursiveMode::NonRecursive)));
        assert!(paths.contains(&(cfg.central_skills_dir.clone(), RecursiveMode::Recursive)));
        assert!(paths.contains(&(cfg.central_agents_dir.clone(), RecursiveMode::Recursive)));
        assert!(paths.contains(&(cfg.central_rules_dir.clone(), RecursiveMode::Recursive)));
        assert!(paths.contains(&(cfg.claude_dir.clone(), RecursiveMode::NonRecursive)));
        assert!(paths.contains(&(cfg.claude_skills_dir.clone(), RecursiveMode::Recursive)));
        assert!(paths.contains(&(
            cfg.opencode_commands_dir.clone(),
            RecursiveMode::NonRecursive
        )));
        assert!(paths.contains(&(cfg.opencode_skills_dir.clone(), RecursiveMode::Recursive)));
        assert!(paths.contains(&(
            cfg.opencode_agents_file.parent().unwrap().to_path_buf(),
            RecursiveMode::NonRecursive
        )));
        assert!(paths.contains(&(cfg.codex_skills_dir.clone(), RecursiveMode::Recursive)));
        assert!(paths.contains(&(
            cfg.codex_rules_file.parent().unwrap().to_path_buf(),
            RecursiveMode::NonRecursive
        )));
        assert!(paths.contains(&(
            cfg.codex_agents_file.parent().unwrap().to_path_buf(),
            RecursiveMode::NonRecursive
        )));
        Ok(())
    }

    #[test]
    fn watch_test_mode_runs() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let cfg = make_config(&tmp);
        watch(&cfg, 10, LogMode::Quiet)
    }

    #[test]
    fn to_io_wraps_error() {
        let err = notify::Error::generic("oops");
        let wrapped = to_io(err);
        assert_eq!(wrapped.kind(), io::ErrorKind::Other);
    }

    #[test]
    fn watch_origin_marks_codex_skills_path() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let cfg = make_config(&tmp);
        let origin = watch_origin(&cfg, &[cfg.codex_skills_dir.join("review/SKILL.md")]);
        assert_eq!(
            origin.as_deref(),
            Some("watch:codex_skills:review/SKILL.md")
        );
        Ok(())
    }

    #[test]
    fn watch_origin_ignores_unrecognized_path() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let cfg = make_config(&tmp);
        let origin = watch_origin(&cfg, &[tmp.path().join("other/path.md")]);
        assert_eq!(origin, None);
        Ok(())
    }

    #[test]
    fn watch_origin_matches_file_inputs_exactly() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let cfg = make_config(&tmp);

        for (path, expected) in [
            (&cfg.codex_agents_file, "watch:codex_agents"),
            (&cfg.codex_rules_file, "watch:codex_rules"),
            (&cfg.opencode_agents_file, "watch:opencode_agents"),
        ] {
            assert_eq!(
                watch_origin(&cfg, std::slice::from_ref(path)).as_deref(),
                Some(expected)
            );
        }
        Ok(())
    }

    #[test]
    fn watch_origin_ignores_siblings_of_file_inputs() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let cfg = make_config(&tmp);

        let codex_sibling = cfg.codex_agents_file.parent().unwrap().join("state.sqlite");
        let opencode_sibling = cfg
            .opencode_agents_file
            .parent()
            .unwrap()
            .join("opencode.json");

        assert_eq!(watch_origin(&cfg, &[codex_sibling]), None);
        assert_eq!(watch_origin(&cfg, &[opencode_sibling]), None);
        Ok(())
    }

    #[test]
    fn watch_origin_uses_relevant_path_from_mixed_batch() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let cfg = make_config(&tmp);
        let unrelated = cfg.codex_agents_file.parent().unwrap().join("state.sqlite");

        let origin = watch_origin(&cfg, &[unrelated, cfg.codex_agents_file.clone()]);

        assert_eq!(origin.as_deref(), Some("watch:codex_agents"));
        Ok(())
    }
}
