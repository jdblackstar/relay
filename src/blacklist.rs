use crate::config::{Config, TOOL_CLAUDE, TOOL_CODEX, TOOL_CURSOR, TOOL_OPENCODE};
use crate::history::HistoryRecorder;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

pub(crate) const LEGACY_AGENTS_BLACKLIST_KEY: &str = "agents/AGENTS.md";
pub(crate) const CODEX_AGENTS_BLACKLIST_KEY: &str = "agents/codex/AGENTS.md";
pub(crate) const OPENCODE_AGENTS_BLACKLIST_KEY: &str = "agents/opencode/AGENTS.md";
pub(crate) const CODEX_RULES_BLACKLIST_KEY: &str = "rules/codex/default.rules";

pub(crate) fn collect_tool_flags(
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
pub(crate) fn add_blacklist(cfg: &mut Config, path: &str, tools: &[String]) -> io::Result<()> {
    validate_blacklist_path(path)?;
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
pub(crate) fn remove_blacklist(cfg: &mut Config, path: &str, tools: &[String]) -> io::Result<()> {
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

fn codex_legacy_prompt_candidate(codex_dir: &Path, suffix: &Path) -> Option<PathBuf> {
    let mut components = suffix.components();
    let name = match (components.next(), components.next()) {
        (Some(Component::Normal(name)), None) => name.to_string_lossy(),
        _ => return None,
    };
    if name.starts_with("prompt:") {
        return None;
    }
    Some(codex_dir.join(format!("prompt:{name}")))
}

fn push_unique(paths: &mut Vec<PathBuf>, candidate: PathBuf) {
    if !paths.iter().any(|existing| existing == &candidate) {
        paths.push(candidate);
    }
}

fn is_supported_agents_blacklist_path(path: &str) -> bool {
    matches!(
        path,
        LEGACY_AGENTS_BLACKLIST_KEY | CODEX_AGENTS_BLACKLIST_KEY | OPENCODE_AGENTS_BLACKLIST_KEY
    )
}

fn validate_blacklist_path(path: &str) -> io::Result<()> {
    if path.starts_with("agents/") && !is_supported_agents_blacklist_path(path) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "invalid agents blacklist path '{path}'; supported paths: {LEGACY_AGENTS_BLACKLIST_KEY}, {CODEX_AGENTS_BLACKLIST_KEY}, {OPENCODE_AGENTS_BLACKLIST_KEY}"
            ),
        ));
    }
    if path.starts_with("rules/") && path != CODEX_RULES_BLACKLIST_KEY {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "invalid rules blacklist path '{path}'; supported path: {CODEX_RULES_BLACKLIST_KEY}"
            ),
        ));
    }
    Ok(())
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

pub(crate) fn resolve_tool_paths(cfg: &Config, relative_path: &str, tool: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Some(raw_suffix) = relative_path.strip_prefix("commands/") {
        let Some(suffix) = validated_tool_suffix(raw_suffix) else {
            return paths;
        };
        match tool {
            TOOL_CLAUDE => paths.push(cfg.claude_dir.join(suffix)),
            TOOL_CODEX => {
                push_unique(&mut paths, cfg.codex_dir.join(suffix));
                if let Some(legacy) = codex_legacy_prompt_candidate(&cfg.codex_dir, suffix) {
                    push_unique(&mut paths, legacy);
                }
            }
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
    } else if is_supported_agents_blacklist_path(relative_path) {
        match (relative_path, tool) {
            (LEGACY_AGENTS_BLACKLIST_KEY, TOOL_CODEX)
            | (CODEX_AGENTS_BLACKLIST_KEY, TOOL_CODEX) => paths.push(cfg.codex_agents_file.clone()),
            (LEGACY_AGENTS_BLACKLIST_KEY, TOOL_OPENCODE)
            | (OPENCODE_AGENTS_BLACKLIST_KEY, TOOL_OPENCODE) => {
                paths.push(cfg.opencode_agents_file.clone())
            }
            _ => {}
        }
    } else if relative_path == CODEX_RULES_BLACKLIST_KEY && tool == TOOL_CODEX {
        paths.push(cfg.codex_rules_file.clone());
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
    fn resolve_tool_paths_commands_valid_inputs_unchanged() {
        let tmp = TempDir::new().unwrap();
        let cfg = make_config(&tmp);
        let paths = resolve_tool_paths(&cfg, "commands/review.md", TOOL_CLAUDE);
        assert_eq!(paths, vec![cfg.claude_dir.join("review.md")]);

        let paths = resolve_tool_paths(&cfg, "commands/review.md", TOOL_CURSOR);
        assert_eq!(paths, vec![cfg.cursor_dir.join("review.md")]);

        let paths = resolve_tool_paths(&cfg, "commands/review.md", TOOL_OPENCODE);
        assert_eq!(paths, vec![cfg.opencode_commands_dir.join("review.md")]);
    }

    #[test]
    fn resolve_tool_paths_codex_commands_include_legacy_prompt_candidate() {
        let tmp = TempDir::new().unwrap();
        let cfg = make_config(&tmp);
        let paths = resolve_tool_paths(&cfg, "commands/review.md", TOOL_CODEX);
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&cfg.codex_dir.join("review.md")));
        assert!(paths.contains(&cfg.codex_dir.join("prompt:review.md")));
    }

    #[test]
    fn resolve_tool_paths_codex_prompt_name_does_not_duplicate_candidate() {
        let tmp = TempDir::new().unwrap();
        let cfg = make_config(&tmp);
        let paths = resolve_tool_paths(&cfg, "commands/prompt:legacy.md", TOOL_CODEX);
        assert_eq!(paths, vec![cfg.codex_dir.join("prompt:legacy.md")]);
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

        let paths = resolve_tool_paths(&cfg, "skills/plan/SKILL.md", TOOL_CURSOR);
        assert!(paths.is_empty());
    }

    #[test]
    fn resolve_tool_paths_agents() {
        let tmp = TempDir::new().unwrap();
        let cfg = make_config(&tmp);
        let paths = resolve_tool_paths(&cfg, "agents/AGENTS.md", TOOL_CODEX);
        assert_eq!(paths, vec![cfg.codex_agents_file.clone()]);

        let paths = resolve_tool_paths(&cfg, "agents/AGENTS.md", TOOL_CLAUDE);
        assert!(paths.is_empty());
    }

    #[test]
    fn resolve_tool_paths_rules() {
        let tmp = TempDir::new().unwrap();
        let cfg = make_config(&tmp);
        let paths = resolve_tool_paths(&cfg, "rules/codex/default.rules", TOOL_CODEX);
        assert_eq!(paths, vec![cfg.codex_rules_file.clone()]);

        let paths = resolve_tool_paths(&cfg, "rules/codex/default.rules", TOOL_CLAUDE);
        assert!(paths.is_empty());
    }

    #[test]
    fn resolve_tool_paths_agents_require_canonical_keys() {
        let tmp = TempDir::new().unwrap();
        let cfg = make_config(&tmp);

        let codex_paths = resolve_tool_paths(&cfg, "agents/codex/AGENTS.md", TOOL_CODEX);
        assert_eq!(codex_paths, vec![cfg.codex_agents_file.clone()]);
        let codex_mismatch = resolve_tool_paths(&cfg, "agents/codex/AGENTS.md", TOOL_OPENCODE);
        assert!(codex_mismatch.is_empty());

        let opencode_paths = resolve_tool_paths(&cfg, "agents/opencode/AGENTS.md", TOOL_OPENCODE);
        assert_eq!(opencode_paths, vec![cfg.opencode_agents_file.clone()]);
        let opencode_mismatch = resolve_tool_paths(&cfg, "agents/opencode/AGENTS.md", TOOL_CODEX);
        assert!(opencode_mismatch.is_empty());
    }

    #[test]
    fn resolve_tool_paths_rejects_noncanonical_agents_and_rules_paths() {
        let tmp = TempDir::new().unwrap();
        let cfg = make_config(&tmp);

        for relative_path in [
            "agents/",
            "agents/foo",
            "agents/codex/",
            "agents/opencode/foo.md",
            "rules/",
            "rules/foo.rules",
            "rules/codex/",
            "rules/codex/other.rules",
        ] {
            for tool in [TOOL_CODEX, TOOL_OPENCODE] {
                assert!(
                    resolve_tool_paths(&cfg, relative_path, tool).is_empty(),
                    "expected no targets for {relative_path} ({tool})",
                );
            }
        }
    }

    #[test]
    fn validate_blacklist_path_rejects_noncanonical_agents_and_rules() {
        for path in [
            "agents/",
            "agents/foo",
            "agents/codex/",
            "rules/",
            "rules/foo",
            "rules/codex/other.rules",
        ] {
            let err = validate_blacklist_path(path).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        }

        for path in [
            "agents/AGENTS.md",
            "agents/codex/AGENTS.md",
            "agents/opencode/AGENTS.md",
            "rules/codex/default.rules",
        ] {
            assert!(validate_blacklist_path(path).is_ok());
        }
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
    fn add_and_remove_blacklist_updates_config() -> io::Result<()> {
        let (_tmp, mut cfg) = setup()?;
        assert!(!cfg.is_blacklisted("commands/review.md", TOOL_CLAUDE));

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
        retroactive_delete(&cfg, "commands/nonexistent.md", &[TOOL_CLAUDE.to_string()])?;
        Ok(())
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

    #[test]
    fn retroactive_delete_removes_legacy_codex_prompt_file() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let legacy = cfg.codex_dir.join("prompt:foo.md");
        write_plain(&legacy, "legacy")?;
        assert!(legacy.exists());

        retroactive_delete(&cfg, "commands/foo.md", &[TOOL_CODEX.to_string()])?;

        assert!(!legacy.exists());
        Ok(())
    }
}
