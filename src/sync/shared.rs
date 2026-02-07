use super::{ExecutionMode, LogMode};
use crate::history::HistoryRecorder;
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub(crate) const TOOL_CENTRAL: &str = "central";
pub(crate) const CONFLICT_WINDOW_NS: u128 = 2_000_000_000;

pub(crate) struct MarkdownDoc {
    pub(crate) raw: String,
    pub(crate) frontmatter: Option<String>,
    pub(crate) body: String,
    pub(crate) body_hash: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RequiredFrontmatter {
    pub(crate) name: String,
    pub(crate) description: String,
}

pub(crate) struct MarkdownVariant {
    pub(crate) tool: &'static str,
    pub(crate) path: PathBuf,
    pub(crate) doc: MarkdownDoc,
    pub(crate) mtime: u128,
}

pub(crate) fn list_if(
    enabled: bool,
    dir: &Path,
    list: fn(&Path) -> io::Result<HashMap<String, PathBuf>>,
) -> io::Result<HashMap<String, PathBuf>> {
    if enabled {
        list(dir)
    } else {
        Ok(HashMap::new())
    }
}

pub(crate) fn collect_names(maps: &[&HashMap<String, PathBuf>]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for map in maps {
        names.extend(map.keys().cloned());
    }
    names
}

pub(crate) fn list_visible_files(dir: &Path) -> io::Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if let Some((name, path, meta)) = read_visible_entry(entry, true)? {
            if meta.is_file() {
                out.push((name, path));
            }
        }
    }
    Ok(out)
}

pub(crate) fn list_files(dir: &Path) -> io::Result<HashMap<String, PathBuf>> {
    Ok(list_visible_files(dir)?.into_iter().collect())
}

pub(crate) fn list_codex_files(dir: &Path) -> io::Result<HashMap<String, PathBuf>> {
    let mut out: HashMap<String, PathBuf> = HashMap::new();
    for (name, path) in list_visible_files(dir)? {
        let (key, prefixed) = if let Some(stripped) = name.strip_prefix("prompt:") {
            (stripped.to_string(), true)
        } else {
            (name.to_string(), false)
        };
        let replace = match out.get(&key) {
            None => true,
            Some(existing) => {
                let existing_prefixed = existing
                    .file_name()
                    .and_then(|os| os.to_str())
                    .map(|existing_name| existing_name.starts_with("prompt:"))
                    .unwrap_or(false);
                existing_prefixed && !prefixed
            }
        };
        if replace {
            out.insert(key, path);
        }
    }
    Ok(out)
}

pub(crate) fn read_markdown_variant(
    tool: &'static str,
    path: &Path,
) -> io::Result<MarkdownVariant> {
    let doc = read_markdown(path)?;
    let mtime = file_mtime_value(path);
    Ok(MarkdownVariant {
        tool,
        path: path.to_path_buf(),
        doc,
        mtime,
    })
}

pub(crate) fn select_markdown_winner(variants: &[MarkdownVariant]) -> &MarkdownVariant {
    variants
        .iter()
        .max_by_key(|variant| (variant.mtime, tool_order(variant.tool)))
        .expect("winner available")
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn update_markdown_target(
    source: &MarkdownDoc,
    existing: Option<&MarkdownDoc>,
    target_path: &Path,
    preserve_frontmatter: bool,
    log_mode: LogMode,
    mode: ExecutionMode,
    history: &mut Option<HistoryRecorder>,
    label: &str,
) -> io::Result<bool> {
    let frontmatter =
        select_frontmatter_for_target(source, existing, preserve_frontmatter, log_mode, label);
    let merged = merge_frontmatter(frontmatter.as_deref(), &source.body);
    if let Some(existing_doc) = existing {
        if existing_doc.raw == merged {
            return Ok(false);
        }
        if mode == ExecutionMode::Plan {
            log_action(log_mode, &format!("{label}: would update"));
            return Ok(true);
        }
        write_file(target_path, merged.as_bytes(), mode, history)?;
        log_action(log_mode, &format!("{label}: updated"));
        Ok(true)
    } else {
        if mode == ExecutionMode::Plan {
            log_action(log_mode, &format!("{label}: would create"));
            return Ok(true);
        }
        write_file(target_path, merged.as_bytes(), mode, history)?;
        log_action(log_mode, &format!("{label}: created"));
        Ok(true)
    }
}

pub(crate) fn write_file(
    path: &Path,
    contents: &[u8],
    mode: ExecutionMode,
    history: &mut Option<HistoryRecorder>,
) -> io::Result<()> {
    if mode == ExecutionMode::Plan {
        return Ok(());
    }
    path.parent().map(fs::create_dir_all).transpose()?;
    if let Ok(meta) = fs::metadata(path) {
        if meta.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "expected file path but found directory",
            ));
        }
    }
    let before = if let Some(recorder) = history.as_ref() {
        Some(recorder.capture_path(path)?)
    } else {
        None
    };
    fs::write(path, contents)?;
    if let Some(recorder) = history.as_mut() {
        let after = recorder.snapshot_file_bytes(contents)?;
        recorder.record_change(
            path,
            before.unwrap_or_else(crate::history::EntityState::missing),
            after,
        );
    }
    Ok(())
}

pub(crate) fn write_raw_if_changed(
    path: &Path,
    contents: &[u8],
    mode: ExecutionMode,
    history: &mut Option<HistoryRecorder>,
) -> io::Result<bool> {
    if fs::read(path).ok().as_deref() == Some(contents) {
        return Ok(false);
    }
    write_file(path, contents, mode, history)?;
    Ok(true)
}

pub(crate) fn merge_frontmatter(frontmatter: Option<&str>, body: &str) -> String {
    match frontmatter {
        Some(frontmatter) => format!("{frontmatter}{body}"),
        None => body.to_string(),
    }
}

pub(crate) fn required_frontmatter_hash(doc: &MarkdownDoc) -> Option<u64> {
    parse_required_frontmatter(doc.frontmatter.as_deref()).map(|required| {
        let serialized = format!(
            "name:{}\ndescription:{}",
            required.name, required.description
        );
        hash_bytes(serialized.as_bytes())
    })
}

pub(crate) fn select_frontmatter_for_target(
    source: &MarkdownDoc,
    existing: Option<&MarkdownDoc>,
    preserve_frontmatter: bool,
    log_mode: LogMode,
    label: &str,
) -> Option<String> {
    if !preserve_frontmatter {
        return source.frontmatter.clone();
    }
    let source_required = match parse_required_frontmatter(source.frontmatter.as_deref()) {
        Some(required) => Some(required),
        None => {
            if source.frontmatter.is_some() {
                log_action(
                    log_mode,
                    &format!(
                        "warning: {label}: skipping frontmatter sync; expected 'name:' and 'description:'"
                    ),
                );
            }
            None
        }
    };
    match (existing, source_required) {
        (Some(existing_doc), Some(required)) => {
            if let Some(frontmatter) = existing_doc.frontmatter.as_deref() {
                match upsert_required_frontmatter(frontmatter, &required) {
                    Some(updated) => Some(updated),
                    None => {
                        log_action(
                            log_mode,
                            &format!(
                                "warning: {label}: keeping existing frontmatter; unsupported format"
                            ),
                        );
                        existing_doc.frontmatter.clone()
                    }
                }
            } else {
                Some(render_required_frontmatter(&required))
            }
        }
        (None, Some(required)) => Some(render_required_frontmatter(&required)),
        (Some(existing_doc), None) => existing_doc.frontmatter.clone(),
        (None, None) => None,
    }
}

pub(crate) fn read_markdown(path: &Path) -> io::Result<MarkdownDoc> {
    let raw = fs::read_to_string(path)?;
    let (frontmatter, body) = split_frontmatter(&raw);
    let body_hash = hash_bytes(body.as_bytes());
    Ok(MarkdownDoc {
        raw,
        frontmatter,
        body,
        body_hash,
    })
}

fn split_frontmatter(contents: &str) -> (Option<String>, String) {
    let mut lines = contents.split_inclusive('\n');
    let first = match lines.next() {
        Some(line) => line,
        None => return (None, String::new()),
    };
    if strip_line_end(first) != "---" {
        return (None, contents.to_string());
    }
    let mut end = first.len();
    for line in lines {
        end += line.len();
        if strip_line_end(line) == "---" {
            let frontmatter = contents[..end].to_string();
            let body = contents[end..].to_string();
            return (Some(frontmatter), body);
        }
    }
    (None, contents.to_string())
}

fn parse_required_frontmatter(frontmatter: Option<&str>) -> Option<RequiredFrontmatter> {
    let frontmatter = frontmatter?;
    let mut lines = frontmatter.split_inclusive('\n');
    let first = lines.next()?;
    if strip_line_end(first) != "---" {
        return None;
    }
    let mut name: Option<String> = None;
    let mut description: Option<String> = None;
    let mut has_end = false;
    for line in lines {
        if strip_line_end(line) == "---" {
            has_end = true;
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("name:") {
            let value = value.trim();
            if value.is_empty() {
                return None;
            }
            name = Some(value.to_string());
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("description:") {
            let value = value.trim();
            if value.is_empty() {
                return None;
            }
            description = Some(value.to_string());
        }
    }
    if !has_end {
        return None;
    }
    Some(RequiredFrontmatter {
        name: name?,
        description: description?,
    })
}

fn upsert_required_frontmatter(
    frontmatter: &str,
    required: &RequiredFrontmatter,
) -> Option<String> {
    let mut lines = frontmatter.split_inclusive('\n');
    let first = lines.next()?;
    if strip_line_end(first) != "---" {
        return None;
    }
    let mut out = String::new();
    out.push_str(first);
    let mut saw_name = false;
    let mut saw_description = false;
    let mut has_end = false;

    for line in lines {
        if strip_line_end(line) == "---" {
            if !saw_name {
                out.push_str(&format!("name: {}\n", required.name));
            }
            if !saw_description {
                out.push_str(&format!("description: {}\n", required.description));
            }
            out.push_str(line);
            has_end = true;
            continue;
        }
        let trimmed_start = line.trim_start();
        let indent_len = line.len().saturating_sub(trimmed_start.len());
        let indent = &line[..indent_len];
        let newline = if line.ends_with('\n') { "\n" } else { "" };
        if trimmed_start.starts_with("name:") {
            out.push_str(&format!("{indent}name: {}{newline}", required.name));
            saw_name = true;
        } else if trimmed_start.starts_with("description:") {
            out.push_str(&format!(
                "{indent}description: {}{newline}",
                required.description
            ));
            saw_description = true;
        } else {
            out.push_str(line);
        }
    }
    if !has_end {
        return None;
    }
    Some(out)
}

fn render_required_frontmatter(required: &RequiredFrontmatter) -> String {
    format!(
        "---\nname: {}\ndescription: {}\n---\n",
        required.name, required.description
    )
}

fn strip_line_end(line: &str) -> &str {
    line.trim_end_matches(['\n', '\r'])
}

pub(crate) fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

pub(crate) fn file_mtime_value(path: &Path) -> u128 {
    fs::metadata(path)
        .ok()
        .map(|meta| file_mtime_value_from_meta(&meta))
        .unwrap_or(0)
}

pub(crate) fn file_mtime_value_from_meta(meta: &fs::Metadata) -> u128 {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

pub(crate) fn tool_order(tool: &str) -> u8 {
    match tool {
        crate::config::TOOL_CLAUDE => 3,
        crate::config::TOOL_CODEX => 2,
        crate::config::TOOL_OPENCODE => 1,
        _ => 0,
    }
}

pub(crate) fn log_action(mode: LogMode, message: &str) {
    crate::logging::debug(message);
    if mode == LogMode::Actions {
        println!("{message}");
    }
}

pub(crate) fn read_visible_entry(
    entry: fs::DirEntry,
    allow_symlink: bool,
) -> io::Result<Option<(String, PathBuf, fs::Metadata)>> {
    let path = entry.path();
    let name = path
        .file_name()
        .map(|os| os.to_string_lossy().to_string())
        .unwrap_or_default();
    if name.starts_with('.') {
        return Ok(None);
    }
    let meta = match fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    if meta.file_type().is_symlink() {
        if !allow_symlink {
            return Ok(None);
        }
        let target_meta = match fs::metadata(&path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        return Ok(Some((name, path, target_meta)));
    }
    Ok(Some((name, path, meta)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::test_support::{doc, write_plain};
    use tempfile::TempDir;

    #[test]
    fn merge_and_split_frontmatter() {
        let merged = merge_frontmatter(Some("---\nname: x\n---\n"), "Body");
        assert!(merged.contains("Body"));
        let merged = merge_frontmatter(None, "Body");
        assert_eq!(merged, "Body");

        let (frontmatter, body) = split_frontmatter("");
        assert!(frontmatter.is_none());
        assert_eq!(body, "");

        let (frontmatter, body) = split_frontmatter("No frontmatter");
        assert!(frontmatter.is_none());
        assert_eq!(body, "No frontmatter");

        let (frontmatter, body) = split_frontmatter("---\nname: x\nBody");
        assert!(frontmatter.is_none());
        assert_eq!(body, "---\nname: x\nBody");
    }

    #[test]
    fn list_files_and_codex_files_filter_entries() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let dir = tmp.path().join("commands");
        fs::create_dir_all(&dir)?;
        for (name, body) in [("visible.md", "ok"), (".hidden.md", "skip")] {
            write_plain(&dir.join(name), body)?;
        }
        fs::create_dir_all(dir.join("nested"))?;
        let list = list_files(&dir)?;
        assert!(list.contains_key("visible.md"));
        assert!(!list.contains_key(".hidden.md"));

        let codex_dir = tmp.path().join("codex");
        fs::create_dir_all(&codex_dir)?;
        for (name, body) in [
            ("prompt:legacy.md", "legacy"),
            ("legacy.md", "new"),
            (".hidden.md", "skip"),
        ] {
            write_plain(&codex_dir.join(name), body)?;
        }
        fs::create_dir_all(codex_dir.join("nested"))?;
        let codex = list_codex_files(&codex_dir)?;
        assert_eq!(
            codex.get("legacy.md").unwrap().file_name().unwrap(),
            "legacy.md"
        );
        Ok(())
    }

    #[test]
    fn update_markdown_target_respects_source_frontmatter_when_requested() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let source_path = tmp.path().join("source.md");
        let target_path = tmp.path().join("target.md");
        write_plain(&source_path, &doc("source", "Body"))?;
        write_plain(&target_path, &doc("target", "Old"))?;
        let source = read_markdown(&source_path)?;
        let existing = read_markdown(&target_path)?;
        let existing = Some(&existing);
        let target = &target_path;
        let quiet = LogMode::Quiet;
        let mut history = None;
        let _ = update_markdown_target(
            &source,
            existing,
            target,
            false,
            quiet,
            ExecutionMode::Apply,
            &mut history,
            "update",
        )?;

        let updated = fs::read_to_string(&target_path)?;
        assert!(updated.contains("name: source"));
        Ok(())
    }

    #[test]
    fn update_markdown_target_syncs_required_frontmatter() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let source_path = tmp.path().join("source.md");
        let target_path = tmp.path().join("target.md");
        write_plain(
            &source_path,
            "---\nname: source\ndescription: source desc\n---\nBody",
        )?;
        write_plain(
            &target_path,
            "---\nname: target\ndescription: target desc\nextra: keep\n---\nBody",
        )?;
        let source = read_markdown(&source_path)?;
        let existing = read_markdown(&target_path)?;
        let existing = Some(&existing);
        let quiet = LogMode::Quiet;
        let mut history = None;
        let changed = update_markdown_target(
            &source,
            existing,
            &target_path,
            true,
            quiet,
            ExecutionMode::Apply,
            &mut history,
            "update",
        )?;
        assert!(changed);
        let updated = fs::read_to_string(&target_path)?;
        assert!(updated.contains("name: source"));
        assert!(updated.contains("description: source desc"));
        assert!(updated.contains("extra: keep"));
        Ok(())
    }

    #[test]
    fn update_markdown_target_warn_path_keeps_existing_when_source_missing_required_keys(
    ) -> io::Result<()> {
        let tmp = TempDir::new()?;
        let source_path = tmp.path().join("source.md");
        let target_path = tmp.path().join("target.md");
        write_plain(&source_path, "---\nname: source\n---\nNew")?;
        write_plain(
            &target_path,
            "---\nname: target\ndescription: target desc\n---\nOld",
        )?;
        let source = read_markdown(&source_path)?;
        let existing = read_markdown(&target_path)?;
        let existing = Some(&existing);
        let quiet = LogMode::Quiet;
        let mut history = None;
        let changed = update_markdown_target(
            &source,
            existing,
            &target_path,
            true,
            quiet,
            ExecutionMode::Apply,
            &mut history,
            "update",
        )?;
        assert!(changed);
        let updated = fs::read_to_string(&target_path)?;
        assert!(updated.contains("name: target"));
        assert!(updated.contains("description: target desc"));
        assert!(updated.ends_with("New"));
        Ok(())
    }

    #[test]
    fn select_frontmatter_for_new_target_uses_required_fields() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let source_path = tmp.path().join("source.md");
        write_plain(
            &source_path,
            "---\nname: source\ndescription: source desc\n---\nBody",
        )?;
        let source = read_markdown(&source_path)?;
        let selected = select_frontmatter_for_target(&source, None, true, LogMode::Quiet, "x");
        assert!(selected.unwrap_or_default().contains("name: source"));
        Ok(())
    }

    #[test]
    fn write_file_handles_dirs() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let dir = tmp.path().join("dir");
        fs::create_dir_all(&dir)?;
        let mut history = None;
        let err = write_file(&dir, b"fail", ExecutionMode::Apply, &mut history).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        Ok(())
    }

    #[test]
    fn write_raw_if_changed_skips_when_same() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let path = tmp.path().join("file.md");
        write_plain(&path, "same")?;
        let mut history = None;
        let changed = write_raw_if_changed(&path, b"same", ExecutionMode::Apply, &mut history)?;
        assert!(!changed);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn list_visible_files_includes_symlink() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let root = tmp.path().join("dir");
        fs::create_dir_all(&root)?;
        write_plain(&root.join("one.md"), "one")?;
        std::os::unix::fs::symlink(root.join("one.md"), root.join("link.md"))?;

        let files = list_visible_files(&root)?;
        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|(name, _)| name == "one.md"));
        assert!(files.iter().any(|(name, _)| name == "link.md"));
        Ok(())
    }

    #[test]
    fn misc_helpers_cover_branches() {
        assert_eq!(file_mtime_value(Path::new("/nope")), 0);
        assert_eq!(tool_order("unknown"), 0);
        log_action(LogMode::Actions, "hello");
        log_action(LogMode::Quiet, "quiet");
    }
}
