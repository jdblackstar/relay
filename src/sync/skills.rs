use super::shared::{
    collect_names, conflict_for_variants, file_mtime_value_from_meta, hash_bytes, log_action,
    read_markdown, read_visible_entry, required_frontmatter_hash, select_frontmatter_for_target,
    tool_order, write_file, TOOL_CENTRAL,
};
use super::{ExecutionMode, LogMode as SyncLogMode, SyncConflict, SyncItemKind, SyncStats};
use crate::config::{Config, TOOL_CLAUDE, TOOL_CODEX, TOOL_OPENCODE};
use crate::history::HistoryRecorder;
use crate::markers::is_relay_generated_command_skill;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap, HashSet};
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

#[derive(Debug, Default, Serialize, Deserialize)]
struct SkillState {
    #[serde(default)]
    skills: BTreeMap<String, SkillStateEntry>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SkillStateEntry {
    canonical_hash: Option<i64>,
    #[serde(default)]
    tombstoned: bool,
    #[serde(default)]
    adapter_hashes: BTreeMap<String, i64>,
}

fn persisted_hash(hash: u64) -> i64 {
    i64::from_ne_bytes(hash.to_ne_bytes())
}

struct SkillLocation {
    label: &'static str,
    path: PathBuf,
    adapter: bool,
    import_managed: bool,
}

pub(crate) fn diagnostics(cfg: &Config) -> io::Result<Vec<String>> {
    let canonical = list_skills_if_exists(&cfg.central_skills_dir, true)?;
    let state = load_skill_state(&cfg.skill_state_path()?)?;
    let tombstones = state
        .skills
        .values()
        .filter(|entry| entry.tombstoned)
        .count();
    let mut lines = vec![format!(
        "skills: canonical={} count={} tombstones={}",
        cfg.central_skills_dir.display(),
        canonical.len(),
        tombstones
    )];
    for location in skill_locations(cfg)? {
        let role = if location.adapter {
            "adapter"
        } else {
            "import"
        };
        let found = list_skills_if_exists(&location.path, location.import_managed)?;
        let mut collisions = 0usize;
        let mut owned = 0usize;
        let mut divergent = 0usize;
        if !location.adapter {
            for (name, path) in &found {
                if let Some(canonical_path) = canonical.get(name) {
                    if digest_skill_dir(path)?.body_hash
                        != digest_skill_dir(canonical_path)?.body_hash
                    {
                        collisions += 1;
                    }
                }
            }
        } else {
            for (name, path) in &found {
                if let Some(expected) = state
                    .skills
                    .get(name)
                    .and_then(|entry| entry.adapter_hashes.get(location.label))
                {
                    owned += 1;
                    if *expected != persisted_hash(digest_skill_dir(path)?.body_hash) {
                        divergent += 1;
                    }
                }
            }
        }
        lines.push(format!(
            "skills: {} role={} path={} count={} owned={} divergent={} collisions={}",
            location.label,
            role,
            location.path.display(),
            found.len(),
            owned,
            divergent,
            collisions
        ));
    }
    Ok(lines)
}

fn skill_locations(cfg: &Config) -> io::Result<Vec<SkillLocation>> {
    let mut out = Vec::new();
    let mut push = |label: &'static str, path: PathBuf, adapter: bool, import_managed: bool| {
        if path == cfg.central_skills_dir
            || out.iter().any(|item: &SkillLocation| item.path == path)
        {
            return;
        }
        out.push(SkillLocation {
            label,
            path,
            adapter,
            import_managed,
        });
    };

    if cfg.tool_enabled(TOOL_CLAUDE) {
        push(TOOL_CLAUDE, cfg.claude_skills_dir.clone(), true, true);
    }
    for (label, enabled, path) in [
        (
            TOOL_CODEX,
            cfg.tool_enabled(TOOL_CODEX),
            &cfg.codex_skills_dir,
        ),
        (
            TOOL_OPENCODE,
            cfg.tool_enabled(TOOL_OPENCODE),
            &cfg.opencode_skills_dir,
        ),
    ] {
        if enabled {
            let legacy_native = cfg.is_legacy_skill_import_dir(path)?;
            push(label, path.clone(), !legacy_native, !legacy_native);
        }
    }
    for path in cfg.legacy_skill_import_dirs()? {
        push("migration", path, false, false);
    }
    Ok(out)
}

fn list_skills_if_exists(dir: &Path, import_managed: bool) -> io::Result<HashMap<String, PathBuf>> {
    if !dir.exists() {
        return Ok(HashMap::new());
    }
    list_skill_dirs_with_policy(dir, import_managed)
}

fn load_skill_state(path: &Path) -> io::Result<SkillState> {
    if !path.exists() {
        return Ok(SkillState::default());
    }
    let raw = fs::read_to_string(path)?;
    toml::from_str(&raw).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid skill state in {}: {err}", path.display()),
        )
    })
}

fn save_skill_state(path: &Path, state: &SkillState) -> io::Result<()> {
    path.parent().map(fs::create_dir_all).transpose()?;
    let raw = toml::to_string_pretty(state)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    crate::atomic::write_atomic(path, raw.as_bytes())
}

fn remove_skill_target(
    path: &Path,
    log_mode: SyncLogMode,
    mode: ExecutionMode,
    history: &mut Option<HistoryRecorder>,
) -> io::Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    if mode == ExecutionMode::Plan {
        log_action(
            log_mode,
            &format!("skills: would remove {}", path.display()),
        );
        return Ok(true);
    }
    let before = history
        .as_ref()
        .map(|recorder| recorder.capture_path(path))
        .transpose()?;
    fs::remove_dir_all(path)?;
    if let (Some(recorder), Some(before)) = (history.as_mut(), before) {
        recorder.record_change(path, before, crate::history::EntityState::missing());
    }
    log_action(log_mode, &format!("skills: removed {}", path.display()));
    Ok(true)
}

impl super::shared::ConflictVariant for SkillVariant {
    fn tool(&self) -> &'static str {
        self.tool
    }

    fn hash(&self) -> u64 {
        self.digest.body_hash
    }

    fn mtime(&self) -> u128 {
        self.digest.mtime
    }
}

#[cfg(any(test, coverage))]
pub(crate) fn sync_skills(cfg: &Config, log_mode: SyncLogMode) -> io::Result<SyncStats> {
    let mut history = None;
    let mut conflicts = Vec::new();
    sync_skills_with_mode(
        cfg,
        log_mode,
        ExecutionMode::Apply,
        &mut history,
        &mut conflicts,
    )
}

pub(crate) fn sync_skills_with_mode(
    cfg: &Config,
    log_mode: SyncLogMode,
    mode: ExecutionMode,
    history: &mut Option<HistoryRecorder>,
    conflicts: &mut Vec<SyncConflict>,
) -> io::Result<SyncStats> {
    let mut stats = SyncStats::default();

    let state_path = cfg.skill_state_path()?;
    let mut state = load_skill_state(&state_path)?;
    let central = list_skills_if_exists(&cfg.central_skills_dir, true)?;
    let locations = skill_locations(cfg)?;
    let mut location_maps = Vec::new();
    for location in &locations {
        location_maps.push(list_skills_if_exists(
            &location.path,
            location.import_managed,
        )?);
    }

    let mut names = collect_names(&[&central]);
    for map in &location_maps {
        names.extend(map.keys().cloned());
    }
    names.extend(state.skills.keys().cloned());

    for name in names {
        let central_path = cfg.central_skills_dir.join(&name);
        let canonical = central
            .get(&name)
            .map(|path| {
                Ok::<_, io::Error>(SkillVariant {
                    tool: TOOL_CENTRAL,
                    path: path.clone(),
                    digest: digest_skill_dir(path)?,
                })
            })
            .transpose()?;
        let entry = state.skills.entry(name.clone()).or_default();

        // Recreating a tombstoned canonical skill is an explicit restore. Drop
        // stale adapter ownership before reconciliation so a preserved adapter
        // cannot overwrite the newly restored canonical contents.
        if entry.tombstoned {
            if let Some(canonical) = canonical.as_ref() {
                entry.tombstoned = false;
                entry.canonical_hash = Some(persisted_hash(canonical.digest.body_hash));
                entry.adapter_hashes.clear();
            }
        }

        // A previously observed canonical skill disappearing is an explicit
        // deletion. Keep a tombstone so stale native copies cannot resurrect it.
        if canonical.is_none() && entry.canonical_hash.is_some() {
            entry.canonical_hash = None;
            entry.tombstoned = true;
            for (location, map) in locations.iter().zip(&location_maps) {
                if !location.adapter {
                    continue;
                }
                let Some(path) = map.get(&name) else { continue };
                let digest = digest_skill_dir(path)?;
                let owned = entry
                    .adapter_hashes
                    .get(location.label)
                    .is_some_and(|hash| *hash == persisted_hash(digest.body_hash));
                if owned {
                    stats.updated +=
                        usize::from(remove_skill_target(path, log_mode, mode, history)?);
                    entry.adapter_hashes.remove(location.label);
                } else {
                    log_action(log_mode, &format!(
                        "warning: skills '{name}' deleted centrally but modified adapter {} was preserved",
                        location.label
                    ));
                }
            }
            continue;
        }

        let mut canonical = canonical;
        let mut sources = Vec::new();
        for (location, map) in locations.iter().zip(&location_maps) {
            let Some(path) = map.get(&name) else { continue };
            sources.push(SkillVariant {
                tool: location.label,
                path: path.clone(),
                digest: digest_skill_dir(path)?,
            });
        }

        // A compatibility adapter can be an authoring location. Accept its
        // edit only when the canonical copy has not also changed since the last
        // reconciliation; simultaneous edits are reported and canonical wins.
        if let Some(current) = canonical.as_ref() {
            let current_digest = current.digest;
            let canonical_changed = entry
                .canonical_hash
                .is_some_and(|hash| hash != persisted_hash(current_digest.body_hash));
            let changed_adapters: Vec<&SkillVariant> = sources
                .iter()
                .filter(|source| {
                    locations
                        .iter()
                        .any(|location| location.label == source.tool && location.adapter)
                        && entry
                            .adapter_hashes
                            .get(source.tool)
                            .is_some_and(|hash| *hash != persisted_hash(source.digest.body_hash))
                })
                .collect();
            if canonical_changed {
                for source in changed_adapters
                    .iter()
                    .filter(|source| source.digest.body_hash != current_digest.body_hash)
                {
                    conflicts.push(SyncConflict {
                        kind: SyncItemKind::Skill,
                        name: name.clone(),
                        winner: TOOL_CENTRAL,
                        others: vec![source.tool],
                    });
                    log_action(log_mode, &format!(
                        "warning: skills '{name}' changed in canonical store and {}; canonical store won",
                        source.tool
                    ));
                }
            } else if let Some(winner) = changed_adapters
                .iter()
                .max_by_key(|source| (source.digest.mtime, tool_order(source.tool)))
            {
                let others: Vec<&'static str> = changed_adapters
                    .iter()
                    .filter(|source| {
                        source.tool != winner.tool
                            && source.digest.body_hash != winner.digest.body_hash
                    })
                    .map(|source| source.tool)
                    .collect();
                if !others.is_empty() {
                    conflicts.push(SyncConflict {
                        kind: SyncItemKind::Skill,
                        name: name.clone(),
                        winner: winner.tool,
                        others,
                    });
                    log_action(
                        log_mode,
                        &format!(
                            "warning: skills '{name}' changed in multiple adapters; newest adapter {} won",
                            winner.tool
                        ),
                    );
                }
                stats.updated += usize::from(sync_skill_target(
                    &winner.path,
                    winner.digest,
                    Some(current_digest),
                    &central_path,
                    log_mode,
                    mode,
                    history,
                )?);
                canonical = Some(SkillVariant {
                    tool: TOOL_CENTRAL,
                    path: central_path.clone(),
                    digest: winner.digest,
                });
            }
            for source in sources.iter().filter(|source| {
                locations
                    .iter()
                    .any(|location| location.label == source.tool && !location.adapter)
            }) {
                if source.digest.body_hash != current_digest.body_hash {
                    log_action(log_mode, &format!(
                        "warning: skills '{name}' in import-only {} collides with canonical store; canonical store won",
                        source.tool
                    ));
                }
            }
        }

        if canonical.is_none() && !entry.tombstoned && !sources.is_empty() {
            let winner = select_skill_winner(&sources);
            if let Some(conflict) = conflict_for_variants(
                &name,
                SyncItemKind::Skill,
                &sources,
                winner.tool,
                winner.digest.body_hash,
            ) {
                conflicts.push(conflict);
            }
            stats.updated += usize::from(sync_skill_target(
                &winner.path,
                winner.digest,
                None,
                &central_path,
                log_mode,
                mode,
                history,
            )?);
            canonical = Some(SkillVariant {
                tool: TOOL_CENTRAL,
                path: central_path.clone(),
                digest: winner.digest,
            });
            log_action(
                log_mode,
                &format!(
                    "skills: imported '{name}' from {} into canonical store",
                    winner.tool
                ),
            );
        }

        let Some(canonical) = canonical else { continue };
        entry.tombstoned = false;
        entry.canonical_hash = Some(persisted_hash(canonical.digest.body_hash));
        for (location, map) in locations.iter().zip(&location_maps) {
            if !location.adapter || cfg.is_blacklisted(&format!("skills/{name}"), location.label) {
                continue;
            }
            let existing = map
                .get(&name)
                .map(|path| digest_skill_dir(path))
                .transpose()?;
            stats.updated += usize::from(sync_skill_target(
                &canonical.path,
                canonical.digest,
                existing,
                &location.path.join(&name),
                log_mode,
                mode,
                history,
            )?);
            entry.adapter_hashes.insert(
                location.label.to_string(),
                persisted_hash(canonical.digest.body_hash),
            );
        }
    }

    if mode == ExecutionMode::Apply {
        save_skill_state(&state_path, &state)?;
    }
    Ok(stats)
}

pub(super) fn codex_real_skill_names(cfg: &Config) -> io::Result<HashSet<String>> {
    let mut names = HashSet::new();
    if !codex_skills_target_enabled(cfg) {
        return Ok(names);
    }

    if cfg.central_skills_dir.exists() {
        for name in list_skill_dirs(&cfg.central_skills_dir)?.into_keys() {
            if !cfg.is_blacklisted(&format!("skills/{name}"), TOOL_CODEX) {
                names.insert(name);
            }
        }
    }

    Ok(names)
}

pub(super) fn codex_skills_target_enabled(cfg: &Config) -> bool {
    cfg.tool_enabled(TOOL_CODEX)
        && cfg
            .codex_skills_dir
            .parent()
            .is_some_and(|parent| parent.exists())
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

    let before_state = history
        .as_ref()
        .map(|recorder| recorder.capture_path(target_path))
        .transpose()?;
    target_path.parent().map(fs::create_dir_all).transpose()?;

    let temp_path = skill_temp_path(target_path);
    if temp_path.exists() {
        fs::remove_dir_all(&temp_path)?;
    }
    copy_dir_all(source, &temp_path)?;

    let source_skill = source.join("SKILL.md");
    let target_skill = target_path.join("SKILL.md");
    merge_skill_frontmatter(&source_skill, &target_skill, &temp_path, log_mode)?;

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
    log_mode: SyncLogMode,
) -> io::Result<()> {
    if !source_skill.exists() {
        return Ok(());
    }
    let source_doc = read_markdown(source_skill)?;
    let target_doc = target_skill
        .exists()
        .then(|| read_markdown(target_skill))
        .transpose()?;
    let label = format!("skills: {}", temp_path.join("SKILL.md").display());
    let frontmatter =
        select_frontmatter_for_target(&source_doc, target_doc.as_ref(), true, log_mode, &label);
    let merged = super::shared::merge_frontmatter(frontmatter.as_deref(), &source_doc.body);
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
    list_skill_dirs_with_policy(dir, true)
}

fn list_skill_dirs_with_policy(
    dir: &Path,
    import_managed: bool,
) -> io::Result<HashMap<String, PathBuf>> {
    let mut out = HashMap::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if !import_managed && entry.file_type()?.is_symlink() {
            continue;
        }
        if let Some((name, path, meta)) = read_visible_entry(entry, true)? {
            if meta.is_dir()
                && path.join("SKILL.md").exists()
                && !is_relay_generated_command_skill(&path)
                && (import_managed || !looks_plugin_managed(&path))
            {
                out.insert(name, path);
            }
        }
    }
    Ok(out)
}

fn looks_plugin_managed(path: &Path) -> bool {
    [".system", ".plugin", ".managed", ".relay-managed"]
        .iter()
        .any(|marker| path.join(marker).exists())
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
            required_frontmatter_hash(&doc)
                .unwrap_or(0)
                .hash(&mut body_hasher);
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
    use crate::markers::RELAY_COMMAND_SKILL_MARKER;
    use crate::sync::test_support::{doc, setup, write_plain, write_skill};
    use tempfile::TempDir;

    #[test]
    fn sync_skills_copies_dir_and_syncs_required_frontmatter() -> io::Result<()> {
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
            .contains("name: codex"));
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
            .contains("name: opencode"));
        assert_eq!(
            fs::read_to_string(claude_skill.join("assets/logo.txt"))?,
            "asset"
        );
        Ok(())
    }

    #[test]
    fn sync_skills_central_wins_and_syncs_required_frontmatter() -> io::Result<()> {
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
            .contains("name: central"));
        let central_doc = read_markdown(&central_skill.join("SKILL.md"))?;
        assert!(central_doc
            .frontmatter
            .unwrap_or_default()
            .contains("name: central"));
        Ok(())
    }

    #[test]
    fn sync_skills_creates_missing_codex_skills_dir() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        fs::remove_dir_all(&cfg.codex_skills_dir)?;
        write_skill(&cfg.central_skills_dir, "plan", &doc("central", "Body"))?;

        sync_skills(&cfg, SyncLogMode::Quiet)?;

        assert_eq!(
            read_markdown(&cfg.codex_skills_dir.join("plan/SKILL.md"))?.body,
            "Body"
        );
        Ok(())
    }

    #[test]
    fn sync_skills_blacklist_skips_tool_but_syncs_others() -> io::Result<()> {
        let (_tmp, mut cfg) = setup()?;

        let claude_skill = write_skill(&cfg.claude_skills_dir, "plan", &doc("claude", "Body"))?;

        // Blacklist skills/plan from codex
        cfg.blacklist
            .entry("skills/plan".to_string())
            .or_default()
            .push("codex".to_string());

        sync_skills(&cfg, SyncLogMode::Quiet)?;

        // Central should get it
        assert!(cfg.central_skills_dir.join("plan/SKILL.md").exists());
        // Claude should keep it
        assert!(claude_skill.join("SKILL.md").exists());
        // Codex should NOT get it
        assert!(!cfg.codex_skills_dir.join("plan/SKILL.md").exists());
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
    fn sync_skills_creates_missing_custom_adapter_dir() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        fs::remove_dir_all(&cfg.opencode_skills_dir)?;
        write_skill(&cfg.claude_skills_dir, "plan", &doc("claude", "Body"))?;
        sync_skills(&cfg, SyncLogMode::Quiet)?;
        assert!(cfg.opencode_skills_dir.join("plan/SKILL.md").exists());
        Ok(())
    }

    #[test]
    fn canonical_deletion_tombstones_and_removes_owned_adapters() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let central = write_skill(&cfg.central_skills_dir, "plan", &doc("plan", "Body"))?;
        sync_skills(&cfg, SyncLogMode::Quiet)?;
        assert!(cfg.claude_skills_dir.join("plan/SKILL.md").exists());

        fs::remove_dir_all(central)?;
        sync_skills(&cfg, SyncLogMode::Quiet)?;
        assert!(!cfg.claude_skills_dir.join("plan").exists());
        assert!(!cfg.codex_skills_dir.join("plan").exists());
        assert!(!cfg.opencode_skills_dir.join("plan").exists());

        sync_skills(&cfg, SyncLogMode::Quiet)?;
        assert!(!cfg.central_skills_dir.join("plan").exists());
        let state = load_skill_state(&cfg.skill_state_path()?)?;
        assert!(state.skills["plan"].tombstoned);
        Ok(())
    }

    #[test]
    fn canonical_deletion_preserves_modified_adapter() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let central = write_skill(&cfg.central_skills_dir, "plan", &doc("plan", "Body"))?;
        sync_skills(&cfg, SyncLogMode::Quiet)?;
        write_plain(
            &cfg.claude_skills_dir.join("plan/SKILL.md"),
            &doc("plan", "Locally changed"),
        )?;
        fs::remove_dir_all(central)?;

        sync_skills(&cfg, SyncLogMode::Quiet)?;

        assert!(cfg.claude_skills_dir.join("plan/SKILL.md").exists());
        assert!(!cfg.central_skills_dir.join("plan").exists());
        Ok(())
    }

    #[test]
    fn recreated_canonical_skill_wins_over_preserved_adapter() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let central = write_skill(&cfg.central_skills_dir, "plan", &doc("plan", "Original"))?;
        sync_skills(&cfg, SyncLogMode::Quiet)?;
        write_plain(
            &cfg.claude_skills_dir.join("plan/SKILL.md"),
            &doc("plan", "Preserved adapter edit"),
        )?;
        fs::remove_dir_all(central)?;
        sync_skills(&cfg, SyncLogMode::Quiet)?;
        assert!(cfg.claude_skills_dir.join("plan/SKILL.md").exists());

        write_skill(
            &cfg.central_skills_dir,
            "plan",
            &doc("plan", "Restored canonical"),
        )?;
        sync_skills(&cfg, SyncLogMode::Quiet)?;

        assert_eq!(
            read_markdown(&cfg.central_skills_dir.join("plan/SKILL.md"))?.body,
            "Restored canonical"
        );
        assert_eq!(
            read_markdown(&cfg.claude_skills_dir.join("plan/SKILL.md"))?.body,
            "Restored canonical"
        );
        Ok(())
    }

    #[test]
    fn adapter_edit_updates_unchanged_canonical_skill() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        write_skill(&cfg.central_skills_dir, "plan", &doc("plan", "Old"))?;
        sync_skills(&cfg, SyncLogMode::Quiet)?;
        write_plain(
            &cfg.claude_skills_dir.join("plan/SKILL.md"),
            &doc("plan", "New"),
        )?;

        sync_skills(&cfg, SyncLogMode::Quiet)?;

        assert_eq!(
            read_markdown(&cfg.central_skills_dir.join("plan/SKILL.md"))?.body,
            "New"
        );
        Ok(())
    }

    #[test]
    fn competing_adapter_edits_report_conflict_and_choose_newest() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        write_skill(&cfg.central_skills_dir, "plan", &doc("plan", "Old"))?;
        sync_skills(&cfg, SyncLogMode::Quiet)?;
        let claude = cfg.claude_skills_dir.join("plan/SKILL.md");
        let codex = cfg.codex_skills_dir.join("plan/SKILL.md");
        write_plain(&claude, &doc("plan", "Claude edit"))?;
        write_plain(&codex, &doc("plan", "Codex edit"))?;
        crate::sync::test_support::set_mtime(&claude, 101)?;
        crate::sync::test_support::set_mtime(&codex, 102)?;
        let mut history = None;
        let mut conflicts = Vec::new();

        sync_skills_with_mode(
            &cfg,
            SyncLogMode::Quiet,
            ExecutionMode::Apply,
            &mut history,
            &mut conflicts,
        )?;

        assert_eq!(
            read_markdown(&cfg.central_skills_dir.join("plan/SKILL.md"))?.body,
            "Codex edit"
        );
        assert!(conflicts.contains(&SyncConflict {
            kind: SyncItemKind::Skill,
            name: "plan".to_string(),
            winner: TOOL_CODEX,
            others: vec![TOOL_CLAUDE],
        }));
        Ok(())
    }

    #[test]
    fn import_policy_skips_symlinked_and_managed_skills() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let source = tmp.path().join("native");
        fs::create_dir_all(&source)?;
        write_skill(&source, "owned", &doc("owned", "Owned"))?;
        let managed = write_skill(&source, "managed", &doc("managed", "Managed"))?;
        write_plain(&managed.join(".plugin"), "managed")?;
        #[cfg(unix)]
        {
            let real = write_skill(tmp.path(), "real", &doc("real", "Real"))?;
            std::os::unix::fs::symlink(real, source.join("linked"))?;
        }

        let found = list_skill_dirs_with_policy(&source, false)?;
        assert!(found.contains_key("owned"));
        assert!(!found.contains_key("managed"));
        assert!(!found.contains_key("linked"));
        Ok(())
    }

    #[test]
    fn diagnostics_reports_roles_and_tombstones() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        write_skill(&cfg.central_skills_dir, "plan", &doc("plan", "Body"))?;
        sync_skills(&cfg, SyncLogMode::Quiet)?;
        let lines = diagnostics(&cfg)?;
        assert!(lines[0].contains("count=1"));
        assert!(lines
            .iter()
            .any(|line| line.contains("claude role=adapter")));
        Ok(())
    }

    #[test]
    fn default_migration_imports_user_skills_without_redundant_native_copies() -> io::Result<()> {
        let _lock = crate::ENV_LOCK.lock().unwrap();
        let tmp = TempDir::new()?;
        let home = tmp.path().join("home");
        fs::create_dir_all(&home)?;
        std::env::set_var("RELAY_HOME", &home);
        std::env::remove_var("CODEX_HOME");
        std::env::remove_var("CLAUDE_HOME");
        std::env::remove_var("OPENCODE_HOME");
        let cfg = Config::default_paths()?;
        let legacy_relay = home.join(".config/relay/skills");
        let legacy_codex = home.join(".codex/skills");
        write_skill(&legacy_relay, "from-relay", &doc("from-relay", "Relay"))?;
        write_skill(&legacy_codex, "from-native", &doc("from-native", "Native"))?;
        let managed = write_skill(&legacy_codex, "plugin", &doc("plugin", "Plugin"))?;
        write_plain(&managed.join(".plugin"), "managed")?;
        fs::create_dir_all(&cfg.claude_skills_dir)?;

        sync_skills(&cfg, SyncLogMode::Quiet)?;

        assert!(cfg.central_skills_dir.join("from-relay/SKILL.md").exists());
        assert!(cfg.central_skills_dir.join("from-native/SKILL.md").exists());
        assert!(!cfg.central_skills_dir.join("plugin").exists());
        assert!(cfg.claude_skills_dir.join("from-relay/SKILL.md").exists());
        assert!(!legacy_codex.join("from-relay").exists());
        assert!(!home.join(".config/opencode/skill/from-relay").exists());

        std::env::remove_var("RELAY_HOME");
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
        let generated = write_skill(&dir, "generated", "generated")?;
        write_plain(&generated.join(RELAY_COMMAND_SKILL_MARKER), "generated")?;
        write_plain(&dir.join("notadir"), "file")?;

        let list = list_skill_dirs(&dir)?;
        assert!(list.contains_key("valid"));
        assert!(!list.contains_key("missing"));
        assert!(!list.contains_key("generated"));
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
    fn sync_skills_collects_conflict_details() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let claude_skill = write_skill(&cfg.claude_skills_dir, "plan", &doc("claude", "Old"))?;
        let codex_skill = write_skill(&cfg.codex_skills_dir, "plan", &doc("codex", "New"))?;
        crate::sync::test_support::set_mtime(&claude_skill.join("SKILL.md"), 100)?;
        crate::sync::test_support::set_mtime(&codex_skill.join("SKILL.md"), 101)?;

        let mut history = None;
        let mut conflicts = Vec::new();
        sync_skills_with_mode(
            &cfg,
            SyncLogMode::Quiet,
            ExecutionMode::Plan,
            &mut history,
            &mut conflicts,
        )?;

        assert_eq!(
            conflicts,
            vec![SyncConflict {
                kind: SyncItemKind::Skill,
                name: "plan".to_string(),
                winner: TOOL_CODEX,
                others: vec![TOOL_CLAUDE],
            }]
        );
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
        merge_skill_frontmatter(&source, &target, &temp, SyncLogMode::Quiet)?;
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
        assert!(updated.contains("name: source"));
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
