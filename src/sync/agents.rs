use super::shared::{
    log_action, read_markdown_variant, select_markdown_winner, update_markdown_target,
    MarkdownVariant, CONFLICT_WINDOW_NS, TOOL_CENTRAL,
};
use super::{ExecutionMode, LogMode, SyncStats};
use crate::config::{Config, TOOL_CODEX, TOOL_OPENCODE};
use crate::history::HistoryRecorder;
use std::collections::HashSet;
use std::fs;
use std::io;

#[cfg(any(test, coverage))]
pub(crate) fn sync_agents(cfg: &Config, log_mode: LogMode) -> io::Result<SyncStats> {
    let mut history = None;
    sync_agents_with_mode(cfg, log_mode, ExecutionMode::Apply, &mut history)
}

pub(crate) fn sync_agents_with_mode(
    cfg: &Config,
    log_mode: LogMode,
    mode: ExecutionMode,
    history: &mut Option<HistoryRecorder>,
) -> io::Result<SyncStats> {
    let mut stats = SyncStats::default();
    fs::create_dir_all(&cfg.central_agents_dir)?;

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
    if should_warn_conflict(&agent_variants) {
        log_action(
            log_mode,
            &format!(
                "warning: agents edited in multiple tools; last-write-wins chose {}",
                winner.tool
            ),
        );
    }
    let source = &winner.doc;

    for (enabled, path) in [
        (codex_enabled, &cfg.codex_agents_file),
        (opencode_enabled, &cfg.opencode_agents_file),
        (true, &central_codex),
        (true, &central_opencode),
    ] {
        if !enabled {
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

fn should_warn_conflict(variants: &[MarkdownVariant]) -> bool {
    if variants.len() < 2 {
        return false;
    }
    let mut min = u128::MAX;
    let mut max = 0u128;
    let mut hashes = HashSet::new();
    for variant in variants {
        min = min.min(variant.mtime);
        max = max.max(variant.mtime);
        hashes.insert(variant.doc.body_hash);
    }
    hashes.len() > 1 && max.saturating_sub(min) <= CONFLICT_WINDOW_NS
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
}
