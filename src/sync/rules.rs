use super::shared::{
    file_mtime_value, hash_bytes, log_action, tool_order, write_raw_if_changed, CONFLICT_WINDOW_NS,
    TOOL_CENTRAL,
};
use super::{ExecutionMode, LogMode, SyncStats};
use crate::config::{Config, TOOL_CODEX};
use crate::history::HistoryRecorder;
use std::collections::HashSet;
use std::fs;
use std::io;

#[cfg(any(test, coverage))]
pub(crate) fn sync_rules(cfg: &Config, log_mode: LogMode) -> io::Result<SyncStats> {
    let mut history = None;
    sync_rules_with_mode(cfg, log_mode, ExecutionMode::Apply, &mut history)
}

pub(crate) fn sync_rules_with_mode(
    cfg: &Config,
    log_mode: LogMode,
    mode: ExecutionMode,
    history: &mut Option<HistoryRecorder>,
) -> io::Result<SyncStats> {
    let mut stats = SyncStats::default();
    fs::create_dir_all(&cfg.central_rules_dir)?;

    let codex_enabled = cfg.tool_enabled(TOOL_CODEX)
        && cfg
            .codex_rules_file
            .parent()
            .is_some_and(|parent| parent.exists());
    let central_path = cfg.central_rules_dir.join("codex/default.rules");

    let mut variants: Vec<RuleVariant> = Vec::new();
    if codex_enabled && cfg.codex_rules_file.exists() {
        let contents = fs::read(&cfg.codex_rules_file)?;
        variants.push(RuleVariant {
            tool: TOOL_CODEX,
            path: cfg.codex_rules_file.clone(),
            mtime: file_mtime_value(&cfg.codex_rules_file),
            hash: hash_bytes(&contents),
        });
    }
    if central_path.exists() {
        let contents = fs::read(&central_path)?;
        variants.push(RuleVariant {
            tool: TOOL_CENTRAL,
            path: central_path.clone(),
            mtime: file_mtime_value(&central_path),
            hash: hash_bytes(&contents),
        });
    }
    let Some(winner) = variants
        .iter()
        .max_by_key(|variant| (variant.mtime, tool_order(variant.tool)))
    else {
        return Ok(stats);
    };
    if should_warn_conflict(&variants) {
        log_action(
            log_mode,
            &format!(
                "warning: rules edited in multiple tools; last-write-wins chose {}",
                winner.tool
            ),
        );
    }
    let winner_contents = fs::read(&winner.path)?;
    for (enabled, path) in [
        (codex_enabled, &cfg.codex_rules_file),
        (true, &central_path),
    ] {
        if !enabled {
            continue;
        }
        if write_raw_if_changed(path, &winner_contents, mode, history)? {
            stats.updated += 1;
            let action = if mode == ExecutionMode::Plan {
                "would update"
            } else {
                "updated"
            };
            log_action(log_mode, &format!("rules: {action} {}", path.display()));
        }
    }

    Ok(stats)
}

struct RuleVariant {
    tool: &'static str,
    path: std::path::PathBuf,
    mtime: u128,
    hash: u64,
}

fn should_warn_conflict(variants: &[RuleVariant]) -> bool {
    if variants.len() < 2 {
        return false;
    }
    let mut min = u128::MAX;
    let mut max = 0u128;
    let mut hashes = HashSet::new();
    for variant in variants {
        min = min.min(variant.mtime);
        max = max.max(variant.mtime);
        hashes.insert(variant.hash);
    }
    hashes.len() > 1 && max.saturating_sub(min) <= CONFLICT_WINDOW_NS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::test_support::{setup, write_plain};

    #[test]
    fn sync_rules_mirrors_codex_rules() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;

        write_plain(&cfg.codex_rules_file, "rule(\"x\")")?;
        sync_rules(&cfg, LogMode::Quiet)?;

        let central = cfg.central_rules_dir.join("codex/default.rules");
        assert_eq!(fs::read_to_string(&central)?, "rule(\"x\")");
        Ok(())
    }

    #[test]
    fn sync_rules_central_wins() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let central = cfg.central_rules_dir.join("codex/default.rules");
        write_plain(&cfg.codex_rules_file, "rule(\"old\")")?;
        write_plain(&central, "rule(\"new\")")?;
        crate::sync::test_support::set_mtime(&central, 2_500_000_100)?;

        sync_rules(&cfg, LogMode::Quiet)?;

        assert_eq!(fs::read_to_string(&cfg.codex_rules_file)?, "rule(\"new\")");
        Ok(())
    }
}
