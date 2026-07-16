use super::shared::{
    collect_names, conflict_for_variants, list_files, list_if, log_action, read_markdown_variant,
    select_markdown_winner, update_markdown_target, MarkdownVariant, TOOL_CENTRAL,
};
use super::{ExecutionMode, LogMode, SyncConflict, SyncItemKind, SyncStats};
use crate::config::{Config, TOOL_CLAUDE, TOOL_CODEX, TOOL_CURSOR, TOOL_OPENCODE};
use crate::history::HistoryRecorder;
use std::collections::{HashMap, HashSet};
use std::io;

#[cfg(any(test, coverage))]
pub(crate) fn sync_commands(cfg: &Config, log_mode: LogMode) -> io::Result<SyncStats> {
    let mut history = None;
    let mut conflicts = Vec::new();
    sync_commands_with_mode(
        cfg,
        log_mode,
        ExecutionMode::Apply,
        &mut history,
        &mut conflicts,
    )
}

#[cfg(any(test, coverage))]
pub(crate) fn sync_commands_with_mode(
    cfg: &Config,
    log_mode: LogMode,
    mode: ExecutionMode,
    history: &mut Option<HistoryRecorder>,
    conflicts: &mut Vec<SyncConflict>,
) -> io::Result<SyncStats> {
    sync_commands_with_reserved_codex_skill_names(
        cfg,
        log_mode,
        mode,
        history,
        conflicts,
        &HashSet::new(),
    )
}

pub(crate) fn sync_commands_with_reserved_codex_skill_names(
    cfg: &Config,
    log_mode: LogMode,
    mode: ExecutionMode,
    history: &mut Option<HistoryRecorder>,
    conflicts: &mut Vec<SyncConflict>,
    reserved_codex_skill_names: &HashSet<String>,
) -> io::Result<SyncStats> {
    let mut stats = SyncStats::default();

    let claude_enabled = cfg.tool_enabled(TOOL_CLAUDE) && cfg.claude_dir.exists();
    let cursor_enabled = cfg.tool_enabled(TOOL_CURSOR) && cfg.cursor_dir.exists();
    let opencode_enabled = cfg.tool_enabled(TOOL_OPENCODE)
        && cfg
            .opencode_commands_dir
            .parent()
            .is_some_and(|parent| parent.exists());
    let opencode_read_enabled = opencode_enabled && cfg.opencode_commands_dir.exists();
    let codex_skills_enabled = super::skills::codex_skills_target_enabled(cfg);

    let claude = list_if(claude_enabled, &cfg.claude_dir, list_files)?;
    let cursor = list_if(cursor_enabled, &cfg.cursor_dir, list_files)?;
    let opencode = list_if(
        opencode_read_enabled,
        &cfg.opencode_commands_dir,
        list_files,
    )?;
    let central = if cfg.central_dir.exists() {
        list_files(&cfg.central_dir)?
    } else {
        HashMap::new()
    };

    let names = collect_names(&[&claude, &cursor, &opencode, &central]);
    for name in &names {
        let blacklist_key = format!("commands/{name}");
        let mut variants: Vec<MarkdownVariant> = Vec::new();
        for (tool, map) in [
            (TOOL_CENTRAL, &central),
            (TOOL_CLAUDE, &claude),
            (TOOL_CURSOR, &cursor),
            (TOOL_OPENCODE, &opencode),
        ] {
            if let Some(path) = map.get(name) {
                variants.push(read_markdown_variant(tool, path)?);
            }
        }
        let winner = select_markdown_winner(&variants);
        if let Some(conflict) = conflict_for_variants(
            name,
            SyncItemKind::Command,
            &variants,
            winner.tool,
            winner.doc.body_hash,
        ) {
            conflicts.push(conflict);
            log_action(
                log_mode,
                &format!(
                    "warning: commands '{name}' edited in multiple tools; last-write-wins chose {}",
                    winner.tool
                ),
            );
        }
        let source = &winner.doc;

        for (tool, enabled, base_dir) in [
            (TOOL_CENTRAL, true, &cfg.central_dir),
            (TOOL_CLAUDE, claude_enabled, &cfg.claude_dir),
            (TOOL_CURSOR, cursor_enabled, &cfg.cursor_dir),
            (TOOL_OPENCODE, opencode_enabled, &cfg.opencode_commands_dir),
        ] {
            if !enabled {
                continue;
            }
            if tool != TOOL_CENTRAL && cfg.is_blacklisted(&blacklist_key, tool) {
                continue;
            }
            let target_path = base_dir.join(name);
            let existing = variants
                .iter()
                .find(|variant| {
                    variant.tool == tool && (tool != TOOL_CODEX || variant.path == target_path)
                })
                .map(|variant| &variant.doc);
            let label = format!("commands: {}", target_path.display());
            let updated = update_markdown_target(
                source,
                existing,
                &target_path,
                true,
                log_mode,
                mode,
                history,
                &label,
            )?;
            stats.updated += usize::from(updated);
        }

        let codex_skill_allowed = super::codex_commands::command_skill_name(name)
            .map(|skill_name| !cfg.is_blacklisted(&format!("skills/{skill_name}"), TOOL_CODEX))
            .unwrap_or(false);
        if codex_skills_enabled
            && codex_skill_allowed
            && !cfg.is_blacklisted(&blacklist_key, TOOL_CODEX)
        {
            let updated = super::codex_commands::sync_codex_command_skill_wrapper(
                source,
                &cfg.codex_skills_dir,
                name,
                log_mode,
                mode,
                history,
                reserved_codex_skill_names,
            )?;
            stats.updated += usize::from(updated);
        }
    }

    if codex_skills_enabled {
        stats.updated += super::codex_commands::prune_stale_codex_command_skill_wrappers(
            &cfg.codex_skills_dir,
            &names,
            log_mode,
            mode,
            history,
        )?;
    }

    Ok(stats)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::test_support::{doc, read_body, read_frontmatter, setup, write_plain};
    use std::collections::HashSet;
    use std::fs;

    #[test]
    fn sync_commands_last_write_wins_and_syncs_required_frontmatter() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;

        let claude = cfg.claude_dir.join("review.md");
        let cursor = cfg.cursor_dir.join("review.md");

        write_plain(&claude, &doc("claude", "Claude body"))?;
        write_plain(&cursor, &doc("cursor", "Cursor body"))?;

        crate::sync::test_support::set_mtime(&cursor, 2_000_000_000)?;

        sync_commands(&cfg, LogMode::Quiet)?;

        assert_eq!(read_body(&claude)?, "Cursor body");
        assert_eq!(read_body(&cursor)?, "Cursor body");

        let claude_frontmatter = read_frontmatter(&claude)?;
        assert!(claude_frontmatter
            .unwrap_or_default()
            .contains("name: cursor"));

        let central = cfg.central_dir.join("review.md");
        assert_eq!(read_body(&central)?, "Cursor body");
        assert!(read_frontmatter(&central)?
            .unwrap_or_default()
            .contains("name: cursor"));
        assert_eq!(
            read_body(&cfg.codex_skills_dir.join("review/SKILL.md"))?,
            "Cursor body"
        );
        Ok(())
    }

    #[test]
    fn sync_commands_skips_update_when_body_same() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;

        let claude = cfg.claude_dir.join("same.md");
        let cursor = cfg.cursor_dir.join("same.md");

        write_plain(&claude, &doc("shared", "Same body"))?;
        write_plain(&cursor, &doc("shared", "Same body"))?;

        crate::sync::test_support::set_mtime(&claude, 2_100_000_000)?;
        crate::sync::test_support::set_mtime(&cursor, 2_100_000_100)?;

        sync_commands(&cfg, LogMode::Quiet)?;

        let expected_nanos = 2_100_000_000u128 * 1_000_000_000u128;
        assert_eq!(
            super::super::shared::file_mtime_value(&claude),
            expected_nanos
        );
        Ok(())
    }

    #[test]
    fn sync_commands_opencode_wins() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;

        let claude = cfg.claude_dir.join("build.md");
        let opencode = cfg.opencode_commands_dir.join("build.md");

        write_plain(&claude, &doc("claude", "Old"))?;
        write_plain(&opencode, &doc("opencode", "New"))?;

        crate::sync::test_support::set_mtime(&opencode, 2_000_000_200)?;

        sync_commands(&cfg, LogMode::Quiet)?;

        assert_eq!(read_body(&claude)?, "New");
        assert!(read_frontmatter(&claude)?
            .unwrap_or_default()
            .contains("name: opencode"));
        assert_eq!(
            read_body(&cfg.codex_skills_dir.join("build/SKILL.md"))?,
            "New"
        );
        Ok(())
    }

    #[test]
    fn sync_commands_central_wins_and_syncs_required_frontmatter() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let claude = cfg.claude_dir.join("review.md");
        let central = cfg.central_dir.join("review.md");

        write_plain(&claude, &doc("claude", "Old"))?;
        write_plain(&central, &doc("central", "New"))?;
        crate::sync::test_support::set_mtime(&central, 2_200_000_300)?;

        sync_commands(&cfg, LogMode::Quiet)?;

        assert_eq!(read_body(&claude)?, "New");
        assert!(read_frontmatter(&claude)?
            .unwrap_or_default()
            .contains("name: central"));
        assert!(read_frontmatter(&central)?
            .unwrap_or_default()
            .contains("name: central"));
        assert_eq!(
            read_body(&cfg.codex_skills_dir.join("review/SKILL.md"))?,
            "New"
        );
        Ok(())
    }

    #[test]
    fn sync_commands_blacklist_skips_codex_skill_wrapper() -> io::Result<()> {
        let (_tmp, mut cfg) = setup()?;

        let claude = cfg.claude_dir.join("review.md");
        let skill_dir = cfg.codex_skills_dir.join("review");

        write_plain(&claude, &doc("claude", "Body"))?;

        cfg.blacklist
            .entry("commands/review.md".to_string())
            .or_default()
            .push(TOOL_CODEX.to_string());

        sync_commands(&cfg, LogMode::Quiet)?;

        assert!(!skill_dir.exists());
        Ok(())
    }

    #[test]
    fn sync_commands_skill_blacklist_skips_codex_skill_wrapper() -> io::Result<()> {
        let (_tmp, mut cfg) = setup()?;

        write_plain(&cfg.central_dir.join("review.md"), &doc("central", "Body"))?;

        cfg.blacklist
            .entry("skills/review".to_string())
            .or_default()
            .push(TOOL_CODEX.to_string());

        sync_commands(&cfg, LogMode::Quiet)?;

        assert!(!cfg.codex_skills_dir.join("review/SKILL.md").exists());
        assert!(cfg.central_dir.join("review.md").exists());
        Ok(())
    }

    #[test]
    fn sync_commands_prunes_stale_codex_skill_wrapper() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;

        let stale = cfg.codex_skills_dir.join("stale");
        write_plain(
            &stale.join("SKILL.md"),
            "---\nname: stale\ndescription: generated\n---\nStale body",
        )?;
        write_plain(
            &stale.join(crate::markers::RELAY_COMMAND_SKILL_MARKER),
            "generated by relay from commands\n",
        )?;
        let real_skill = cfg.codex_skills_dir.join("real");
        write_plain(
            &real_skill.join("SKILL.md"),
            "---\nname: real\ndescription: user skill\n---\nReal body",
        )?;

        sync_commands(&cfg, LogMode::Quiet)?;

        assert!(!stale.exists());
        assert!(real_skill.join("SKILL.md").exists());
        Ok(())
    }

    #[test]
    fn sync_commands_reserved_codex_skill_name_skips_wrapper() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;

        write_plain(&cfg.central_dir.join("review.md"), "Review command body.")?;

        let mut history = None;
        let mut conflicts = Vec::new();
        let reserved = HashSet::from(["review".to_string()]);
        sync_commands_with_reserved_codex_skill_names(
            &cfg,
            LogMode::Quiet,
            ExecutionMode::Apply,
            &mut history,
            &mut conflicts,
            &reserved,
        )?;

        assert!(!cfg.codex_skills_dir.join("review/SKILL.md").exists());
        assert!(cfg.central_dir.join("review.md").exists());
        Ok(())
    }

    #[test]
    fn sync_commands_blacklist_skips_tool_but_syncs_others() -> io::Result<()> {
        let (_tmp, mut cfg) = setup()?;

        let claude = cfg.claude_dir.join("review.md");
        let central = cfg.central_dir.join("review.md");

        write_plain(&claude, &doc("claude", "Body"))?;

        // Blacklist review.md from codex
        cfg.blacklist
            .entry("commands/review.md".to_string())
            .or_default()
            .push("codex".to_string());

        sync_commands(&cfg, LogMode::Quiet)?;

        // Central should get it
        assert!(central.exists());
        assert_eq!(read_body(&central)?, "Body");
        // Codex should NOT get a generated command skill wrapper
        assert!(!cfg.codex_skills_dir.join("review/SKILL.md").exists());
        // Claude should keep it
        assert!(claude.exists());
        Ok(())
    }

    #[test]
    fn sync_commands_cursor_wins() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;

        let claude = cfg.claude_dir.join("cursor-check.md");
        let cursor = cfg.cursor_dir.join("cursor-check.md");

        write_plain(&claude, &doc("claude", "Old"))?;
        write_plain(&cursor, &doc("cursor", "Newest from cursor"))?;

        crate::sync::test_support::set_mtime(&cursor, 2_400_000_100)?;

        sync_commands(&cfg, LogMode::Quiet)?;

        assert_eq!(read_body(&claude)?, "Newest from cursor");
        assert_eq!(read_body(&cursor)?, "Newest from cursor");
        assert!(read_frontmatter(&claude)?
            .unwrap_or_default()
            .contains("name: cursor"));
        assert_eq!(
            read_body(&cfg.codex_skills_dir.join("cursor-check/SKILL.md"))?,
            "Newest from cursor"
        );
        Ok(())
    }

    #[test]
    fn sync_commands_ignores_existing_codex_prompts() -> io::Result<()> {
        let (tmp, cfg) = setup()?;

        let codex = tmp.path().join(".codex/prompts/review.md");
        let central = cfg.central_dir.join("review.md");
        write_plain(&codex, &doc("codex", "Stale codex prompt"))?;
        write_plain(&central, &doc("central", "Current command"))?;
        crate::sync::test_support::set_mtime(&codex, 2_500_000_000)?;

        sync_commands(&cfg, LogMode::Quiet)?;

        assert_eq!(read_body(&central)?, "Current command");
        assert_eq!(
            read_body(&cfg.codex_skills_dir.join("review/SKILL.md"))?,
            "Current command"
        );
        assert_eq!(read_body(&codex)?, "Stale codex prompt");
        Ok(())
    }

    #[test]
    fn sync_commands_creates_codex_skill_dir_when_missing() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        fs::remove_dir_all(&cfg.codex_skills_dir)?;

        write_plain(&cfg.central_dir.join("review.md"), "Review body")?;

        sync_commands(&cfg, LogMode::Quiet)?;

        assert_eq!(
            read_body(&cfg.codex_skills_dir.join("review/SKILL.md"))?,
            "Review body"
        );
        Ok(())
    }

    #[test]
    fn sync_commands_creates_missing_opencode_commands_dir() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        fs::remove_dir_all(&cfg.opencode_commands_dir)?;
        write_plain(&cfg.central_dir.join("review.md"), "Review body")?;

        sync_commands(&cfg, LogMode::Quiet)?;

        assert_eq!(
            read_body(&cfg.opencode_commands_dir.join("review.md"))?,
            "Review body"
        );
        Ok(())
    }

    #[test]
    fn sync_commands_does_not_write_codex_prompts() -> io::Result<()> {
        let (tmp, cfg) = setup()?;

        let claude = cfg.claude_dir.join("review.md");
        write_plain(&claude, &doc("claude", "Review body"))?;

        sync_commands(&cfg, LogMode::Quiet)?;

        assert!(!tmp.path().join(".codex/prompts/review.md").exists());
        assert!(cfg.codex_skills_dir.join("review/SKILL.md").exists());
        Ok(())
    }

    #[test]
    fn sync_commands_collects_conflict_details() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;

        let claude = cfg.claude_dir.join("review.md");
        let cursor = cfg.cursor_dir.join("review.md");
        write_plain(&claude, &doc("claude", "Claude body"))?;
        write_plain(&cursor, &doc("cursor", "Cursor body"))?;
        crate::sync::test_support::set_mtime(&claude, 100)?;
        crate::sync::test_support::set_mtime(&cursor, 101)?;

        let mut history = None;
        let mut conflicts = Vec::new();
        sync_commands_with_mode(
            &cfg,
            LogMode::Quiet,
            ExecutionMode::Plan,
            &mut history,
            &mut conflicts,
        )?;

        assert_eq!(
            conflicts,
            vec![SyncConflict {
                kind: SyncItemKind::Command,
                name: "review.md".to_string(),
                winner: TOOL_CURSOR,
                others: vec![TOOL_CLAUDE],
            }]
        );
        Ok(())
    }
}
