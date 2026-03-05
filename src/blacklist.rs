use crate::config::{Config, TOOL_CLAUDE, TOOL_CODEX, TOOL_CURSOR, TOOL_OPENCODE};
use crate::history::HistoryRecorder;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

fn validated_tool_suffix(raw_suffix: &str) -> Option<&Path> {
    let suffix = Path::new(raw_suffix);
    if raw_suffix.is_empty() || suffix.is_absolute() {
        return None;
    }

    let mut has_normal = false;
    for component in suffix.components() {
        match component {
            Component::Normal(_) => has_normal = true,
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => return None,
        }
    }

    has_normal.then_some(suffix)
}

#[cfg_attr(any(test, coverage), allow(dead_code))]
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

    if let Some(raw_suffix) = relative_path.strip_prefix("commands/") {
        let Some(suffix) = validated_tool_suffix(raw_suffix) else {
            return paths;
        };
        match tool {
            TOOL_CLAUDE => paths.push(cfg.claude_dir.join(suffix)),
            TOOL_CODEX => paths.push(cfg.codex_dir.join(suffix)),
            TOOL_CURSOR => paths.push(cfg.cursor_dir.join(suffix)),
            TOOL_OPENCODE => paths.push(cfg.opencode_commands_dir.join(suffix)),
            _ => {}
        }
    } else if let Some(raw_suffix) = relative_path.strip_prefix("skills/") {
        let Some(suffix) = validated_tool_suffix(raw_suffix) else {
            return paths;
        };
        match tool {
            TOOL_CLAUDE => paths.push(cfg.claude_skills_dir.join(suffix)),
            TOOL_CODEX => paths.push(cfg.codex_skills_dir.join(suffix)),
            TOOL_OPENCODE => paths.push(cfg.opencode_skills_dir.join(suffix)),
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
    fn resolve_tool_paths_commands_valid_inputs_unchanged() {
        let tmp = TempDir::new().unwrap();
        let cfg = make_config(&tmp);
        let paths = resolve_tool_paths(&cfg, "commands/review.md", TOOL_CLAUDE);
        assert_eq!(paths, vec![cfg.claude_dir.join("review.md")]);

        let paths = resolve_tool_paths(&cfg, "commands/review.md", TOOL_CODEX);
        assert_eq!(paths, vec![cfg.codex_dir.join("review.md")]);

        let paths = resolve_tool_paths(&cfg, "commands/review.md", TOOL_CURSOR);
        assert_eq!(paths, vec![cfg.cursor_dir.join("review.md")]);

        let paths = resolve_tool_paths(&cfg, "commands/review.md", TOOL_OPENCODE);
        assert_eq!(paths, vec![cfg.opencode_commands_dir.join("review.md")]);
    }

    #[test]
    fn resolve_tool_paths_skills_valid_inputs_unchanged() {
        let tmp = TempDir::new().unwrap();
        let cfg = make_config(&tmp);
        let paths = resolve_tool_paths(&cfg, "skills/plan/SKILL.md", TOOL_CLAUDE);
        assert_eq!(paths, vec![cfg.claude_skills_dir.join("plan/SKILL.md")]);

        let paths = resolve_tool_paths(&cfg, "skills/plan/SKILL.md", TOOL_CODEX);
        assert_eq!(paths, vec![cfg.codex_skills_dir.join("plan/SKILL.md")]);

        let paths = resolve_tool_paths(&cfg, "skills/plan/SKILL.md", TOOL_OPENCODE);
        assert_eq!(paths, vec![cfg.opencode_skills_dir.join("plan/SKILL.md")]);
    }

    #[test]
    fn resolve_tool_paths_rejects_malformed_command_suffixes() {
        let tmp = TempDir::new().unwrap();
        let cfg = make_config(&tmp);
        let absolute = if cfg!(windows) {
            "commands/C:/tmp/evil.md"
        } else {
            "commands//tmp/evil.md"
        };

        for relative_path in [
            "commands/",
            "commands/./review.md",
            "commands/../review.md",
            "commands/review/../final.md",
            absolute,
        ] {
            for tool in [TOOL_CLAUDE, TOOL_CODEX, TOOL_CURSOR, TOOL_OPENCODE] {
                assert!(
                    resolve_tool_paths(&cfg, relative_path, tool).is_empty(),
                    "expected no targets for {relative_path} ({tool})",
                );
            }
        }
    }

    #[test]
    fn resolve_tool_paths_rejects_malformed_skill_suffixes() {
        let tmp = TempDir::new().unwrap();
        let cfg = make_config(&tmp);
        let absolute = if cfg!(windows) {
            "skills/C:/tmp/evil"
        } else {
            "skills//tmp/evil"
        };

        for relative_path in [
            "skills/",
            "skills/./plan",
            "skills/../plan",
            "skills/plan/../other",
            absolute,
        ] {
            for tool in [TOOL_CLAUDE, TOOL_CODEX, TOOL_OPENCODE] {
                assert!(
                    resolve_tool_paths(&cfg, relative_path, tool).is_empty(),
                    "expected no targets for {relative_path} ({tool})",
                );
            }
        }
    }

    #[test]
    fn retroactive_delete_malformed_command_path_keeps_tool_root() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let marker = cfg.claude_dir.join("keep.md");
        write_plain(&marker, "keep")?;

        retroactive_delete(&cfg, "commands/", &[TOOL_CLAUDE.to_string()])?;

        assert!(cfg.claude_dir.is_dir());
        assert!(marker.exists());
        Ok(())
    }

    #[test]
    fn retroactive_delete_malformed_skill_path_keeps_tool_root() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let marker = cfg.codex_skills_dir.join("keep.txt");
        write_plain(&marker, "keep")?;

        retroactive_delete(&cfg, "skills/", &[TOOL_CODEX.to_string()])?;

        assert!(cfg.codex_skills_dir.is_dir());
        assert!(marker.exists());
        Ok(())
    }
}
