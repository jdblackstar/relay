use crate::config::{Config, TOOL_CLAUDE, TOOL_CODEX, TOOL_CURSOR, TOOL_OPENCODE};
use crate::history::HistoryRecorder;
use std::fs;
use std::io;
use std::path::PathBuf;

pub fn collect_tool_flags(
    claude: bool,
    codex: bool,
    cursor: bool,
    opencode: bool,
) -> Vec<String> {
    let mut tools = Vec::new();
    if claude {
        tools.push(TOOL_CLAUDE.to_string());
    }
    if codex {
        tools.push(TOOL_CODEX.to_string());
    }
    if cursor {
        tools.push(TOOL_CURSOR.to_string());
    }
    if opencode {
        tools.push(TOOL_OPENCODE.to_string());
    }
    tools
}

#[cfg_attr(any(test, coverage), allow(dead_code))]
pub fn add_blacklist(cfg: &mut Config, path: &str, tools: &[String]) -> io::Result<()> {
    let entry = cfg.blacklist.entry(path.to_string()).or_default();
    for tool in tools {
        if !entry.iter().any(|t| t == tool) {
            entry.push(tool.clone());
        }
    }
    save_config(cfg)?;
    retroactive_delete(cfg, path, tools)?;
    Ok(())
}

#[cfg_attr(any(test, coverage), allow(dead_code))]
pub fn remove_blacklist(cfg: &mut Config, path: &str, tools: &[String]) -> io::Result<()> {
    if let Some(entry) = cfg.blacklist.get_mut(path) {
        entry.retain(|t| !tools.iter().any(|tool| tool == t));
        if entry.is_empty() {
            cfg.blacklist.remove(path);
        }
    }
    save_config(cfg)
}

#[cfg_attr(any(test, coverage), allow(dead_code))]
fn save_config(cfg: &Config) -> io::Result<()> {
    let config_path = Config::config_path()?;
    cfg.save(&config_path)
}

fn retroactive_delete(cfg: &Config, path: &str, tools: &[String]) -> io::Result<()> {
    let mut recorder = HistoryRecorder::new(cfg, &format!("blacklist:{path}"))?;
    for tool in tools {
        let targets = resolve_tool_paths(cfg, path, tool);
        for target in targets {
            if target.exists() {
                let before = recorder.capture_path(&target)?;
                if target.is_dir() {
                    fs::remove_dir_all(&target)?;
                } else {
                    fs::remove_file(&target)?;
                }
                let after = recorder.capture_path(&target)?;
                recorder.record_change(&target, before, after);
                eprintln!("blacklist: deleted {}", target.display());
            }
        }
    }
    if let Some(event_id) = recorder.finish()? {
        eprintln!("history: recorded event {event_id}");
    }
    Ok(())
}

pub fn resolve_tool_paths(cfg: &Config, relative_path: &str, tool: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Some(rest) = relative_path.strip_prefix("commands/") {
        match tool {
            TOOL_CLAUDE => paths.push(cfg.claude_dir.join(rest)),
            TOOL_CODEX => paths.push(cfg.codex_dir.join(rest)),
            TOOL_CURSOR => paths.push(cfg.cursor_dir.join(rest)),
            TOOL_OPENCODE => paths.push(cfg.opencode_commands_dir.join(rest)),
            _ => {}
        }
    } else if let Some(rest) = relative_path.strip_prefix("skills/") {
        match tool {
            TOOL_CLAUDE => paths.push(cfg.claude_skills_dir.join(rest)),
            TOOL_CODEX => paths.push(cfg.codex_skills_dir.join(rest)),
            TOOL_OPENCODE => paths.push(cfg.opencode_skills_dir.join(rest)),
            _ => {}
        }
    } else if relative_path.starts_with("agents/") {
        match tool {
            TOOL_CODEX => paths.push(cfg.codex_agents_file.clone()),
            TOOL_OPENCODE => paths.push(cfg.opencode_agents_file.clone()),
            _ => {}
        }
    } else if relative_path.starts_with("rules/") {
        if tool == TOOL_CODEX {
            paths.push(cfg.codex_rules_file.clone());
        }
    }

    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::test_support::{make_config, setup, write_plain};
    use tempfile::TempDir;

    #[test]
    fn collect_tool_flags_gathers_selected() {
        let flags = collect_tool_flags(true, false, true, false);
        assert_eq!(flags, vec!["claude", "cursor"]);
    }

    #[test]
    fn collect_tool_flags_empty_when_none() {
        let flags = collect_tool_flags(false, false, false, false);
        assert!(flags.is_empty());
    }

    #[test]
    fn collect_tool_flags_all() {
        let flags = collect_tool_flags(true, true, true, true);
        assert_eq!(flags, vec!["claude", "codex", "cursor", "opencode"]);
    }

    #[test]
    fn resolve_tool_paths_commands() {
        let tmp = TempDir::new().unwrap();
        let cfg = make_config(&tmp);
        let paths = resolve_tool_paths(&cfg, "commands/review.md", TOOL_CLAUDE);
        assert_eq!(paths.len(), 1);
        assert!(paths[0].ends_with("review.md"));

        let paths = resolve_tool_paths(&cfg, "commands/review.md", TOOL_CODEX);
        assert_eq!(paths.len(), 1);

        let paths = resolve_tool_paths(&cfg, "commands/review.md", TOOL_CURSOR);
        assert_eq!(paths.len(), 1);

        let paths = resolve_tool_paths(&cfg, "commands/review.md", TOOL_OPENCODE);
        assert_eq!(paths.len(), 1);
    }

    #[test]
    fn resolve_tool_paths_skills() {
        let tmp = TempDir::new().unwrap();
        let cfg = make_config(&tmp);
        let paths = resolve_tool_paths(&cfg, "skills/plan", TOOL_CLAUDE);
        assert_eq!(paths.len(), 1);

        // cursor has no skills dir
        let paths = resolve_tool_paths(&cfg, "skills/plan", TOOL_CURSOR);
        assert!(paths.is_empty());
    }

    #[test]
    fn resolve_tool_paths_agents() {
        let tmp = TempDir::new().unwrap();
        let cfg = make_config(&tmp);
        let paths = resolve_tool_paths(&cfg, "agents/AGENTS.md", TOOL_CODEX);
        assert_eq!(paths.len(), 1);

        let paths = resolve_tool_paths(&cfg, "agents/AGENTS.md", TOOL_CLAUDE);
        assert!(paths.is_empty());
    }

    #[test]
    fn resolve_tool_paths_rules() {
        let tmp = TempDir::new().unwrap();
        let cfg = make_config(&tmp);
        let paths = resolve_tool_paths(&cfg, "rules/codex/default.rules", TOOL_CODEX);
        assert_eq!(paths.len(), 1);

        let paths = resolve_tool_paths(&cfg, "rules/codex/default.rules", TOOL_CLAUDE);
        assert!(paths.is_empty());
    }

    #[test]
    fn add_and_remove_blacklist_updates_config() -> io::Result<()> {
        let (_tmp, mut cfg) = setup()?;
        assert!(!cfg.is_blacklisted("commands/review.md", TOOL_CLAUDE));

        // Can't save without a config path, so test the in-memory mutation
        cfg.blacklist
            .entry("commands/review.md".to_string())
            .or_default()
            .push(TOOL_CLAUDE.to_string());
        assert!(cfg.is_blacklisted("commands/review.md", TOOL_CLAUDE));
        assert!(!cfg.is_blacklisted("commands/review.md", TOOL_CODEX));

        cfg.blacklist
            .get_mut("commands/review.md")
            .unwrap()
            .retain(|t| t != TOOL_CLAUDE);
        if cfg.blacklist["commands/review.md"].is_empty() {
            cfg.blacklist.remove("commands/review.md");
        }
        assert!(!cfg.is_blacklisted("commands/review.md", TOOL_CLAUDE));
        Ok(())
    }

    #[test]
    fn retroactive_delete_removes_files() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let claude_file = cfg.claude_dir.join("review.md");
        write_plain(&claude_file, "hello")?;
        assert!(claude_file.exists());

        retroactive_delete(&cfg, "commands/review.md", &[TOOL_CLAUDE.to_string()])?;
        assert!(!claude_file.exists());
        Ok(())
    }

    #[test]
    fn retroactive_delete_noop_for_missing() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        // Should not error when file doesn't exist
        retroactive_delete(&cfg, "commands/nonexistent.md", &[TOOL_CLAUDE.to_string()])?;
        Ok(())
    }
}
