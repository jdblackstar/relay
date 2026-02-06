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
pub enum LogMode {
    Quiet,
    Actions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    Apply,
    Plan,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SyncStats {
    pub updated: usize,
}

impl SyncStats {
    pub fn is_empty(&self) -> bool {
        self.updated == 0
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SyncReport {
    pub commands: SyncStats,
    pub skills: SyncStats,
    pub agents: SyncStats,
    pub rules: SyncStats,
}

impl SyncReport {
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
            && self.skills.is_empty()
            && self.agents.is_empty()
            && self.rules.is_empty()
    }
}

#[cfg_attr(any(test, coverage), allow(dead_code))]
#[derive(Debug, Clone)]
pub struct SyncOutcome {
    pub report: SyncReport,
    pub history_event_id: Option<String>,
}

pub fn sync_all(cfg: &Config, log_mode: LogMode) -> io::Result<SyncReport> {
    Ok(sync_all_with_mode(cfg, log_mode, ExecutionMode::Apply, "sync")?.report)
}

pub fn sync_all_with_mode(
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
    let commands = commands::sync_commands_with_mode(cfg, log_mode, mode, &mut history)?;
    let skills = skills::sync_skills_with_mode(cfg, log_mode, mode, &mut history)?;
    let agents = agents::sync_agents_with_mode(cfg, log_mode, mode, &mut history)?;
    let rules = rules::sync_rules_with_mode(cfg, log_mode, mode, &mut history)?;
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
        history_event_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::HistoryStore;
    use crate::sync::test_support::{setup, write_plain};
    use std::fs;

    #[test]
    fn sync_all_with_mode_records_history_event() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        write_plain(&cfg.claude_dir.join("demo.md"), "hello")?;

        let outcome = sync_all_with_mode(&cfg, LogMode::Quiet, ExecutionMode::Apply, "sync")?;
        assert!(outcome.report.commands.updated > 0);
        assert!(outcome.history_event_id.is_some());

        let store = HistoryStore::from_config(&cfg)?;
        let recent = store.list_recent(5)?;
        assert_eq!(recent.len(), 1);
        assert!(fs::read_to_string(cfg.central_dir.join("demo.md"))?.contains("hello"));
        Ok(())
    }
}
