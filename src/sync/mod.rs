use crate::config::Config;
use crate::history::HistoryRecorder;
use std::io;

mod agents;
mod codex_commands;
mod commands;
mod rules;
mod shared;
mod skills;

pub(crate) use skills::{discover_scoped_skills, ScopedSkill};

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

pub(crate) fn skill_diagnostics(cfg: &Config) -> io::Result<Vec<String>> {
    skills::diagnostics(cfg)
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
    let skill_outcome =
        skills::sync_skills_with_mode(cfg, log_mode, mode, &mut history, &mut conflicts)?;
    let commands = commands::sync_commands_with_reserved_codex_skill_names(
        cfg,
        log_mode,
        mode,
        &mut history,
        &mut conflicts,
        &skill_outcome.codex_real_skill_names,
    )?;
    let agents = agents::sync_agents_with_mode(cfg, log_mode, mode, &mut history, &mut conflicts)?;
    let rules = rules::sync_rules_with_mode(cfg, log_mode, mode, &mut history, &mut conflicts)?;
    let report = SyncReport {
        commands,
        skills: skill_outcome.stats,
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

pub(crate) fn sync_scoped_skills_with_mode(
    cfg: &Config,
    selected: &[ScopedSkill],
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
    let skill_outcome = match skills::sync_scoped_skills_with_mode(
        cfg,
        selected,
        log_mode,
        mode,
        &mut history,
        &mut conflicts,
    ) {
        Ok(outcome) => outcome,
        Err(sync_err) => {
            if let Some(recorder) = history.take() {
                if let Err(rollback_err) = recorder.rollback_pending() {
                    return Err(io::Error::new(
                        sync_err.kind(),
                        format!(
                            "scoped sync failed ({sync_err}) and failed to revert earlier writes ({rollback_err})"
                        ),
                    ));
                }
            }
            return Err(sync_err);
        }
    };
    let report = SyncReport {
        skills: skill_outcome.stats,
        ..SyncReport::default()
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
    fn skill_diagnostics_are_available_for_status() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let lines = skill_diagnostics(&cfg)?;
        assert!(lines[0].starts_with("skills: canonical="));
        Ok(())
    }

    #[test]
    fn scoped_canonical_sync_is_strict_and_following_full_sync_remains_unfiltered() -> io::Result<()>
    {
        let (_tmp, cfg) = setup()?;
        let unrelated = write_skill(
            &cfg.central_skills_dir,
            "unrelated",
            &doc("unrelated", "Unrelated"),
        )?;
        sync_all_with_mode(&cfg, LogMode::Quiet, ExecutionMode::Apply, "seed")?;
        assert!(cfg.claude_skills_dir.join("unrelated/SKILL.md").exists());
        let state_before: toml::Value =
            toml::from_str(&fs::read_to_string(cfg.skill_state_path()?)?).unwrap();
        let unrelated_state_before = state_before["skills"]["unrelated"].clone();
        fs::remove_dir_all(unrelated)?;

        write_plain(&cfg.claude_dir.join("pending.md"), "Pending command")?;
        write_plain(&cfg.codex_agents_file, "Pending agent")?;
        write_plain(&cfg.codex_rules_file, "Pending rule")?;
        let selected_path = write_skill(
            &cfg.central_skills_dir,
            "selected",
            &doc("selected", "Selected skill"),
        )?;
        let selected = discover_scoped_skills(&[selected_path])?;

        let scoped = sync_scoped_skills_with_mode(
            &cfg,
            &selected,
            LogMode::Quiet,
            ExecutionMode::Apply,
            "sync:scoped",
        )?;

        assert_eq!(scoped.report.commands.updated, 0);
        assert_eq!(scoped.report.agents.updated, 0);
        assert_eq!(scoped.report.rules.updated, 0);
        assert!(scoped.report.skills.updated > 0);
        assert!(!cfg.central_dir.join("pending.md").exists());
        assert!(!cfg.central_agents_dir.join("codex/AGENTS.md").exists());
        assert!(!cfg.central_rules_dir.join("codex/default.rules").exists());
        assert!(cfg.claude_skills_dir.join("unrelated/SKILL.md").exists());
        assert!(cfg.central_skills_dir.join("selected/SKILL.md").exists());
        assert!(cfg.claude_skills_dir.join("selected/SKILL.md").exists());
        let state_after: toml::Value =
            toml::from_str(&fs::read_to_string(cfg.skill_state_path()?)?).unwrap();
        assert_eq!(state_after["skills"]["unrelated"], unrelated_state_before);

        let full = sync_all_with_mode(&cfg, LogMode::Quiet, ExecutionMode::Apply, "sync")?;
        assert!(full.report.commands.updated > 0);
        assert!(full.report.agents.updated > 0);
        assert!(full.report.rules.updated > 0);
        assert!(!cfg.claude_skills_dir.join("unrelated/SKILL.md").exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn read_only_scoped_lifecycle_tombstones_and_removes_owned_adapters() -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let (_tmp, cfg) = setup()?;
        let source = tempfile::TempDir::new()?;
        let selected_path = write_skill(
            source.path(),
            "read-only-lifecycle",
            &doc("read-only-lifecycle", "Read only"),
        )?;
        write_plain(&selected_path.join("references/guide.md"), "guide")?;
        fs::set_permissions(
            selected_path.join("references"),
            fs::Permissions::from_mode(0o500),
        )?;
        fs::set_permissions(
            selected_path.join("SKILL.md"),
            fs::Permissions::from_mode(0o400),
        )?;
        fs::set_permissions(&selected_path, fs::Permissions::from_mode(0o500))?;
        let selected = discover_scoped_skills(std::slice::from_ref(&selected_path))?;

        let scoped = sync_scoped_skills_with_mode(
            &cfg,
            &selected,
            LogMode::Quiet,
            ExecutionMode::Apply,
            "sync:scoped",
        )?;
        assert!(scoped.history_event_id.is_some());
        crate::path_cleanup::remove_with_owner_access(
            &cfg.central_skills_dir.join("read-only-lifecycle"),
        )?;

        let full = sync_all_with_mode(&cfg, LogMode::Quiet, ExecutionMode::Apply, "sync")?;

        assert!(full.history_event_id.is_some());
        for root in [
            &cfg.central_skills_dir,
            &cfg.claude_skills_dir,
            &cfg.codex_skills_dir,
            &cfg.opencode_skills_dir,
        ] {
            assert!(!root.join("read-only-lifecycle").exists());
            if root.exists() {
                for entry in fs::read_dir(root)? {
                    let name = entry?.file_name().to_string_lossy().to_string();
                    assert!(!name.contains("read-only-lifecycle.relay.tmp"));
                }
            }
        }
        let state: toml::Value =
            toml::from_str(&fs::read_to_string(cfg.skill_state_path()?)?).unwrap();
        let entry = state["skills"]["read-only-lifecycle"]
            .as_table()
            .expect("skill state entry");
        assert_eq!(entry["tombstoned"].as_bool(), Some(true));
        assert!(!entry.contains_key("canonical_hash"));
        assert!(entry["adapter_hashes"]
            .as_table()
            .is_some_and(|hashes| hashes.is_empty()));
        let recent = HistoryStore::from_config(&cfg)?.list_recent(10)?;
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].origin, "sync:scoped");
        assert_eq!(recent[1].origin, "sync");

        crate::path_cleanup::remove_with_owner_access(&selected_path)?;
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn full_sync_rejects_non_utf8_skill_paths_before_any_write() -> io::Result<()> {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let (_tmp, cfg) = setup()?;
        let package = write_skill(
            &cfg.central_skills_dir,
            "non-utf8-full",
            &doc("non-utf8-full", "Body"),
        )?;
        for bytes in [b"invalid-\x80".to_vec(), b"invalid-\x81".to_vec()] {
            fs::write(package.join(OsString::from_vec(bytes)), "invalid")?;
        }
        write_plain(&cfg.claude_dir.join("pending.md"), "must not import")?;
        let outside = cfg.central_skills_dir.parent().unwrap().join("outside.txt");
        fs::write(&outside, "outside")?;

        let err =
            sync_all_with_mode(&cfg, LogMode::Quiet, ExecutionMode::Apply, "sync").unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("not valid UTF-8"));
        assert!(!cfg.central_dir.join("pending.md").exists());
        assert!(!cfg.claude_skills_dir.join("non-utf8-full").exists());
        assert!(!cfg.skill_state_path()?.exists());
        assert!(HistoryStore::from_config(&cfg)?.list_recent(5)?.is_empty());
        assert_eq!(fs::read_to_string(&outside)?, "outside");
        Ok(())
    }

    #[test]
    fn scoped_mixed_preflight_conflict_is_all_or_nothing() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let source = tempfile::TempDir::new()?;
        let valid = write_skill(source.path(), "valid", &doc("valid", "Valid"))?;
        let conflict = write_skill(source.path(), "conflict", &doc("conflict", "Selected"))?;
        write_skill(
            &cfg.central_skills_dir,
            "conflict",
            &doc("conflict", "Canonical"),
        )?;
        let selected = discover_scoped_skills(&[valid, conflict])?;

        let outcome = sync_scoped_skills_with_mode(
            &cfg,
            &selected,
            LogMode::Quiet,
            ExecutionMode::Apply,
            "sync:scoped",
        )?;

        assert_eq!(outcome.conflicts.len(), 1);
        assert_eq!(outcome.report.skills.updated, 0);
        assert_eq!(outcome.history_event_id, None);
        assert!(!cfg.central_skills_dir.join("valid").exists());
        for root in [
            &cfg.claude_skills_dir,
            &cfg.codex_skills_dir,
            &cfg.opencode_skills_dir,
        ] {
            assert!(!root.join("valid").exists());
            assert!(!root.join("conflict").exists());
        }
        assert!(
            fs::read_to_string(cfg.central_skills_dir.join("conflict/SKILL.md"))?
                .contains("Canonical")
        );
        assert!(!cfg.skill_state_path()?.exists());
        assert!(HistoryStore::from_config(&cfg)?.list_recent(5)?.is_empty());
        Ok(())
    }

    #[test]
    fn scoped_history_contains_only_selected_writes_and_rolls_them_back() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let unrelated = write_skill(
            &cfg.central_skills_dir,
            "unrelated",
            &doc("unrelated", "Unrelated"),
        )?;
        let source = tempfile::TempDir::new()?;
        let selected_path = write_skill(source.path(), "selected", &doc("selected", "Selected"))?;
        let selected = discover_scoped_skills(&[selected_path])?;

        let outcome = sync_scoped_skills_with_mode(
            &cfg,
            &selected,
            LogMode::Quiet,
            ExecutionMode::Apply,
            "sync:scoped",
        )?;
        let event_id = outcome.history_event_id.expect("scoped history event");
        let store = HistoryStore::from_config(&cfg)?;
        let recent = store.list_recent(5)?;
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].origin, "sync:scoped");
        assert_eq!(recent[0].writes, 5);

        let rollback = store.rollback(&event_id, false)?;
        assert_eq!(rollback.restored, 5);
        assert!(unrelated.join("SKILL.md").exists());
        for root in [
            &cfg.central_skills_dir,
            &cfg.claude_skills_dir,
            &cfg.codex_skills_dir,
            &cfg.opencode_skills_dir,
        ] {
            assert!(!root.join("selected").exists());
        }
        assert!(!cfg.skill_state_path()?.exists());
        Ok(())
    }

    #[test]
    fn scoped_mid_assembly_failures_clean_temps_and_preserve_transaction_state() -> io::Result<()> {
        let _env = crate::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        for phase in ["after-copy", "after-frontmatter", "before-snapshot"] {
            let (_tmp, cfg) = setup()?;
            let canonical = write_skill(
                &cfg.central_skills_dir,
                "selected",
                &doc("selected", "Original"),
            )?;
            let baseline = discover_scoped_skills(std::slice::from_ref(&canonical))?;
            sync_scoped_skills_with_mode(
                &cfg,
                &baseline,
                LogMode::Quiet,
                ExecutionMode::Apply,
                "sync:scoped",
            )?;
            write_plain(
                &canonical.join("SKILL.md"),
                &doc("selected", "Updated source"),
            )?;
            let selected = discover_scoped_skills(std::slice::from_ref(&canonical))?;
            let state_before = fs::read(cfg.skill_state_path()?)?;
            let history_before: Vec<_> = HistoryStore::from_config(&cfg)?
                .list_recent(10)?
                .into_iter()
                .map(|event| event.id)
                .collect();
            let adapter_before = [
                &cfg.claude_skills_dir,
                &cfg.codex_skills_dir,
                &cfg.opencode_skills_dir,
            ]
            .into_iter()
            .map(|root| {
                let path = root.join("selected/SKILL.md");
                fs::read(&path).map(|contents| (path, contents))
            })
            .collect::<io::Result<Vec<_>>>()?;
            skills::set_skill_assembly_failure_for_test(Some(phase));

            let result = sync_scoped_skills_with_mode(
                &cfg,
                &selected,
                LogMode::Quiet,
                ExecutionMode::Apply,
                "sync:scoped",
            );

            skills::set_skill_assembly_failure_for_test(None);
            let err = result.unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "phase {phase}");
            assert!(
                err.to_string()
                    .contains(&format!("injected skill assembly failure {phase}")),
                "phase {phase}: {err}"
            );
            for (path, contents) in adapter_before {
                assert_eq!(fs::read(path)?, contents, "phase {phase}");
            }
            assert_eq!(
                fs::read(cfg.skill_state_path()?)?,
                state_before,
                "phase {phase}"
            );
            assert_eq!(
                HistoryStore::from_config(&cfg)?
                    .list_recent(10)?
                    .into_iter()
                    .map(|event| event.id)
                    .collect::<Vec<_>>(),
                history_before,
                "phase {phase}"
            );
            for root in [
                &cfg.central_skills_dir,
                &cfg.claude_skills_dir,
                &cfg.codex_skills_dir,
                &cfg.opencode_skills_dir,
            ] {
                assert!(fs::read_dir(root)?.all(|entry| {
                    entry
                        .map(|entry| !entry.file_name().to_string_lossy().contains(".relay.tmp."))
                        .unwrap_or(false)
                }));
            }
        }
        Ok(())
    }

    #[test]
    fn scoped_late_adapter_failure_reverts_earlier_publications() -> io::Result<()> {
        let _env = crate::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_tmp, cfg) = setup()?;
        let source = tempfile::TempDir::new()?;
        let selected_path = write_skill(source.path(), "selected", &doc("selected", "Selected"))?;
        let selected = discover_scoped_skills(&[selected_path])?;
        let fault_target = cfg.codex_skills_dir.join("selected");
        std::env::set_var("RELAY_TEST_FAIL_SKILL_TARGET", &fault_target);

        let result = sync_scoped_skills_with_mode(
            &cfg,
            &selected,
            LogMode::Quiet,
            ExecutionMode::Apply,
            "sync:scoped",
        );

        std::env::remove_var("RELAY_TEST_FAIL_SKILL_TARGET");
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(err
            .to_string()
            .contains("injected late skill target failure"));
        for root in [
            &cfg.central_skills_dir,
            &cfg.claude_skills_dir,
            &cfg.codex_skills_dir,
            &cfg.opencode_skills_dir,
        ] {
            assert!(!root.join("selected").exists());
        }
        assert!(!cfg.skill_state_path()?.exists());
        assert!(HistoryStore::from_config(&cfg)?.list_recent(5)?.is_empty());
        Ok(())
    }

    #[test]
    fn scoped_late_second_package_failure_restores_every_package_state_and_history(
    ) -> io::Result<()> {
        let _env = crate::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_tmp, cfg) = setup()?;
        let alpha = write_skill(
            &cfg.central_skills_dir,
            "alpha",
            &doc("alpha", "Alpha original"),
        )?;
        let omega = write_skill(
            &cfg.central_skills_dir,
            "omega",
            &doc("omega", "Omega original"),
        )?;
        let baseline = discover_scoped_skills(&[alpha.clone(), omega.clone()])?;
        sync_scoped_skills_with_mode(
            &cfg,
            &baseline,
            LogMode::Quiet,
            ExecutionMode::Apply,
            "sync:scoped",
        )?;
        let state_before = fs::read(cfg.skill_state_path()?)?;
        let history_before: Vec<_> = HistoryStore::from_config(&cfg)?
            .list_recent(10)?
            .into_iter()
            .map(|event| event.id)
            .collect();
        let mut adapter_before = Vec::new();
        for root in [
            &cfg.claude_skills_dir,
            &cfg.codex_skills_dir,
            &cfg.opencode_skills_dir,
        ] {
            for name in ["alpha", "omega"] {
                let path = root.join(name).join("SKILL.md");
                adapter_before.push((path.clone(), fs::read(path)?));
            }
        }

        write_plain(&alpha.join("SKILL.md"), &doc("alpha", "Alpha updated"))?;
        write_plain(&omega.join("SKILL.md"), &doc("omega", "Omega updated"))?;
        let selected = discover_scoped_skills(&[alpha, omega])?;
        let fault_target = cfg.codex_skills_dir.join("omega");
        std::env::set_var("RELAY_TEST_FAIL_SKILL_TARGET", &fault_target);

        let result = sync_scoped_skills_with_mode(
            &cfg,
            &selected,
            LogMode::Quiet,
            ExecutionMode::Apply,
            "sync:scoped",
        );

        std::env::remove_var("RELAY_TEST_FAIL_SKILL_TARGET");
        let err = result.unwrap_err();
        assert!(err
            .to_string()
            .contains("injected late skill target failure"));
        for (path, expected) in adapter_before {
            assert_eq!(fs::read(path)?, expected);
        }
        assert_eq!(fs::read(cfg.skill_state_path()?)?, state_before);
        assert_eq!(
            HistoryStore::from_config(&cfg)?
                .list_recent(10)?
                .into_iter()
                .map(|event| event.id)
                .collect::<Vec<_>>(),
            history_before
        );
        for root in [
            &cfg.central_skills_dir,
            &cfg.claude_skills_dir,
            &cfg.codex_skills_dir,
            &cfg.opencode_skills_dir,
        ] {
            assert!(fs::read_dir(root)?.all(|entry| {
                entry
                    .map(|entry| !entry.file_name().to_string_lossy().contains(".relay.tmp."))
                    .unwrap_or(false)
            }));
        }
        Ok(())
    }

    #[test]
    fn scoped_late_failure_reports_partial_rollback_and_preserves_recovery_snapshot(
    ) -> io::Result<()> {
        let _env = crate::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_tmp, cfg) = setup()?;
        let canonical = write_skill(
            &cfg.central_skills_dir,
            "selected",
            &doc("selected", "Canonical"),
        )?;
        let selected = discover_scoped_skills(std::slice::from_ref(&canonical))?;
        sync_scoped_skills_with_mode(
            &cfg,
            &selected,
            LogMode::Quiet,
            ExecutionMode::Apply,
            "sync:scoped",
        )?;
        let state_before = fs::read(cfg.skill_state_path()?)?;
        let damaged_rollback_target = cfg.claude_skills_dir.join("selected");
        let late_failure_target = cfg.codex_skills_dir.join("selected");
        write_plain(
            &damaged_rollback_target.join("SKILL.md"),
            &doc("selected", "Claude before"),
        )?;
        write_plain(
            &late_failure_target.join("SKILL.md"),
            &doc("selected", "Codex before"),
        )?;
        std::env::set_var(
            "RELAY_TEST_CORRUPT_BEFORE_SNAPSHOT",
            &damaged_rollback_target,
        );
        std::env::set_var("RELAY_TEST_FAIL_SKILL_TARGET", &late_failure_target);

        let result = sync_scoped_skills_with_mode(
            &cfg,
            &selected,
            LogMode::Quiet,
            ExecutionMode::Apply,
            "sync:scoped",
        );

        std::env::remove_var("RELAY_TEST_CORRUPT_BEFORE_SNAPSHOT");
        std::env::remove_var("RELAY_TEST_FAIL_SKILL_TARGET");
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        let message = err.to_string();
        assert!(message.contains("injected late skill target failure"));
        assert!(message.contains("failed to revert earlier writes"));
        assert!(message.contains("restored 0 of 1 pending writes"));
        assert!(message.contains("recovery snapshots were preserved"));
        assert!(message.contains("history/blobs"));
        assert!(
            fs::read_to_string(damaged_rollback_target.join("SKILL.md"))?.contains("Canonical")
        );
        assert!(fs::read_to_string(late_failure_target.join("SKILL.md"))?.contains("Codex before"));
        assert_eq!(fs::read(cfg.skill_state_path()?)?, state_before);
        assert_eq!(HistoryStore::from_config(&cfg)?.list_recent(5)?.len(), 1);
        Ok(())
    }

    #[test]
    fn malformed_scoped_skill_state_is_write_free_in_apply_and_plan() -> io::Result<()> {
        for mode in [ExecutionMode::Apply, ExecutionMode::Plan] {
            let (_tmp, cfg) = setup()?;
            let canonical = write_skill(
                &cfg.central_skills_dir,
                "baseline",
                &doc("baseline", "Canonical before"),
            )?;
            let adapter = write_skill(
                &cfg.claude_skills_dir,
                "baseline",
                &doc("baseline", "Adapter before"),
            )?;
            let state_path = cfg.skill_state_path()?;
            write_plain(&state_path, "[skills\ninvalid = true\n")?;
            let history_marker = cfg
                .central_dir
                .parent()
                .expect("central dir should have a parent")
                .join("history/existing.marker");
            write_plain(&history_marker, "history before")?;
            let source = tempfile::TempDir::new()?;
            let incoming = write_skill(source.path(), "incoming", &doc("incoming", "Incoming"))?;
            let selected = discover_scoped_skills(&[incoming])?;
            let canonical_before = fs::read(canonical.join("SKILL.md"))?;
            let adapter_before = fs::read(adapter.join("SKILL.md"))?;
            let state_before = fs::read(&state_path)?;
            let history_before = fs::read(&history_marker)?;

            let err =
                sync_scoped_skills_with_mode(&cfg, &selected, LogMode::Quiet, mode, "sync:scoped")
                    .unwrap_err();

            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
            assert!(err.to_string().contains("invalid skill state"));
            assert_eq!(fs::read(canonical.join("SKILL.md"))?, canonical_before);
            assert_eq!(fs::read(adapter.join("SKILL.md"))?, adapter_before);
            assert_eq!(fs::read(&state_path)?, state_before);
            assert_eq!(fs::read(&history_marker)?, history_before);
            for root in [
                &cfg.central_skills_dir,
                &cfg.claude_skills_dir,
                &cfg.codex_skills_dir,
                &cfg.opencode_skills_dir,
            ] {
                assert!(!root.join("incoming").exists());
            }
        }
        Ok(())
    }

    #[test]
    fn repeated_identical_scoped_apply_is_byte_and_history_idempotent() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let canonical = write_skill(
            &cfg.central_skills_dir,
            "selected",
            &doc("selected", "Selected"),
        )?;
        write_plain(&canonical.join("assets/value.txt"), "same")?;
        let selected = discover_scoped_skills(std::slice::from_ref(&canonical))?;
        let first = sync_scoped_skills_with_mode(
            &cfg,
            &selected,
            LogMode::Quiet,
            ExecutionMode::Apply,
            "sync:scoped",
        )?;
        assert!(first.report.skills.updated > 0);

        let state_path = cfg.skill_state_path()?;
        let state_before = fs::read(&state_path)?;
        sync::test_support::set_mtime(&state_path, 100)?;
        let state_mtime = fs::metadata(&state_path)?.modified()?;
        let history_before: Vec<_> = HistoryStore::from_config(&cfg)?
            .list_recent(10)?
            .into_iter()
            .map(|event| event.id)
            .collect();
        let mut package_mtimes = Vec::new();
        for root in [
            &cfg.central_skills_dir,
            &cfg.claude_skills_dir,
            &cfg.codex_skills_dir,
            &cfg.opencode_skills_dir,
        ] {
            let skill_file = root.join("selected/SKILL.md");
            sync::test_support::set_mtime(&skill_file, 100)?;
            package_mtimes.push((skill_file.clone(), fs::metadata(skill_file)?.modified()?));
        }

        let second = sync_scoped_skills_with_mode(
            &cfg,
            &selected,
            LogMode::Quiet,
            ExecutionMode::Apply,
            "sync:scoped",
        )?;

        assert_eq!(second.report.skills.updated, 0);
        assert_eq!(second.history_event_id, None);
        assert_eq!(fs::read(&state_path)?, state_before);
        assert_eq!(fs::metadata(&state_path)?.modified()?, state_mtime);
        assert_eq!(
            HistoryStore::from_config(&cfg)?
                .list_recent(10)?
                .into_iter()
                .map(|event| event.id)
                .collect::<Vec<_>>(),
            history_before
        );
        for (path, modified) in package_mtimes {
            assert_eq!(fs::metadata(path)?.modified()?, modified);
        }
        for root in [
            &cfg.central_skills_dir,
            &cfg.claude_skills_dir,
            &cfg.codex_skills_dir,
            &cfg.opencode_skills_dir,
        ] {
            assert!(fs::read_dir(root)?.all(|entry| {
                entry
                    .map(|entry| !entry.file_name().to_string_lossy().contains(".relay.tmp."))
                    .unwrap_or(false)
            }));
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn scoped_late_adapter_failure_exactly_restores_replaced_package() -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let _env = crate::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_tmp, cfg) = setup()?;
        let canonical = write_skill(
            &cfg.central_skills_dir,
            "selected",
            &doc("selected", "Canonical"),
        )?;
        let selected = discover_scoped_skills(std::slice::from_ref(&canonical))?;
        sync_scoped_skills_with_mode(
            &cfg,
            &selected,
            LogMode::Quiet,
            ExecutionMode::Apply,
            "sync:scoped",
        )?;

        let replaced = cfg.claude_skills_dir.join("selected");
        fs::remove_dir_all(&replaced)?;
        write_skill(
            &cfg.claude_skills_dir,
            "selected",
            &doc("selected", "Previous adapter"),
        )?;
        write_plain(&replaced.join(".hidden"), "preserve hidden")?;
        fs::create_dir_all(replaced.join("empty/nested"))?;
        let script = replaced.join("scripts/run.sh");
        write_plain(&script, "#!/bin/sh\necho previous\n")?;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755))?;

        let fault_target = cfg.codex_skills_dir.join("selected");
        write_plain(
            &fault_target.join("SKILL.md"),
            &doc("selected", "Diverged adapter"),
        )?;
        let fault_before = fs::read(fault_target.join("SKILL.md"))?;
        let state_before = fs::read(cfg.skill_state_path()?)?;
        std::env::set_var("RELAY_TEST_FAIL_SKILL_TARGET", &fault_target);

        let result = sync_scoped_skills_with_mode(
            &cfg,
            &selected,
            LogMode::Quiet,
            ExecutionMode::Apply,
            "sync:scoped",
        );

        std::env::remove_var("RELAY_TEST_FAIL_SKILL_TARGET");
        let err = result.unwrap_err();
        assert!(err
            .to_string()
            .contains("injected late skill target failure"));
        assert!(fs::read_to_string(replaced.join("SKILL.md"))?.contains("Previous adapter"));
        assert_eq!(
            fs::read_to_string(replaced.join(".hidden"))?,
            "preserve hidden"
        );
        assert!(replaced.join("empty/nested").is_dir());
        assert_eq!(fs::read_to_string(&script)?, "#!/bin/sh\necho previous\n");
        assert_eq!(fs::metadata(&script)?.permissions().mode() & 0o777, 0o755);
        assert_eq!(fs::read(fault_target.join("SKILL.md"))?, fault_before);
        assert_eq!(fs::read(cfg.skill_state_path()?)?, state_before);
        assert_eq!(HistoryStore::from_config(&cfg)?.list_recent(5)?.len(), 1);
        Ok(())
    }

    #[test]
    fn scoped_state_save_failure_reverts_publications_and_preserves_prior_state() -> io::Result<()>
    {
        let _env = crate::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_tmp, cfg) = setup()?;
        let baseline = write_skill(
            &cfg.central_skills_dir,
            "baseline",
            &doc("baseline", "Baseline"),
        )?;
        let baseline_selected = discover_scoped_skills(&[baseline])?;
        sync_scoped_skills_with_mode(
            &cfg,
            &baseline_selected,
            LogMode::Quiet,
            ExecutionMode::Apply,
            "sync:scoped",
        )?;
        let state_before = fs::read(cfg.skill_state_path()?)?;
        let source = tempfile::TempDir::new()?;
        let added = write_skill(source.path(), "added", &doc("added", "Added"))?;
        let selected = discover_scoped_skills(&[added])?;
        std::env::set_var("RELAY_TEST_FAIL_SAVE_SKILL_STATE", cfg.skill_state_path()?);

        let result = sync_scoped_skills_with_mode(
            &cfg,
            &selected,
            LogMode::Quiet,
            ExecutionMode::Apply,
            "sync:scoped",
        );

        std::env::remove_var("RELAY_TEST_FAIL_SAVE_SKILL_STATE");
        let err = result.unwrap_err();
        assert!(err
            .to_string()
            .contains("injected skill state save failure"));
        for root in [
            &cfg.central_skills_dir,
            &cfg.claude_skills_dir,
            &cfg.codex_skills_dir,
            &cfg.opencode_skills_dir,
        ] {
            assert!(!root.join("added").exists());
            assert!(root.join("baseline/SKILL.md").exists());
        }
        assert_eq!(fs::read(cfg.skill_state_path()?)?, state_before);
        assert_eq!(HistoryStore::from_config(&cfg)?.list_recent(5)?.len(), 1);
        Ok(())
    }

    #[test]
    fn scoped_history_append_failure_reverts_packages_and_state() -> io::Result<()> {
        let _env = crate::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_tmp, cfg) = setup()?;
        let baseline = write_skill(
            &cfg.central_skills_dir,
            "baseline",
            &doc("baseline", "Baseline"),
        )?;
        let baseline_selected = discover_scoped_skills(&[baseline])?;
        sync_scoped_skills_with_mode(
            &cfg,
            &baseline_selected,
            LogMode::Quiet,
            ExecutionMode::Apply,
            "sync:scoped",
        )?;
        let state_before = fs::read(cfg.skill_state_path()?)?;
        let source = tempfile::TempDir::new()?;
        let added = write_skill(source.path(), "added", &doc("added", "Added"))?;
        let selected = discover_scoped_skills(&[added])?;
        let history_root = cfg
            .central_dir
            .parent()
            .expect("central dir should have a parent")
            .join("history");
        std::env::set_var("RELAY_TEST_FAIL_HISTORY_APPEND", history_root);

        let result = sync_scoped_skills_with_mode(
            &cfg,
            &selected,
            LogMode::Quiet,
            ExecutionMode::Apply,
            "sync:scoped",
        );

        std::env::remove_var("RELAY_TEST_FAIL_HISTORY_APPEND");
        let err = result.unwrap_err();
        assert!(err
            .to_string()
            .contains("failed to append history event; reverted recorded writes"));
        for root in [
            &cfg.central_skills_dir,
            &cfg.claude_skills_dir,
            &cfg.codex_skills_dir,
            &cfg.opencode_skills_dir,
        ] {
            assert!(!root.join("added").exists());
            assert!(root.join("baseline/SKILL.md").exists());
        }
        assert_eq!(fs::read(cfg.skill_state_path()?)?, state_before);
        assert_eq!(HistoryStore::from_config(&cfg)?.list_recent(5)?.len(), 1);
        Ok(())
    }

    #[test]
    fn full_sync_history_append_failure_reverts_every_category_and_skill_state() -> io::Result<()> {
        let _env = crate::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_tmp, cfg) = setup()?;
        let command = cfg.central_dir.join("demo.md");
        let skill = cfg.central_skills_dir.join("demo-skill");
        let agent = cfg.central_agents_dir.join("codex/AGENTS.md");
        let rule = cfg.central_rules_dir.join("codex/default.rules");
        write_plain(&command, &doc("demo", "Command source"))?;
        write_skill(
            &cfg.central_skills_dir,
            "demo-skill",
            &doc("demo-skill", "Skill source"),
        )?;
        write_plain(&agent, &doc("agent", "Agent source"))?;
        write_plain(&rule, "allow_rule")?;
        let command_before = fs::read(&command)?;
        let skill_before = fs::read(skill.join("SKILL.md"))?;
        let agent_before = fs::read(&agent)?;
        let rule_before = fs::read(&rule)?;
        let history_root = cfg
            .central_dir
            .parent()
            .expect("central dir should have a parent")
            .join("history");
        std::env::set_var("RELAY_TEST_FAIL_HISTORY_APPEND", history_root);

        let result = sync_all_with_mode(&cfg, LogMode::Quiet, ExecutionMode::Apply, "sync");

        std::env::remove_var("RELAY_TEST_FAIL_HISTORY_APPEND");
        let err = result.unwrap_err();
        assert!(err
            .to_string()
            .contains("failed to append history event; reverted recorded writes"));
        assert_eq!(fs::read(&command)?, command_before);
        assert_eq!(fs::read(skill.join("SKILL.md"))?, skill_before);
        assert_eq!(fs::read(&agent)?, agent_before);
        assert_eq!(fs::read(&rule)?, rule_before);
        for path in [
            cfg.claude_dir.join("demo.md"),
            cfg.cursor_dir.join("demo.md"),
            cfg.opencode_commands_dir.join("demo.md"),
            cfg.codex_skills_dir.join("demo/SKILL.md"),
            cfg.claude_skills_dir.join("demo-skill/SKILL.md"),
            cfg.codex_skills_dir.join("demo-skill/SKILL.md"),
            cfg.opencode_skills_dir.join("demo-skill/SKILL.md"),
            cfg.codex_agents_file.clone(),
            cfg.opencode_agents_file.clone(),
            cfg.central_agents_dir.join("opencode/AGENTS.md"),
            cfg.codex_rules_file.clone(),
        ] {
            assert!(!path.exists(), "rollback left {}", path.display());
        }
        assert!(!cfg.skill_state_path()?.exists());
        assert!(HistoryStore::from_config(&cfg)?.list_recent(5)?.is_empty());
        Ok(())
    }

    #[test]
    fn sync_all_preserves_real_codex_skill_when_command_name_collides() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        write_plain(&cfg.central_dir.join("map.md"), "Map command body.")?;
        write_skill(
            &cfg.central_skills_dir,
            "map",
            &doc("central", "Central skill body."),
        )?;
        let generated = write_skill(
            &cfg.codex_skills_dir,
            "map",
            &doc("generated", "Map command body."),
        )?;
        write_plain(
            &generated.join(crate::markers::RELAY_COMMAND_SKILL_MARKER),
            "generated by relay from commands\n",
        )?;

        let outcome = sync_all_with_mode(&cfg, LogMode::Quiet, ExecutionMode::Apply, "sync")?;

        assert!(outcome.report.skills.updated > 0);
        let skill = fs::read_to_string(cfg.codex_skills_dir.join("map/SKILL.md"))?;
        assert!(skill.contains("Central skill body."));
        assert!(!skill.contains("Map command body."));
        assert!(!cfg
            .codex_skills_dir
            .join(format!(
                "map/{}",
                crate::markers::RELAY_COMMAND_SKILL_MARKER
            ))
            .exists());
        Ok(())
    }

    #[test]
    fn sync_all_preserves_real_codex_skill_when_codex_skills_dir_is_missing() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        fs::remove_dir_all(&cfg.codex_skills_dir)?;
        write_plain(&cfg.central_dir.join("map.md"), "Map command body.")?;
        write_skill(
            &cfg.central_skills_dir,
            "map",
            &doc("central", "Central skill body."),
        )?;

        let outcome = sync_all_with_mode(&cfg, LogMode::Quiet, ExecutionMode::Apply, "sync")?;

        assert!(outcome.report.skills.updated > 0);
        let skill = fs::read_to_string(cfg.codex_skills_dir.join("map/SKILL.md"))?;
        assert!(skill.contains("Central skill body."));
        assert!(!skill.contains("Map command body."));
        assert!(!cfg
            .codex_skills_dir
            .join(format!(
                "map/{}",
                crate::markers::RELAY_COMMAND_SKILL_MARKER
            ))
            .exists());
        Ok(())
    }

    #[test]
    fn sync_plan_projects_imported_skill_names_before_command_wrappers() -> io::Result<()> {
        fn setup_collision() -> io::Result<(tempfile::TempDir, Config)> {
            let (tmp, cfg) = setup()?;
            write_plain(&cfg.central_dir.join("review.md"), "Review command body.")?;
            write_skill(
                &cfg.claude_skills_dir,
                "review",
                &doc("review", "Real skill body."),
            )?;
            Ok((tmp, cfg))
        }

        let (_plan_tmp, plan_cfg) = setup_collision()?;
        let plan = sync_all_with_mode(&plan_cfg, LogMode::Quiet, ExecutionMode::Plan, "sync")?;
        assert!(!plan_cfg.central_skills_dir.join("review").exists());

        let (_apply_tmp, apply_cfg) = setup_collision()?;
        let apply = sync_all_with_mode(&apply_cfg, LogMode::Quiet, ExecutionMode::Apply, "sync")?;

        assert_eq!(plan.report.skills.updated, 3);
        assert_eq!(apply.report.skills.updated, 3);
        assert_eq!(plan.report.commands.updated, 3);
        assert_eq!(apply.report.commands.updated, 3);
        assert!(
            fs::read_to_string(apply_cfg.central_skills_dir.join("review/SKILL.md"))?
                .contains("Real skill body.")
        );
        assert!(
            fs::read_to_string(apply_cfg.codex_skills_dir.join("review/SKILL.md"))?
                .contains("Real skill body.")
        );
        assert!(!apply_cfg
            .codex_skills_dir
            .join(format!(
                "review/{}",
                crate::markers::RELAY_COMMAND_SKILL_MARKER
            ))
            .exists());
        Ok(())
    }

    #[test]
    fn sync_all_does_not_reimport_codex_command_skill_wrappers() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        write_plain(&cfg.central_dir.join("map.md"), "Map the repository.")?;

        let outcome = sync_all_with_mode(&cfg, LogMode::Quiet, ExecutionMode::Apply, "sync")?;

        assert!(outcome.report.commands.updated > 0);
        assert_eq!(outcome.report.skills.updated, 0);
        assert!(cfg.codex_skills_dir.join("map/SKILL.md").exists());
        assert!(!cfg.central_skills_dir.join("map/SKILL.md").exists());
        assert!(!cfg.claude_skills_dir.join("map/SKILL.md").exists());
        assert!(!cfg.opencode_skills_dir.join("map/SKILL.md").exists());
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
