use super::shared::{
    collect_names, conflict_for_variants, list_codex_files, list_files, list_if, log_action,
    read_markdown_variant, select_markdown_winner, update_markdown_target, MarkdownVariant,
    TOOL_CENTRAL,
};
use super::{ExecutionMode, LogMode, SyncConflict, SyncItemKind, SyncStats};
use crate::config::{Config, TOOL_CLAUDE, TOOL_CODEX, TOOL_CURSOR, TOOL_OPENCODE};
use crate::history::HistoryRecorder;
use std::collections::HashMap;
use std::io;

#[cfg(test)]
use std::fs;

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

pub(crate) fn sync_commands_with_mode(
    cfg: &Config,
    log_mode: LogMode,
    mode: ExecutionMode,
    history: &mut Option<HistoryRecorder>,
    conflicts: &mut Vec<SyncConflict>,
) -> io::Result<SyncStats> {
    let mut stats = SyncStats::default();

    let claude_enabled = cfg.tool_enabled(TOOL_CLAUDE) && cfg.claude_dir.exists();
    let cursor_enabled = cfg.tool_enabled(TOOL_CURSOR) && cfg.cursor_dir.exists();
    let opencode_enabled = cfg.tool_enabled(TOOL_OPENCODE) && cfg.opencode_commands_dir.exists();
    let codex_enabled = cfg.tool_enabled(TOOL_CODEX) && cfg.codex_dir.exists();

    let claude = list_if(claude_enabled, &cfg.claude_dir, list_files)?;
    let cursor = list_if(cursor_enabled, &cfg.cursor_dir, list_files)?;
    let opencode = list_if(opencode_enabled, &cfg.opencode_commands_dir, list_files)?;
    let codex = list_if(codex_enabled, &cfg.codex_dir, list_codex_files)?;
    let central = if cfg.central_dir.exists() {
        list_files(&cfg.central_dir)?
    } else {
        HashMap::new()
    };

    let names = collect_names(&[&claude, &cursor, &opencode, &codex, &central]);
    for name in names {
        let mut variants: Vec<MarkdownVariant> = Vec::new();
        for (tool, map) in [
            (TOOL_CENTRAL, &central),
            (TOOL_CLAUDE, &claude),
            (TOOL_CURSOR, &cursor),
            (TOOL_OPENCODE, &opencode),
            (TOOL_CODEX, &codex),
        ] {
            if let Some(path) = map.get(&name) {
                variants.push(read_markdown_variant(tool, path)?);
            }
        }
        let winner = select_markdown_winner(&variants);
        if let Some(conflict) = conflict_for_variants(
            &name,
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
            (TOOL_CODEX, codex_enabled, &cfg.codex_dir),
        ] {
            if enabled
                && (tool == TOOL_CENTRAL || !cfg.is_blacklisted(&format!("commands/{name}"), tool))
            {
                let target_path = base_dir.join(&name);
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
        }
    }

    Ok(stats)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::test_support::{doc, read_body, read_frontmatter, setup, write_plain};

    #[test]
    fn sync_commands_last_write_wins_and_syncs_required_frontmatter() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;

        let claude = cfg.claude_dir.join("review.md");
        let codex = cfg.codex_dir.join("review.md");

        write_plain(&claude, &doc("claude", "Claude body"))?;
        write_plain(&codex, &doc("codex", "Codex body"))?;

        crate::sync::test_support::set_mtime(&codex, 2_000_000_000)?;

        sync_commands(&cfg, LogMode::Quiet)?;

        assert_eq!(read_body(&claude)?, "Codex body");
        assert_eq!(read_body(&codex)?, "Codex body");

        let claude_frontmatter = read_frontmatter(&claude)?;
        assert!(claude_frontmatter
            .unwrap_or_default()
            .contains("name: codex"));

        let central = cfg.central_dir.join("review.md");
        assert_eq!(read_body(&central)?, "Codex body");
        assert!(read_frontmatter(&central)?
            .unwrap_or_default()
            .contains("name: codex"));
        Ok(())
    }

    #[test]
    fn sync_commands_skips_update_when_body_same() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;

        let claude = cfg.claude_dir.join("same.md");
        let codex = cfg.codex_dir.join("same.md");

        write_plain(&claude, &doc("shared", "Same body"))?;
        write_plain(&codex, &doc("shared", "Same body"))?;

        crate::sync::test_support::set_mtime(&claude, 2_100_000_000)?;
        crate::sync::test_support::set_mtime(&codex, 2_100_000_100)?;

        sync_commands(&cfg, LogMode::Quiet)?;

        let expected_nanos = 2_100_000_000u128 * 1_000_000_000u128;
        assert_eq!(
            super::super::shared::file_mtime_value(&claude),
            expected_nanos
        );
        Ok(())
    }

    #[test]
    fn sync_commands_supports_codex_prompt_prefix() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;

        let legacy = cfg.codex_dir.join("prompt:legacy.md");
        write_plain(&legacy, "Legacy body")?;

        sync_commands(&cfg, LogMode::Quiet)?;

        for path in [
            cfg.codex_dir.join("legacy.md"),
            cfg.claude_dir.join("legacy.md"),
            cfg.central_dir.join("legacy.md"),
        ] {
            assert_eq!(fs::read_to_string(path)?, "Legacy body");
        }
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
        let codex = cfg.codex_dir.join("build.md");
        assert_eq!(read_body(&codex)?, "New");
        Ok(())
    }

    #[test]
    fn sync_commands_central_wins_and_syncs_required_frontmatter() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let claude = cfg.claude_dir.join("review.md");
        let codex = cfg.codex_dir.join("review.md");
        let central = cfg.central_dir.join("review.md");

        write_plain(&claude, &doc("claude", "Old"))?;
        write_plain(&codex, &doc("codex", "Old"))?;
        write_plain(&central, &doc("central", "New"))?;
        crate::sync::test_support::set_mtime(&central, 2_200_000_300)?;

        sync_commands(&cfg, LogMode::Quiet)?;

        assert_eq!(read_body(&claude)?, "New");
        assert!(read_frontmatter(&claude)?
            .unwrap_or_default()
            .contains("name: central"));
        assert_eq!(read_body(&codex)?, "New");
        assert!(read_frontmatter(&codex)?
            .unwrap_or_default()
            .contains("name: central"));
        assert!(read_frontmatter(&central)?
            .unwrap_or_default()
            .contains("name: central"));
        Ok(())
    }

    #[test]
    fn sync_commands_blacklist_skips_tool_but_syncs_others() -> io::Result<()> {
        let (_tmp, mut cfg) = setup()?;

        let claude = cfg.claude_dir.join("review.md");
        let codex = cfg.codex_dir.join("review.md");
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
        // Codex should NOT get it
        assert!(!codex.exists());
        // Claude should keep it
        assert!(claude.exists());
        Ok(())
    }

    #[test]
    fn sync_commands_cursor_wins() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;

        let claude = cfg.claude_dir.join("cursor-check.md");
        let cursor = cfg.cursor_dir.join("cursor-check.md");
        let codex = cfg.codex_dir.join("cursor-check.md");

        write_plain(&claude, &doc("claude", "Old"))?;
        write_plain(&cursor, &doc("cursor", "Newest from cursor"))?;
        write_plain(&codex, &doc("codex", "Old"))?;

        crate::sync::test_support::set_mtime(&cursor, 2_400_000_100)?;

        sync_commands(&cfg, LogMode::Quiet)?;

        assert_eq!(read_body(&claude)?, "Newest from cursor");
        assert_eq!(read_body(&cursor)?, "Newest from cursor");
        assert_eq!(read_body(&codex)?, "Newest from cursor");
        assert!(read_frontmatter(&claude)?
            .unwrap_or_default()
            .contains("name: cursor"));
        assert!(read_frontmatter(&codex)?
            .unwrap_or_default()
            .contains("name: cursor"));
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
