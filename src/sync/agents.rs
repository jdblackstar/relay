use super::shared::{
    log_action, read_markdown_variant, select_markdown_winner, update_markdown_target,
    MarkdownVariant, CONFLICT_WINDOW_NS, TOOL_CENTRAL,
};
use super::{ExecutionMode, LogMode, SyncConflict, SyncItemKind, SyncStats};
use crate::blacklist::{
    CODEX_AGENTS_BLACKLIST_KEY, LEGACY_AGENTS_BLACKLIST_KEY, OPENCODE_AGENTS_BLACKLIST_KEY,
};
use crate::config::{Config, TOOL_CODEX, TOOL_OPENCODE};
use crate::history::HistoryRecorder;
use std::collections::HashSet;
use std::io;

#[cfg(test)]
use std::fs;

fn is_agent_target_blacklisted(cfg: &Config, tool: &str) -> bool {
    match tool {
        TOOL_CODEX => {
            cfg.is_blacklisted(CODEX_AGENTS_BLACKLIST_KEY, TOOL_CODEX)
                || cfg.is_blacklisted(LEGACY_AGENTS_BLACKLIST_KEY, TOOL_CODEX)
        }
        TOOL_OPENCODE => {
            cfg.is_blacklisted(OPENCODE_AGENTS_BLACKLIST_KEY, TOOL_OPENCODE)
                || cfg.is_blacklisted(LEGACY_AGENTS_BLACKLIST_KEY, TOOL_OPENCODE)
        }
        _ => false,
    }
}

#[cfg(any(test, coverage))]
pub(crate) fn sync_agents(cfg: &Config, log_mode: LogMode) -> io::Result<SyncStats> {
    let mut history = None;
    let mut conflicts = Vec::new();
    sync_agents_with_mode(
        cfg,
        log_mode,
        ExecutionMode::Apply,
        &mut history,
        &mut conflicts,
    )
}

pub(crate) fn sync_agents_with_mode(
    cfg: &Config,
    log_mode: LogMode,
    mode: ExecutionMode,
    history: &mut Option<HistoryRecorder>,
    conflicts: &mut Vec<SyncConflict>,
) -> io::Result<SyncStats> {
    let mut stats = SyncStats::default();

    let codex_enabled = cfg.tool_enabled(TOOL_CODEX)
        && cfg
            .codex_agents_file
            .parent()
            .is_some_and(|parent| parent.exists());
    let opencode_enabled = cfg.tool_enabled(TOOL_OPENCODE)
        && cfg
            .opencode_agents_file
            .parent()
            .is_some_and(|parent| parent.exists());

    let central_codex = cfg.central_agents_dir.join("codex/AGENTS.md");
    let central_opencode = cfg.central_agents_dir.join("opencode/AGENTS.md");

    let mut agent_variants: Vec<MarkdownVariant> = Vec::new();
    if codex_enabled && cfg.codex_agents_file.exists() {
        agent_variants.push(read_markdown_variant(TOOL_CODEX, &cfg.codex_agents_file)?);
    }
    if opencode_enabled && cfg.opencode_agents_file.exists() {
        agent_variants.push(read_markdown_variant(
            TOOL_OPENCODE,
            &cfg.opencode_agents_file,
        )?);
    }
    for path in [&central_codex, &central_opencode] {
        if path.exists() {
            agent_variants.push(read_markdown_variant(TOOL_CENTRAL, path)?);
        }
    }

    if agent_variants.is_empty() {
        return Ok(stats);
    }

    let winner = select_markdown_winner(&agent_variants);
    if let Some(conflict) = conflict_for_variants(
        "AGENTS.md",
        &agent_variants,
        winner.tool,
        winner.doc.body_hash,
    ) {
        conflicts.push(conflict);
        log_action(
            log_mode,
            &format!(
                "warning: agents edited in multiple tools; last-write-wins chose {}",
                winner.tool
            ),
        );
    }
    let source = &winner.doc;

    for (tool, enabled, path) in [
        (TOOL_CODEX, codex_enabled, &cfg.codex_agents_file),
        (TOOL_OPENCODE, opencode_enabled, &cfg.opencode_agents_file),
        (TOOL_CENTRAL, true, &central_codex),
        (TOOL_CENTRAL, true, &central_opencode),
    ] {
        if !enabled {
            continue;
        }
        if tool != TOOL_CENTRAL && is_agent_target_blacklisted(cfg, tool) {
            continue;
        }
        let existing = agent_variants
            .iter()
            .find(|variant| variant.path == *path)
            .map(|variant| &variant.doc);
        let label = format!("agents: {}", path.display());
        let updated = update_markdown_target(
            source, existing, path, true, log_mode, mode, history, &label,
        )?;
        stats.updated += usize::from(updated);
    }

    Ok(stats)
}

fn conflict_for_variants(
    name: &str,
    variants: &[MarkdownVariant],
    winner: &'static str,
    winner_hash: u64,
) -> Option<SyncConflict> {
    if variants.len() < 2 {
        return None;
    }
    let mut min = u128::MAX;
    let mut max = 0u128;
    let mut hashes = HashSet::new();
    for variant in variants {
        min = min.min(variant.mtime);
        max = max.max(variant.mtime);
        hashes.insert(variant.doc.body_hash);
    }
    if hashes.len() < 2 || max.saturating_sub(min) > CONFLICT_WINDOW_NS {
        return None;
    }

    let mut others = Vec::new();
    for variant in variants {
        if variant.tool != winner
            && variant.doc.body_hash != winner_hash
            && !others.contains(&variant.tool)
        {
            others.push(variant.tool);
        }
    }
    Some(SyncConflict {
        kind: SyncItemKind::Agent,
        name: name.to_string(),
        winner,
        others,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::test_support::{doc, read_body, read_frontmatter, setup, write_plain};

    #[test]
    fn sync_agents_last_write_wins() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;

        let codex_doc = doc("codex", "Codex agent");
        let opencode_doc = doc("opencode", "OpenCode agent");
        write_plain(&cfg.codex_agents_file, &codex_doc)?;
        write_plain(&cfg.opencode_agents_file, &opencode_doc)?;

        crate::sync::test_support::set_mtime(&cfg.opencode_agents_file, 2_100_000_200)?;

        sync_agents(&cfg, LogMode::Quiet)?;

        let codex_body = read_body(&cfg.codex_agents_file)?;
        assert_eq!(codex_body, "OpenCode agent");
        let codex_frontmatter = read_frontmatter(&cfg.codex_agents_file)?.unwrap_or_default();
        assert!(codex_frontmatter.contains("name: opencode"));
        Ok(())
    }

    #[test]
    fn sync_agents_updates_opencode_when_codex_wins() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;

        let codex_doc = doc("codex", "Codex agent");
        let opencode_doc = doc("opencode", "OpenCode agent");
        write_plain(&cfg.codex_agents_file, &codex_doc)?;
        write_plain(&cfg.opencode_agents_file, &opencode_doc)?;

        crate::sync::test_support::set_mtime(&cfg.codex_agents_file, 2_200_000_200)?;

        sync_agents(&cfg, LogMode::Quiet)?;

        let opencode_body = read_body(&cfg.opencode_agents_file)?;
        assert_eq!(opencode_body, "Codex agent");
        Ok(())
    }

    #[test]
    fn sync_agents_central_wins_and_syncs_required_frontmatter() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let central = cfg.central_agents_dir.join("codex/AGENTS.md");
        write_plain(&cfg.codex_agents_file, &doc("codex", "Old"))?;
        write_plain(&cfg.opencode_agents_file, &doc("opencode", "Old"))?;
        write_plain(&central, &doc("central", "New"))?;
        crate::sync::test_support::set_mtime(&central, 2_400_000_100)?;

        sync_agents(&cfg, LogMode::Quiet)?;

        assert_eq!(read_body(&cfg.codex_agents_file)?, "New");
        assert!(read_frontmatter(&cfg.codex_agents_file)?
            .unwrap_or_default()
            .contains("name: central"));
        assert_eq!(read_body(&cfg.opencode_agents_file)?, "New");
        assert!(read_frontmatter(&cfg.opencode_agents_file)?
            .unwrap_or_default()
            .contains("name: central"));
        Ok(())
    }

    #[test]
    fn sync_agents_blacklist_skips_tool() -> io::Result<()> {
        let (_tmp, mut cfg) = setup()?;

        write_plain(&cfg.codex_agents_file, &doc("codex", "Codex agent"))?;
        write_plain(
            &cfg.opencode_agents_file,
            &doc("opencode", "OpenCode agent"),
        )?;
        crate::sync::test_support::set_mtime(&cfg.codex_agents_file, 2_200_000_200)?;

        // Legacy blacklist key should still be honored for compatibility.
        cfg.blacklist
            .entry(LEGACY_AGENTS_BLACKLIST_KEY.to_string())
            .or_default()
            .push("opencode".to_string());

        sync_agents(&cfg, LogMode::Quiet)?;

        // Codex wins and codex file should be updated (it's the winner)
        assert!(cfg.codex_agents_file.exists());
        // OpenCode should NOT be updated (blacklisted)
        let opencode_body = read_body(&cfg.opencode_agents_file)?;
        assert_eq!(opencode_body, "OpenCode agent");
        Ok(())
    }

    #[test]
    fn sync_agents_blacklist_codex_key_skips_codex_only() -> io::Result<()> {
        let (_tmp, mut cfg) = setup()?;

        write_plain(&cfg.codex_agents_file, &doc("codex", "Codex old"))?;
        write_plain(&cfg.opencode_agents_file, &doc("opencode", "OpenCode new"))?;
        crate::sync::test_support::set_mtime(&cfg.opencode_agents_file, 2_500_000_200)?;

        cfg.blacklist
            .entry(CODEX_AGENTS_BLACKLIST_KEY.to_string())
            .or_default()
            .push(TOOL_CODEX.to_string());

        sync_agents(&cfg, LogMode::Quiet)?;

        assert_eq!(read_body(&cfg.codex_agents_file)?, "Codex old");
        assert_eq!(read_body(&cfg.opencode_agents_file)?, "OpenCode new");
        Ok(())
    }

    #[test]
    fn sync_agents_blacklist_opencode_key_skips_opencode_only() -> io::Result<()> {
        let (_tmp, mut cfg) = setup()?;

        write_plain(&cfg.codex_agents_file, &doc("codex", "Codex new"))?;
        write_plain(&cfg.opencode_agents_file, &doc("opencode", "OpenCode old"))?;
        crate::sync::test_support::set_mtime(&cfg.codex_agents_file, 2_600_000_200)?;

        cfg.blacklist
            .entry(OPENCODE_AGENTS_BLACKLIST_KEY.to_string())
            .or_default()
            .push(TOOL_OPENCODE.to_string());

        sync_agents(&cfg, LogMode::Quiet)?;

        assert_eq!(read_body(&cfg.codex_agents_file)?, "Codex new");
        assert_eq!(read_body(&cfg.opencode_agents_file)?, "OpenCode old");
        Ok(())
    }

    #[test]
    fn sync_agents_skips_disabled_tool() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        cfg.opencode_agents_file
            .parent()
            .map(fs::remove_dir_all)
            .transpose()?;
        write_plain(&cfg.codex_agents_file, &doc("codex", "Codex agent"))?;

        sync_agents(&cfg, LogMode::Quiet)?;

        assert!(cfg.codex_agents_file.exists());
        assert!(!cfg.opencode_agents_file.exists());
        assert!(cfg.central_agents_dir.join("codex/AGENTS.md").exists());
        Ok(())
    }

    #[test]
    fn sync_agents_collects_conflict_details() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;

        write_plain(&cfg.codex_agents_file, &doc("codex", "Codex agent"))?;
        write_plain(
            &cfg.opencode_agents_file,
            &doc("opencode", "OpenCode agent"),
        )?;
        crate::sync::test_support::set_mtime(&cfg.codex_agents_file, 100)?;
        crate::sync::test_support::set_mtime(&cfg.opencode_agents_file, 101)?;

        let mut history = None;
        let mut conflicts = Vec::new();
        sync_agents_with_mode(
            &cfg,
            LogMode::Quiet,
            ExecutionMode::Plan,
            &mut history,
            &mut conflicts,
        )?;

        assert_eq!(
            conflicts,
            vec![SyncConflict {
                kind: SyncItemKind::Agent,
                name: "AGENTS.md".to_string(),
                winner: TOOL_OPENCODE,
                others: vec![TOOL_CODEX],
            }]
        );
        Ok(())
    }
}
