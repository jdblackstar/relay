use crate::config::Config;
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const DIR_SNAPSHOT_MAGIC: &[u8] = b"RLYD1";

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
pub struct HistorySummary {
    pub id: String,
    pub timestamp_ms: u64,
    pub origin: String,
    pub writes: usize,
}

#[cfg_attr(any(test, coverage), allow(dead_code))]
#[derive(Debug, Clone)]
pub struct RollbackReport {
    pub target_event_id: String,
    pub restored: usize,
    pub rollback_event_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HistoryStore {
    root: PathBuf,
}

impl HistoryStore {
    pub fn from_config(cfg: &Config) -> io::Result<Self> {
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

    pub fn list_recent(&self, limit: usize) -> io::Result<Vec<HistorySummary>> {
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
    pub fn latest_event_id(&self) -> io::Result<Option<String>> {
        Ok(self.read_events()?.into_iter().last().map(|event| event.id))
    }

    pub fn rollback(&self, event_id: &str, force: bool) -> io::Result<RollbackReport> {
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
            if !force && !state_matches(&current, &write.after) {
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
                remove_path_if_exists(path)?;
                path.parent().map(fs::create_dir_all).transpose()?;
                fs::write(path, self.read_blob(blob_id)?)
            }
            EntityKind::Dir => {
                let Some(blob_id) = state.blob.as_deref() else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "missing dir blob reference",
                    ));
                };
                remove_path_if_exists(path)?;
                fs::create_dir_all(path)?;
                let snapshot = self.read_blob(blob_id)?;
                for (rel, contents) in decode_dir_snapshot(&snapshot)? {
                    let file_path = path.join(rel);
                    file_path.parent().map(fs::create_dir_all).transpose()?;
                    fs::write(file_path, contents)?;
                }
                Ok(())
            }
        }
    }

    pub(crate) fn capture_path(&self, path: &Path) -> io::Result<EntityState> {
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

pub struct HistoryRecorder {
    store: HistoryStore,
    event: HistoryEvent,
}

impl HistoryRecorder {
    pub fn new(cfg: &Config, origin: &str) -> io::Result<Self> {
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

    pub fn finish(self) -> io::Result<Option<String>> {
        if self.event.writes.is_empty() {
            return Ok(None);
        }
        self.store.append_event(&self.event)?;
        Ok(Some(self.event.id))
    }
}

fn state_matches(current: &EntityState, expected: &EntityRef) -> bool {
    current.kind == expected.kind && current.hash == expected.hash
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
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

fn encode_dir_snapshot(root: &Path) -> io::Result<Vec<u8>> {
    let mut entries = Vec::new();
    collect_dir_entries(root, root, &mut entries)?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut out = Vec::new();
    out.extend_from_slice(DIR_SNAPSHOT_MAGIC);
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for (rel, bytes) in entries {
        let rel_bytes = rel.as_bytes();
        out.extend_from_slice(&(rel_bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(rel_bytes);
        out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        out.extend_from_slice(&bytes);
    }
    Ok(out)
}

fn decode_dir_snapshot(bytes: &[u8]) -> io::Result<Vec<(String, Vec<u8>)>> {
    let mut idx = 0usize;
    if bytes.len() < DIR_SNAPSHOT_MAGIC.len()
        || &bytes[..DIR_SNAPSHOT_MAGIC.len()] != DIR_SNAPSHOT_MAGIC
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid directory snapshot header",
        ));
    }
    idx += DIR_SNAPSHOT_MAGIC.len();
    let count = read_u32(bytes, &mut idx)? as usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let rel_len = read_u32(bytes, &mut idx)? as usize;
        if idx + rel_len > bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "directory snapshot path overflow",
            ));
        }
        let rel = String::from_utf8(bytes[idx..idx + rel_len].to_vec()).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid utf8 path: {err}"),
            )
        })?;
        idx += rel_len;
        let data_len = read_u64(bytes, &mut idx)? as usize;
        if idx + data_len > bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "directory snapshot data overflow",
            ));
        }
        out.push((rel, bytes[idx..idx + data_len].to_vec()));
        idx += data_len;
    }
    Ok(out)
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

fn collect_dir_entries(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, Vec<u8>)>,
) -> io::Result<()> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)? {
        entries.push(entry?);
    }
    entries.sort_by_key(|entry| file_name(entry.path().file_name()));
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
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            out.push((rel, fs::read(&path)?));
        }
    }
    Ok(())
}

fn file_name(name: Option<&OsStr>) -> String {
    name.map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_config(tmp: &TempDir) -> Config {
        Config {
            enabled_tools: vec!["codex".to_string()],
            verified_versions: std::collections::HashMap::new(),
            central_dir: tmp.path().join("relay/commands"),
            central_skills_dir: tmp.path().join("relay/skills"),
            central_agents_dir: tmp.path().join("relay/agents"),
            central_rules_dir: tmp.path().join("relay/rules"),
            claude_dir: tmp.path().join("claude/commands"),
            claude_skills_dir: tmp.path().join("claude/skills"),
            cursor_dir: tmp.path().join("cursor/commands"),
            opencode_commands_dir: tmp.path().join("opencode/command"),
            opencode_skills_dir: tmp.path().join("opencode/skill"),
            opencode_agents_file: tmp.path().join("opencode/AGENTS.md"),
            codex_dir: tmp.path().join("codex/prompts"),
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
}
