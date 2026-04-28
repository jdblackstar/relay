use crate::config::Config;
use crate::history::HistoryRecorder;
use std::io;

mod agents;
mod commands;
mod rules;
mod shared;
mod skills;

#[cfg(test)]
pub(crate) mod test_support;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LogMode {
    Quiet,
    Actions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecutionMode {
    Apply,
    Plan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyncItemKind {
    Command,
    Skill,
    Agent,
    Rule,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SyncConflict {
    pub kind: SyncItemKind,
    pub name: String,
    pub winner: &'static str,
    pub others: Vec<&'static str>,
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct SyncStats {
    pub updated: usize,
}

impl SyncStats {
    pub(crate) fn is_empty(&self) -> bool {
        self.updated == 0
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct SyncReport {
    pub commands: SyncStats,
    pub skills: SyncStats,
    pub agents: SyncStats,
    pub rules: SyncStats,
}

impl SyncReport {
    pub(crate) fn is_empty(&self) -> bool {
        self.commands.is_empty()
            && self.skills.is_empty()
            && self.agents.is_empty()
            && self.rules.is_empty()
    }
}

#[cfg_attr(any(test, coverage), allow(dead_code))]
#[derive(Debug, Clone)]
pub(crate) struct SyncOutcome {
    pub report: SyncReport,
    pub conflicts: Vec<SyncConflict>,
    pub history_event_id: Option<String>,
}

impl SyncOutcome {
    pub(crate) fn has_conflicts(&self) -> bool {
        !self.conflicts.is_empty()
    }
}

pub(crate) fn sync_all(cfg: &Config, log_mode: LogMode) -> io::Result<SyncReport> {
    Ok(sync_all_with_mode(cfg, log_mode, ExecutionMode::Apply, "sync")?.report)
}

pub(crate) fn sync_all_with_mode(
    cfg: &Config,
    log_mode: LogMode,
    mode: ExecutionMode,
    origin: &str,
) -> io::Result<SyncOutcome> {
    let mut history = if mode == ExecutionMode::Apply {
        Some(HistoryRecorder::new(cfg, origin)?)
    } else {
        None
    };
    let mut conflicts = Vec::new();
    let commands =
        commands::sync_commands_with_mode(cfg, log_mode, mode, &mut history, &mut conflicts)?;
    let skills = skills::sync_skills_with_mode(cfg, log_mode, mode, &mut history, &mut conflicts)?;
    let agents = agents::sync_agents_with_mode(cfg, log_mode, mode, &mut history, &mut conflicts)?;
    let rules = rules::sync_rules_with_mode(cfg, log_mode, mode, &mut history, &mut conflicts)?;
    let report = SyncReport {
        commands,
        skills,
        agents,
        rules,
    };
    let history_event_id = match history {
        Some(recorder) => recorder.finish()?,
        None => None,
    };
    Ok(SyncOutcome {
        report,
        conflicts,
        history_event_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::HistoryStore;
    use crate::sync::test_support::{doc, setup, write_plain, write_skill};
    use crate::{config, sync};
    use std::fs;

    #[test]
    fn sync_all_with_mode_records_history_event() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        write_plain(&cfg.claude_dir.join("demo.md"), "hello")?;

        let outcome = sync_all_with_mode(&cfg, LogMode::Quiet, ExecutionMode::Apply, "sync")?;
        assert!(outcome.report.commands.updated > 0);
        assert!(outcome.conflicts.is_empty());
        assert!(outcome.history_event_id.is_some());

        let store = HistoryStore::from_config(&cfg)?;
        let recent = store.list_recent(5)?;
        assert_eq!(recent.len(), 1);
        assert!(fs::read_to_string(cfg.central_dir.join("demo.md"))?.contains("hello"));
        Ok(())
    }

    #[test]
    fn sync_all_with_mode_collects_conflicts_across_categories() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;

        let command_claude = cfg.claude_dir.join("review.md");
        let command_cursor = cfg.cursor_dir.join("review.md");
        write_plain(&command_claude, &doc("claude", "Claude command"))?;
        write_plain(&command_cursor, &doc("cursor", "Cursor command"))?;
        sync::test_support::set_mtime(&command_claude, 100)?;
        sync::test_support::set_mtime(&command_cursor, 101)?;

        let skill_claude = write_skill(&cfg.claude_skills_dir, "plan", &doc("claude", "Old"))?;
        let skill_codex = write_skill(&cfg.codex_skills_dir, "plan", &doc("codex", "New"))?;
        sync::test_support::set_mtime(&skill_claude.join("SKILL.md"), 100)?;
        sync::test_support::set_mtime(&skill_codex.join("SKILL.md"), 101)?;

        write_plain(&cfg.codex_agents_file, &doc("codex", "Codex agent"))?;
        write_plain(
            &cfg.opencode_agents_file,
            &doc("opencode", "OpenCode agent"),
        )?;
        sync::test_support::set_mtime(&cfg.codex_agents_file, 100)?;
        sync::test_support::set_mtime(&cfg.opencode_agents_file, 101)?;

        let central_rules = cfg.central_rules_dir.join("codex/default.rules");
        write_plain(&cfg.codex_rules_file, "rule(\"codex\")")?;
        write_plain(&central_rules, "rule(\"central\")")?;
        sync::test_support::set_mtime(&cfg.codex_rules_file, 100)?;
        sync::test_support::set_mtime(&central_rules, 101)?;

        let outcome = sync_all_with_mode(&cfg, LogMode::Quiet, ExecutionMode::Plan, "sync")?;

        assert!(outcome.has_conflicts());
        assert_eq!(outcome.history_event_id, None);
        assert_eq!(outcome.conflicts.len(), 4);
        assert!(outcome.conflicts.contains(&SyncConflict {
            kind: SyncItemKind::Command,
            name: "review.md".to_string(),
            winner: config::TOOL_CURSOR,
            others: vec![config::TOOL_CLAUDE],
        }));
        assert!(outcome.conflicts.contains(&SyncConflict {
            kind: SyncItemKind::Skill,
            name: "plan".to_string(),
            winner: config::TOOL_CODEX,
            others: vec![config::TOOL_CLAUDE],
        }));
        assert!(outcome.conflicts.contains(&SyncConflict {
            kind: SyncItemKind::Agent,
            name: "AGENTS.md".to_string(),
            winner: config::TOOL_OPENCODE,
            others: vec![config::TOOL_CODEX],
        }));
        assert!(outcome.conflicts.contains(&SyncConflict {
            kind: SyncItemKind::Rule,
            name: "codex/default.rules".to_string(),
            winner: "central",
            others: vec![config::TOOL_CODEX],
        }));
        Ok(())
    }

    #[test]
    fn sync_all_with_mode_preflight_conflicts_do_not_create_outputs_or_history() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;

        write_plain(&cfg.claude_dir.join("demo.md"), "hello")?;
        let claude_skill = write_skill(&cfg.claude_skills_dir, "plan", &doc("claude", "Old"))?;
        let codex_skill = write_skill(&cfg.codex_skills_dir, "plan", &doc("codex", "New"))?;
        sync::test_support::set_mtime(&claude_skill.join("SKILL.md"), 100)?;
        sync::test_support::set_mtime(&codex_skill.join("SKILL.md"), 101)?;

        let outcome = sync_all_with_mode(&cfg, LogMode::Quiet, ExecutionMode::Plan, "sync")?;

        assert!(outcome.has_conflicts());
        assert!(outcome.report.commands.updated > 0);
        assert_eq!(outcome.history_event_id, None);
        assert!(!cfg.central_dir.exists());
        assert!(!cfg.central_dir.join("demo.md").exists());
        assert!(!cfg.central_skills_dir.exists());

        let store = HistoryStore::from_config(&cfg)?;
        assert!(store.list_recent(5)?.is_empty());
        Ok(())
    }
}
