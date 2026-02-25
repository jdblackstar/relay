use crate::config::{Config, TOOL_CLAUDE, TOOL_CODEX, TOOL_CURSOR, TOOL_OPENCODE};
use crate::sync::shared::read_markdown;
use filetime::{set_file_mtime, FileTime};
use std::fs;
use std::io;
use std::path::Path;
use tempfile::TempDir;

pub(crate) fn make_config(tmp: &TempDir) -> Config {
    Config {
        enabled_tools: vec![
            TOOL_CLAUDE.to_string(),
            TOOL_CODEX.to_string(),
            TOOL_CURSOR.to_string(),
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

pub(crate) fn ensure_tool_dirs(cfg: &Config) -> io::Result<()> {
    for dir in [
        &cfg.claude_dir,
        &cfg.claude_skills_dir,
        &cfg.cursor_dir,
        &cfg.opencode_commands_dir,
        &cfg.opencode_skills_dir,
        &cfg.codex_dir,
        &cfg.codex_skills_dir,
    ] {
        fs::create_dir_all(dir)?;
    }
    for file in [
        &cfg.opencode_agents_file,
        &cfg.codex_rules_file,
        &cfg.codex_agents_file,
    ] {
        file.parent().map(fs::create_dir_all).transpose()?;
    }
    Ok(())
}

pub(crate) fn setup() -> io::Result<(TempDir, Config)> {
    let tmp = TempDir::new()?;
    let cfg = make_config(&tmp);
    ensure_tool_dirs(&cfg)?;
    Ok((tmp, cfg))
}

pub(crate) fn set_mtime(path: &Path, secs: i64) -> io::Result<()> {
    set_file_mtime(path, FileTime::from_unix_time(secs, 0))
}

pub(crate) fn write_skill(
    dir: &Path,
    name: &str,
    contents: &str,
) -> io::Result<std::path::PathBuf> {
    let path = dir.join(name);
    fs::create_dir_all(&path)?;
    write_plain(&path.join("SKILL.md"), contents)?;
    Ok(path)
}

pub(crate) fn write_plain(path: &Path, contents: &str) -> io::Result<()> {
    path.parent().map(fs::create_dir_all).transpose()?;
    fs::write(path, contents)
}

pub(crate) fn doc(name: &str, body: &str) -> String {
    format!("---\nname: {name}\ndescription: {name} description\n---\n{body}")
}

pub(crate) fn read_body(path: &Path) -> io::Result<String> {
    read_markdown(path).map(|doc| doc.body)
}

pub(crate) fn read_frontmatter(path: &Path) -> io::Result<Option<String>> {
    read_markdown(path).map(|doc| doc.frontmatter)
}
