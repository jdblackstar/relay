use crate::config::{Config, TOOL_CLAUDE, TOOL_CODEX, TOOL_CURSOR, TOOL_OPENCODE};
use std::path::PathBuf;

pub struct ToolDefinition {
    pub id: &'static str,
    pub label: &'static str,
    pub commands_dir: Option<fn(&Config) -> &PathBuf>,
    pub skills_dir: Option<fn(&Config) -> &PathBuf>,
    pub agents_file: Option<fn(&Config) -> &PathBuf>,
    pub rules_file: Option<fn(&Config) -> &PathBuf>,
}

fn claude_commands(cfg: &Config) -> &PathBuf {
    &cfg.claude_dir
}

fn claude_skills(cfg: &Config) -> &PathBuf {
    &cfg.claude_skills_dir
}

fn cursor_commands(cfg: &Config) -> &PathBuf {
    &cfg.cursor_dir
}

fn codex_commands(cfg: &Config) -> &PathBuf {
    &cfg.codex_dir
}

fn codex_skills(cfg: &Config) -> &PathBuf {
    &cfg.codex_skills_dir
}

fn codex_rules(cfg: &Config) -> &PathBuf {
    &cfg.codex_rules_file
}

fn codex_agents(cfg: &Config) -> &PathBuf {
    &cfg.codex_agents_file
}

fn opencode_commands(cfg: &Config) -> &PathBuf {
    &cfg.opencode_commands_dir
}

fn opencode_skills(cfg: &Config) -> &PathBuf {
    &cfg.opencode_skills_dir
}

fn opencode_agents(cfg: &Config) -> &PathBuf {
    &cfg.opencode_agents_file
}

pub const TOOL_DEFINITIONS: [ToolDefinition; 4] = [
    ToolDefinition {
        id: TOOL_CLAUDE,
        label: "Claude Code",
        commands_dir: Some(claude_commands),
        skills_dir: Some(claude_skills),
        agents_file: None,
        rules_file: None,
    },
    ToolDefinition {
        id: TOOL_CODEX,
        label: "Codex CLI",
        commands_dir: Some(codex_commands),
        skills_dir: Some(codex_skills),
        agents_file: Some(codex_agents),
        rules_file: Some(codex_rules),
    },
    ToolDefinition {
        id: TOOL_CURSOR,
        label: "Cursor",
        commands_dir: Some(cursor_commands),
        skills_dir: None,
        agents_file: None,
        rules_file: None,
    },
    ToolDefinition {
        id: TOOL_OPENCODE,
        label: "OpenCode",
        commands_dir: Some(opencode_commands),
        skills_dir: Some(opencode_skills),
        agents_file: Some(opencode_agents),
        rules_file: None,
    },
];

fn tool_paths<'a>(cfg: &'a Config, tool: &str) -> Option<Vec<&'a PathBuf>> {
    let definition = TOOL_DEFINITIONS.iter().find(|spec| spec.id == tool)?;
    let mut paths = Vec::new();
    if let Some(getter) = definition.commands_dir {
        paths.push(getter(cfg));
    }
    if let Some(getter) = definition.skills_dir {
        paths.push(getter(cfg));
    }
    if let Some(getter) = definition.agents_file {
        paths.push(getter(cfg));
    }
    if let Some(getter) = definition.rules_file {
        paths.push(getter(cfg));
    }
    Some(paths)
}

pub fn tool_expected_paths(cfg: &Config, tool: &str) -> Option<String> {
    let definition = TOOL_DEFINITIONS.iter().find(|spec| spec.id == tool)?;
    let paths = tool_paths(cfg, tool)?;
    let path_list = paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    if path_list.is_empty() {
        return None;
    }
    Some(format!("{} (expected at {})", definition.label, path_list))
}

pub fn tool_detected(cfg: &Config, tool: &str) -> bool {
    tool_paths(cfg, tool)
        .map(|paths| paths.iter().any(|path| path.exists()))
        .unwrap_or(false)
}
