use crate::config::Config;
use crate::sync::LogMode;
use crate::tools::{ToolDefinition, TOOL_DEFINITIONS};
use notify::RecursiveMode;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(not(any(test, coverage)))]
use crate::sync::{self, ExecutionMode};
#[cfg(not(any(test, coverage)))]
use notify::{Config as NotifyConfig, RecommendedWatcher, Watcher};
#[cfg(not(any(test, coverage)))]
use std::sync::mpsc::{self, RecvTimeoutError};
#[cfg(not(any(test, coverage)))]
use std::time::{Duration, Instant};

pub fn build_watch_list(cfg: &Config) -> Vec<(PathBuf, RecursiveMode)> {
    let mut paths = Vec::new();
    for (path, mode) in [
        (&cfg.central_dir, RecursiveMode::NonRecursive),
        (&cfg.central_skills_dir, RecursiveMode::Recursive),
        (&cfg.central_agents_dir, RecursiveMode::Recursive),
        (&cfg.central_rules_dir, RecursiveMode::Recursive),
    ] {
        if path.exists() {
            paths.push((path.clone(), mode));
        }
    }

    for definition in TOOL_DEFINITIONS.iter() {
        if !cfg.tool_enabled(definition.id) {
            continue;
        }
        for (path, mode) in tool_watch_paths(cfg, definition) {
            if path.exists() {
                paths.push((path, mode));
            }
        }
    }
    paths
}

fn watch_origin(cfg: &Config, paths: &[PathBuf]) -> String {
    for path in paths {
        if let Some(origin) = classify_origin(cfg, path) {
            return origin;
        }
    }
    "watch".to_string()
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

    let mut roots: Vec<(&str, &Path)> = vec![
        ("central", &cfg.central_dir),
        ("central_skills", &cfg.central_skills_dir),
        ("central_agents", &cfg.central_agents_dir),
        ("central_rules", &cfg.central_rules_dir),
        ("claude", &cfg.claude_dir),
        ("claude_skills", &cfg.claude_skills_dir),
        ("codex", &cfg.codex_dir),
        ("codex_skills", &cfg.codex_skills_dir),
        ("opencode", &cfg.opencode_commands_dir),
        ("opencode_skills", &cfg.opencode_skills_dir),
        ("cursor", &cfg.cursor_dir),
    ];
    if let Some(parent) = cfg.opencode_agents_file.parent() {
        roots.push(("opencode_agents", parent));
    }
    for (label, root) in roots {
        if let Some(origin) = format_origin(path, root, label) {
            return Some(origin);
        }
    }

    let file_roots: [(&str, &Path); 2] = [
        ("codex_agents", &cfg.codex_agents_file),
        ("codex_rules", &cfg.codex_rules_file),
    ];
    for (label, root) in file_roots {
        if path == root {
            return Some(format!("watch:{label}"));
        }
        if let Some(parent) = root.parent() {
            if let Some(origin) = format_origin(path, parent, label) {
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
pub fn watch(cfg: &Config, debounce_ms: u64, log_mode: LogMode) -> io::Result<()> {
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
        let origin = watch_origin(cfg, &changed_paths);
        crate::logging::debug(&format!("watch applying sync origin={origin}"));
        let _ = sync::sync_all_with_mode(cfg, log_mode, ExecutionMode::Apply, &origin)?;
    }
}

#[cfg(any(test, coverage))]
pub fn watch(cfg: &Config, _debounce_ms: u64, _log_mode: LogMode) -> io::Result<()> {
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
            codex_dir: tmp.path().join("codex_prompts"),
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
        fs::create_dir_all(&cfg.codex_dir)?;
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
        assert!(paths.contains(&(cfg.codex_dir.clone(), RecursiveMode::NonRecursive)));
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
    fn watch_origin_marks_codex_path() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let cfg = make_config(&tmp);
        let origin = watch_origin(&cfg, &[cfg.codex_dir.join("review.md")]);
        assert_eq!(origin, "watch:codex:review.md");
        Ok(())
    }

    #[test]
    fn watch_origin_falls_back_when_unrecognized() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let cfg = make_config(&tmp);
        let origin = watch_origin(&cfg, &[tmp.path().join("other/path.md")]);
        assert_eq!(origin, "watch");
        Ok(())
    }
}
