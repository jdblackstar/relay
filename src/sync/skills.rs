use super::shared::{
    collect_names, conflict_for_variants, file_mtime_value_from_meta, hash_bytes, log_action,
    parse_required_frontmatter, read_markdown, read_visible_entry, required_frontmatter_hash,
    select_frontmatter_for_target, tool_order, write_file, write_raw_if_changed, TOOL_CENTRAL,
};
use super::{ExecutionMode, LogMode as SyncLogMode, SyncConflict, SyncItemKind, SyncStats};
use crate::config::{Config, TOOL_CLAUDE, TOOL_CODEX, TOOL_OPENCODE};
use crate::history::HistoryRecorder;
use crate::markers::is_relay_generated_command_skill;
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::cell::Cell;
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::ffi::CString;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use unicode_normalization::UnicodeNormalization;

const TOOL_SELECTED: &str = "selected input";

#[cfg(test)]
thread_local! {
    static SKILL_ASSEMBLY_FAILURE_PHASE: Cell<Option<&'static str>> = const { Cell::new(None) };
}

#[derive(Debug, Clone, Copy)]
struct DirDigest {
    body_hash: u64,
    mtime: u128,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PackagePolicy {
    LegacyVisible,
    CompleteStrict,
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
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    adapter_complete_hashes: BTreeMap<String, i64>,
}

impl SkillStateEntry {
    fn record_adapter_ownership(
        &mut self,
        labels: &[&str],
        legacy_hash: i64,
        complete_hash: Option<i64>,
    ) {
        for label in labels {
            self.adapter_hashes
                .insert((*label).to_string(), legacy_hash);
            if let Some(complete_hash) = complete_hash {
                self.adapter_complete_hashes
                    .insert((*label).to_string(), complete_hash);
            }
        }
    }

    fn clear_adapter_ownership(&mut self, labels: &[&str]) {
        for label in labels {
            self.adapter_hashes.remove(*label);
            self.adapter_complete_hashes.remove(*label);
        }
    }
}

fn persisted_hash(hash: u64) -> i64 {
    i64::from_ne_bytes(hash.to_ne_bytes())
}

struct SkillLocation {
    label: &'static str,
    labels: Vec<&'static str>,
    path: PathBuf,
    adapter: bool,
    import_managed: bool,
}

impl SkillLocation {
    fn allowed_adapter_label(&self, cfg: &Config, skill_name: &str) -> Option<&'static str> {
        self.labels
            .iter()
            .copied()
            .find(|label| !cfg.is_blacklisted(&format!("skills/{skill_name}"), label))
    }
}

pub(crate) struct SkillSyncOutcome {
    pub(crate) stats: SyncStats,
    pub(crate) codex_real_skill_names: HashSet<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ScopedSkill {
    name: String,
    path: PathBuf,
    digest: DirDigest,
    sync_digest: DirDigest,
}

pub(crate) fn discover_scoped_skills(inputs: &[PathBuf]) -> io::Result<Vec<ScopedSkill>> {
    if inputs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "relay sync skill requires at least one path",
        ));
    }

    let mut roots = Vec::new();
    for input in inputs {
        let input_metadata = fs::symlink_metadata(input).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid skill path {}: {err}", input.display()),
            )
        })?;
        if input_metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "directory and file symlinks are not supported as skill inputs: {}",
                    input.display()
                ),
            ));
        }
        let canonical = fs::canonicalize(input).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid skill path {}: {err}", input.display()),
            )
        })?;
        let metadata = fs::metadata(&canonical).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid skill path {}: {err}", input.display()),
            )
        })?;
        let before = roots.len();
        if metadata.is_file() {
            if canonical.file_name().and_then(|name| name.to_str()) != Some("SKILL.md") {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "skill file must be named exactly SKILL.md: {}",
                        input.display()
                    ),
                ));
            }
            let root = canonical.parent().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("SKILL.md has no parent directory: {}", input.display()),
                )
            })?;
            discover_skill_roots(root, &mut roots)?;
        } else if metadata.is_dir() {
            discover_skill_roots(&canonical, &mut roots)?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("skill path is not a file or directory: {}", input.display()),
            ));
        }
        if roots.len() == before {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("skill path contains no skill packages: {}", input.display()),
            ));
        }
    }

    roots.sort();
    roots.dedup();
    for (index, ancestor) in roots.iter().enumerate() {
        if let Some(descendant) = roots
            .iter()
            .skip(index + 1)
            .find(|candidate| candidate.starts_with(ancestor))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "overlapping skill roots are ambiguous: {} contains {}",
                    ancestor.display(),
                    descendant.display()
                ),
            ));
        }
    }

    let mut by_name: BTreeMap<String, ScopedSkill> = BTreeMap::new();
    let mut by_portable_destination_name: BTreeMap<String, (String, PathBuf)> = BTreeMap::new();
    for root in roots {
        let directory_name = scoped_skill_directory_name(&root)?;
        let skill_file = root.join("SKILL.md");
        let skill_metadata = fs::symlink_metadata(&skill_file)?;
        if !skill_metadata.is_file() || skill_metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("expected a regular SKILL.md file: {}", skill_file.display()),
            ));
        }
        let doc = read_markdown(&skill_file)?;
        let required = parse_required_frontmatter(doc.frontmatter.as_deref()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "invalid skill frontmatter in {}: expected non-empty name and description",
                    skill_file.display()
                ),
            )
        })?;
        if required.name != directory_name {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "skill name '{}' does not match parent directory '{}': {}",
                    required.name,
                    directory_name,
                    skill_file.display()
                ),
            ));
        }
        let name = required.name;
        register_portable_selected_name(&mut by_portable_destination_name, &name, &root)?;
        let digest = digest_complete_skill_identity(&root)?;
        let sync_digest = digest_complete_skill_dir(&root)?;
        let selected = ScopedSkill {
            name: name.clone(),
            path: root.clone(),
            digest,
            sync_digest,
        };
        if let Some(existing) = by_name.get(&name) {
            if existing.path == root {
                continue;
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "multiple selected paths resolve to skill name '{name}': {} and {}",
                    existing.path.display(),
                    root.display()
                ),
            ));
        }
        by_name.insert(name, selected);
    }
    Ok(by_name.into_values().collect())
}

fn scoped_skill_directory_name(root: &Path) -> io::Result<String> {
    root.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "skill directory has no valid UTF-8 name: {}",
                    root.display()
                ),
            )
        })
}

fn portable_skill_destination_name(name: &str) -> String {
    name.nfd()
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .nfd()
        .collect()
}

fn register_portable_selected_name(
    destinations: &mut BTreeMap<String, (String, PathBuf)>,
    name: &str,
    root: &Path,
) -> io::Result<()> {
    let portable_name = portable_skill_destination_name(name);
    if let Some((existing_name, existing_path)) = destinations.get(&portable_name) {
        if existing_name != name && existing_path != root {
            let collision = portable_collision_description(existing_name, name);
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "selected skill names {collision} and are not portable: '{existing_name}' at {} and '{name}' at {}",
                    existing_path.display(),
                    root.display()
                ),
            ));
        }
    } else {
        destinations.insert(portable_name, (name.to_owned(), root.to_path_buf()));
    }
    Ok(())
}

fn portable_collision_description(existing_name: &str, selected_name: &str) -> &'static str {
    if existing_name.eq_ignore_ascii_case(selected_name) {
        "differ only by ASCII case"
    } else {
        "are not distinct on case-insensitive or Unicode-normalizing filesystems"
    }
}

fn validate_scoped_destination_names(
    cfg: &Config,
    selected: &[ScopedSkill],
    locations: &[SkillLocation],
) -> io::Result<()> {
    for skill in selected {
        let portable_selected_name = portable_skill_destination_name(&skill.name);
        let destinations = std::iter::once((TOOL_CENTRAL, cfg.central_skills_dir.as_path())).chain(
            locations
                .iter()
                .filter(|location| location.adapter)
                .filter_map(|location| {
                    location
                        .allowed_adapter_label(cfg, &skill.name)
                        .map(|label| (label, location.path.as_path()))
                }),
        );
        for (label, root) in destinations {
            let entries = match fs::read_dir(root) {
                Ok(entries) => entries,
                Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                Err(err) => return Err(err),
            };
            let mut existing = entries.collect::<Result<Vec<_>, _>>()?;
            existing.sort_by_key(|entry| entry.file_name());
            for entry in existing {
                if !entry.file_type()?.is_dir() {
                    continue;
                }
                let Some(existing_name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if existing_name == skill.name
                    || portable_skill_destination_name(&existing_name) != portable_selected_name
                {
                    continue;
                }
                let collision = portable_collision_description(&existing_name, &skill.name);
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "selected skill '{}' at {} and existing {label} destination directory '{}' at {} {collision}; refusing non-portable destination names",
                        skill.name,
                        skill.path.display(),
                        existing_name,
                        entry.path().display()
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn discover_skill_roots(dir: &Path, roots: &mut Vec<PathBuf>) -> io::Result<()> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)? {
        entries.push(entry?);
    }
    entries.sort_by_key(|entry| entry.file_name());

    if entries
        .iter()
        .any(|entry| entry.file_name() == std::ffi::OsStr::new("SKILL.md"))
    {
        roots.push(dir.to_path_buf());
    }

    for entry in entries {
        let file_type = entry.file_type()?;
        let name = entry.file_name();
        if !file_type.is_dir()
            || file_type.is_symlink()
            || name == std::ffi::OsStr::new(".git")
            || name == std::ffi::OsStr::new("node_modules")
        {
            continue;
        }
        discover_skill_roots(&entry.path(), roots)?;
    }
    Ok(())
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
                if let Some(entry) = state.skills.get(name).filter(|entry| {
                    entry.adapter_hashes.contains_key(location.label)
                        || entry.adapter_complete_hashes.contains_key(location.label)
                }) {
                    owned += 1;
                    let legacy_digest = digest_skill_dir(path)?;
                    if !adapter_matches_recorded_ownership(
                        entry,
                        &location.labels,
                        path,
                        legacy_digest,
                    ) {
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
    let mut out: Vec<SkillLocation> = Vec::new();
    let mut push = |label: &'static str, path: PathBuf, adapter: bool, import_managed: bool| {
        if path == cfg.central_skills_dir {
            return;
        }
        if let Some(existing) = out.iter_mut().find(|item| item.path == path) {
            if existing.adapter == adapter && !existing.labels.contains(&label) {
                existing.labels.push(label);
            }
            return;
        }
        out.push(SkillLocation {
            label,
            labels: vec![label],
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

fn save_skill_state(
    path: &Path,
    state: &SkillState,
    history: &mut Option<HistoryRecorder>,
) -> io::Result<()> {
    #[cfg(test)]
    if env::var_os("RELAY_TEST_FAIL_SAVE_SKILL_STATE")
        .as_deref()
        .is_some_and(|fault_path| fault_path == path.as_os_str())
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "injected skill state save failure",
        ));
    }
    let raw = toml::to_string_pretty(state)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    write_raw_if_changed(path, raw.as_bytes(), ExecutionMode::Apply, history).map(|_| ())
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
    crate::path_cleanup::remove_with_owner_access(path)?;
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
    Ok(sync_skills_with_mode(
        cfg,
        log_mode,
        ExecutionMode::Apply,
        &mut history,
        &mut conflicts,
    )?
    .stats)
}

pub(crate) fn sync_skills_with_mode(
    cfg: &Config,
    log_mode: SyncLogMode,
    mode: ExecutionMode,
    history: &mut Option<HistoryRecorder>,
    conflicts: &mut Vec<SyncConflict>,
) -> io::Result<SkillSyncOutcome> {
    sync_all_skills_with_mode(cfg, log_mode, mode, history, conflicts)
}

pub(crate) fn sync_scoped_skills_with_mode(
    cfg: &Config,
    selected: &[ScopedSkill],
    log_mode: SyncLogMode,
    mode: ExecutionMode,
    history: &mut Option<HistoryRecorder>,
    conflicts: &mut Vec<SyncConflict>,
) -> io::Result<SkillSyncOutcome> {
    let prepared = prepare_scoped_skills(cfg, selected, conflicts)?;
    if !conflicts.is_empty() {
        return Ok(SkillSyncOutcome {
            stats: SyncStats::default(),
            codex_real_skill_names: HashSet::new(),
        });
    }

    let mut stats = SyncStats::default();
    let mut codex_real_skill_names = HashSet::new();
    let state_path = cfg.skill_state_path()?;
    let mut state = load_skill_state(&state_path)?;

    for skill in prepared {
        let entry = state.skills.entry(skill.name.clone()).or_default();
        if entry.tombstoned {
            entry.adapter_hashes.clear();
            entry.adapter_complete_hashes.clear();
        }

        if skill.canonical_digest.is_none() {
            stats.updated += usize::from(sync_complete_skill_target(
                &skill.selected_path,
                skill.selected_digest,
                None,
                &skill.canonical_path,
                log_mode,
                mode,
                history,
            )?);
            let action = if mode == ExecutionMode::Plan {
                "would import"
            } else {
                "imported"
            };
            log_action(
                log_mode,
                &format!(
                    "skills: {action} '{}' from selected input into canonical store",
                    skill.name
                ),
            );
        }

        entry.tombstoned = false;
        entry.canonical_hash = Some(persisted_hash(skill.state_digest.body_hash));
        if codex_skills_target_enabled(cfg)
            && !cfg.is_blacklisted(&format!("skills/{}", skill.name), TOOL_CODEX)
        {
            codex_real_skill_names.insert(skill.name.clone());
        }

        for adapter in skill.adapters {
            stats.updated += usize::from(sync_complete_skill_target(
                &skill.canonical_path,
                skill.selected_digest,
                adapter.existing_digest,
                &adapter.path,
                log_mode,
                mode,
                history,
            )?);
            let complete_hash = (mode == ExecutionMode::Apply)
                .then(|| digest_complete_skill_identity(&adapter.path))
                .transpose()?
                .map(|digest| persisted_hash(digest.body_hash));
            entry.record_adapter_ownership(
                &adapter.labels,
                persisted_hash(skill.state_digest.body_hash),
                complete_hash,
            );
        }
    }

    if mode == ExecutionMode::Apply {
        save_skill_state(&state_path, &state, history)?;
    }
    Ok(SkillSyncOutcome {
        stats,
        codex_real_skill_names,
    })
}

struct PreparedScopedSkill {
    name: String,
    selected_path: PathBuf,
    selected_digest: DirDigest,
    state_digest: DirDigest,
    canonical_path: PathBuf,
    canonical_digest: Option<DirDigest>,
    adapters: Vec<PreparedAdapter>,
}

struct PreparedAdapter {
    labels: Vec<&'static str>,
    path: PathBuf,
    existing_digest: Option<DirDigest>,
}

fn prepare_scoped_skills(
    cfg: &Config,
    selected: &[ScopedSkill],
    conflicts: &mut Vec<SyncConflict>,
) -> io::Result<Vec<PreparedScopedSkill>> {
    let locations = skill_locations(cfg)?;
    validate_scoped_destination_names(cfg, selected, &locations)?;
    let mut prepared = Vec::with_capacity(selected.len());

    for skill in selected {
        let refreshed = refresh_scoped_skill(skill)?;
        let canonical_path = cfg.central_skills_dir.join(&refreshed.name);
        let canonical_digest = digest_complete_skill_identity_if_exists(&canonical_path)?;
        let selected_is_canonical = paths_refer_to_same_entry(&refreshed.path, &canonical_path);
        if !selected_is_canonical
            && canonical_digest.is_some_and(|digest| digest.body_hash != refreshed.digest.body_hash)
        {
            conflicts.push(SyncConflict {
                kind: SyncItemKind::Skill,
                name: refreshed.name.clone(),
                winner: TOOL_CENTRAL,
                others: vec![TOOL_SELECTED],
            });
            log_action(
                SyncLogMode::Quiet,
                &format!(
                    "warning: selected skill '{}' differs from canonical store; refusing to overwrite {}",
                    refreshed.name,
                    canonical_path.display()
                ),
            );
        }

        let mut adapters = Vec::new();
        for location in locations.iter().filter(|location| location.adapter) {
            if location
                .allowed_adapter_label(cfg, &refreshed.name)
                .is_none()
            {
                continue;
            }
            let path = location.path.join(&refreshed.name);
            adapters.push(PreparedAdapter {
                labels: location.labels.clone(),
                existing_digest: digest_complete_skill_if_exists(&path)?,
                path,
            });
        }
        prepared.push(PreparedScopedSkill {
            name: refreshed.name,
            selected_path: refreshed.path.clone(),
            selected_digest: refreshed.sync_digest,
            state_digest: if canonical_digest.is_some() {
                digest_skill_dir(&canonical_path)?
            } else {
                digest_skill_dir(&refreshed.path)?
            },
            canonical_path,
            canonical_digest,
            adapters,
        });
    }
    Ok(prepared)
}

fn refresh_scoped_skill(discovered: &ScopedSkill) -> io::Result<ScopedSkill> {
    let metadata = fs::symlink_metadata(&discovered.path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "expected a regular skill directory: {}",
                discovered.path.display()
            ),
        ));
    }
    let skill_file = discovered.path.join("SKILL.md");
    let skill_metadata = fs::symlink_metadata(&skill_file)?;
    if skill_metadata.file_type().is_symlink() || !skill_metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("expected a regular SKILL.md file: {}", skill_file.display()),
        ));
    }
    let directory_name = scoped_skill_directory_name(&discovered.path)?;
    let doc = read_markdown(&skill_file)?;
    let required = parse_required_frontmatter(doc.frontmatter.as_deref()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "invalid skill frontmatter in {}: expected non-empty name and description",
                skill_file.display()
            ),
        )
    })?;
    if required.name != directory_name || required.name != discovered.name {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "skill name '{}' does not match discovered parent directory '{}': {}",
                required.name,
                directory_name,
                skill_file.display()
            ),
        ));
    }
    Ok(ScopedSkill {
        name: required.name,
        path: discovered.path.clone(),
        digest: digest_complete_skill_identity(&discovered.path)?,
        sync_digest: digest_complete_skill_dir(&discovered.path)?,
    })
}

fn digest_complete_skill_if_exists(path: &Path) -> io::Result<Option<DirDigest>> {
    digest_complete_skill_if_exists_with(path, digest_complete_skill_dir)
}

fn digest_complete_skill_identity_if_exists(path: &Path) -> io::Result<Option<DirDigest>> {
    digest_complete_skill_if_exists_with(path, digest_complete_skill_identity)
}

fn digest_complete_skill_if_exists_with(
    path: &Path,
    digest: impl FnOnce(&Path) -> io::Result<DirDigest>,
) -> io::Result<Option<DirDigest>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("expected a regular skill directory: {}", path.display()),
        ));
    }
    let skill_file = path.join("SKILL.md");
    if !fs::symlink_metadata(&skill_file)
        .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("expected a regular SKILL.md file: {}", skill_file.display()),
        ));
    }
    digest(path).map(Some)
}

fn sync_all_skills_with_mode(
    cfg: &Config,
    log_mode: SyncLogMode,
    mode: ExecutionMode,
    history: &mut Option<HistoryRecorder>,
    conflicts: &mut Vec<SyncConflict>,
) -> io::Result<SkillSyncOutcome> {
    let mut stats = SyncStats::default();
    let mut codex_real_skill_names = HashSet::new();
    let codex_skills_enabled = codex_skills_target_enabled(cfg);

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
    for path in central
        .values()
        .chain(location_maps.iter().flat_map(|skills| skills.values()))
    {
        validate_skill_tree_utf8(path, path)?;
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
                entry.adapter_complete_hashes.clear();
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
                let owned =
                    adapter_matches_recorded_ownership(entry, &location.labels, path, digest);
                if owned {
                    stats.updated +=
                        usize::from(remove_skill_target(path, log_mode, mode, history)?);
                    entry.clear_adapter_ownership(&location.labels);
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
                        && adapter_changed_since_sync(entry, source)
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
            let action = if mode == ExecutionMode::Plan {
                "would import"
            } else {
                "imported"
            };
            log_action(
                log_mode,
                &format!(
                    "skills: {action} '{name}' from {} into canonical store",
                    winner.tool
                ),
            );
        }

        let Some(canonical) = canonical else { continue };
        if codex_skills_enabled && !cfg.is_blacklisted(&format!("skills/{name}"), TOOL_CODEX) {
            codex_real_skill_names.insert(name.clone());
        }
        entry.tombstoned = false;
        entry.canonical_hash = Some(persisted_hash(canonical.digest.body_hash));
        for (location, map) in locations.iter().zip(&location_maps) {
            if !location.adapter || location.allowed_adapter_label(cfg, &name).is_none() {
                continue;
            }
            let existing = map
                .get(&name)
                .map(|path| digest_skill_dir(path))
                .transpose()?;
            let updated = sync_skill_target(
                &canonical.path,
                canonical.digest,
                existing,
                &location.path.join(&name),
                log_mode,
                mode,
                history,
            )?;
            stats.updated += usize::from(updated);
            let complete_hash = (updated && mode == ExecutionMode::Apply)
                .then(|| digest_complete_skill_identity(&location.path.join(&name)))
                .transpose()?
                .map(|digest| persisted_hash(digest.body_hash));
            entry.record_adapter_ownership(
                &location.labels,
                persisted_hash(canonical.digest.body_hash),
                complete_hash,
            );
        }
    }

    if mode == ExecutionMode::Apply {
        save_skill_state(&state_path, &state, history)?;
    }
    Ok(SkillSyncOutcome {
        stats,
        codex_real_skill_names,
    })
}

pub(super) fn codex_skills_target_enabled(cfg: &Config) -> bool {
    cfg.tool_enabled(TOOL_CODEX)
        && (cfg.codex_skills_dir == cfg.central_skills_dir
            || cfg
                .codex_skills_dir
                .parent()
                .is_some_and(|parent| parent.exists()))
}

fn select_skill_winner(variants: &[SkillVariant]) -> &SkillVariant {
    variants
        .iter()
        .max_by_key(|variant| (variant.digest.mtime, tool_order(variant.tool)))
        .expect("winner available")
}

fn paths_refer_to_same_entry(left: &Path, right: &Path) -> bool {
    left == right
        || fs::canonicalize(left)
            .ok()
            .zip(fs::canonicalize(right).ok())
            .is_some_and(|(left, right)| left == right)
}

fn adapter_matches_recorded_ownership(
    entry: &SkillStateEntry,
    labels: &[&str],
    path: &Path,
    legacy_digest: DirDigest,
) -> bool {
    if labels
        .iter()
        .any(|label| entry.adapter_complete_hashes.contains_key(*label))
    {
        let Ok(digest) = digest_complete_skill_identity(path) else {
            return false;
        };
        let actual = persisted_hash(digest.body_hash);
        return labels.iter().any(|label| {
            entry
                .adapter_complete_hashes
                .get(*label)
                .is_some_and(|expected| *expected == actual)
        });
    }
    let actual = persisted_hash(legacy_digest.body_hash);
    labels.iter().any(|label| {
        entry
            .adapter_hashes
            .get(*label)
            .is_some_and(|expected| *expected == actual)
    })
}

fn adapter_changed_since_sync(entry: &SkillStateEntry, source: &SkillVariant) -> bool {
    if let Some(expected) = entry.adapter_complete_hashes.get(source.tool) {
        return digest_complete_skill_identity(&source.path)
            .map_or(true, |digest| *expected != persisted_hash(digest.body_hash));
    }
    entry
        .adapter_hashes
        .get(source.tool)
        .is_some_and(|expected| *expected != persisted_hash(source.digest.body_hash))
}

fn sync_complete_skill_target(
    source: &Path,
    source_digest: DirDigest,
    existing: Option<DirDigest>,
    target_path: &Path,
    log_mode: SyncLogMode,
    mode: ExecutionMode,
    history: &mut Option<HistoryRecorder>,
) -> io::Result<bool> {
    sync_skill_target_with_policy(
        source,
        source_digest,
        existing,
        target_path,
        log_mode,
        mode,
        history,
        PackagePolicy::CompleteStrict,
    )
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
    sync_skill_target_with_policy(
        source,
        source_digest,
        existing,
        target_path,
        log_mode,
        mode,
        history,
        PackagePolicy::LegacyVisible,
    )
}

#[allow(clippy::too_many_arguments)]
fn sync_skill_target_with_policy(
    source: &Path,
    source_digest: DirDigest,
    existing: Option<DirDigest>,
    target_path: &Path,
    log_mode: SyncLogMode,
    mode: ExecutionMode,
    history: &mut Option<HistoryRecorder>,
    policy: PackagePolicy,
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

    #[cfg(test)]
    if env::var_os("RELAY_TEST_FAIL_SKILL_TARGET")
        .as_deref()
        .is_some_and(|path| path == target_path.as_os_str())
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "injected late skill target failure: {}",
                target_path.display()
            ),
        ));
    }

    let before_state = history
        .as_ref()
        .map(|recorder| recorder.capture_path(target_path))
        .transpose()?;
    target_path.parent().map(fs::create_dir_all).transpose()?;

    let temp_path = skill_temp_path(target_path);
    if temp_path.exists() {
        remove_prepared_skill_directory(&temp_path)?;
    }
    let deferred_root_permissions = match copy_skill_dir_for_assembly(source, &temp_path, policy) {
        Ok(permissions) => permissions,
        Err(err) => {
            let _ = remove_prepared_skill_directory(&temp_path);
            return Err(err);
        }
    };

    let source_skill = source.join("SKILL.md");
    let target_skill = target_path.join("SKILL.md");
    let after_state = match (|| {
        #[cfg(test)]
        fail_skill_assembly_for_test("after-copy", &temp_path)?;
        merge_skill_frontmatter(&source_skill, &target_skill, &temp_path, log_mode)?;
        #[cfg(test)]
        fail_skill_assembly_for_test("after-frontmatter", &temp_path)?;
        if let Some(permissions) = deferred_root_permissions {
            fs::set_permissions(&temp_path, permissions)?;
        }
        #[cfg(test)]
        fail_skill_assembly_for_test("before-snapshot", &temp_path)?;
        history
            .as_ref()
            .map(|recorder| recorder.capture_path(&temp_path))
            .transpose()
    })() {
        Ok(after_state) => after_state,
        Err(err) => {
            let _ = remove_prepared_skill_directory(&temp_path);
            return Err(err);
        }
    };
    publish_prepared_skill_directory(&temp_path, target_path)?;

    if let (Some(recorder), Some(after_state)) = (history.as_mut(), after_state) {
        recorder.record_change(
            target_path,
            before_state.unwrap_or_else(crate::history::EntityState::missing),
            after_state,
        );
        #[cfg(test)]
        if env::var_os("RELAY_TEST_CORRUPT_BEFORE_SNAPSHOT")
            .as_deref()
            .is_some_and(|path| path == target_path.as_os_str())
        {
            recorder.corrupt_latest_before_dir_snapshot()?;
        }
    }

    log_action(
        log_mode,
        &format!("skills: updated {}", target_path.display()),
    );
    Ok(true)
}

#[cfg(test)]
fn fail_skill_assembly_for_test(phase: &str, temp_path: &Path) -> io::Result<()> {
    if SKILL_ASSEMBLY_FAILURE_PHASE.get() == Some(phase) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "injected skill assembly failure {phase}: {}",
                temp_path.display()
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn set_skill_assembly_failure_for_test(phase: Option<&'static str>) {
    SKILL_ASSEMBLY_FAILURE_PHASE.set(phase);
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

fn list_skill_dirs_with_policy(
    dir: &Path,
    import_managed: bool,
) -> io::Result<HashMap<String, PathBuf>> {
    let mut out = HashMap::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_name().to_str().is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "skill package path is not valid UTF-8: {}",
                    entry.path().display()
                ),
            ));
        }
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
    digest_skill_dir_with_policy(dir, PackagePolicy::LegacyVisible, false)
}

fn digest_complete_skill_dir(dir: &Path) -> io::Result<DirDigest> {
    digest_skill_dir_with_policy(dir, PackagePolicy::CompleteStrict, false)
}

fn digest_complete_skill_identity(dir: &Path) -> io::Result<DirDigest> {
    digest_skill_dir_with_policy(dir, PackagePolicy::CompleteStrict, true)
}

fn digest_skill_dir_with_policy(
    dir: &Path,
    policy: PackagePolicy,
    raw_skill_file: bool,
) -> io::Result<DirDigest> {
    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    collect_skill_entries(dir, dir, &mut entries, policy)?;
    let mut digest = digest_skill_entries(
        entries,
        policy == PackagePolicy::CompleteStrict,
        raw_skill_file,
    )?;
    if policy == PackagePolicy::CompleteStrict {
        let metadata = fs::metadata(dir)?;
        let mut hasher = DefaultHasher::new();
        digest.body_hash.hash(&mut hasher);
        "package-root-mode".hash(&mut hasher);
        package_entry_mode(&metadata).hash(&mut hasher);
        digest.body_hash = hasher.finish();
        digest.mtime = digest.mtime.max(file_mtime_value_from_meta(&metadata));
    }
    Ok(digest)
}

fn digest_skill_entries(
    mut entries: Vec<(String, PathBuf)>,
    include_modes: bool,
    raw_skill_file: bool,
) -> io::Result<DirDigest> {
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut body_hasher = DefaultHasher::new();
    let mut mtime = 0u128;

    for (rel, path) in entries {
        let meta = fs::metadata(&path)?;
        let entry_mtime = file_mtime_value_from_meta(&meta);
        if entry_mtime > mtime {
            mtime = entry_mtime;
        }
        if include_modes {
            package_entry_mode(&meta).hash(&mut body_hasher);
        }
        if meta.is_dir() {
            rel.hash(&mut body_hasher);
            "directory".hash(&mut body_hasher);
            continue;
        }
        let file_name = path.file_name().and_then(|os| os.to_str()).unwrap_or("");
        if file_name == "SKILL.md" && !raw_skill_file {
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

#[cfg(unix)]
fn package_entry_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o7777
}

#[cfg(not(unix))]
fn package_entry_mode(_metadata: &fs::Metadata) -> u32 {
    0
}

fn collect_skill_entries(
    root: &Path,
    dir: &Path,
    entries: &mut Vec<(String, PathBuf)>,
    policy: PackagePolicy,
) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let (path, metadata) = match policy {
            PackagePolicy::LegacyVisible => {
                let Some((_name, path, metadata)) = read_visible_entry(entry, false)? else {
                    continue;
                };
                (path, metadata)
            }
            PackagePolicy::CompleteStrict => strict_package_entry(&entry.path())?,
        };
        let rel = strict_package_relative_utf8(root, &path)?;
        if metadata.is_dir() {
            if policy == PackagePolicy::CompleteStrict {
                entries.push((rel, path.clone()));
            }
            collect_skill_entries(root, &path, entries, policy)?;
        } else if metadata.is_file() {
            entries.push((rel, path));
        }
    }
    Ok(())
}

fn strict_package_relative_utf8(root: &Path, path: &Path) -> io::Result<String> {
    let relative = path.strip_prefix(root).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "skill package entry is outside its package root: {}",
                path.display()
            ),
        )
    })?;
    relative.to_str().map(str::to_owned).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "skill package entry path is not valid UTF-8: {}",
                path.display()
            ),
        )
    })
}

fn validate_skill_tree_utf8(root: &Path, dir: &Path) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        strict_package_relative_utf8(root, &path)?;
        if entry.file_type()?.is_dir() {
            validate_skill_tree_utf8(root, &path)?;
        }
    }
    Ok(())
}

fn copy_skill_dir_for_assembly(
    src: &Path,
    dst: &Path,
    policy: PackagePolicy,
) -> io::Result<Option<fs::Permissions>> {
    if policy == PackagePolicy::LegacyVisible {
        copy_skill_dir(src, dst, policy)?;
        return Ok(None);
    }
    let deferred_root_permissions = fs::metadata(src)?.permissions();
    copy_skill_dir_inner(src, dst, policy, false)?;
    Ok(Some(deferred_root_permissions))
}

fn copy_skill_dir(src: &Path, dst: &Path, policy: PackagePolicy) -> io::Result<()> {
    copy_skill_dir_inner(src, dst, policy, true)
}

fn copy_skill_dir_inner(
    src: &Path,
    dst: &Path,
    policy: PackagePolicy,
    preserve_root_permissions: bool,
) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    let source_permissions = (policy == PackagePolicy::CompleteStrict && preserve_root_permissions)
        .then(|| fs::metadata(src).map(|metadata| metadata.permissions()))
        .transpose()?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        if policy == PackagePolicy::LegacyVisible
            && entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with('.'))
        {
            continue;
        }
        let metadata = match policy {
            PackagePolicy::LegacyVisible => fs::symlink_metadata(&from)?,
            PackagePolicy::CompleteStrict => strict_package_entry(&from)?.1,
        };
        let to = dst.join(entry.file_name());
        if metadata.is_dir() {
            copy_skill_dir_inner(&from, &to, policy, true)?;
        } else if metadata.is_file() {
            fs::copy(&from, &to)?;
        }
    }
    if let Some(permissions) = source_permissions {
        fs::set_permissions(dst, permissions)?;
    }
    Ok(())
}

fn remove_prepared_skill_directory(path: &Path) -> io::Result<()> {
    crate::path_cleanup::remove_with_owner_access(path)
}

fn strict_package_entry(path: &Path) -> io::Result<(PathBuf, fs::Metadata)> {
    let metadata = fs::symlink_metadata(path)?;
    let message = if metadata.file_type().is_symlink() {
        "symlinks are not supported inside selected skill packages"
    } else if !metadata.is_dir() && !metadata.is_file() {
        "unsupported non-regular entry inside selected skill package"
    } else {
        return Ok((path.to_path_buf(), metadata));
    };
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{message}: {}", path.display()),
    ))
}

fn publish_skill_directory(temp_path: &Path, target_path: &Path) -> io::Result<()> {
    publish_skill_directory_with(
        temp_path,
        target_path,
        |from, to| fs::rename(from, to),
        exchange_directories,
        crate::path_cleanup::remove_with_owner_access,
    )
}

fn publish_prepared_skill_directory(temp_path: &Path, target_path: &Path) -> io::Result<()> {
    publish_prepared_skill_directory_with(temp_path, target_path, publish_skill_directory)
}

fn publish_prepared_skill_directory_with<F>(
    temp_path: &Path,
    target_path: &Path,
    publish: F,
) -> io::Result<()>
where
    F: FnOnce(&Path, &Path) -> io::Result<()>,
{
    if let Err(err) = publish(temp_path, target_path) {
        let _ = remove_prepared_skill_directory(temp_path);
        return Err(err);
    }
    Ok(())
}

fn publish_skill_directory_with<R, E, D>(
    temp_path: &Path,
    target_path: &Path,
    mut rename: R,
    mut exchange: E,
    mut remove: D,
) -> io::Result<()>
where
    R: FnMut(&Path, &Path) -> io::Result<()>,
    E: FnMut(&Path, &Path) -> io::Result<bool>,
    D: FnMut(&Path) -> io::Result<()>,
{
    if !target_path.exists() {
        return rename(temp_path, target_path);
    }

    if exchange(temp_path, target_path)? {
        let _ = remove(temp_path);
        return Ok(());
    }

    let backup_path = skill_backup_path(target_path);
    if backup_path.exists() {
        remove(&backup_path)?;
    }
    rename(target_path, &backup_path)?;
    if let Err(publish_err) = rename(temp_path, target_path) {
        return match rename(&backup_path, target_path) {
            Ok(()) => Err(io::Error::new(
                publish_err.kind(),
                format!(
                    "failed to publish replacement for {}; previous package was restored: {publish_err}",
                    target_path.display()
                ),
            )),
            Err(restore_err) => Err(io::Error::new(
                publish_err.kind(),
                format!(
                    "failed to publish replacement for {} ({publish_err}) and failed to restore previous package from {} ({restore_err})",
                    target_path.display(),
                    backup_path.display()
                ),
            )),
        };
    }
    let _ = remove(&backup_path);
    Ok(())
}

#[cfg(target_os = "macos")]
fn exchange_directories(left: &Path, right: &Path) -> io::Result<bool> {
    let left = CString::new(left.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory path contains a NUL byte",
        )
    })?;
    let right = CString::new(right.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory path contains a NUL byte",
        )
    })?;
    // SAFETY: both pointers reference live, NUL-terminated path buffers for this call.
    let result = unsafe {
        libc::renameatx_np(
            libc::AT_FDCWD,
            left.as_ptr(),
            libc::AT_FDCWD,
            right.as_ptr(),
            libc::RENAME_SWAP,
        )
    };
    if result == 0 {
        return Ok(true);
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error().is_some_and(exchange_is_unsupported) {
        return Ok(false);
    }
    Err(err)
}

#[cfg(target_os = "linux")]
fn exchange_directories(left: &Path, right: &Path) -> io::Result<bool> {
    let left = CString::new(left.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory path contains a NUL byte",
        )
    })?;
    let right = CString::new(right.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory path contains a NUL byte",
        )
    })?;
    // SAFETY: both pointers reference live, NUL-terminated path buffers for this call.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            left.as_ptr(),
            libc::AT_FDCWD,
            right.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if result == 0 {
        return Ok(true);
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error().is_some_and(exchange_is_unsupported) {
        return Ok(false);
    }
    Err(err)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn exchange_directories(_left: &Path, _right: &Path) -> io::Result<bool> {
    Ok(false)
}

#[cfg(target_os = "macos")]
fn exchange_is_unsupported(code: i32) -> bool {
    [libc::EINVAL, libc::ENOTSUP].contains(&code)
}

#[cfg(target_os = "linux")]
fn exchange_is_unsupported(code: i32) -> bool {
    [libc::ENOSYS, libc::EINVAL, libc::ENOTSUP].contains(&code)
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

#[inline(never)]
fn skill_backup_path(target: &Path) -> PathBuf {
    let mut backup = skill_temp_path(target);
    let name = backup
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("skill.relay.tmp");
    backup.set_file_name(format!("{name}.backup"));
    backup
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markers::RELAY_COMMAND_SKILL_MARKER;
    use crate::sync::test_support::{doc, setup, write_plain, write_skill};
    use tempfile::TempDir;

    fn sync_selected(
        cfg: &Config,
        selected: &[ScopedSkill],
        mode: ExecutionMode,
    ) -> io::Result<(SkillSyncOutcome, Vec<SyncConflict>)> {
        let mut history = None;
        let mut conflicts = Vec::new();
        let outcome = sync_scoped_skills_with_mode(
            cfg,
            selected,
            SyncLogMode::Quiet,
            mode,
            &mut history,
            &mut conflicts,
        )?;
        Ok((outcome, conflicts))
    }

    #[test]
    fn scoped_discovery_accepts_recursive_collections_and_skill_files() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let collection = tmp.path().join("collection");
        let alpha = write_skill(&collection, "alpha", &doc("alpha", "Alpha"))?;
        let beta = write_skill(
            &collection.join("one/two/three"),
            "beta",
            &doc("beta", "Beta"),
        )?;

        let selected = discover_scoped_skills(&[collection, beta.join("SKILL.md"), alpha.clone()])?;

        assert_eq!(
            selected
                .iter()
                .map(|skill| skill.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "beta"]
        );
        assert!(selected
            .iter()
            .any(|skill| skill.path == fs::canonicalize(&alpha).unwrap()));
        assert!(selected
            .iter()
            .any(|skill| skill.path == fs::canonicalize(&beta).unwrap()));
        Ok(())
    }

    #[test]
    fn scoped_discovery_traverses_hidden_collections_but_excludes_known_noise() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let collection = tmp.path().join("collection");
        write_skill(&collection, "selected", &doc("selected", "Selected"))?;
        write_skill(
            &collection.join(".local/skills"),
            "hidden-skill",
            &doc("hidden-skill", "Hidden"),
        )?;
        write_skill(
            &collection.join(".git/nested"),
            "git-skill",
            &doc("git-skill", "Git"),
        )?;
        write_skill(
            &collection.join("node_modules/package"),
            "dependency-skill",
            &doc("dependency-skill", "Dependency"),
        )?;

        let selected = discover_scoped_skills(std::slice::from_ref(&collection))?;

        assert_eq!(
            selected
                .iter()
                .map(|skill| skill.name.as_str())
                .collect::<Vec<_>>(),
            vec!["hidden-skill", "selected"]
        );
        Ok(())
    }

    #[test]
    fn scoped_discovery_rejects_duplicate_names_from_different_roots() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let first = write_skill(&tmp.path().join("one"), "plan", &doc("plan", "One"))?;
        let second = write_skill(&tmp.path().join("two"), "plan", &doc("plan", "Two"))?;

        let err = discover_scoped_skills(&[first, second]).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("skill name 'plan'"));
        assert!(!tmp.path().join("canonical/plan").exists());
        Ok(())
    }

    #[test]
    fn scoped_discovery_rejects_ascii_case_folded_name_collisions() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let upper = write_skill(&tmp.path().join("one"), "Plan", &doc("Plan", "Upper"))?;
        let lower = write_skill(&tmp.path().join("two"), "plan", &doc("plan", "Lower"))?;

        let err = discover_scoped_skills(&[upper, lower]).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("differ only by ASCII case"));
        assert!(err.to_string().contains("'Plan'"));
        assert!(err.to_string().contains("'plan'"));
        Ok(())
    }

    #[test]
    fn scoped_discovery_rejects_unicode_case_destination_collisions_without_writes(
    ) -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let source = TempDir::new()?;
        let upper = write_skill(&source.path().join("one"), "École", &doc("École", "Upper"))?;
        let lower = write_skill(&source.path().join("two"), "école", &doc("école", "Lower"))?;

        let err = discover_scoped_skills(&[upper, lower]).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("case-insensitive"));
        assert!(!cfg.central_skills_dir.exists());
        assert!(!cfg.skill_state_path()?.exists());
        Ok(())
    }

    #[test]
    fn portable_destination_validation_rejects_canonical_unicode_equivalents() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let mut destinations = BTreeMap::new();
        register_portable_selected_name(&mut destinations, "café", Path::new("source-one/café"))?;

        let err = register_portable_selected_name(
            &mut destinations,
            "cafe\u{301}",
            Path::new("source-two/cafe-normalized"),
        )
        .unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("Unicode-normalizing"));
        assert!(!cfg.central_skills_dir.exists());
        assert!(!cfg.skill_state_path()?.exists());
        Ok(())
    }

    #[test]
    fn scoped_sync_rejects_existing_canonical_ascii_case_collision_before_writes() -> io::Result<()>
    {
        let (_tmp, cfg) = setup()?;
        let existing = cfg.central_skills_dir.join("Portable");
        fs::create_dir_all(existing.join("contents-are-not-scanned"))?;
        let selected = ScopedSkill {
            name: "portable".to_string(),
            path: PathBuf::from("selected/portable"),
            digest: DirDigest {
                body_hash: 0,
                mtime: 0,
            },
            sync_digest: DirDigest {
                body_hash: 0,
                mtime: 0,
            },
        };

        let err = match sync_selected(&cfg, &[selected], ExecutionMode::Apply) {
            Ok(_) => panic!("canonical destination collision should fail"),
            Err(err) => err,
        };

        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("existing central"));
        assert!(err.to_string().contains("differ only by ASCII case"));
        assert!(existing.join("contents-are-not-scanned").exists());
        assert!(!cfg.skill_state_path()?.exists());
        assert!(!cfg.claude_skills_dir.join("portable").exists());
        Ok(())
    }

    #[test]
    fn scoped_sync_rejects_existing_adapter_unicode_collision_before_writes() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let existing_name = "cafe\u{301}";
        let existing = cfg.claude_skills_dir.join(existing_name);
        fs::create_dir_all(&existing)?;
        let selected = ScopedSkill {
            name: "CAFÉ".to_string(),
            path: PathBuf::from("selected/CAFÉ"),
            digest: DirDigest {
                body_hash: 0,
                mtime: 0,
            },
            sync_digest: DirDigest {
                body_hash: 0,
                mtime: 0,
            },
        };

        let err = match sync_selected(&cfg, &[selected], ExecutionMode::Apply) {
            Ok(_) => panic!("adapter destination collision should fail"),
            Err(err) => err,
        };

        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("existing claude"));
        assert!(err.to_string().contains("Unicode-normalizing"));
        assert!(existing.exists());
        assert!(!cfg.skill_state_path()?.exists());
        assert!(!cfg.central_skills_dir.join("CAFÉ").exists());
        Ok(())
    }

    #[test]
    fn scoped_discovery_rejects_nested_skill_roots() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let parent = write_skill(tmp.path(), "parent", &doc("parent", "Parent"))?;
        write_skill(&parent, "child", &doc("child", "Child"))?;

        let err = discover_scoped_skills(&[parent]).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("overlapping skill roots"));
        assert!(err.to_string().contains("parent/child"));
        Ok(())
    }

    #[test]
    fn scoped_discovery_validates_every_input_before_sync_can_write() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let source_root = TempDir::new()?;
        let valid = write_skill(source_root.path(), "valid", &doc("valid", "Body"))?;
        let missing = source_root.path().join("missing");

        let err = discover_scoped_skills(&[valid, missing]).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(!cfg.central_skills_dir.join("valid").exists());
        assert!(!cfg.skill_state_path()?.exists());
        Ok(())
    }

    #[test]
    fn scoped_discovery_rejects_declared_name_directory_mismatch() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let path = write_skill(tmp.path(), "directory-name", &doc("declared-name", "Body"))?;

        let err = discover_scoped_skills(&[path]).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("does not match parent directory"));
        Ok(())
    }

    #[test]
    fn scoped_discovery_rejects_invalid_inputs_without_writes() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let source = TempDir::new()?;
        let empty = source.path().join("empty");
        fs::create_dir_all(&empty)?;
        let wrong_case = source.path().join("skill.md");
        write_plain(&wrong_case, "not an exact-case skill file")?;

        let frontmatter_cases = [
            ("missing-name", "---\ndescription: present\n---\nBody"),
            ("empty-name", "---\nname: \ndescription: present\n---\nBody"),
            (
                "missing-description",
                "---\nname: missing-description\n---\nBody",
            ),
            (
                "empty-description",
                "---\nname: empty-description\ndescription: \n---\nBody",
            ),
        ];
        let mut cases = vec![
            (empty, "contains no skill packages"),
            (wrong_case, "must be named exactly SKILL.md"),
        ];
        for (name, contents) in frontmatter_cases {
            let root = source.path().join(name);
            fs::create_dir_all(&root)?;
            write_plain(&root.join("SKILL.md"), contents)?;
            cases.push((root, "expected non-empty name and description"));
        }

        #[cfg(unix)]
        {
            let linked = source.path().join("linked");
            fs::create_dir_all(&linked)?;
            let target = source.path().join("linked-target.md");
            write_plain(&target, "target")?;
            std::os::unix::fs::symlink(&target, linked.join("SKILL.md"))?;
            cases.push((linked, "expected a regular SKILL.md file"));

            let fifo = source.path().join("skill-fifo");
            let fifo_c = CString::new(fifo.as_os_str().as_bytes()).map_err(io::Error::other)?;
            // SAFETY: `fifo_c` is a live NUL-terminated path and `mkfifo` does not retain it.
            if unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) } != 0 {
                return Err(io::Error::last_os_error());
            }
            cases.push((fifo, "is not a file or directory"));
        }

        for (path, expected) in cases {
            let err = discover_scoped_skills(&[path]).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
            assert!(
                err.to_string().contains(expected),
                "expected {expected:?} in {err}"
            );
            assert!(!cfg.central_skills_dir.join("missing-name").exists());
            assert!(!cfg.skill_state_path()?.exists());
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn scoped_basename_validation_rejects_non_utf8_before_writes() -> io::Result<()> {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let (_tmp, cfg) = setup()?;
        let root =
            PathBuf::from("selected").join(OsString::from_vec(b"invalid-name-\x80".to_vec()));

        let err = scoped_skill_directory_name(&root).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err
            .to_string()
            .contains("skill directory has no valid UTF-8 name"));
        assert!(!cfg.central_skills_dir.exists());
        assert!(!cfg.claude_skills_dir.join("invalid-name").exists());
        assert!(!cfg.skill_state_path()?.exists());
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn complete_strict_traversal_rejects_non_utf8_internal_paths_before_writes() -> io::Result<()> {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        for location in ["selected", "canonical", "adapter"] {
            let (_tmp, cfg) = setup()?;
            let source = TempDir::new()?;
            let selected_path =
                write_skill(source.path(), "utf8-guard", &doc("utf8-guard", "Selected"))?;
            let invalid_root = match location {
                "selected" => selected_path.clone(),
                "canonical" => write_skill(
                    &cfg.central_skills_dir,
                    "utf8-guard",
                    &doc("utf8-guard", "Selected"),
                )?,
                "adapter" => write_skill(
                    &cfg.claude_skills_dir,
                    "utf8-guard",
                    &doc("utf8-guard", "Selected"),
                )?,
                _ => unreachable!(),
            };
            for bytes in [b"invalid-\x80".to_vec(), b"invalid-\x81".to_vec()] {
                fs::write(invalid_root.join(OsString::from_vec(bytes)), "invalid")?;
            }

            let result = if location == "selected" {
                discover_scoped_skills(std::slice::from_ref(&selected_path)).map(|_| ())
            } else {
                let selected = discover_scoped_skills(std::slice::from_ref(&selected_path))?;
                sync_selected(&cfg, &selected, ExecutionMode::Apply).map(|_| ())
            };
            let err = result.unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{location}");
            assert!(
                err.to_string().contains("not valid UTF-8"),
                "{location}: {err}"
            );
            if location != "canonical" {
                assert!(!cfg.central_skills_dir.join("utf8-guard").exists());
            }
            if location != "adapter" {
                assert!(!cfg.claude_skills_dir.join("utf8-guard").exists());
            }
            assert!(!cfg.skill_state_path()?.exists());
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn strict_relative_path_validation_distinguishes_non_utf8_names() -> io::Result<()> {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let root = PathBuf::from("package");
        let first = root.join(OsString::from_vec(b"invalid-\x80".to_vec()));
        let second = root.join(OsString::from_vec(b"invalid-\x81".to_vec()));

        for path in [&first, &second] {
            let err = strict_package_relative_utf8(&root, path).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
            assert!(err.to_string().contains("not valid UTF-8"));
        }
        assert_ne!(first.as_os_str().as_bytes(), second.as_os_str().as_bytes());
        Ok(())
    }

    #[test]
    fn scoped_sync_imports_selected_package_when_canonical_is_missing() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let source_root = TempDir::new()?;
        let selected_path = write_skill(source_root.path(), "plan", &doc("plan", "Selected new"))?;
        let selected = discover_scoped_skills(&[selected_path])?;
        let (outcome, conflicts) = sync_selected(&cfg, &selected, ExecutionMode::Apply)?;

        assert!(outcome.stats.updated > 0);
        assert!(conflicts.is_empty());
        assert_eq!(
            read_markdown(&cfg.central_skills_dir.join("plan/SKILL.md"))?.body,
            "Selected new"
        );
        assert_eq!(
            read_markdown(&cfg.claude_skills_dir.join("plan/SKILL.md"))?.body,
            "Selected new"
        );
        Ok(())
    }

    #[test]
    fn scoped_sync_refuses_to_overwrite_differing_canonical_skill() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        write_skill(&cfg.central_skills_dir, "plan", &doc("plan", "Canonical"))?;
        let source_root = TempDir::new()?;
        let selected_path = write_skill(source_root.path(), "plan", &doc("plan", "External"))?;
        let selected = discover_scoped_skills(&[selected_path])?;
        let (outcome, conflicts) = sync_selected(&cfg, &selected, ExecutionMode::Apply)?;

        assert_eq!(outcome.stats.updated, 0);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(
            read_markdown(&cfg.central_skills_dir.join("plan/SKILL.md"))?.body,
            "Canonical"
        );
        assert!(!cfg.claude_skills_dir.join("plan").exists());
        assert!(!cfg.skill_state_path()?.exists());
        Ok(())
    }

    #[test]
    fn scoped_sync_uses_canonical_when_external_package_is_identical() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let canonical = write_skill(
            &cfg.central_skills_dir,
            "identical",
            &doc("identical", "Same body"),
        )?;
        write_plain(&canonical.join("references/guide.md"), "same reference")?;
        let source_root = TempDir::new()?;
        let external = write_skill(
            source_root.path(),
            "identical",
            &doc("identical", "Same body"),
        )?;
        write_plain(&external.join("references/guide.md"), "same reference")?;
        assert_eq!(
            digest_complete_skill_identity(&canonical)?.body_hash,
            digest_complete_skill_identity(&external)?.body_hash
        );
        let selected = discover_scoped_skills(&[external])?;
        let (outcome, conflicts) = sync_selected(&cfg, &selected, ExecutionMode::Apply)?;

        assert!(conflicts.is_empty());
        assert!(outcome.stats.updated > 0);
        assert_eq!(
            fs::read_to_string(cfg.claude_skills_dir.join("identical/references/guide.md"))?,
            "same reference"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn scoped_sync_copies_complete_package_including_nested_and_hidden_content() -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let (_tmp, cfg) = setup()?;
        let source_root = TempDir::new()?;
        let selected_path = write_skill(source_root.path(), "bundle", &doc("bundle", "Body"))?;
        write_plain(&selected_path.join("scripts/run.sh"), "#!/bin/sh\n")?;
        fs::set_permissions(
            selected_path.join("scripts/run.sh"),
            fs::Permissions::from_mode(0o755),
        )?;
        write_plain(&selected_path.join("references/guide.md"), "guide")?;
        write_plain(&selected_path.join("assets/icon.txt"), "icon")?;
        write_plain(&selected_path.join("node_modules/pkg/index.js"), "module")?;
        write_plain(&selected_path.join(".git/config"), "git metadata")?;
        write_plain(&selected_path.join(".metadata/value"), "hidden")?;
        fs::create_dir_all(selected_path.join("assets/empty/nested"))?;
        fs::set_permissions(
            selected_path.join("references"),
            fs::Permissions::from_mode(0o750),
        )?;
        fs::set_permissions(
            selected_path.join("assets/empty"),
            fs::Permissions::from_mode(0o700),
        )?;
        fs::set_permissions(
            selected_path.join("assets/empty/nested"),
            fs::Permissions::from_mode(0o750),
        )?;
        let selected = discover_scoped_skills(&[selected_path])?;
        let (_, conflicts) = sync_selected(&cfg, &selected, ExecutionMode::Apply)?;
        assert!(conflicts.is_empty());

        for relative in [
            "scripts/run.sh",
            "references/guide.md",
            "assets/icon.txt",
            "node_modules/pkg/index.js",
            ".git/config",
            ".metadata/value",
        ] {
            assert!(cfg
                .central_skills_dir
                .join("bundle")
                .join(relative)
                .exists());
            assert!(cfg.claude_skills_dir.join("bundle").join(relative).exists());
        }
        assert!(cfg
            .central_skills_dir
            .join("bundle/assets/empty/nested")
            .is_dir());
        assert!(cfg
            .claude_skills_dir
            .join("bundle/assets/empty/nested")
            .is_dir());
        for root in [
            &cfg.central_skills_dir,
            &cfg.claude_skills_dir,
            &cfg.codex_skills_dir,
            &cfg.opencode_skills_dir,
        ] {
            assert_eq!(
                fs::metadata(root.join("bundle/scripts/run.sh"))?
                    .permissions()
                    .mode()
                    & 0o777,
                0o755
            );
            assert_eq!(
                fs::metadata(root.join("bundle/references"))?
                    .permissions()
                    .mode()
                    & 0o777,
                0o750
            );
            assert_eq!(
                fs::metadata(root.join("bundle/assets/empty"))?
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(root.join("bundle/assets/empty/nested"))?
                    .permissions()
                    .mode()
                    & 0o777,
                0o750
            );
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn scoped_sync_imports_and_propagates_read_only_package_root() -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let (_tmp, cfg) = setup()?;
        let source_root = TempDir::new()?;
        let selected_path = write_skill(
            source_root.path(),
            "read-only",
            &doc("read-only", "Read-only package"),
        )?;
        write_plain(
            &cfg.claude_skills_dir.join("read-only/SKILL.md"),
            "---\nname: read-only\ndescription: Previous description\nallowed-tools: Read\n---\nRead-only package",
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
        let (outcome, conflicts) = sync_selected(&cfg, &selected, ExecutionMode::Apply)?;

        assert!(outcome.stats.updated > 0);
        assert!(conflicts.is_empty());
        for root in [
            &cfg.central_skills_dir,
            &cfg.claude_skills_dir,
            &cfg.codex_skills_dir,
            &cfg.opencode_skills_dir,
        ] {
            let package = root.join("read-only");
            assert_eq!(fs::metadata(&package)?.permissions().mode() & 0o777, 0o500);
            assert_eq!(
                fs::metadata(package.join("SKILL.md"))?.permissions().mode() & 0o777,
                0o400
            );
            assert_eq!(
                fs::metadata(package.join("references"))?
                    .permissions()
                    .mode()
                    & 0o777,
                0o500
            );
            assert_eq!(
                read_markdown(&package.join("SKILL.md"))?.body,
                "Read-only package"
            );
            assert_eq!(
                fs::read_to_string(package.join("references/guide.md"))?,
                "guide"
            );
        }
        assert!(
            fs::read_to_string(cfg.claude_skills_dir.join("read-only/SKILL.md"))?
                .contains("allowed-tools: Read")
        );
        let (no_op, no_op_conflicts) = sync_selected(&cfg, &selected, ExecutionMode::Plan)?;
        assert!(no_op_conflicts.is_empty());
        assert_eq!(no_op.stats.updated, 0);

        crate::path_cleanup::remove_with_owner_access(&selected_path)?;
        for root in [
            &cfg.central_skills_dir,
            &cfg.claude_skills_dir,
            &cfg.codex_skills_dir,
            &cfg.opencode_skills_dir,
        ] {
            crate::path_cleanup::remove_with_owner_access(&root.join("read-only"))?;
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn successive_scoped_updates_clean_read_only_replacements_without_changing_new_modes(
    ) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        fn write_version(package: &Path, body: &str) -> io::Result<()> {
            fs::set_permissions(package, fs::Permissions::from_mode(0o700))?;
            fs::set_permissions(
                package.join("references"),
                fs::Permissions::from_mode(0o700),
            )?;
            fs::set_permissions(package.join("SKILL.md"), fs::Permissions::from_mode(0o600))?;
            fs::set_permissions(
                package.join("references/guide.md"),
                fs::Permissions::from_mode(0o600),
            )?;
            fs::write(package.join("SKILL.md"), doc("read-only-update", body))?;
            fs::write(package.join("references/guide.md"), body)?;
            fs::set_permissions(package.join("SKILL.md"), fs::Permissions::from_mode(0o400))?;
            fs::set_permissions(
                package.join("references/guide.md"),
                fs::Permissions::from_mode(0o400),
            )?;
            fs::set_permissions(
                package.join("references"),
                fs::Permissions::from_mode(0o500),
            )?;
            fs::set_permissions(package, fs::Permissions::from_mode(0o500))
        }

        let _lock = crate::ENV_LOCK.lock().unwrap();
        let (_tmp, cfg) = setup()?;
        let package = write_skill(
            &cfg.central_skills_dir,
            "read-only-update",
            &doc("read-only-update", "version zero"),
        )?;
        write_plain(&package.join("references/guide.md"), "version zero")?;
        fs::set_permissions(package.join("SKILL.md"), fs::Permissions::from_mode(0o400))?;
        fs::set_permissions(
            package.join("references/guide.md"),
            fs::Permissions::from_mode(0o400),
        )?;
        fs::set_permissions(
            package.join("references"),
            fs::Permissions::from_mode(0o500),
        )?;
        fs::set_permissions(&package, fs::Permissions::from_mode(0o500))?;
        std::env::set_var("RELAY_TEST_TEMP_STAMP", "4242");

        let result = (|| {
            let selected = discover_scoped_skills(std::slice::from_ref(&package))?;
            let (_, conflicts) = sync_selected(&cfg, &selected, ExecutionMode::Apply)?;
            assert!(conflicts.is_empty());

            for body in ["version one", "version two"] {
                write_version(&package, body)?;
                let selected = discover_scoped_skills(std::slice::from_ref(&package))?;
                let (outcome, conflicts) = sync_selected(&cfg, &selected, ExecutionMode::Apply)?;
                assert!(outcome.stats.updated > 0);
                assert!(conflicts.is_empty());

                for root in [
                    &cfg.claude_skills_dir,
                    &cfg.codex_skills_dir,
                    &cfg.opencode_skills_dir,
                ] {
                    let published = root.join("read-only-update");
                    assert_eq!(read_markdown(&published.join("SKILL.md"))?.body, body);
                    assert_eq!(
                        fs::read_to_string(published.join("references/guide.md"))?,
                        body
                    );
                    assert_eq!(
                        fs::metadata(&published)?.permissions().mode() & 0o777,
                        0o500
                    );
                    assert_eq!(
                        fs::metadata(published.join("references"))?
                            .permissions()
                            .mode()
                            & 0o777,
                        0o500
                    );
                    assert_eq!(
                        fs::metadata(published.join("SKILL.md"))?
                            .permissions()
                            .mode()
                            & 0o777,
                        0o400
                    );
                    assert_eq!(
                        fs::metadata(published.join("references/guide.md"))?
                            .permissions()
                            .mode()
                            & 0o777,
                        0o400
                    );
                    let artifacts = fs::read_dir(root)?
                        .map(|entry| entry.map(|entry| entry.file_name()))
                        .collect::<io::Result<Vec<_>>>()?;
                    assert!(artifacts.iter().all(|name| {
                        !name
                            .to_string_lossy()
                            .starts_with("read-only-update.relay.tmp.")
                    }));
                }
            }
            Ok(())
        })();

        std::env::remove_var("RELAY_TEST_TEMP_STAMP");
        for root in [
            &cfg.central_skills_dir,
            &cfg.claude_skills_dir,
            &cfg.codex_skills_dir,
            &cfg.opencode_skills_dir,
        ] {
            let _ = crate::path_cleanup::remove_with_owner_access(&root.join("read-only-update"));
        }
        result
    }

    #[cfg(unix)]
    #[test]
    fn prepared_skill_cleanup_handles_read_only_directories() -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new()?;
        let prepared = tmp.path().join("prepared");
        write_plain(&prepared.join("nested/file.txt"), "contents")?;
        fs::set_permissions(prepared.join("nested"), fs::Permissions::from_mode(0o500))?;
        fs::set_permissions(&prepared, fs::Permissions::from_mode(0o500))?;

        remove_prepared_skill_directory(&prepared)?;

        assert!(!prepared.exists());
        Ok(())
    }

    #[test]
    fn scoped_sync_detects_package_difference_of_only_an_empty_directory() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        write_skill(
            &cfg.central_skills_dir,
            "empty-difference",
            &doc("empty-difference", "Same body"),
        )?;
        let source_root = TempDir::new()?;
        let external = write_skill(
            source_root.path(),
            "empty-difference",
            &doc("empty-difference", "Same body"),
        )?;
        fs::create_dir_all(external.join("empty/nested"))?;
        let selected = discover_scoped_skills(&[external])?;
        let (outcome, conflicts) = sync_selected(&cfg, &selected, ExecutionMode::Plan)?;

        assert_eq!(outcome.stats.updated, 0);
        assert_eq!(conflicts.len(), 1);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn scoped_conflict_identity_includes_complete_contents_frontmatter_and_modes() -> io::Result<()>
    {
        use std::os::unix::fs::PermissionsExt;

        for case in [
            "hidden-content",
            "git-content",
            "node-modules-content",
            "nested-extra-file",
            "frontmatter",
            "executable-bit",
            "directory-mode",
            "root-mode",
        ] {
            let (_tmp, cfg) = setup()?;
            let canonical = write_skill(
                &cfg.central_skills_dir,
                "identity",
                &doc("identity", "Same body"),
            )?;
            let source_root = TempDir::new()?;
            let external = write_skill(
                source_root.path(),
                "identity",
                &doc("identity", "Same body"),
            )?;

            match case {
                "hidden-content" => {
                    write_plain(&canonical.join(".metadata"), "canonical")?;
                    write_plain(&external.join(".metadata"), "selected")?;
                }
                "git-content" => {
                    write_plain(&canonical.join(".git/config"), "canonical")?;
                    write_plain(&external.join(".git/config"), "selected")?;
                }
                "node-modules-content" => {
                    write_plain(&canonical.join("node_modules/pkg/index.js"), "canonical")?;
                    write_plain(&external.join("node_modules/pkg/index.js"), "selected")?;
                }
                "nested-extra-file" => {
                    write_plain(&external.join("references/nested/extra.md"), "extra")?;
                }
                "frontmatter" => {
                    write_plain(
                        &external.join("SKILL.md"),
                        "---\nname: identity\ndescription: identity description\nmetadata: selected\n---\nSame body",
                    )?;
                }
                "executable-bit" => {
                    for root in [&canonical, &external] {
                        write_plain(&root.join("scripts/run.sh"), "#!/bin/sh\n")?;
                    }
                    fs::set_permissions(
                        canonical.join("scripts/run.sh"),
                        fs::Permissions::from_mode(0o644),
                    )?;
                    fs::set_permissions(
                        external.join("scripts/run.sh"),
                        fs::Permissions::from_mode(0o755),
                    )?;
                }
                "directory-mode" => {
                    for root in [&canonical, &external] {
                        fs::create_dir_all(root.join("references"))?;
                    }
                    fs::set_permissions(
                        canonical.join("references"),
                        fs::Permissions::from_mode(0o700),
                    )?;
                    fs::set_permissions(
                        external.join("references"),
                        fs::Permissions::from_mode(0o750),
                    )?;
                }
                "root-mode" => {
                    fs::set_permissions(&canonical, fs::Permissions::from_mode(0o700))?;
                    fs::set_permissions(&external, fs::Permissions::from_mode(0o750))?;
                }
                _ => unreachable!(),
            }

            let selected = discover_scoped_skills(&[external])?;
            let (outcome, conflicts) = sync_selected(&cfg, &selected, ExecutionMode::Plan)?;

            assert_eq!(outcome.stats.updated, 0, "case {case}");
            assert_eq!(conflicts.len(), 1, "case {case}");
            assert!(!cfg.claude_skills_dir.join("identity").exists());
            assert!(!cfg.skill_state_path()?.exists());
        }
        Ok(())
    }

    #[test]
    fn scoped_sync_multiple_selected_packages_apply_together() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let source_root = TempDir::new()?;
        let first = write_skill(source_root.path(), "first", &doc("first", "First"))?;
        let second = write_skill(source_root.path(), "second", &doc("second", "Second"))?;
        let selected = discover_scoped_skills(&[first, second])?;
        let (outcome, conflicts) = sync_selected(&cfg, &selected, ExecutionMode::Apply)?;

        assert_eq!(outcome.stats.updated, 8);
        assert!(conflicts.is_empty());
        for name in ["first", "second"] {
            assert!(cfg.central_skills_dir.join(name).join("SKILL.md").exists());
            assert!(cfg.claude_skills_dir.join(name).join("SKILL.md").exists());
        }
        Ok(())
    }

    #[test]
    fn scoped_sync_refreshes_canonical_file_content_after_discovery() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let canonical = write_skill(
            &cfg.central_skills_dir,
            "selected",
            &doc("selected", "Before discovery"),
        )?;
        let initially_selected = discover_scoped_skills(std::slice::from_ref(&canonical))?;
        let (_, initial_conflicts) =
            sync_selected(&cfg, &initially_selected, ExecutionMode::Apply)?;
        assert!(initial_conflicts.is_empty());

        let selected = discover_scoped_skills(std::slice::from_ref(&canonical))?;
        write_plain(
            &canonical.join("SKILL.md"),
            &doc("selected", "After discovery"),
        )?;
        let (_, conflicts) = sync_selected(&cfg, &selected, ExecutionMode::Apply)?;

        assert!(conflicts.is_empty());
        let expected_state = persisted_hash(digest_skill_dir(&canonical)?.body_hash);
        let state = load_skill_state(&cfg.skill_state_path()?)?;
        assert_eq!(
            state.skills["selected"].canonical_hash,
            Some(expected_state)
        );
        for root in [
            &cfg.claude_skills_dir,
            &cfg.codex_skills_dir,
            &cfg.opencode_skills_dir,
        ] {
            assert_eq!(
                read_markdown(&root.join("selected/SKILL.md"))?.body,
                "After discovery"
            );
            assert_eq!(
                state.skills["selected"].adapter_hashes[root_label(&cfg, root)],
                expected_state
            );
        }
        Ok(())
    }

    #[test]
    fn scoped_sync_refreshes_external_file_content_into_conflict_before_writes() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let canonical = write_skill(
            &cfg.central_skills_dir,
            "selected",
            &doc("selected", "Initially identical"),
        )?;
        let source = TempDir::new()?;
        let external = write_skill(
            source.path(),
            "selected",
            &doc("selected", "Initially identical"),
        )?;
        let selected = discover_scoped_skills(std::slice::from_ref(&external))?;
        write_plain(
            &external.join("SKILL.md"),
            &doc("selected", "Changed after discovery"),
        )?;

        let (outcome, conflicts) = sync_selected(&cfg, &selected, ExecutionMode::Apply)?;

        assert_eq!(outcome.stats.updated, 0);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(
            read_markdown(&canonical.join("SKILL.md"))?.body,
            "Initially identical"
        );
        assert!(!cfg.claude_skills_dir.join("selected").exists());
        assert!(!cfg.skill_state_path()?.exists());
        Ok(())
    }

    #[test]
    fn scoped_preflight_revalidates_required_name_for_every_selection_before_writes(
    ) -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let source = TempDir::new()?;
        let first = write_skill(source.path(), "first", &doc("first", "First"))?;
        let second = write_skill(source.path(), "second", &doc("second", "Second"))?;
        let selected = discover_scoped_skills(&[first, second.clone()])?;
        write_plain(
            &second.join("SKILL.md"),
            &doc("renamed-after-discovery", "Second"),
        )?;

        let err = match sync_selected(&cfg, &selected, ExecutionMode::Apply) {
            Ok(_) => panic!("renamed selected skill should fail preflight"),
            Err(err) => err,
        };

        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("does not match discovered parent"));
        assert!(!cfg.central_skills_dir.join("first").exists());
        assert!(!cfg.central_skills_dir.join("second").exists());
        assert!(!cfg.skill_state_path()?.exists());
        Ok(())
    }

    fn root_label(cfg: &Config, root: &Path) -> &'static str {
        if root == cfg.claude_skills_dir {
            TOOL_CLAUDE
        } else if root == cfg.codex_skills_dir {
            TOOL_CODEX
        } else {
            TOOL_OPENCODE
        }
    }

    #[cfg(unix)]
    #[test]
    fn scoped_preflight_rejects_internal_adapter_symlink_and_fifo_before_any_write(
    ) -> io::Result<()> {
        use std::os::unix::fs::symlink;

        for unsupported in ["symlink", "fifo"] {
            let (_tmp, cfg) = setup()?;
            let source = TempDir::new()?;
            let first = write_skill(source.path(), "first", &doc("first", "First"))?;
            let second = write_skill(source.path(), "second", &doc("second", "Second"))?;
            let second_adapter =
                write_skill(&cfg.claude_skills_dir, "second", &doc("second", "Existing"))?;
            let outside = source.path().join("outside.txt");
            write_plain(&outside, "outside")?;
            let internal = second_adapter.join("references/unsupported");
            fs::create_dir_all(internal.parent().unwrap())?;
            if unsupported == "symlink" {
                symlink(&outside, &internal)?;
            } else {
                let fifo =
                    CString::new(internal.as_os_str().as_bytes()).map_err(io::Error::other)?;
                // SAFETY: `fifo` is a live NUL-terminated path and `mkfifo` does not retain it.
                if unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) } != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            let selected = discover_scoped_skills(&[first, second])?;

            let err = match sync_selected(&cfg, &selected, ExecutionMode::Apply) {
                Ok(_) => panic!("unsupported adapter entry should fail preflight"),
                Err(err) => err,
            };

            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
            assert!(!cfg.central_skills_dir.join("first").exists());
            assert!(!cfg.central_skills_dir.join("second").exists());
            assert!(!cfg.skill_state_path()?.exists());
            assert_eq!(fs::read_to_string(&outside)?, "outside");
            assert!(
                fs::symlink_metadata(&internal)?.file_type().is_symlink() || unsupported == "fifo"
            );
        }
        Ok(())
    }

    #[test]
    fn scoped_sync_blacklist_preserves_existing_adapter_and_ownership_claim() -> io::Result<()> {
        let (_tmp, mut cfg) = setup()?;
        let canonical = write_skill(
            &cfg.central_skills_dir,
            "selected",
            &doc("selected", "Original"),
        )?;
        let selected = discover_scoped_skills(std::slice::from_ref(&canonical))?;
        let (_, initial_conflicts) = sync_selected(&cfg, &selected, ExecutionMode::Apply)?;
        assert!(initial_conflicts.is_empty());
        let adapter = cfg.claude_skills_dir.join("selected");
        write_plain(&adapter.join(".preserved"), "user content")?;
        let adapter_skill_before = fs::read(adapter.join("SKILL.md"))?;
        let adapter_hidden_before = fs::read(adapter.join(".preserved"))?;
        let state_before = load_skill_state(&cfg.skill_state_path()?)?;
        let ownership_before = state_before.skills["selected"].adapter_hashes[TOOL_CLAUDE];

        cfg.blacklist
            .entry("skills/selected".to_string())
            .or_default()
            .push(TOOL_CLAUDE.to_string());
        write_plain(
            &canonical.join("SKILL.md"),
            &doc("selected", "Updated canonical"),
        )?;
        let selected = discover_scoped_skills(&[canonical])?;

        let (_, conflicts) = sync_selected(&cfg, &selected, ExecutionMode::Apply)?;

        assert!(conflicts.is_empty());
        assert_eq!(fs::read(adapter.join("SKILL.md"))?, adapter_skill_before);
        assert_eq!(fs::read(adapter.join(".preserved"))?, adapter_hidden_before);
        assert!(
            fs::read_to_string(cfg.codex_skills_dir.join("selected/SKILL.md"))?
                .contains("Updated canonical")
        );
        let state = load_skill_state(&cfg.skill_state_path()?)?;
        let entry = &state.skills["selected"];
        assert_eq!(entry.adapter_hashes[TOOL_CLAUDE], ownership_before);
        assert!(entry.adapter_hashes.contains_key(TOOL_CODEX));
        Ok(())
    }

    #[test]
    fn scoped_shared_adapter_remains_owned_through_full_sync() -> io::Result<()> {
        let (tmp, mut cfg) = setup()?;
        let shared = tmp.path().join("shared-skills");
        cfg.claude_skills_dir = shared.clone();
        cfg.codex_skills_dir = shared.clone();
        cfg.enabled_tools = vec![TOOL_CLAUDE.to_string(), TOOL_CODEX.to_string()];
        cfg.blacklist
            .entry("skills/selected".to_string())
            .or_default()
            .push(TOOL_CLAUDE.to_string());
        let unrelated = write_skill(
            &cfg.opencode_skills_dir,
            "unrelated",
            &doc("unrelated", "Must remain unchanged"),
        )?;
        let unrelated_before = fs::read(unrelated.join("SKILL.md"))?;
        let source = TempDir::new()?;
        let selected_path = write_skill(source.path(), "selected", &doc("selected", "Selected"))?;
        let selected = discover_scoped_skills(&[selected_path])?;

        let (outcome, conflicts) = sync_selected(&cfg, &selected, ExecutionMode::Apply)?;

        assert!(conflicts.is_empty());
        assert_eq!(outcome.stats.updated, 2);
        assert!(outcome.codex_real_skill_names.contains("selected"));
        assert!(cfg.central_skills_dir.join("selected/SKILL.md").exists());
        assert!(shared.join("selected/SKILL.md").exists());
        assert_eq!(fs::read(unrelated.join("SKILL.md"))?, unrelated_before);
        let state = load_skill_state(&cfg.skill_state_path()?)?;
        let entry = &state.skills["selected"];
        assert_eq!(
            entry
                .adapter_hashes
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec![TOOL_CLAUDE, TOOL_CODEX]
        );
        assert_eq!(
            entry
                .adapter_complete_hashes
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec![TOOL_CLAUDE, TOOL_CODEX]
        );

        let canonical = cfg.central_skills_dir.join("selected");
        write_plain(
            &canonical.join("SKILL.md"),
            &doc("selected", "Updated canonical"),
        )?;
        sync_skills(&cfg, SyncLogMode::Quiet)?;
        assert_eq!(
            read_markdown(&shared.join("selected/SKILL.md"))?.body,
            "Updated canonical"
        );

        fs::remove_dir_all(&canonical)?;
        sync_skills(&cfg, SyncLogMode::Quiet)?;
        assert!(!shared.join("selected").exists());
        let state = load_skill_state(&cfg.skill_state_path()?)?;
        let entry = &state.skills["selected"];
        assert!(entry.tombstoned);
        assert!(entry.adapter_hashes.is_empty());
        assert!(entry.adapter_complete_hashes.is_empty());
        Ok(())
    }

    #[test]
    fn scoped_sync_blacklist_bypasses_nonportable_adapter_collisions() -> io::Result<()> {
        for (selected_name, colliding_name) in [("Portable", "portable"), ("CAFÉ", "cafe\u{301}")]
        {
            let (_tmp, mut cfg) = setup()?;
            cfg.blacklist
                .entry(format!("skills/{selected_name}"))
                .or_default()
                .push(TOOL_CLAUDE.to_string());
            let colliding = cfg.claude_skills_dir.join(colliding_name);
            write_plain(&colliding.join("sentinel.txt"), "must remain untouched")?;
            let adapter_entries_before = fs::read_dir(&cfg.claude_skills_dir)?
                .map(|entry| entry.map(|entry| entry.file_name()))
                .collect::<io::Result<Vec<_>>>()?;
            let source = TempDir::new()?;
            let external = write_skill(
                source.path(),
                selected_name,
                &doc(selected_name, "Selected"),
            )?;
            let selected = discover_scoped_skills(&[external])?;

            let (outcome, conflicts) = sync_selected(&cfg, &selected, ExecutionMode::Apply)?;

            assert!(outcome.stats.updated > 0);
            assert!(conflicts.is_empty());
            assert_eq!(
                fs::read_to_string(colliding.join("sentinel.txt"))?,
                "must remain untouched"
            );
            assert_eq!(
                fs::read_dir(&cfg.claude_skills_dir)?
                    .map(|entry| entry.map(|entry| entry.file_name()))
                    .collect::<io::Result<Vec<_>>>()?,
                adapter_entries_before
            );
            assert!(
                !load_skill_state(&cfg.skill_state_path()?)?.skills[selected_name]
                    .adapter_hashes
                    .contains_key(TOOL_CLAUDE)
            );
            for root in [
                &cfg.central_skills_dir,
                &cfg.codex_skills_dir,
                &cfg.opencode_skills_dir,
            ] {
                assert_eq!(
                    read_markdown(&root.join(selected_name).join("SKILL.md"))?.body,
                    "Selected"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn scoped_external_selection_rebuilds_tombstoned_ownership() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let mut state = SkillState::default();
        state.skills.insert(
            "restored".to_string(),
            SkillStateEntry {
                canonical_hash: None,
                tombstoned: true,
                adapter_hashes: BTreeMap::from([
                    (TOOL_CLAUDE.to_string(), 11),
                    ("retired-adapter".to_string(), 12),
                ]),
                adapter_complete_hashes: BTreeMap::from([
                    (TOOL_CLAUDE.to_string(), 21),
                    ("retired-adapter".to_string(), 22),
                ]),
            },
        );
        save_skill_state(&cfg.skill_state_path()?, &state, &mut None)?;
        let source = TempDir::new()?;
        let external = write_skill(
            source.path(),
            "restored",
            &doc("restored", "Restored externally"),
        )?;
        let selected = discover_scoped_skills(&[external])?;

        let (outcome, conflicts) = sync_selected(&cfg, &selected, ExecutionMode::Apply)?;

        assert_eq!(outcome.stats.updated, 4);
        assert!(conflicts.is_empty());
        let state = load_skill_state(&cfg.skill_state_path()?)?;
        let entry = &state.skills["restored"];
        assert!(!entry.tombstoned);
        assert!(entry.canonical_hash.is_some());
        assert_eq!(entry.adapter_hashes.len(), 3);
        assert_eq!(entry.adapter_complete_hashes.len(), 3);
        assert!(!entry.adapter_hashes.contains_key("retired-adapter"));
        assert!(!entry
            .adapter_complete_hashes
            .contains_key("retired-adapter"));
        for root in [
            &cfg.central_skills_dir,
            &cfg.claude_skills_dir,
            &cfg.codex_skills_dir,
            &cfg.opencode_skills_dir,
        ] {
            assert_eq!(
                read_markdown(&root.join("restored/SKILL.md"))?.body,
                "Restored externally"
            );
        }
        Ok(())
    }

    #[test]
    fn scoped_selection_ignores_divergent_same_name_legacy_import_source() -> io::Result<()> {
        let _env = crate::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let tmp = TempDir::new()?;
        let previous_home = env::var_os("RELAY_HOME");
        env::set_var("RELAY_HOME", tmp.path());

        let result = (|| -> io::Result<()> {
            let cfg = Config::default_paths()?;
            let legacy = write_skill(
                &tmp.path().join(".codex/skills"),
                "selected",
                &doc("selected", "Divergent legacy source"),
            )?;
            let legacy_before = fs::read(legacy.join("SKILL.md"))?;
            let external_root = TempDir::new()?;
            let external = write_skill(
                external_root.path(),
                "selected",
                &doc("selected", "Explicit selection"),
            )?;
            let selected = discover_scoped_skills(&[external])?;

            let (outcome, conflicts) = sync_selected(&cfg, &selected, ExecutionMode::Apply)?;

            assert_eq!(outcome.stats.updated, 2);
            assert!(conflicts.is_empty());
            assert_eq!(fs::read(legacy.join("SKILL.md"))?, legacy_before);
            assert_eq!(
                read_markdown(&cfg.central_skills_dir.join("selected/SKILL.md"))?.body,
                "Explicit selection"
            );
            assert_eq!(
                read_markdown(&cfg.claude_skills_dir.join("selected/SKILL.md"))?.body,
                "Explicit selection"
            );
            Ok(())
        })();

        match previous_home {
            Some(value) => env::set_var("RELAY_HOME", value),
            None => env::remove_var("RELAY_HOME"),
        }
        result
    }

    #[test]
    fn scoped_sync_restores_selected_tombstone_without_changing_unrelated_state() -> io::Result<()>
    {
        let (_tmp, cfg) = setup()?;
        let selected_canonical = write_skill(
            &cfg.central_skills_dir,
            "selected",
            &doc("selected", "Original"),
        )?;
        write_skill(
            &cfg.central_skills_dir,
            "unrelated",
            &doc("unrelated", "Unrelated"),
        )?;
        sync_skills(&cfg, SyncLogMode::Quiet)?;
        fs::remove_dir_all(selected_canonical)?;
        sync_skills(&cfg, SyncLogMode::Quiet)?;
        let tombstoned = load_skill_state(&cfg.skill_state_path()?)?;
        assert!(tombstoned.skills["selected"].tombstoned);
        let unrelated_before =
            toml::to_string(&tombstoned.skills["unrelated"]).map_err(io::Error::other)?;

        let restored = write_skill(
            &cfg.central_skills_dir,
            "selected",
            &doc("selected", "Restored"),
        )?;
        let selected = discover_scoped_skills(&[restored])?;
        let (_, conflicts) = sync_selected(&cfg, &selected, ExecutionMode::Apply)?;

        assert!(conflicts.is_empty());
        let state = load_skill_state(&cfg.skill_state_path()?)?;
        let entry = &state.skills["selected"];
        assert!(!entry.tombstoned);
        assert!(entry.canonical_hash.is_some());
        assert_eq!(entry.adapter_hashes.len(), 3);
        assert_eq!(
            toml::to_string(&state.skills["unrelated"]).map_err(io::Error::other)?,
            unrelated_before
        );
        for root in [
            &cfg.claude_skills_dir,
            &cfg.codex_skills_dir,
            &cfg.opencode_skills_dir,
        ] {
            assert!(fs::read_to_string(root.join("selected/SKILL.md"))?.contains("Restored"));
        }
        Ok(())
    }

    #[test]
    fn scoped_sync_preserves_unselected_tombstone_adapters_and_import_source() -> io::Result<()> {
        let _env = crate::ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let tmp = TempDir::new()?;
        let previous_home = std::env::var_os("RELAY_HOME");
        std::env::set_var("RELAY_HOME", tmp.path());

        let result = (|| -> io::Result<()> {
            let cfg = Config::default_paths()?;
            let retired_adapter = write_skill(
                &cfg.claude_skills_dir,
                "retired",
                &doc("retired", "Preserved adapter"),
            )?;
            let retired_hash = persisted_hash(digest_skill_dir(&retired_adapter)?.body_hash);
            let mut state = SkillState::default();
            state.skills.insert(
                "retired".to_string(),
                SkillStateEntry {
                    canonical_hash: None,
                    tombstoned: true,
                    adapter_hashes: BTreeMap::from([(TOOL_CLAUDE.to_string(), retired_hash)]),
                    adapter_complete_hashes: BTreeMap::new(),
                },
            );
            save_skill_state(&cfg.skill_state_path()?, &state, &mut None)?;
            let state_before = fs::read_to_string(cfg.skill_state_path()?)?;
            let adapter_before = fs::read_to_string(retired_adapter.join("SKILL.md"))?;

            let import_root = tmp.path().join(".codex/skills");
            let import_only = write_skill(
                &import_root,
                "legacy-only",
                &doc("legacy-only", "Legacy source"),
            )?;
            let import_before = fs::read_to_string(import_only.join("SKILL.md"))?;

            let source = TempDir::new()?;
            let selected_path =
                write_skill(source.path(), "selected", &doc("selected", "Selected"))?;
            let selected = discover_scoped_skills(&[selected_path])?;
            let (_, conflicts) = sync_selected(&cfg, &selected, ExecutionMode::Apply)?;

            assert!(conflicts.is_empty());
            let state_after = load_skill_state(&cfg.skill_state_path()?)?;
            let before_value: toml::Value = toml::from_str(&state_before).unwrap();
            assert_eq!(
                toml::Value::try_from(&state_after.skills["retired"]).unwrap(),
                before_value["skills"]["retired"]
            );
            assert_eq!(
                fs::read_to_string(retired_adapter.join("SKILL.md"))?,
                adapter_before
            );
            assert_eq!(
                fs::read_to_string(import_only.join("SKILL.md"))?,
                import_before
            );
            assert!(!cfg.central_skills_dir.join("retired").exists());
            assert!(!cfg.central_skills_dir.join("legacy-only").exists());
            assert!(cfg.central_skills_dir.join("selected/SKILL.md").exists());
            Ok(())
        })();

        match previous_home {
            Some(value) => std::env::set_var("RELAY_HOME", value),
            None => std::env::remove_var("RELAY_HOME"),
        }
        result
    }

    #[test]
    fn scoped_sync_plan_matches_apply_without_writes() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let source_root = TempDir::new()?;
        let path = write_skill(source_root.path(), "plan", &doc("plan", "Body"))?;
        write_plain(&path.join("scripts/run.sh"), "run")?;
        let selected = discover_scoped_skills(&[path])?;
        let (plan, plan_conflicts) = sync_selected(&cfg, &selected, ExecutionMode::Plan)?;

        assert!(!cfg.central_skills_dir.join("plan").exists());
        assert!(!cfg.claude_skills_dir.join("plan").exists());
        assert!(!cfg.skill_state_path()?.exists());

        let (apply, apply_conflicts) = sync_selected(&cfg, &selected, ExecutionMode::Apply)?;

        assert_eq!(plan.stats.updated, apply.stats.updated);
        assert_eq!(plan_conflicts, apply_conflicts);
        assert!(cfg.central_skills_dir.join("plan/scripts/run.sh").exists());
        Ok(())
    }

    #[test]
    fn scoped_plan_apply_parity_covers_mixed_state_custom_paths_and_operands() -> io::Result<()> {
        let (tmp, mut cfg) = setup()?;
        let custom = tmp.path().join("custom");
        cfg.central_skills_dir = custom.join("canonical");
        cfg.claude_skills_dir = custom.join("claude");
        cfg.codex_skills_dir = custom.join("codex");
        cfg.opencode_skills_dir = custom.join("opencode");

        let existing = write_skill(
            &cfg.central_skills_dir,
            "existing",
            &doc("existing", "Canonical current"),
        )?;
        write_skill(
            &cfg.claude_skills_dir,
            "existing",
            &doc("existing", "Adapter stale"),
        )?;
        let restored = write_skill(
            &cfg.central_skills_dir,
            "restored",
            &doc("restored", "Restored canonical"),
        )?;
        let unrelated = write_skill(
            &cfg.central_skills_dir,
            "unrelated",
            &doc("unrelated", "Do not touch"),
        )?;
        let unrelated_before = fs::read(unrelated.join("SKILL.md"))?;
        let preserved = write_skill(
            &cfg.claude_skills_dir,
            "new",
            &doc("new", "Blacklisted adapter"),
        )?;
        let preserved_before = fs::read(preserved.join("SKILL.md"))?;
        cfg.blacklist
            .entry("skills/new".to_string())
            .or_default()
            .push(TOOL_CLAUDE.to_string());
        let mut state = SkillState::default();
        state.skills.insert(
            "restored".to_string(),
            SkillStateEntry {
                canonical_hash: None,
                tombstoned: true,
                adapter_hashes: BTreeMap::from([("stale".to_string(), 1)]),
                adapter_complete_hashes: BTreeMap::new(),
            },
        );
        save_skill_state(&cfg.skill_state_path()?, &state, &mut None)?;
        let state_before = fs::read(cfg.skill_state_path()?)?;
        let source = TempDir::new()?;
        let new = write_skill(source.path(), "new", &doc("new", "New external"))?;
        let selected = discover_scoped_skills(&[existing, restored, new])?;

        let (plan, plan_conflicts) = sync_selected(&cfg, &selected, ExecutionMode::Plan)?;

        assert!(plan_conflicts.is_empty());
        assert_eq!(fs::read(cfg.skill_state_path()?)?, state_before);
        assert!(!cfg.central_skills_dir.join("new").exists());
        assert!(!cfg.codex_skills_dir.join("existing").exists());
        assert!(!cfg.opencode_skills_dir.join("restored").exists());
        assert_eq!(fs::read(preserved.join("SKILL.md"))?, preserved_before);
        assert_eq!(fs::read(unrelated.join("SKILL.md"))?, unrelated_before);

        let (apply, apply_conflicts) = sync_selected(&cfg, &selected, ExecutionMode::Apply)?;

        assert_eq!(plan.stats.updated, apply.stats.updated);
        assert_eq!(plan_conflicts, apply_conflicts);
        assert_eq!(fs::read(preserved.join("SKILL.md"))?, preserved_before);
        assert_eq!(fs::read(unrelated.join("SKILL.md"))?, unrelated_before);
        assert_eq!(
            read_markdown(&cfg.central_skills_dir.join("new/SKILL.md"))?.body,
            "New external"
        );
        for (name, expected) in [
            ("existing", "Canonical current"),
            ("restored", "Restored canonical"),
            ("new", "New external"),
        ] {
            for root in [&cfg.codex_skills_dir, &cfg.opencode_skills_dir] {
                assert_eq!(
                    read_markdown(&root.join(name).join("SKILL.md"))?.body,
                    expected
                );
            }
        }
        let state = load_skill_state(&cfg.skill_state_path()?)?;
        assert!(!state.skills["restored"].tombstoned);
        assert!(!state.skills["restored"]
            .adapter_hashes
            .contains_key("stale"));
        Ok(())
    }

    #[test]
    fn scoped_sync_honors_custom_canonical_and_adapter_paths() -> io::Result<()> {
        let (_tmp, mut cfg) = setup()?;
        cfg.central_skills_dir = cfg.central_dir.parent().unwrap().join("custom/canonical");
        cfg.claude_skills_dir = cfg
            .central_dir
            .parent()
            .unwrap()
            .join("custom/claude-adapter");
        cfg.codex_skills_dir = cfg
            .central_dir
            .parent()
            .unwrap()
            .join("custom/disabled-codex");
        cfg.opencode_skills_dir = cfg
            .central_dir
            .parent()
            .unwrap()
            .join("custom/disabled-opencode");
        cfg.enabled_tools = vec![TOOL_CLAUDE.to_string()];
        let disabled_opencode = write_skill(
            &cfg.opencode_skills_dir,
            "preserved",
            &doc("preserved", "Unchanged"),
        )?;
        let disabled_opencode_before = fs::read(disabled_opencode.join("SKILL.md"))?;
        let source_root = TempDir::new()?;
        let path = write_skill(source_root.path(), "custom", &doc("custom", "Body"))?;
        let selected = discover_scoped_skills(&[path])?;
        let (_, conflicts) = sync_selected(&cfg, &selected, ExecutionMode::Apply)?;

        assert!(conflicts.is_empty());
        assert!(cfg.central_skills_dir.join("custom/SKILL.md").exists());
        assert!(cfg.claude_skills_dir.join("custom/SKILL.md").exists());
        assert!(!cfg.codex_skills_dir.exists());
        assert!(!cfg.opencode_skills_dir.join("custom").exists());
        assert_eq!(
            fs::read(disabled_opencode.join("SKILL.md"))?,
            disabled_opencode_before
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn scoped_discovery_skips_collection_symlinks_and_rejects_package_symlinks() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let collection = tmp.path().join("collection");
        fs::create_dir_all(&collection)?;
        let real = write_skill(&collection, "real", &doc("real", "Real"))?;
        let external = write_skill(
            &tmp.path().join("external"),
            "linked",
            &doc("linked", "Linked"),
        )?;
        std::os::unix::fs::symlink(&external, collection.join("linked"))?;

        let selected = discover_scoped_skills(std::slice::from_ref(&collection))?;
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name, "real");

        let err = discover_scoped_skills(&[tmp.path().join("collection/linked")]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err
            .to_string()
            .contains("symlinks are not supported as skill inputs"));

        std::os::unix::fs::symlink(real.join("SKILL.md"), real.join("alias.md"))?;
        let err = discover_scoped_skills(&[real]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("symlinks are not supported"));

        fs::remove_file(collection.join("real/alias.md"))?;
        std::os::unix::fs::symlink(&external, collection.join("real/references"))?;
        let err = discover_scoped_skills(&[collection.join("real")]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("symlinks are not supported"));
        Ok(())
    }

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

        // State written before complete-package ownership hashes existed remains valid.
        let mut state = load_skill_state(&cfg.skill_state_path()?)?;
        state
            .skills
            .get_mut("plan")
            .expect("plan state exists")
            .adapter_complete_hashes
            .clear();
        save_skill_state(&cfg.skill_state_path()?, &state, &mut None)?;

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

    #[cfg(unix)]
    #[test]
    fn canonical_deletion_after_scoped_import_preserves_complete_package_edits() -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        for change in ["hidden", "empty-directory", "mode"] {
            let (_tmp, cfg) = setup()?;
            let source = TempDir::new()?;
            let package = write_skill(source.path(), change, &doc(change, "Body"))?;
            let selected = discover_scoped_skills(&[package])?;
            let (_, conflicts) = sync_selected(&cfg, &selected, ExecutionMode::Apply)?;
            assert!(conflicts.is_empty());

            let canonical = cfg.central_skills_dir.join(change);
            let adapter = cfg.claude_skills_dir.join(change);
            match change {
                "hidden" => write_plain(&adapter.join(".local/settings"), "user-owned")?,
                "empty-directory" => fs::create_dir_all(adapter.join("local-empty/nested"))?,
                "mode" => fs::set_permissions(
                    adapter.join("SKILL.md"),
                    fs::Permissions::from_mode(0o600),
                )?,
                _ => unreachable!(),
            }
            fs::remove_dir_all(&canonical)?;

            sync_skills(&cfg, SyncLogMode::Quiet)?;

            assert!(adapter.exists(), "{change} edit should preserve adapter");
            match change {
                "hidden" => assert_eq!(
                    fs::read_to_string(adapter.join(".local/settings"))?,
                    "user-owned"
                ),
                "empty-directory" => assert!(adapter.join("local-empty/nested").is_dir()),
                "mode" => assert_eq!(
                    fs::metadata(adapter.join("SKILL.md"))?.permissions().mode() & 0o777,
                    0o600
                ),
                _ => unreachable!(),
            }
            assert!(!canonical.exists());
            let state = load_skill_state(&cfg.skill_state_path()?)?;
            assert!(state.skills[change].tombstoned);
            assert!(state.skills[change]
                .adapter_complete_hashes
                .contains_key(TOOL_CLAUDE));
        }
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

        let list = list_skill_dirs_with_policy(&dir, true)?;
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
    fn publish_skill_directory_replaces_existing_package() -> io::Result<()> {
        let _lock = crate::ENV_LOCK.lock().unwrap();
        let tmp = TempDir::new()?;
        let target = tmp.path().join("target");
        std::env::set_var("RELAY_TEST_TEMP_STAMP", "111");
        let prepared = skill_temp_path(&target);
        let backup = skill_backup_path(&target);
        write_plain(&target.join("old.txt"), "old")?;
        write_plain(&prepared.join("new.txt"), "new")?;

        let result = publish_skill_directory(&prepared, &target);
        std::env::remove_var("RELAY_TEST_TEMP_STAMP");
        result?;

        assert!(!prepared.exists());
        assert!(!backup.exists());
        assert!(!target.join("old.txt").exists());
        assert_eq!(fs::read_to_string(target.join("new.txt"))?, "new");
        Ok(())
    }

    #[test]
    fn absent_target_publish_failure_cleans_temp_and_allows_retry() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let target = tmp.path().join("target");
        let prepared = tmp.path().join("prepared");
        write_plain(&prepared.join("new.txt"), "new")?;

        let err = publish_prepared_skill_directory_with(&prepared, &target, |_from, _to| {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "injected absent-target publish failure",
            ))
        })
        .unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(!prepared.exists());
        assert!(!target.exists());
        write_plain(&prepared.join("new.txt"), "new")?;
        publish_prepared_skill_directory_with(&prepared, &target, |from, to| fs::rename(from, to))?;
        assert_eq!(fs::read_to_string(target.join("new.txt"))?, "new");
        assert!(!prepared.exists());
        Ok(())
    }

    #[test]
    fn publish_skill_directory_non_exchange_fallback_succeeds() -> io::Result<()> {
        let _lock = crate::ENV_LOCK.lock().unwrap();
        let tmp = TempDir::new()?;
        let target = tmp.path().join("target");
        let prepared = tmp.path().join("prepared");
        write_plain(&target.join("old.txt"), "old")?;
        write_plain(&prepared.join("new.txt"), "new")?;
        std::env::set_var("RELAY_TEST_TEMP_STAMP", "321");
        let backup = skill_backup_path(&target);

        publish_skill_directory_with(
            &prepared,
            &target,
            |from, to| fs::rename(from, to),
            |_left, _right| Ok(false),
            |path| fs::remove_dir_all(path),
        )?;

        std::env::remove_var("RELAY_TEST_TEMP_STAMP");
        assert!(!prepared.exists());
        assert!(!backup.exists());
        assert!(!target.join("old.txt").exists());
        assert_eq!(fs::read_to_string(target.join("new.txt"))?, "new");
        Ok(())
    }

    #[test]
    fn fallback_stale_backup_cleanup_failure_keeps_target_and_prepared_intact() -> io::Result<()> {
        let _lock = crate::ENV_LOCK.lock().unwrap();
        let tmp = TempDir::new()?;
        let target = tmp.path().join("target");
        let prepared = tmp.path().join("prepared");
        write_plain(&target.join("old.txt"), "old")?;
        write_plain(&prepared.join("new.txt"), "new")?;
        std::env::set_var("RELAY_TEST_TEMP_STAMP", "765");
        let backup = skill_backup_path(&target);
        write_plain(&backup.join("stale.txt"), "stale")?;
        let mut rename_called = false;

        let err = publish_skill_directory_with(
            &prepared,
            &target,
            |_from, _to| {
                rename_called = true;
                Ok(())
            },
            |_left, _right| Ok(false),
            |_path| {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected stale backup cleanup failure",
                ))
            },
        )
        .unwrap_err();

        std::env::remove_var("RELAY_TEST_TEMP_STAMP");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(err
            .to_string()
            .contains("injected stale backup cleanup failure"));
        assert!(!rename_called);
        assert_eq!(fs::read_to_string(target.join("old.txt"))?, "old");
        assert_eq!(fs::read_to_string(prepared.join("new.txt"))?, "new");
        assert_eq!(fs::read_to_string(backup.join("stale.txt"))?, "stale");
        Ok(())
    }

    #[test]
    fn fallback_target_backup_rename_failure_keeps_target_and_prepared_intact() -> io::Result<()> {
        let _lock = crate::ENV_LOCK.lock().unwrap();
        let tmp = TempDir::new()?;
        let target = tmp.path().join("target");
        let prepared = tmp.path().join("prepared");
        write_plain(&target.join("old.txt"), "old")?;
        write_plain(&prepared.join("new.txt"), "new")?;
        std::env::set_var("RELAY_TEST_TEMP_STAMP", "876");
        let backup = skill_backup_path(&target);

        let err = publish_skill_directory_with(
            &prepared,
            &target,
            |_from, _to| {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected target backup rename failure",
                ))
            },
            |_left, _right| Ok(false),
            |path| fs::remove_dir_all(path),
        )
        .unwrap_err();

        std::env::remove_var("RELAY_TEST_TEMP_STAMP");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(err
            .to_string()
            .contains("injected target backup rename failure"));
        assert_eq!(fs::read_to_string(target.join("old.txt"))?, "old");
        assert_eq!(fs::read_to_string(prepared.join("new.txt"))?, "new");
        assert!(!backup.exists());
        Ok(())
    }

    #[test]
    fn publish_skill_directory_exchange_cleanup_failure_is_committed() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let target = tmp.path().join("target");
        let prepared = tmp.path().join("prepared");
        let swap = tmp.path().join("swap");
        write_plain(&target.join("old.txt"), "old")?;
        write_plain(&prepared.join("new.txt"), "new")?;

        publish_skill_directory_with(
            &prepared,
            &target,
            |from, to| fs::rename(from, to),
            |left, right| {
                fs::rename(left, &swap)?;
                fs::rename(right, left)?;
                fs::rename(&swap, right)?;
                Ok(true)
            },
            |_path| {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected exchange cleanup failure",
                ))
            },
        )?;

        assert_eq!(fs::read_to_string(target.join("new.txt"))?, "new");
        assert_eq!(fs::read_to_string(prepared.join("old.txt"))?, "old");
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_exchange_unsupported_errno_classification_is_explicit() {
        assert!(exchange_is_unsupported(libc::EINVAL));
        assert!(exchange_is_unsupported(libc::ENOTSUP));
        assert!(!exchange_is_unsupported(libc::EPERM));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_exchange_unsupported_errno_classification_is_explicit() {
        assert!(exchange_is_unsupported(libc::ENOSYS));
        assert!(exchange_is_unsupported(libc::EINVAL));
        assert!(exchange_is_unsupported(libc::ENOTSUP));
        assert!(!exchange_is_unsupported(libc::EPERM));
    }

    #[test]
    fn publish_skill_directory_backup_cleanup_failure_is_committed() -> io::Result<()> {
        let _lock = crate::ENV_LOCK.lock().unwrap();
        let tmp = TempDir::new()?;
        let target = tmp.path().join("target");
        let prepared = tmp.path().join("prepared");
        write_plain(&target.join("old.txt"), "old")?;
        write_plain(&prepared.join("new.txt"), "new")?;
        std::env::set_var("RELAY_TEST_TEMP_STAMP", "987");
        let backup = skill_backup_path(&target);

        let result = publish_skill_directory_with(
            &prepared,
            &target,
            |from, to| fs::rename(from, to),
            |_left, _right| Ok(false),
            |_path| {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected backup cleanup failure",
                ))
            },
        );

        std::env::remove_var("RELAY_TEST_TEMP_STAMP");
        result?;
        assert_eq!(fs::read_to_string(target.join("new.txt"))?, "new");
        assert_eq!(fs::read_to_string(backup.join("old.txt"))?, "old");
        Ok(())
    }

    #[test]
    fn publish_skill_directory_propagates_unexpected_exchange_error_without_renames(
    ) -> io::Result<()> {
        let tmp = TempDir::new()?;
        let target = tmp.path().join("target");
        let prepared = tmp.path().join("prepared");
        write_plain(&target.join("old.txt"), "old")?;
        write_plain(&prepared.join("new.txt"), "new")?;
        let mut rename_called = false;

        let err = publish_skill_directory_with(
            &prepared,
            &target,
            |_from, _to| {
                rename_called = true;
                Ok(())
            },
            |_left, _right| {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected exchange failure",
                ))
            },
            |path| fs::remove_dir_all(path),
        )
        .unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(err.to_string().contains("injected exchange failure"));
        assert!(!rename_called);
        assert_eq!(fs::read_to_string(target.join("old.txt"))?, "old");
        assert_eq!(fs::read_to_string(prepared.join("new.txt"))?, "new");
        Ok(())
    }

    #[test]
    fn publish_skill_directory_restores_previous_package_when_publish_rename_fails(
    ) -> io::Result<()> {
        let _lock = crate::ENV_LOCK.lock().unwrap();
        let tmp = TempDir::new()?;
        let target = tmp.path().join("target");
        let prepared = tmp.path().join("prepared");
        write_plain(&target.join("old.txt"), "old")?;
        write_plain(&prepared.join("new.txt"), "new")?;
        std::env::set_var("RELAY_TEST_TEMP_STAMP", "456");
        let backup = skill_backup_path(&target);
        let mut rename_count = 0;

        let err = publish_skill_directory_with(
            &prepared,
            &target,
            |from, to| {
                rename_count += 1;
                if rename_count == 2 {
                    Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "injected publish failure",
                    ))
                } else {
                    fs::rename(from, to)
                }
            },
            |_left, _right| Ok(false),
            |path| fs::remove_dir_all(path),
        )
        .unwrap_err();

        std::env::remove_var("RELAY_TEST_TEMP_STAMP");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(err.to_string().contains("previous package was restored"));
        assert_eq!(fs::read_to_string(target.join("old.txt"))?, "old");
        assert_eq!(fs::read_to_string(prepared.join("new.txt"))?, "new");
        assert!(!backup.exists());
        Ok(())
    }

    #[test]
    fn publish_skill_directory_preserves_backup_when_publish_and_restore_fail() -> io::Result<()> {
        let _lock = crate::ENV_LOCK.lock().unwrap();
        let tmp = TempDir::new()?;
        let target = tmp.path().join("target");
        let prepared = tmp.path().join("prepared");
        write_plain(&target.join("old.txt"), "old")?;
        write_plain(&prepared.join("new.txt"), "new")?;
        std::env::set_var("RELAY_TEST_TEMP_STAMP", "654");
        let backup = skill_backup_path(&target);
        let mut rename_count = 0;

        let err = publish_skill_directory_with(
            &prepared,
            &target,
            |from, to| {
                rename_count += 1;
                match rename_count {
                    1 => fs::rename(from, to),
                    2 => Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "injected publish failure",
                    )),
                    _ => Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "injected restore failure",
                    )),
                }
            },
            |_left, _right| Ok(false),
            |path| fs::remove_dir_all(path),
        )
        .unwrap_err();

        std::env::remove_var("RELAY_TEST_TEMP_STAMP");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(err.to_string().contains("injected publish failure"));
        assert!(err.to_string().contains("injected restore failure"));
        assert!(err.to_string().contains(&backup.display().to_string()));
        assert!(!target.exists());
        assert_eq!(fs::read_to_string(backup.join("old.txt"))?, "old");
        assert_eq!(fs::read_to_string(prepared.join("new.txt"))?, "new");
        Ok(())
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
        collect_skill_entries(&root, &root, &mut entries, PackagePolicy::LegacyVisible)?;
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
        copy_skill_dir(&src, &dst, PackagePolicy::LegacyVisible)?;
        assert!(dst.join("file").exists());
        assert!(!dst.join(".hidden").exists());
        Ok(())
    }
}
