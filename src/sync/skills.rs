use super::shared::{
    collect_names, file_mtime_value_from_meta, hash_bytes, list_if, log_action, merge_frontmatter,
    read_markdown, read_visible_entry, tool_order, write_file, CONFLICT_WINDOW_NS, TOOL_CENTRAL,
};
use super::{ExecutionMode, LogMode as SyncLogMode, SyncStats};
use crate::config::{Config, TOOL_CLAUDE, TOOL_CODEX, TOOL_OPENCODE};
use crate::history::HistoryRecorder;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::collections::HashSet;
use std::env;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy)]
struct DirDigest {
    body_hash: u64,
    mtime: u128,
}

struct SkillVariant {
    tool: &'static str,
    path: PathBuf,
    digest: DirDigest,
}

#[cfg(any(test, coverage))]
pub(crate) fn sync_skills(cfg: &Config, log_mode: SyncLogMode) -> io::Result<SyncStats> {
    let mut history = None;
    sync_skills_with_mode(cfg, log_mode, ExecutionMode::Apply, &mut history)
}

pub(crate) fn sync_skills_with_mode(
    cfg: &Config,
    log_mode: SyncLogMode,
    mode: ExecutionMode,
    history: &mut Option<HistoryRecorder>,
) -> io::Result<SyncStats> {
    let mut stats = SyncStats::default();
    fs::create_dir_all(&cfg.central_skills_dir)?;

    let claude_enabled = cfg.tool_enabled(TOOL_CLAUDE) && cfg.claude_skills_dir.exists();
    let opencode_enabled = cfg.tool_enabled(TOOL_OPENCODE) && cfg.opencode_skills_dir.exists();
    let codex_enabled = cfg.tool_enabled(TOOL_CODEX) && cfg.codex_skills_dir.exists();

    let claude = list_if(claude_enabled, &cfg.claude_skills_dir, list_skill_dirs)?;
    let opencode = list_if(opencode_enabled, &cfg.opencode_skills_dir, list_skill_dirs)?;
    let codex = list_if(codex_enabled, &cfg.codex_skills_dir, list_skill_dirs)?;
    let central = list_skill_dirs(&cfg.central_skills_dir)?;

    let names = collect_names(&[&claude, &opencode, &codex, &central]);
    for name in names {
        let mut variants: Vec<SkillVariant> = Vec::new();
        for (tool, map) in [
            (TOOL_CENTRAL, &central),
            (TOOL_CLAUDE, &claude),
            (TOOL_OPENCODE, &opencode),
            (TOOL_CODEX, &codex),
        ] {
            if let Some(path) = map.get(&name) {
                variants.push(SkillVariant {
                    tool,
                    path: path.clone(),
                    digest: digest_skill_dir(path)?,
                });
            }
        }
        let winner = select_skill_winner(&variants);
        if should_warn_conflict(&variants) {
            log_action(
                log_mode,
                &format!(
                    "warning: skills '{name}' edited in multiple tools; last-write-wins chose {}",
                    winner.tool
                ),
            );
        }

        for (tool, enabled, base_dir) in [
            (TOOL_CENTRAL, true, &cfg.central_skills_dir),
            (TOOL_CLAUDE, claude_enabled, &cfg.claude_skills_dir),
            (TOOL_OPENCODE, opencode_enabled, &cfg.opencode_skills_dir),
            (TOOL_CODEX, codex_enabled, &cfg.codex_skills_dir),
        ] {
            let updated = sync_skill_for_tool(
                tool, enabled, base_dir, &name, winner, &variants, log_mode, mode, history,
            )?;
            stats.updated += usize::from(updated);
        }
    }

    Ok(stats)
}

fn should_warn_conflict(variants: &[SkillVariant]) -> bool {
    if variants.len() < 2 {
        return false;
    }
    let mut min = u128::MAX;
    let mut max = 0u128;
    let mut hashes = HashSet::new();
    for variant in variants {
        min = min.min(variant.digest.mtime);
        max = max.max(variant.digest.mtime);
        hashes.insert(variant.digest.body_hash);
    }
    hashes.len() > 1 && max.saturating_sub(min) <= CONFLICT_WINDOW_NS
}

#[allow(clippy::too_many_arguments)]
fn sync_skill_for_tool(
    tool: &'static str,
    enabled: bool,
    base_dir: &Path,
    name: &str,
    winner: &SkillVariant,
    variants: &[SkillVariant],
    log_mode: SyncLogMode,
    mode: ExecutionMode,
    history: &mut Option<HistoryRecorder>,
) -> io::Result<bool> {
    if !enabled {
        return Ok(false);
    }
    let target_path = base_dir.join(name);
    let existing = variants
        .iter()
        .find(|variant| variant.tool == tool)
        .map(|variant| variant.digest);
    sync_skill_target(
        &winner.path,
        winner.digest,
        existing,
        &target_path,
        log_mode,
        mode,
        history,
    )
}

fn select_skill_winner(variants: &[SkillVariant]) -> &SkillVariant {
    variants
        .iter()
        .max_by_key(|variant| (variant.digest.mtime, tool_order(variant.tool)))
        .expect("winner available")
}

fn sync_skill_target(
    source: &Path,
    source_digest: DirDigest,
    existing: Option<DirDigest>,
    target_path: &Path,
    log_mode: SyncLogMode,
    mode: ExecutionMode,
    history: &mut Option<HistoryRecorder>,
) -> io::Result<bool> {
    if existing.is_some_and(|digest| digest.body_hash == source_digest.body_hash) {
        return Ok(false);
    }
    if mode == ExecutionMode::Plan {
        log_action(
            log_mode,
            &format!("skills: would update {}", target_path.display()),
        );
        return Ok(true);
    }

    let before_state = if let Some(recorder) = history.as_ref() {
        Some(recorder.capture_path(target_path)?)
    } else {
        None
    };
    target_path.parent().map(fs::create_dir_all).transpose()?;

    let temp_path = skill_temp_path(target_path);
    if temp_path.exists() {
        fs::remove_dir_all(&temp_path)?;
    }
    copy_dir_all(source, &temp_path)?;

    let source_skill = source.join("SKILL.md");
    let target_skill = target_path.join("SKILL.md");
    merge_skill_frontmatter(&source_skill, &target_skill, &temp_path, history)?;

    if target_path.exists() {
        fs::remove_dir_all(target_path)?;
    }
    fs::rename(&temp_path, target_path)?;

    if let Some(recorder) = history.as_mut() {
        let after_state = recorder.capture_path(target_path)?;
        recorder.record_change(
            target_path,
            before_state.unwrap_or_else(crate::history::EntityState::missing),
            after_state,
        );
    }

    log_action(
        log_mode,
        &format!("skills: updated {}", target_path.display()),
    );
    Ok(true)
}

fn merge_skill_frontmatter(
    source_skill: &Path,
    target_skill: &Path,
    temp_path: &Path,
    _history: &mut Option<HistoryRecorder>,
) -> io::Result<()> {
    if !source_skill.exists() {
        return Ok(());
    }
    let source_doc = read_markdown(source_skill)?;
    let target_doc = target_skill
        .exists()
        .then(|| read_markdown(target_skill))
        .transpose()?;
    let merged = match target_doc {
        Some(target_doc) => merge_frontmatter(target_doc.frontmatter.as_deref(), &source_doc.body),
        None => source_doc.body,
    };
    let mut history = None;
    write_file(
        &temp_path.join("SKILL.md"),
        merged.as_bytes(),
        ExecutionMode::Apply,
        &mut history,
    )?;
    Ok(())
}

fn list_skill_dirs(dir: &Path) -> io::Result<HashMap<String, PathBuf>> {
    let mut out = HashMap::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if let Some((name, path, meta)) = read_visible_entry(entry, true)? {
            if meta.is_dir() && path.join("SKILL.md").exists() {
                out.insert(name, path);
            }
        }
    }
    Ok(out)
}

fn digest_skill_dir(dir: &Path) -> io::Result<DirDigest> {
    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    collect_skill_entries(dir, dir, &mut entries)?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut body_hasher = DefaultHasher::new();
    let mut mtime = 0u128;

    for (rel, path) in entries {
        let meta = fs::metadata(&path)?;
        let entry_mtime = file_mtime_value_from_meta(&meta);
        if entry_mtime > mtime {
            mtime = entry_mtime;
        }
        let file_name = path.file_name().and_then(|os| os.to_str()).unwrap_or("");
        if file_name == "SKILL.md" {
            let doc = read_markdown(&path)?;
            rel.hash(&mut body_hasher);
            doc.body_hash.hash(&mut body_hasher);
        } else {
            let bytes = fs::read(&path)?;
            let hash = hash_bytes(&bytes);
            rel.hash(&mut body_hasher);
            hash.hash(&mut body_hasher);
        }
    }

    Ok(DirDigest {
        body_hash: body_hasher.finish(),
        mtime,
    })
}

fn collect_skill_entries(
    root: &Path,
    dir: &Path,
    entries: &mut Vec<(String, PathBuf)>,
) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if let Some((_name, path, meta)) = read_visible_entry(entry, false)? {
            if meta.is_dir() {
                collect_skill_entries(root, &path, entries)?;
            } else if meta.is_file() {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string();
                entries.push((rel, path));
            }
        }
    }
    Ok(())
}

fn copy_dir_all(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let from = entry.path();
        let name = entry.file_name();
        if name
            .to_str()
            .map(|name| name.starts_with('.'))
            .unwrap_or(false)
        {
            continue;
        }
        let to = dst.join(name);
        if file_type.is_dir() {
            copy_dir_all(&from, &to)?;
        } else if file_type.is_file() {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[inline(never)]
fn skill_temp_path(target: &Path) -> PathBuf {
    let stamp = env::var("RELAY_TEST_TEMP_STAMP")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        });
    let name = target
        .file_name()
        .and_then(|os| os.to_str())
        .unwrap_or("skill");
    let temp_name = format!("{name}.relay.tmp.{stamp}");
    target
        .parent()
        .map(|parent| parent.join(&temp_name))
        .unwrap_or_else(|| PathBuf::from(temp_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::test_support::{doc, setup, write_plain, write_skill};
    use tempfile::TempDir;

    #[test]
    fn sync_skills_copies_dir_and_preserves_skill_frontmatter() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;

        let claude_doc = doc("claude", "Claude skill body");
        let codex_doc = doc("codex", "Codex skill body");
        let claude_skill = write_skill(&cfg.claude_skills_dir, "plan", &claude_doc)?;
        let codex_skill = write_skill(&cfg.codex_skills_dir, "plan", &codex_doc)?;
        write_plain(&codex_skill.join("scripts/run.sh"), "echo hi")?;

        crate::sync::test_support::set_mtime(&codex_skill.join("SKILL.md"), 2_000_000_100)?;

        sync_skills(&cfg, SyncLogMode::Quiet)?;

        let claude_skill_doc = read_markdown(&claude_skill.join("SKILL.md"))?;
        assert_eq!(claude_skill_doc.body, "Codex skill body");
        assert!(claude_skill_doc
            .frontmatter
            .unwrap_or_default()
            .contains("name: claude"));
        let script = fs::read_to_string(claude_skill.join("scripts/run.sh"))?;
        assert_eq!(script, "echo hi");
        Ok(())
    }

    #[test]
    fn sync_skills_handles_existing_central_dir() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;

        let _claude_skill = write_skill(&cfg.claude_skills_dir, "plan", &doc("claude", "Body"))?;
        let _central_skill = write_skill(&cfg.central_skills_dir, "plan", &doc("central", "Old"))?;

        sync_skills(&cfg, SyncLogMode::Quiet)?;
        Ok(())
    }

    #[test]
    fn sync_skills_opencode_wins() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let claude_skill = write_skill(&cfg.claude_skills_dir, "plan", &doc("claude", "Old"))?;
        let opencode_skill =
            write_skill(&cfg.opencode_skills_dir, "plan", &doc("opencode", "New"))?;
        write_plain(&opencode_skill.join("assets/logo.txt"), "asset")?;

        crate::sync::test_support::set_mtime(&opencode_skill.join("SKILL.md"), 2_000_000_300)?;

        sync_skills(&cfg, SyncLogMode::Quiet)?;

        let claude_doc = read_markdown(&claude_skill.join("SKILL.md"))?;
        assert_eq!(claude_doc.body, "New");
        assert!(claude_doc
            .frontmatter
            .unwrap_or_default()
            .contains("name: claude"));
        assert_eq!(
            fs::read_to_string(claude_skill.join("assets/logo.txt"))?,
            "asset"
        );
        Ok(())
    }

    #[test]
    fn sync_skills_central_wins_and_preserves_tool_frontmatter() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let claude_skill = write_skill(&cfg.claude_skills_dir, "plan", &doc("claude", "Old"))?;
        let central_skill = write_skill(&cfg.central_skills_dir, "plan", &doc("central", "New"))?;
        crate::sync::test_support::set_mtime(&central_skill.join("SKILL.md"), 2_300_000_100)?;

        sync_skills(&cfg, SyncLogMode::Quiet)?;

        let claude_doc = read_markdown(&claude_skill.join("SKILL.md"))?;
        assert_eq!(claude_doc.body, "New");
        assert!(claude_doc
            .frontmatter
            .unwrap_or_default()
            .contains("name: claude"));
        let central_doc = read_markdown(&central_skill.join("SKILL.md"))?;
        assert!(central_doc
            .frontmatter
            .unwrap_or_default()
            .contains("name: central"));
        Ok(())
    }

    #[test]
    fn sync_skills_central_roundtrip_is_stable() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let central_skill = write_skill(&cfg.central_skills_dir, "plan", &doc("central", "Body"))?;
        sync_skills(&cfg, SyncLogMode::Quiet)?;
        let central_doc = read_markdown(&central_skill.join("SKILL.md"))?;
        assert_eq!(central_doc.body, "Body");
        Ok(())
    }

    #[test]
    fn sync_skills_skips_missing_tool_dir() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        fs::remove_dir_all(&cfg.opencode_skills_dir)?;
        write_skill(&cfg.claude_skills_dir, "plan", &doc("claude", "Body"))?;
        sync_skills(&cfg, SyncLogMode::Quiet)?;
        assert!(!cfg.opencode_skills_dir.exists());
        Ok(())
    }

    #[test]
    fn list_skill_dirs_filters_missing_skill_md() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let dir = tmp.path().join("skills");
        fs::create_dir_all(&dir)?;
        let missing = dir.join("missing");
        fs::create_dir_all(&missing)?;
        let _valid = write_skill(&dir, "valid", "skill")?;
        let _hidden = write_skill(&dir, ".hidden", "hidden")?;
        write_plain(&dir.join("notadir"), "file")?;

        let list = list_skill_dirs(&dir)?;
        assert!(list.contains_key("valid"));
        assert!(!list.contains_key("missing"));
        Ok(())
    }

    #[test]
    fn sync_skill_target_skips_when_unchanged() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let source = write_skill(tmp.path(), "source", &doc("x", "Body"))?;
        let digest = digest_skill_dir(&source)?;

        let target = tmp.path().join("target");
        let mut history = None;
        let updated = sync_skill_target(
            &source,
            digest,
            Some(digest),
            &target,
            SyncLogMode::Quiet,
            ExecutionMode::Apply,
            &mut history,
        )?;
        assert!(!updated);
        Ok(())
    }

    #[test]
    fn merge_skill_frontmatter_skips_missing_source() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let source = tmp.path().join("missing/SKILL.md");
        let target = tmp.path().join("target/SKILL.md");
        let temp = tmp.path().join("temp");
        fs::create_dir_all(&temp)?;
        write_plain(&target, &doc("target", "Body"))?;
        let mut history = None;
        merge_skill_frontmatter(&source, &target, &temp, &mut history)?;
        Ok(())
    }

    #[test]
    #[inline(never)]
    fn sync_skill_target_merges_frontmatter_and_replaces_existing() -> io::Result<()> {
        let _lock = crate::ENV_LOCK.lock().unwrap();
        let tmp = TempDir::new()?;
        let source = write_skill(tmp.path(), "source", &doc("source", "New"))?;
        let target = write_skill(tmp.path(), "target", &doc("target", "Old"))?;

        std::env::set_var("RELAY_TEST_TEMP_STAMP", "123");
        let temp = skill_temp_path(&target);
        fs::create_dir_all(&temp)?;

        let digest = digest_skill_dir(&source)?;
        let existing = digest_skill_dir(&target)?;
        let mut history = None;
        sync_skill_target(
            &source,
            digest,
            Some(existing),
            &target,
            SyncLogMode::Quiet,
            ExecutionMode::Apply,
            &mut history,
        )?;

        let updated = fs::read_to_string(target.join("SKILL.md"))?;
        assert!(updated.contains("name: target"));
        assert!(updated.contains("New"));
        std::env::remove_var("RELAY_TEST_TEMP_STAMP");
        Ok(())
    }

    #[test]
    fn skill_temp_path_without_parent_uses_fallback() {
        let _lock = crate::ENV_LOCK.lock().unwrap();
        std::env::set_var("RELAY_TEST_TEMP_STAMP", "77");
        let temp = skill_temp_path(Path::new("/"));
        assert_eq!(temp, PathBuf::from("skill.relay.tmp.77"));
        std::env::remove_var("RELAY_TEST_TEMP_STAMP");
    }

    #[test]
    fn collect_skill_entries_skips_hidden() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let root = tmp.path().join("skill");
        fs::create_dir_all(&root)?;
        write_plain(&root.join("SKILL.md"), "skill")?;
        write_plain(&root.join(".hidden"), "skip")?;
        let target = root.join("target");
        write_plain(&target, "file")?;
        let mut entries = Vec::new();
        collect_skill_entries(&root, &root, &mut entries)?;
        assert!(entries.iter().any(|(name, _)| name.ends_with("SKILL.md")));
        Ok(())
    }

    #[test]
    fn copy_dir_all_skips_hidden() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        fs::create_dir_all(&src)?;
        write_plain(&src.join(".hidden"), "skip")?;
        write_plain(&src.join("file"), "ok")?;
        copy_dir_all(&src, &dst)?;
        assert!(dst.join("file").exists());
        assert!(!dst.join(".hidden").exists());
        Ok(())
    }
}
