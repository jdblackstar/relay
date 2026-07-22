use crate::config::Config;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const DIR_SNAPSHOT_MAGIC_V1: &[u8] = b"RLYD1";
const DIR_SNAPSHOT_MAGIC_V2: &[u8] = b"RLYD2";

#[derive(Debug, Clone, PartialEq, Eq)]
enum DirSnapshotEntry {
    Directory {
        rel: String,
        mode: u32,
    },
    File {
        rel: String,
        mode: u32,
        contents: Vec<u8>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum EntityKind {
    Missing,
    File,
    Dir,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct EntityRef {
    kind: EntityKind,
    hash: Option<String>,
    blob: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HistoryWrite {
    path: String,
    before: EntityRef,
    after: EntityRef,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HistoryEvent {
    id: String,
    timestamp_ms: u64,
    origin: String,
    writes: Vec<HistoryWrite>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EntityState {
    kind: EntityKind,
    hash: Option<String>,
    blob: Option<String>,
}

impl EntityState {
    pub(crate) fn missing() -> Self {
        Self {
            kind: EntityKind::Missing,
            hash: None,
            blob: None,
        }
    }

    fn as_ref(&self) -> EntityRef {
        EntityRef {
            kind: self.kind,
            hash: self.hash.clone(),
            blob: self.blob.clone(),
        }
    }
}

#[cfg_attr(any(test, coverage), allow(dead_code))]
#[derive(Debug, Clone)]
pub(crate) struct HistorySummary {
    pub id: String,
    pub timestamp_ms: u64,
    pub origin: String,
    pub writes: usize,
}

#[cfg_attr(any(test, coverage), allow(dead_code))]
#[derive(Debug, Clone)]
pub(crate) struct RollbackReport {
    pub target_event_id: String,
    pub restored: usize,
    pub rollback_event_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct HistoryStore {
    root: PathBuf,
}

impl HistoryStore {
    pub(crate) fn from_config(cfg: &Config) -> io::Result<Self> {
        let roots = [
            cfg.central_dir.parent(),
            cfg.central_skills_dir.parent(),
            cfg.central_agents_dir.parent(),
            cfg.central_rules_dir.parent(),
        ];
        let Some(first) = roots[0] else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "central_dir has no parent",
            ));
        };
        let root = first.to_path_buf();
        Ok(Self {
            root: root.join("history"),
        })
    }

    fn blobs_dir(&self) -> PathBuf {
        self.root.join("blobs")
    }

    fn events_dir(&self) -> PathBuf {
        self.root.join("events")
    }

    fn ensure_dirs(&self) -> io::Result<()> {
        fs::create_dir_all(self.blobs_dir())?;
        fs::create_dir_all(self.events_dir())
    }

    fn event_path(&self, event_id: &str) -> PathBuf {
        self.events_dir().join(format!("{event_id}.toml"))
    }

    fn blob_path(&self, blob_id: &str) -> PathBuf {
        self.blobs_dir().join(blob_id)
    }

    fn read_events(&self) -> io::Result<Vec<HistoryEvent>> {
        let dir = self.events_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut paths = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext == "toml")
            {
                paths.push(path);
            }
        }
        paths.sort();
        let mut events = Vec::new();
        for path in paths {
            let raw = fs::read_to_string(&path)?;
            let event: HistoryEvent = toml::from_str(&raw).map_err(|err| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid history event in {}: {err}", path.display()),
                )
            })?;
            events.push(event);
        }
        Ok(events)
    }

    fn append_event(&self, event: &HistoryEvent) -> io::Result<()> {
        #[cfg(test)]
        if std::env::var_os("RELAY_TEST_FAIL_HISTORY_APPEND")
            .as_deref()
            .is_some_and(|path| path == self.root.as_os_str())
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "injected history append failure",
            ));
        }
        self.ensure_dirs()?;
        let path = self.event_path(&event.id);
        let serialized = toml::to_string_pretty(event)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        fs::write(path, serialized)
    }

    fn store_blob(&self, bytes: &[u8]) -> io::Result<String> {
        self.ensure_dirs()?;
        let base = hash_hex(bytes);
        let mut candidate = base.clone();
        let mut counter = 0usize;
        loop {
            let path = self.blob_path(&candidate);
            if !path.exists() {
                fs::write(path, bytes)?;
                return Ok(candidate);
            }
            if fs::read(&path)? == bytes {
                return Ok(candidate);
            }
            counter += 1;
            candidate = format!("{base}-{counter}");
        }
    }

    fn read_blob(&self, blob_id: &str) -> io::Result<Vec<u8>> {
        fs::read(self.blob_path(blob_id))
    }

    pub(crate) fn list_recent(&self, limit: usize) -> io::Result<Vec<HistorySummary>> {
        let mut out: Vec<HistorySummary> = self
            .read_events()?
            .into_iter()
            .rev()
            .take(limit)
            .map(|event| HistorySummary {
                id: event.id,
                timestamp_ms: event.timestamp_ms,
                origin: event.origin,
                writes: event.writes.len(),
            })
            .collect();
        out.reverse();
        Ok(out)
    }

    #[cfg_attr(any(test, coverage), allow(dead_code))]
    pub(crate) fn latest_event_id(&self) -> io::Result<Option<String>> {
        Ok(self.read_events()?.into_iter().last().map(|event| event.id))
    }

    pub(crate) fn rollback(&self, event_id: &str, force: bool) -> io::Result<RollbackReport> {
        let events = self.read_events()?;
        let Some(event) = events.into_iter().find(|event| event.id == event_id) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("history event not found: {event_id}"),
            ));
        };
        let mut recorder = HistoryRecorder::with_store(
            self.clone(),
            format!("rollback:{event_id}"),
            now_millis(),
            unique_event_id(),
        );

        let mut restored = 0usize;
        for write in event.writes.iter().rev() {
            let path = PathBuf::from(&write.path);
            let current = self.capture_path(&path)?;
            if !force && !self.state_matches(&path, &current, &write.after)? {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!(
                        "refusing to rollback changed path without --force: {}",
                        path.display()
                    ),
                ));
            }
            self.restore_path(&path, &write.before)?;
            let restored_state = self.capture_path(&path)?;
            recorder.record_change(&path, current, restored_state);
            restored += 1;
        }

        let rollback_event_id = recorder.finish()?;
        Ok(RollbackReport {
            target_event_id: event_id.to_string(),
            restored,
            rollback_event_id,
        })
    }

    fn restore_path(&self, path: &Path, state: &EntityRef) -> io::Result<()> {
        match state.kind {
            EntityKind::Missing => remove_path_if_exists(path),
            EntityKind::File => {
                let Some(blob_id) = state.blob.as_deref() else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "missing file blob reference",
                    ));
                };
                let contents = self.read_blob(blob_id)?;
                remove_path_if_exists(path)?;
                path.parent().map(fs::create_dir_all).transpose()?;
                fs::write(path, contents)
            }
            EntityKind::Dir => {
                let Some(blob_id) = state.blob.as_deref() else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "missing dir blob reference",
                    ));
                };
                let snapshot = self.read_blob(blob_id)?;
                let entries = decode_dir_snapshot(&snapshot)?;
                restore_directory_snapshot(path, &entries)
            }
        }
    }

    fn state_matches(
        &self,
        path: &Path,
        current: &EntityState,
        expected: &EntityRef,
    ) -> io::Result<bool> {
        if current.kind != expected.kind {
            return Ok(false);
        }
        if current.hash == expected.hash {
            return Ok(true);
        }
        if expected.kind != EntityKind::Dir {
            return Ok(false);
        }
        let Some(blob_id) = expected.blob.as_deref() else {
            return Ok(false);
        };
        let expected_snapshot = self.read_blob(blob_id)?;
        if !expected_snapshot.starts_with(DIR_SNAPSHOT_MAGIC_V1) {
            return Ok(false);
        }
        Ok(encode_dir_snapshot_v1(path)? == expected_snapshot)
    }

    pub(crate) fn capture_path(&self, path: &Path) -> io::Result<EntityState> {
        if path.to_str().is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("history path is not valid UTF-8: {}", path.display()),
            ));
        }
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(EntityState::missing()),
            Err(err) => return Err(err),
        };
        if metadata.is_file() {
            let bytes = fs::read(path)?;
            let blob = self.store_blob(&bytes)?;
            return Ok(EntityState {
                kind: EntityKind::File,
                hash: Some(blob.clone()),
                blob: Some(blob),
            });
        }
        if metadata.is_dir() {
            let snapshot = encode_dir_snapshot(path)?;
            let blob = self.store_blob(&snapshot)?;
            return Ok(EntityState {
                kind: EntityKind::Dir,
                hash: Some(blob.clone()),
                blob: Some(blob),
            });
        }
        if metadata.file_type().is_symlink() {
            let target_meta = fs::metadata(path);
            if target_meta.as_ref().is_ok_and(|meta| meta.is_file()) {
                let bytes = fs::read(path)?;
                let blob = self.store_blob(&bytes)?;
                return Ok(EntityState {
                    kind: EntityKind::File,
                    hash: Some(blob.clone()),
                    blob: Some(blob),
                });
            }
        }
        Ok(EntityState::missing())
    }

    pub(crate) fn snapshot_file_bytes(&self, contents: &[u8]) -> io::Result<EntityState> {
        let blob = self.store_blob(contents)?;
        Ok(EntityState {
            kind: EntityKind::File,
            hash: Some(blob.clone()),
            blob: Some(blob),
        })
    }
}

pub(crate) struct HistoryRecorder {
    store: HistoryStore,
    event: HistoryEvent,
}

impl HistoryRecorder {
    pub(crate) fn new(cfg: &Config, origin: &str) -> io::Result<Self> {
        let store = HistoryStore::from_config(cfg)?;
        Ok(Self::with_store(
            store,
            origin.to_string(),
            now_millis(),
            unique_event_id(),
        ))
    }

    fn with_store(store: HistoryStore, origin: String, timestamp_ms: u64, id: String) -> Self {
        Self {
            store,
            event: HistoryEvent {
                id,
                timestamp_ms,
                origin,
                writes: Vec::new(),
            },
        }
    }

    pub(crate) fn capture_path(&self, path: &Path) -> io::Result<EntityState> {
        self.store.capture_path(path)
    }

    pub(crate) fn snapshot_file_bytes(&self, contents: &[u8]) -> io::Result<EntityState> {
        self.store.snapshot_file_bytes(contents)
    }

    pub(crate) fn record_change(&mut self, path: &Path, before: EntityState, after: EntityState) {
        if before == after {
            return;
        }
        self.event.writes.push(HistoryWrite {
            path: path.to_string_lossy().to_string(),
            before: before.as_ref(),
            after: after.as_ref(),
        });
    }

    pub(crate) fn finish(self) -> io::Result<Option<String>> {
        if self.event.writes.is_empty() {
            return Ok(None);
        }
        if let Err(append_err) = self.store.append_event(&self.event) {
            let kind = append_err.kind();
            return match self.rollback_pending() {
                Ok(_) => Err(io::Error::new(
                    kind,
                    format!(
                        "failed to append history event; reverted recorded writes: {append_err}"
                    ),
                )),
                Err(rollback_err) => Err(io::Error::new(
                    kind,
                    format!(
                        "failed to append history event ({append_err}) and failed to revert recorded writes ({rollback_err})"
                    ),
                )),
            };
        }
        Ok(Some(self.event.id))
    }

    pub(crate) fn rollback_pending(self) -> io::Result<usize> {
        let mut restored = 0;
        let total = self.event.writes.len();
        let mut failures = Vec::new();
        let mut failure_kind = None;
        for write in self.event.writes.iter().rev() {
            match self
                .store
                .restore_path(Path::new(&write.path), &write.before)
            {
                Ok(()) => restored += 1,
                Err(err) => {
                    failure_kind.get_or_insert(err.kind());
                    let recovery = write
                        .before
                        .blob
                        .as_deref()
                        .map(|blob| self.store.blob_path(blob).display().to_string())
                        .unwrap_or_else(|| "no snapshot blob".to_string());
                    failures.push(format!(
                        "{} from {}: {err}",
                        Path::new(&write.path).display(),
                        recovery
                    ));
                }
            }
        }
        if !failures.is_empty() {
            return Err(io::Error::new(
                failure_kind.unwrap_or(io::ErrorKind::Other),
                format!(
                    "restored {restored} of {total} pending writes; recovery snapshots were preserved; failures: {}",
                    failures.join("; ")
                ),
            ));
        }
        Ok(restored)
    }

    #[cfg(test)]
    pub(crate) fn corrupt_latest_before_dir_snapshot(&self) -> io::Result<PathBuf> {
        let write = self.event.writes.last().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "no recorded write to corrupt")
        })?;
        if write.before.kind != EntityKind::Dir {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "latest recorded before-state is not a directory",
            ));
        }
        let blob = write.before.blob.as_deref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "latest recorded before-state has no blob",
            )
        })?;
        let path = self.store.blob_path(blob);
        fs::write(&path, DIR_SNAPSHOT_MAGIC_V2)?;
        Ok(path)
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn unique_event_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{nanos}-{}", std::process::id())
}

fn hash_hex(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn remove_path_if_exists(path: &Path) -> io::Result<()> {
    crate::path_cleanup::remove_with_owner_access(path)
}

fn restore_directory_snapshot(path: &Path, entries: &[DirSnapshotEntry]) -> io::Result<()> {
    path.parent().map(fs::create_dir_all).transpose()?;
    let staged = unused_sibling_path(path, "restore")?;
    fs::create_dir(&staged)?;
    if let Err(err) = materialize_directory_snapshot(&staged, entries) {
        let _ = remove_path_if_exists(&staged);
        return Err(err);
    }

    match fs::symlink_metadata(path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            if let Err(err) = fs::rename(&staged, path) {
                let _ = remove_path_if_exists(&staged);
                return Err(err);
            }
            return Ok(());
        }
        Err(err) => {
            let _ = remove_path_if_exists(&staged);
            return Err(err);
        }
        Ok(_) => {}
    }

    let backup = unused_sibling_path(path, "backup")?;
    publish_staged_directory_with(
        &staged,
        path,
        &backup,
        |from, to| fs::rename(from, to),
        remove_path_if_exists,
    )
}

fn publish_staged_directory_with<R, D>(
    staged: &Path,
    target: &Path,
    backup: &Path,
    mut rename: R,
    mut remove: D,
) -> io::Result<()>
where
    R: FnMut(&Path, &Path) -> io::Result<()>,
    D: FnMut(&Path) -> io::Result<()>,
{
    if let Err(err) = rename(target, backup) {
        let _ = remove(staged);
        return Err(err);
    }
    if let Err(publish_err) = rename(staged, target) {
        return match rename(backup, target) {
            Ok(()) => {
                let _ = remove(staged);
                Err(io::Error::new(
                    publish_err.kind(),
                    format!(
                        "failed to publish restored directory for {}; previous path was restored: {publish_err}",
                        target.display()
                    ),
                ))
            }
            Err(restore_err) => Err(io::Error::new(
                publish_err.kind(),
                format!(
                    "failed to publish restored directory for {} ({publish_err}) and failed to restore previous path from {} ({restore_err})",
                    target.display(),
                    backup.display()
                ),
            )),
        };
    }
    let _ = remove(backup);
    Ok(())
}

fn materialize_directory_snapshot(root: &Path, entries: &[DirSnapshotEntry]) -> io::Result<()> {
    for entry in entries {
        if let DirSnapshotEntry::Directory { rel, .. } = entry {
            fs::create_dir_all(root.join(rel))?;
        }
    }
    for entry in entries {
        if let DirSnapshotEntry::File {
            rel,
            mode,
            contents,
        } = entry
        {
            let file_path = root.join(rel);
            file_path.parent().map(fs::create_dir_all).transpose()?;
            fs::write(&file_path, contents)?;
            set_snapshot_permissions(&file_path, *mode)?;
        }
    }
    for entry in entries.iter().rev() {
        if let DirSnapshotEntry::Directory { rel, mode } = entry {
            set_snapshot_permissions(&root.join(rel), *mode)?;
        }
    }
    Ok(())
}

fn unused_sibling_path(path: &Path, role: &str) -> io::Result<PathBuf> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("relay");
    for attempt in 0..100u32 {
        let candidate = parent.join(format!(
            ".{name}.relay-{role}-{}-{attempt}",
            unique_event_id()
        ));
        match fs::symlink_metadata(&candidate) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(candidate),
            Ok(_) => continue,
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("could not reserve a restore path beside {}", path.display()),
    ))
}

fn encode_dir_snapshot(root: &Path) -> io::Result<Vec<u8>> {
    let mut entries = Vec::new();
    collect_dir_entries(root, root, &mut entries)?;
    entries.sort_by(|a, b| snapshot_entry_rel(a).cmp(snapshot_entry_rel(b)));

    let mut out = Vec::new();
    out.extend_from_slice(DIR_SNAPSHOT_MAGIC_V2);
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for entry in entries {
        let (kind, rel, mode, contents) = match entry {
            DirSnapshotEntry::Directory { rel, mode } => (1u8, rel, mode, None),
            DirSnapshotEntry::File {
                rel,
                mode,
                contents,
            } => (2u8, rel, mode, Some(contents)),
        };
        out.push(kind);
        let rel_bytes = rel.as_bytes();
        out.extend_from_slice(&(rel_bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(rel_bytes);
        out.extend_from_slice(&mode.to_le_bytes());
        if let Some(contents) = contents {
            out.extend_from_slice(&(contents.len() as u64).to_le_bytes());
            out.extend_from_slice(&contents);
        }
    }
    Ok(out)
}

fn encode_dir_snapshot_v1(root: &Path) -> io::Result<Vec<u8>> {
    let mut entries = Vec::new();
    collect_dir_entries_v1(root, root, &mut entries)?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut out = Vec::new();
    out.extend_from_slice(DIR_SNAPSHOT_MAGIC_V1);
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for (rel, contents) in entries {
        let rel_bytes = rel.as_bytes();
        out.extend_from_slice(&(rel_bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(rel_bytes);
        out.extend_from_slice(&(contents.len() as u64).to_le_bytes());
        out.extend_from_slice(&contents);
    }
    Ok(out)
}

fn decode_dir_snapshot(bytes: &[u8]) -> io::Result<Vec<DirSnapshotEntry>> {
    let mut idx = 0usize;
    if bytes.starts_with(DIR_SNAPSHOT_MAGIC_V1) {
        idx += DIR_SNAPSHOT_MAGIC_V1.len();
        let entries = decode_dir_snapshot_v1(bytes, &mut idx)?;
        validate_snapshot_entries(&entries, false)?;
        if idx != bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "directory snapshot has trailing data",
            ));
        }
        return Ok(entries);
    }
    if !bytes.starts_with(DIR_SNAPSHOT_MAGIC_V2) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid directory snapshot header",
        ));
    }
    idx += DIR_SNAPSHOT_MAGIC_V2.len();
    let count = read_u32(bytes, &mut idx)? as usize;
    let mut out = Vec::new();
    for _ in 0..count {
        let kind = read_u8(bytes, &mut idx)?;
        let rel_len = read_u32(bytes, &mut idx)? as usize;
        let rel_end = idx.checked_add(rel_len).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "directory snapshot path overflow",
            )
        })?;
        if rel_end > bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "directory snapshot path overflow",
            ));
        }
        let rel = String::from_utf8(bytes[idx..rel_end].to_vec()).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid utf8 path: {err}"),
            )
        })?;
        idx = rel_end;
        let mode = read_u32(bytes, &mut idx)?;
        match kind {
            1 => out.push(DirSnapshotEntry::Directory { rel, mode }),
            2 => {
                let data_len = usize::try_from(read_u64(bytes, &mut idx)?).map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "directory snapshot data overflow",
                    )
                })?;
                let data_end = idx.checked_add(data_len).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "directory snapshot data overflow",
                    )
                })?;
                if data_end > bytes.len() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "directory snapshot data overflow",
                    ));
                }
                out.push(DirSnapshotEntry::File {
                    rel,
                    mode,
                    contents: bytes[idx..data_end].to_vec(),
                });
                idx = data_end;
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid directory snapshot entry kind",
                ));
            }
        }
    }
    validate_snapshot_entries(&out, true)?;
    if idx != bytes.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "directory snapshot has trailing data",
        ));
    }
    Ok(out)
}

fn validate_snapshot_entries(entries: &[DirSnapshotEntry], require_root: bool) -> io::Result<()> {
    let mut kinds = HashMap::new();
    let mut root_directories = 0usize;
    for entry in entries {
        let rel = snapshot_entry_rel(entry);
        let is_directory = matches!(entry, DirSnapshotEntry::Directory { .. });
        if rel.is_empty() {
            if !is_directory || !require_root {
                return Err(invalid_snapshot_path(rel));
            }
            root_directories += 1;
        }
        let path = Path::new(rel);
        let mut normalized = PathBuf::new();
        for component in path.components() {
            match component {
                Component::Normal(component) => normalized.push(component),
                _ => return Err(invalid_snapshot_path(rel)),
            }
        }
        if !rel.is_empty() && normalized.as_os_str() != path.as_os_str() {
            return Err(invalid_snapshot_path(rel));
        }
        if kinds.insert(normalized, is_directory).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("duplicate directory snapshot path: {rel:?}"),
            ));
        }
    }
    if require_root && root_directories != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "directory snapshot must contain exactly one root directory entry",
        ));
    }
    for path in kinds.keys() {
        if path.as_os_str().is_empty() {
            continue;
        }
        let mut ancestor = path.parent();
        while let Some(parent) = ancestor {
            if parent.as_os_str().is_empty() {
                break;
            }
            if kinds.get(parent).is_some_and(|kind| !kind) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "directory snapshot path is nested below a file: {:?}",
                        path.to_string_lossy()
                    ),
                ));
            }
            ancestor = parent.parent();
        }
    }
    Ok(())
}

fn invalid_snapshot_path(rel: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("unsafe or non-normal directory snapshot path: {rel:?}"),
    )
}

fn decode_dir_snapshot_v1(bytes: &[u8], idx: &mut usize) -> io::Result<Vec<DirSnapshotEntry>> {
    let count = read_u32(bytes, idx)? as usize;
    let mut out = Vec::new();
    for _ in 0..count {
        let rel_len = read_u32(bytes, idx)? as usize;
        let rel_end = idx.checked_add(rel_len).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "directory snapshot path overflow",
            )
        })?;
        if rel_end > bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "directory snapshot path overflow",
            ));
        }
        let rel = String::from_utf8(bytes[*idx..rel_end].to_vec()).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid utf8 path: {err}"),
            )
        })?;
        *idx = rel_end;
        let data_len = usize::try_from(read_u64(bytes, idx)?).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "directory snapshot data overflow",
            )
        })?;
        let data_end = idx.checked_add(data_len).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "directory snapshot data overflow",
            )
        })?;
        if data_end > bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "directory snapshot data overflow",
            ));
        }
        out.push(DirSnapshotEntry::File {
            rel,
            mode: 0,
            contents: bytes[*idx..data_end].to_vec(),
        });
        *idx = data_end;
    }
    Ok(out)
}

fn read_u8(bytes: &[u8], idx: &mut usize) -> io::Result<u8> {
    if *idx >= bytes.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "snapshot ended unexpectedly",
        ));
    }
    let value = bytes[*idx];
    *idx += 1;
    Ok(value)
}

fn read_u32(bytes: &[u8], idx: &mut usize) -> io::Result<u32> {
    if *idx + 4 > bytes.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "snapshot ended unexpectedly",
        ));
    }
    let mut buf = [0u8; 4];
    buf.copy_from_slice(&bytes[*idx..*idx + 4]);
    *idx += 4;
    Ok(u32::from_le_bytes(buf))
}

fn read_u64(bytes: &[u8], idx: &mut usize) -> io::Result<u64> {
    if *idx + 8 > bytes.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "snapshot ended unexpectedly",
        ));
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&bytes[*idx..*idx + 8]);
    *idx += 8;
    Ok(u64::from_le_bytes(buf))
}

fn collect_dir_entries(root: &Path, dir: &Path, out: &mut Vec<DirSnapshotEntry>) -> io::Result<()> {
    let rel = snapshot_relative_utf8(root, dir)?;
    out.push(DirSnapshotEntry::Directory {
        rel,
        mode: snapshot_mode(&fs::metadata(dir)?),
    });
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)? {
        entries.push(entry?);
    }
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_dir_entries(root, &path, out)?;
            continue;
        }
        if file_type.is_symlink()
            && fs::metadata(&path)
                .map(|meta| meta.is_dir())
                .unwrap_or(false)
        {
            continue;
        }
        if file_type.is_file() || file_type.is_symlink() {
            let rel = snapshot_relative_utf8(root, &path)?;
            out.push(DirSnapshotEntry::File {
                rel,
                mode: snapshot_mode(&fs::metadata(&path)?),
                contents: fs::read(&path)?,
            });
        }
    }
    Ok(())
}

fn collect_dir_entries_v1(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, Vec<u8>)>,
) -> io::Result<()> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)? {
        entries.push(entry?);
    }
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_dir_entries_v1(root, &path, out)?;
            continue;
        }
        if file_type.is_symlink()
            && fs::metadata(&path)
                .map(|meta| meta.is_dir())
                .unwrap_or(false)
        {
            continue;
        }
        if file_type.is_file() || file_type.is_symlink() {
            let rel = snapshot_relative_utf8(root, &path)?;
            out.push((rel, fs::read(&path)?));
        }
    }
    Ok(())
}

fn snapshot_entry_rel(entry: &DirSnapshotEntry) -> &str {
    match entry {
        DirSnapshotEntry::Directory { rel, .. } | DirSnapshotEntry::File { rel, .. } => rel,
    }
}

#[cfg(unix)]
fn snapshot_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode()
}

#[cfg(not(unix))]
fn snapshot_mode(_metadata: &fs::Metadata) -> u32 {
    0
}

#[cfg(unix)]
fn set_snapshot_permissions(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if mode != 0 {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_snapshot_permissions(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

fn snapshot_relative_utf8(root: &Path, path: &Path) -> io::Result<String> {
    let relative = path.strip_prefix(root).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("history path is outside snapshot root: {}", path.display()),
        )
    })?;
    relative.to_str().map(str::to_owned).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("history path is not valid UTF-8: {}", path.display()),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_config(tmp: &TempDir) -> Config {
        Config {
            enabled_tools: vec!["codex".to_string()],
            verified_versions: std::collections::HashMap::new(),
            blacklist: std::collections::HashMap::new(),
            central_dir: tmp.path().join("relay/commands"),
            central_skills_dir: tmp.path().join("relay/skills"),
            central_agents_dir: tmp.path().join("relay/agents"),
            central_rules_dir: tmp.path().join("relay/rules"),
            claude_dir: tmp.path().join("claude/commands"),
            claude_skills_dir: tmp.path().join("claude/skills"),
            cursor_dir: tmp.path().join("cursor/commands"),
            opencode_commands_dir: tmp.path().join("opencode/commands"),
            opencode_legacy_commands_dir: None,
            opencode_skills_dir: tmp.path().join("opencode/skills"),
            opencode_agents_file: tmp.path().join("opencode/AGENTS.md"),
            codex_skills_dir: tmp.path().join("codex/skills"),
            codex_rules_file: tmp.path().join("codex/rules/default.rules"),
            codex_agents_file: tmp.path().join("codex/AGENTS.md"),
        }
    }

    #[test]
    fn recorder_and_rollback_for_file() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let cfg = make_config(&tmp);
        let path = tmp.path().join("relay/commands/test.md");
        path.parent().map(fs::create_dir_all).transpose()?;
        fs::write(&path, "before")?;

        let mut recorder = HistoryRecorder::new(&cfg, "sync")?;
        let before = recorder.capture_path(&path)?;
        fs::write(&path, "after")?;
        let after = recorder.capture_path(&path)?;
        recorder.record_change(&path, before, after);
        let event_id = recorder.finish()?.expect("event id");

        let store = HistoryStore::from_config(&cfg)?;
        fs::write(&path, "after")?;
        let report = store.rollback(&event_id, false)?;
        assert_eq!(report.target_event_id, event_id);
        assert_eq!(fs::read_to_string(&path)?, "before");
        Ok(())
    }

    #[test]
    fn list_recent_returns_entries() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let cfg = make_config(&tmp);
        let path = tmp.path().join("relay/commands/test.md");
        path.parent().map(fs::create_dir_all).transpose()?;
        fs::write(&path, "before")?;

        let mut recorder = HistoryRecorder::new(&cfg, "sync")?;
        let before = recorder.capture_path(&path)?;
        fs::write(&path, "after")?;
        let after = recorder.capture_path(&path)?;
        recorder.record_change(&path, before, after);
        let _ = recorder.finish()?;

        let store = HistoryStore::from_config(&cfg)?;
        let recent = store.list_recent(10)?;
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].writes, 1);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn directory_history_restores_hidden_files_empty_directories_and_modes() -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new()?;
        let cfg = make_config(&tmp);
        let path = tmp.path().join("relay/skills/package");
        fs::create_dir_all(path.join("empty/nested"))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o750))?;
        fs::set_permissions(path.join("empty"), fs::Permissions::from_mode(0o700))?;
        fs::set_permissions(path.join("empty/nested"), fs::Permissions::from_mode(0o750))?;
        fs::write(path.join(".hidden"), "before hidden")?;
        fs::create_dir_all(path.join("scripts"))?;
        let script = path.join("scripts/run.sh");
        fs::write(&script, "#!/bin/sh\necho before\n")?;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755))?;

        let mut recorder = HistoryRecorder::new(&cfg, "sync")?;
        let before = recorder.capture_path(&path)?;
        fs::remove_dir_all(path.join("empty"))?;
        fs::remove_file(path.join(".hidden"))?;
        fs::write(&script, "#!/bin/sh\necho after\n")?;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o644))?;
        let after = recorder.capture_path(&path)?;
        recorder.record_change(&path, before, after);
        let event_id = recorder.finish()?.expect("event id");

        fs::create_dir_all(path.join("unexpected-empty"))?;
        let topology_err = HistoryStore::from_config(&cfg)?
            .rollback(&event_id, false)
            .unwrap_err();
        assert_eq!(topology_err.kind(), io::ErrorKind::AlreadyExists);
        fs::remove_dir_all(path.join("unexpected-empty"))?;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755))?;
        let mode_err = HistoryStore::from_config(&cfg)?
            .rollback(&event_id, false)
            .unwrap_err();
        assert_eq!(mode_err.kind(), io::ErrorKind::AlreadyExists);
        fs::set_permissions(&script, fs::Permissions::from_mode(0o644))?;
        HistoryStore::from_config(&cfg)?.rollback(&event_id, false)?;

        assert_eq!(fs::read_to_string(path.join(".hidden"))?, "before hidden");
        assert!(path.join("empty/nested").is_dir());
        assert_eq!(fs::read_to_string(&script)?, "#!/bin/sh\necho before\n");
        assert_eq!(fs::metadata(&script)?.permissions().mode() & 0o777, 0o755);
        assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o750);
        assert_eq!(
            fs::metadata(path.join("empty"))?.permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(path.join("empty/nested"))?
                .permissions()
                .mode()
                & 0o777,
            0o750
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn directory_history_restores_read_only_root_after_materialization() -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new()?;
        let cfg = make_config(&tmp);
        let path = tmp.path().join("relay/skills/read-only");
        fs::create_dir_all(path.join("references"))?;
        fs::write(path.join("SKILL.md"), "before\n")?;
        fs::write(path.join("references/guide.md"), "guide\n")?;
        fs::set_permissions(path.join("SKILL.md"), fs::Permissions::from_mode(0o400))?;
        fs::set_permissions(
            path.join("references/guide.md"),
            fs::Permissions::from_mode(0o400),
        )?;
        fs::set_permissions(path.join("references"), fs::Permissions::from_mode(0o500))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o500))?;

        let recorder = HistoryRecorder::new(&cfg, "sync")?;
        let snapshot = recorder.capture_path(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        fs::set_permissions(path.join("references"), fs::Permissions::from_mode(0o700))?;
        fs::set_permissions(path.join("SKILL.md"), fs::Permissions::from_mode(0o600))?;
        fs::write(path.join("SKILL.md"), "after\n")?;
        fs::set_permissions(path.join("SKILL.md"), fs::Permissions::from_mode(0o400))?;
        fs::set_permissions(path.join("references"), fs::Permissions::from_mode(0o500))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o500))?;

        HistoryStore::from_config(&cfg)?.restore_path(&path, &snapshot.as_ref())?;

        assert_eq!(fs::read_to_string(path.join("SKILL.md"))?, "before\n");
        assert_eq!(
            fs::read_to_string(path.join("references/guide.md"))?,
            "guide\n"
        );
        assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o500);
        assert_eq!(
            fs::metadata(path.join("references"))?.permissions().mode() & 0o777,
            0o500
        );
        assert_eq!(
            fs::metadata(path.join("SKILL.md"))?.permissions().mode() & 0o777,
            0o400
        );
        assert_eq!(
            fs::metadata(path.join("references/guide.md"))?
                .permissions()
                .mode()
                & 0o777,
            0o400
        );
        let siblings = fs::read_dir(path.parent().unwrap())?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<io::Result<Vec<_>>>()?;
        assert!(siblings.iter().all(|name| {
            let name = name.to_string_lossy();
            !name.starts_with(".read-only.relay-restore-")
                && !name.starts_with(".read-only.relay-backup-")
        }));
        crate::path_cleanup::remove_with_owner_access(&path)?;
        Ok(())
    }

    #[test]
    fn legacy_directory_snapshot_still_decodes() -> io::Result<()> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(DIR_SNAPSHOT_MAGIC_V1);
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&8u32.to_le_bytes());
        bytes.extend_from_slice(b"file.txt");
        bytes.extend_from_slice(&7u64.to_le_bytes());
        bytes.extend_from_slice(b"legacy\n");

        assert_eq!(
            decode_dir_snapshot(&bytes)?,
            vec![DirSnapshotEntry::File {
                rel: "file.txt".to_string(),
                mode: 0,
                contents: b"legacy\n".to_vec(),
            }]
        );
        Ok(())
    }

    #[test]
    fn corrupt_or_missing_directory_snapshot_does_not_modify_live_path() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let cfg = make_config(&tmp);
        let store = HistoryStore::from_config(&cfg)?;
        let path = tmp.path().join("relay/skills/live-package");
        fs::create_dir_all(&path)?;
        fs::write(path.join("SKILL.md"), "live\n")?;
        fs::write(path.join("keep.txt"), "keep\n")?;
        let live_snapshot = encode_dir_snapshot(&path)?;

        let corrupt_blob = store.store_blob(DIR_SNAPSHOT_MAGIC_V2)?;
        let corrupt = EntityRef {
            kind: EntityKind::Dir,
            hash: Some(corrupt_blob.clone()),
            blob: Some(corrupt_blob),
        };
        let corrupt_err = store.restore_path(&path, &corrupt).unwrap_err();
        assert_eq!(corrupt_err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(encode_dir_snapshot(&path)?, live_snapshot);

        let missing = EntityRef {
            kind: EntityKind::Dir,
            hash: Some("missing-blob".to_string()),
            blob: Some("missing-blob".to_string()),
        };
        let missing_err = store.restore_path(&path, &missing).unwrap_err();
        assert_eq!(missing_err.kind(), io::ErrorKind::NotFound);
        assert_eq!(encode_dir_snapshot(&path)?, live_snapshot);
        Ok(())
    }

    #[test]
    fn malformed_v2_snapshots_are_rejected_before_live_mutation() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let cfg = make_config(&tmp);
        let store = HistoryStore::from_config(&cfg)?;
        let path = tmp.path().join("relay/skills/live-package");
        fs::create_dir_all(&path)?;
        fs::write(path.join("SKILL.md"), "live\n")?;
        fs::write(path.join("keep.txt"), "keep\n")?;
        let live_snapshot = encode_dir_snapshot(&path)?;
        let root = DirSnapshotEntry::Directory {
            rel: String::new(),
            mode: 0o755,
        };
        let file = |rel: &str| DirSnapshotEntry::File {
            rel: rel.to_string(),
            mode: 0o644,
            contents: b"snapshot\n".to_vec(),
        };

        let mut unknown_kind = encode_test_snapshot_v2(std::slice::from_ref(&root));
        unknown_kind.extend_from_slice(&[9, 0, 0, 0, 0, 0, 0, 0, 0]);
        unknown_kind[5..9].copy_from_slice(&2u32.to_le_bytes());
        let mut trailing = encode_test_snapshot_v2(std::slice::from_ref(&root));
        trailing.push(0xff);
        let mut truncated_path_length = Vec::from(DIR_SNAPSHOT_MAGIC_V2);
        truncated_path_length.extend_from_slice(&1u32.to_le_bytes());
        truncated_path_length.extend_from_slice(&[1, 0, 0]);
        let mut overflowing_path = Vec::from(DIR_SNAPSHOT_MAGIC_V2);
        overflowing_path.extend_from_slice(&1u32.to_le_bytes());
        overflowing_path.push(1);
        overflowing_path.extend_from_slice(&u32::MAX.to_le_bytes());
        let mut overflowing_data = Vec::from(DIR_SNAPSHOT_MAGIC_V2);
        overflowing_data.extend_from_slice(&1u32.to_le_bytes());
        overflowing_data.push(2);
        overflowing_data.extend_from_slice(&1u32.to_le_bytes());
        overflowing_data.push(b'x');
        overflowing_data.extend_from_slice(&0o644u32.to_le_bytes());
        overflowing_data.extend_from_slice(&u64::MAX.to_le_bytes());
        let mut overflowing_count = Vec::from(DIR_SNAPSHOT_MAGIC_V2);
        overflowing_count.extend_from_slice(&u32::MAX.to_le_bytes());

        let cases = vec![
            encode_test_snapshot_v2(&[root.clone(), root.clone()]),
            encode_test_snapshot_v2(&[root.clone(), file("same"), file("same")]),
            encode_test_snapshot_v2(&[file("missing-root")]),
            encode_test_snapshot_v2(&[root.clone(), file("parent"), file("parent/child")]),
            unknown_kind,
            trailing,
            truncated_path_length,
            overflowing_path,
            overflowing_data,
            overflowing_count,
        ];

        for snapshot in cases {
            let blob = store.store_blob(&snapshot)?;
            let state = EntityRef {
                kind: EntityKind::Dir,
                hash: Some(blob.clone()),
                blob: Some(blob),
            };
            let err = store.restore_path(&path, &state).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
            assert_eq!(encode_dir_snapshot(&path)?, live_snapshot);
        }
        Ok(())
    }

    #[test]
    fn unsafe_v1_and_v2_snapshot_paths_are_rejected_before_live_mutation() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let cfg = make_config(&tmp);
        let store = HistoryStore::from_config(&cfg)?;
        let path = tmp.path().join("relay/skills/live-package");
        fs::create_dir_all(&path)?;
        fs::write(path.join("SKILL.md"), "live\n")?;
        let outside = path.parent().unwrap().join("outside.txt");
        fs::write(&outside, "outside\n")?;
        let absolute = tmp.path().join("absolute-escape.txt");
        fs::write(&absolute, "absolute\n")?;
        let live_snapshot = encode_dir_snapshot(&path)?;

        let mut cases = Vec::new();
        for rel in [
            "../outside.txt".to_string(),
            absolute.to_string_lossy().to_string(),
            "nested/../escape.txt".to_string(),
            "./escape.txt".to_string(),
            "nested//escape.txt".to_string(),
            "".to_string(),
        ] {
            cases.push(encode_test_snapshot_v1(&[(rel.as_str(), b"escaped")]))
        }
        for rel in [
            "../outside.txt".to_string(),
            absolute.to_string_lossy().to_string(),
            "nested/../escape.txt".to_string(),
            "./escape.txt".to_string(),
            "nested//escape.txt".to_string(),
        ] {
            cases.push(encode_test_snapshot_v2(&[
                DirSnapshotEntry::Directory {
                    rel: String::new(),
                    mode: 0o755,
                },
                DirSnapshotEntry::File {
                    rel,
                    mode: 0o644,
                    contents: b"escaped".to_vec(),
                },
            ]));
        }
        cases.push(encode_test_snapshot_v2(&[
            DirSnapshotEntry::Directory {
                rel: String::new(),
                mode: 0o755,
            },
            DirSnapshotEntry::File {
                rel: String::new(),
                mode: 0o644,
                contents: b"escaped".to_vec(),
            },
        ]));

        #[cfg(windows)]
        for rel in [r"C:\escape.txt", r"\\server\share\escape.txt"] {
            cases.push(encode_test_snapshot_v1(&[(rel, b"escaped")]));
        }

        for snapshot in cases {
            let blob = store.store_blob(&snapshot)?;
            let state = EntityRef {
                kind: EntityKind::Dir,
                hash: Some(blob.clone()),
                blob: Some(blob),
            };
            let err = store.restore_path(&path, &state).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
            assert_eq!(encode_dir_snapshot(&path)?, live_snapshot);
            assert_eq!(fs::read_to_string(&outside)?, "outside\n");
            assert_eq!(fs::read_to_string(&absolute)?, "absolute\n");
        }
        Ok(())
    }

    #[test]
    fn directory_materialization_failure_preserves_live_tree() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let cfg = make_config(&tmp);
        let store = HistoryStore::from_config(&cfg)?;
        let path = tmp.path().join("relay/skills/live-package");
        fs::create_dir_all(&path)?;
        fs::write(path.join("SKILL.md"), "live\n")?;
        let live_snapshot = encode_dir_snapshot(&path)?;
        let snapshot = encode_test_snapshot_v2(&[
            DirSnapshotEntry::Directory {
                rel: String::new(),
                mode: 0o755,
            },
            DirSnapshotEntry::File {
                rel: "x".repeat(300),
                mode: 0o644,
                contents: b"too long".to_vec(),
            },
        ]);
        let blob = store.store_blob(&snapshot)?;
        let state = EntityRef {
            kind: EntityKind::Dir,
            hash: Some(blob.clone()),
            blob: Some(blob),
        };

        let err = store.restore_path(&path, &state).unwrap_err();

        assert_ne!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(encode_dir_snapshot(&path)?, live_snapshot);
        assert_eq!(fs::read_to_string(path.join("SKILL.md"))?, "live\n");
        Ok(())
    }

    #[test]
    fn staged_directory_publish_target_to_backup_failure_cleans_staged() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let target = tmp.path().join("target");
        let staged = tmp.path().join("staged");
        let backup = tmp.path().join("backup");
        fs::create_dir_all(&target)?;
        fs::create_dir_all(&staged)?;
        fs::write(target.join("value"), "live")?;
        fs::write(staged.join("value"), "restored")?;

        let err = publish_staged_directory_with(
            &staged,
            &target,
            &backup,
            |_from, _to| {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected target backup failure",
                ))
            },
            remove_path_if_exists,
        )
        .unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(err.to_string().contains("injected target backup failure"));
        assert_eq!(fs::read_to_string(target.join("value"))?, "live");
        assert!(!staged.exists());
        assert!(!backup.exists());
        Ok(())
    }

    #[test]
    fn staged_directory_publish_failure_restores_backup_and_cleans_staged() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let target = tmp.path().join("target");
        let staged = tmp.path().join("staged");
        let backup = tmp.path().join("backup");
        fs::create_dir_all(&target)?;
        fs::create_dir_all(&staged)?;
        fs::write(target.join("value"), "live")?;
        fs::write(staged.join("value"), "restored")?;

        let err = publish_staged_directory_with(
            &staged,
            &target,
            &backup,
            |from, to| {
                if from == staged && to == target {
                    Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "injected staged publish failure",
                    ))
                } else {
                    fs::rename(from, to)
                }
            },
            remove_path_if_exists,
        )
        .unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(err.to_string().contains("previous path was restored"));
        assert!(err.to_string().contains("injected staged publish failure"));
        assert_eq!(fs::read_to_string(target.join("value"))?, "live");
        assert!(!staged.exists());
        assert!(!backup.exists());
        Ok(())
    }

    #[test]
    fn staged_directory_double_rename_failure_preserves_both_recovery_paths() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let target = tmp.path().join("target");
        let staged = tmp.path().join("staged");
        let backup = tmp.path().join("backup");
        fs::create_dir_all(&target)?;
        fs::create_dir_all(&staged)?;
        fs::write(target.join("value"), "live")?;
        fs::write(staged.join("value"), "restored")?;

        let err = publish_staged_directory_with(
            &staged,
            &target,
            &backup,
            |from, to| {
                if from == target && to == backup {
                    return fs::rename(from, to);
                }
                let message = if from == staged {
                    "injected staged publish failure"
                } else {
                    "injected backup restore failure"
                };
                Err(io::Error::new(io::ErrorKind::PermissionDenied, message))
            },
            remove_path_if_exists,
        )
        .unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        let message = err.to_string();
        assert!(message.contains("injected staged publish failure"));
        assert!(message.contains("injected backup restore failure"));
        assert!(message.contains(&backup.display().to_string()));
        assert!(!target.exists());
        assert_eq!(fs::read_to_string(staged.join("value"))?, "restored");
        assert_eq!(fs::read_to_string(backup.join("value"))?, "live");
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn history_capture_rejects_distinct_non_utf8_paths_without_writes() -> io::Result<()> {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let tmp = TempDir::new()?;
        let cfg = make_config(&tmp);
        let path = tmp.path().join("relay/skills/non-utf8");
        fs::create_dir_all(&path)?;
        fs::write(
            path.join(OsString::from_vec(b"invalid-\x80".to_vec())),
            "one",
        )?;
        fs::write(
            path.join(OsString::from_vec(b"invalid-\x81".to_vec())),
            "two",
        )?;
        let outside = tmp.path().join("outside.txt");
        fs::write(&outside, "outside")?;
        let store = HistoryStore::from_config(&cfg)?;

        let err = store.capture_path(&path).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("history path is not valid UTF-8"));
        assert_eq!(fs::read_to_string(&outside)?, "outside");
        assert!(!store.root.exists());
        assert_eq!(fs::read_dir(&path)?.count(), 2);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn history_relative_path_validation_distinguishes_non_utf8_names() -> io::Result<()> {
        use std::ffi::OsString;
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let root = PathBuf::from("snapshot");
        let first = root.join(OsString::from_vec(b"invalid-\x80".to_vec()));
        let second = root.join(OsString::from_vec(b"invalid-\x81".to_vec()));

        for path in [&first, &second] {
            let err = snapshot_relative_utf8(&root, path).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
            assert!(err.to_string().contains("not valid UTF-8"));
        }
        assert_ne!(first.as_os_str().as_bytes(), second.as_os_str().as_bytes());
        let tmp = TempDir::new()?;
        let cfg = make_config(&tmp);
        let store = HistoryStore::from_config(&cfg)?;
        let invalid_root = tmp
            .path()
            .join(OsString::from_vec(b"invalid-root-\x80".to_vec()));
        let err = store.capture_path(&invalid_root).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(!store.root.exists());
        Ok(())
    }

    fn encode_test_snapshot_v1(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(DIR_SNAPSHOT_MAGIC_V1);
        out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        for (rel, contents) in entries {
            out.extend_from_slice(&(rel.len() as u32).to_le_bytes());
            out.extend_from_slice(rel.as_bytes());
            out.extend_from_slice(&(contents.len() as u64).to_le_bytes());
            out.extend_from_slice(contents);
        }
        out
    }

    fn encode_test_snapshot_v2(entries: &[DirSnapshotEntry]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(DIR_SNAPSHOT_MAGIC_V2);
        out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        for entry in entries {
            let (kind, rel, mode, contents) = match entry {
                DirSnapshotEntry::Directory { rel, mode } => (1u8, rel, mode, None),
                DirSnapshotEntry::File {
                    rel,
                    mode,
                    contents,
                } => (2u8, rel, mode, Some(contents)),
            };
            out.push(kind);
            out.extend_from_slice(&(rel.len() as u32).to_le_bytes());
            out.extend_from_slice(rel.as_bytes());
            out.extend_from_slice(&mode.to_le_bytes());
            if let Some(contents) = contents {
                out.extend_from_slice(&(contents.len() as u64).to_le_bytes());
                out.extend_from_slice(contents);
            }
        }
        out
    }

    #[test]
    fn finish_reports_append_and_partial_rollback_failures_and_keeps_recovery_blob(
    ) -> io::Result<()> {
        let _lock = crate::ENV_LOCK.lock().unwrap();
        let tmp = TempDir::new()?;
        let cfg = make_config(&tmp);
        let store = HistoryStore::from_config(&cfg)?;
        let restorable = tmp.path().join("relay/skills/restorable");
        let unrestorable = tmp.path().join("relay/skills/unrestorable");
        fs::create_dir_all(&restorable)?;
        fs::create_dir_all(&unrestorable)?;
        fs::write(restorable.join("SKILL.md"), "restorable before\n")?;
        fs::write(unrestorable.join("SKILL.md"), "unrestorable before\n")?;

        let mut recorder = HistoryRecorder::new(&cfg, "sync")?;
        let restorable_before = recorder.capture_path(&restorable)?;
        fs::write(restorable.join("SKILL.md"), "restorable after\n")?;
        let restorable_after = recorder.capture_path(&restorable)?;
        recorder.record_change(&restorable, restorable_before, restorable_after);
        let unrestorable_before = recorder.capture_path(&unrestorable)?;
        fs::write(unrestorable.join("SKILL.md"), "unrestorable after\n")?;
        let unrestorable_after = recorder.capture_path(&unrestorable)?;
        recorder.record_change(&unrestorable, unrestorable_before, unrestorable_after);
        let recovery_blob = recorder.corrupt_latest_before_dir_snapshot()?;

        std::env::set_var("RELAY_TEST_FAIL_HISTORY_APPEND", store.root.as_os_str());
        let err = recorder.finish().unwrap_err();
        std::env::remove_var("RELAY_TEST_FAIL_HISTORY_APPEND");

        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        let message = err.to_string();
        assert!(message.contains("injected history append failure"));
        assert!(message.contains("failed to revert recorded writes"));
        assert!(message.contains("restored 1 of 2 pending writes"));
        assert!(message.contains(&recovery_blob.display().to_string()));
        assert_eq!(
            fs::read_to_string(restorable.join("SKILL.md"))?,
            "restorable before\n"
        );
        assert_eq!(
            fs::read_to_string(unrestorable.join("SKILL.md"))?,
            "unrestorable after\n"
        );
        assert!(recovery_blob.exists());
        Ok(())
    }

    #[test]
    fn legacy_directory_event_matches_current_files_for_non_force_rollback() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let cfg = make_config(&tmp);
        let store = HistoryStore::from_config(&cfg)?;
        let path = tmp.path().join("relay/skills/legacy-package");
        fs::create_dir_all(path.join("nested"))?;
        fs::write(path.join("nested/file.txt"), "before\n")?;
        fs::write(path.join("removed-by-event.txt"), "restore me\n")?;
        let before_blob = store.store_blob(&encode_dir_snapshot_v1(&path)?)?;

        fs::write(path.join("nested/file.txt"), "after\n")?;
        fs::remove_file(path.join("removed-by-event.txt"))?;
        fs::write(path.join("added-by-event.txt"), "remove me\n")?;
        let after_blob = store.store_blob(&encode_dir_snapshot_v1(&path)?)?;
        let after = EntityRef {
            kind: EntityKind::Dir,
            hash: Some(after_blob.clone()),
            blob: Some(after_blob),
        };
        let event_id = "legacy-directory-event";
        store.append_event(&HistoryEvent {
            id: event_id.to_string(),
            timestamp_ms: 1,
            origin: "sync".to_string(),
            writes: vec![HistoryWrite {
                path: path.to_string_lossy().to_string(),
                before: EntityRef {
                    kind: EntityKind::Dir,
                    hash: Some(before_blob.clone()),
                    blob: Some(before_blob),
                },
                after: after.clone(),
            }],
        })?;

        let current = store.capture_path(&path)?;
        assert_ne!(current.hash, after.hash);
        let report = store.rollback(event_id, false)?;
        assert_eq!(report.restored, 1);
        assert_eq!(
            fs::read_to_string(path.join("nested/file.txt"))?,
            "before\n"
        );
        assert_eq!(
            fs::read_to_string(path.join("removed-by-event.txt"))?,
            "restore me\n"
        );
        assert!(!path.join("added-by-event.txt").exists());

        store.restore_path(&path, &after)?;
        fs::write(path.join("nested/file.txt"), "changed later\n")?;
        let changed_err = store.rollback(event_id, false).unwrap_err();
        assert_eq!(changed_err.kind(), io::ErrorKind::AlreadyExists);

        store.restore_path(&path, &after)?;
        fs::write(path.join("unexpected.txt"), "added later\n")?;
        let added_err = store.rollback(event_id, false).unwrap_err();
        assert_eq!(added_err.kind(), io::ErrorKind::AlreadyExists);

        store.restore_path(&path, &after)?;
        fs::remove_file(path.join("nested/file.txt"))?;
        let removed_err = store.rollback(event_id, false).unwrap_err();
        assert_eq!(removed_err.kind(), io::ErrorKind::AlreadyExists);
        Ok(())
    }
}
