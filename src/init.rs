use crate::config::{
    resolve_home_dir, Config, TOOL_CLAUDE, TOOL_CODEX, TOOL_CURSOR, TOOL_OPENCODE,
};
use crate::report::print_sync_summary;
use crate::sync::{self, LogMode};
use crate::tools::{tool_detected, tool_expected_paths, TOOL_DEFINITIONS};
use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::symlink;

#[cfg(not(test))]
use crate::config::expand_tilde;
#[cfg(not(test))]
use dialoguer::{theme::ColorfulTheme, Confirm, Input, MultiSelect};

pub fn init() -> io::Result<()> {
    let defaults = Config::default_paths()?;
    let enabled_tools = select_tools(&defaults)?;
    let tool_selected = |name: &str| enabled_tools.iter().any(|tool| tool == name);
    let claude_selected = tool_selected(TOOL_CLAUDE);
    let cursor_selected = tool_selected(TOOL_CURSOR);
    let codex_selected = tool_selected(TOOL_CODEX);
    let opencode_selected = tool_selected(TOOL_OPENCODE);
    let central_root = prompt_central_root(&defaults)?;
    let central_dir = central_root.join("commands");
    let central_skills_dir = central_root.join("skills");
    let central_agents_dir = central_root.join("agents");
    let central_rules_dir = central_root.join("rules");

    let claude_base_default = tool_base_default(&defaults.claude_dir);
    let cursor_base_default = tool_base_default(&defaults.cursor_dir);
    let codex_base_default = tool_base_default(&defaults.codex_dir);
    let opencode_base_default = tool_base_default(&defaults.opencode_commands_dir);

    let claude_base = prompt_base_if_missing(
        claude_selected,
        tool_detected(&defaults, TOOL_CLAUDE),
        "Claude base directory",
        &claude_base_default,
    )?;
    let cursor_base = prompt_base_if_missing(
        cursor_selected,
        tool_detected(&defaults, TOOL_CURSOR),
        "Cursor base directory",
        &cursor_base_default,
    )?;
    let codex_base = prompt_base_if_missing(
        codex_selected,
        tool_detected(&defaults, TOOL_CODEX),
        "Codex base directory",
        &codex_base_default,
    )?;
    let opencode_base = prompt_base_if_missing(
        opencode_selected,
        tool_detected(&defaults, TOOL_OPENCODE),
        "OpenCode base directory",
        &opencode_base_default,
    )?;

    let claude_dir = derive_from_base(claude_base.as_deref(), &defaults.claude_dir, "commands");
    let claude_skills_dir = derive_from_base(
        claude_base.as_deref(),
        &defaults.claude_skills_dir,
        "skills",
    );
    let cursor_dir = derive_from_base(cursor_base.as_deref(), &defaults.cursor_dir, "commands");
    let codex_dir = derive_from_base(codex_base.as_deref(), &defaults.codex_dir, "prompts");
    let codex_skills_dir =
        derive_from_base(codex_base.as_deref(), &defaults.codex_skills_dir, "skills");
    let codex_rules_file = derive_from_base(
        codex_base.as_deref(),
        &defaults.codex_rules_file,
        "rules/default.rules",
    );
    let codex_agents_file = derive_from_base(
        codex_base.as_deref(),
        &defaults.codex_agents_file,
        "AGENTS.md",
    );
    let oc_dir = derive_from_base(
        opencode_base.as_deref(),
        &defaults.opencode_commands_dir,
        "command",
    );
    let os_dir = derive_from_base(
        opencode_base.as_deref(),
        &defaults.opencode_skills_dir,
        "skill",
    );
    let oa_file = derive_from_base(
        opencode_base.as_deref(),
        &defaults.opencode_agents_file,
        "AGENTS.md",
    );

    let cfg = Config {
        enabled_tools,
        verified_versions: defaults.verified_versions.clone(),
        central_dir,
        central_skills_dir,
        central_agents_dir,
        central_rules_dir,
        claude_dir,
        claude_skills_dir,
        cursor_dir,
        opencode_commands_dir: oc_dir,
        opencode_skills_dir: os_dir,
        opencode_agents_file: oa_file,
        codex_dir,
        codex_skills_dir,
        codex_rules_file,
        codex_agents_file,
    };
    let config_path = Config::config_path()?;
    cfg.save(&config_path)?;
    ensure_tool_bases(&cfg)?;
    maybe_setup_dotfiles_backup(&cfg)?;
    let report = sync::sync_all(&cfg, LogMode::Quiet)?;
    println!("Saved config to {}", config_path.display());
    if !report.is_empty() {
        print_sync_summary(&report);
    }
    print_tool_detection_note(&cfg);
    Ok(())
}

#[cfg(not(any(test, coverage)))]
fn prompt_path(label: &str, default: Option<&Path>) -> io::Result<PathBuf> {
    let theme = ColorfulTheme::default();
    loop {
        let input = Input::with_theme(&theme).with_prompt(label);
        let input = if let Some(default_path) = default {
            input.default(default_path.to_string_lossy().to_string())
        } else {
            input
        };
        let value: String = match input.interact_text() {
            Ok(value) => value,
            Err(dialoguer::Error::IO(err)) if err.kind() == io::ErrorKind::NotConnected => {
                if let Some(default_path) = default {
                    return Ok(default_path.to_path_buf());
                }
                return Err(io::Error::new(
                    io::ErrorKind::NotConnected,
                    "not a terminal",
                ));
            }
            Err(err) => return Err(dialoguer_to_io(err)),
        };
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        return expand_tilde(trimmed);
    }
}

#[cfg(not(any(test, coverage)))]
fn prompt_yes_no(label: &str) -> io::Result<bool> {
    let theme = ColorfulTheme::default();
    match Confirm::with_theme(&theme)
        .with_prompt(label)
        .default(false)
        .interact()
    {
        Ok(value) => Ok(value),
        Err(dialoguer::Error::IO(err)) if err.kind() == io::ErrorKind::NotConnected => Ok(false),
        Err(err) => Err(dialoguer_to_io(err)),
    }
}

#[cfg(any(test, coverage))]
fn prompt_path(_label: &str, default: Option<&Path>) -> io::Result<PathBuf> {
    if let Some(path) = default {
        return Ok(path.to_path_buf());
    }
    if let Ok(value) = std::env::var("RELAY_TEST_PROMPT_PATH") {
        let value = value.trim();
        if !value.is_empty() {
            return Ok(PathBuf::from(value));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "missing default path in test prompt",
    ))
}

#[cfg(any(test, coverage))]
fn prompt_yes_no(_label: &str) -> io::Result<bool> {
    Ok(std::env::var("RELAY_TEST_CONFIRM").ok().as_deref() == Some("1"))
}

fn ensure_tool_bases(cfg: &Config) -> io::Result<()> {
    if cfg.tool_enabled(TOOL_CLAUDE) {
        for path in [&cfg.claude_dir, &cfg.claude_skills_dir] {
            ensure_parent_if_exists(path)?;
        }
    }
    if cfg.tool_enabled(TOOL_CURSOR) {
        ensure_parent_if_exists(&cfg.cursor_dir)?;
    }
    if cfg.tool_enabled(TOOL_CODEX) {
        for path in [
            &cfg.codex_dir,
            &cfg.codex_skills_dir,
            &cfg.codex_rules_file,
            &cfg.codex_agents_file,
        ] {
            ensure_parent_if_exists(path)?;
        }
    }
    if cfg.tool_enabled(TOOL_OPENCODE) {
        for path in [
            &cfg.opencode_commands_dir,
            &cfg.opencode_skills_dir,
            &cfg.opencode_agents_file,
        ] {
            ensure_parent_if_exists(path)?;
        }
    }
    Ok(())
}

fn ensure_parent_if_exists(path: &Path) -> io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    path.parent().map(std::fs::create_dir_all).transpose()?;
    Ok(())
}

fn maybe_setup_dotfiles_backup(cfg: &Config) -> io::Result<()> {
    let home = resolve_home_dir()?.ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "could not resolve home directory")
    })?;
    let dotfiles_root = home.join(".dotfiles");
    if !dotfiles_root.is_dir() {
        return Ok(());
    }
    let central_root = match central_root(cfg) {
        Some(root) => root,
        None => return Ok(()),
    };
    let backup_root = dotfiles_root.join("config/relay");
    let prompt = format!(
        "Detected ~/.dotfiles. Use {} as source of truth and link {} -> {}? Relay will move existing data if needed.",
        backup_root.display(),
        central_root.display(),
        backup_root.display()
    );
    if !prompt_yes_no(&prompt)? {
        return Ok(());
    }
    backup_root
        .parent()
        .map(std::fs::create_dir_all)
        .transpose()?;
    if let Ok(meta) = std::fs::symlink_metadata(&central_root) {
        if meta.file_type().is_symlink() {
            let current = std::fs::read_link(&central_root)?;
            if current == backup_root {
                return Ok(());
            }
            std::fs::remove_file(&central_root)?;
        } else if meta.is_dir() {
            if backup_root.exists() {
                println!(
                    "Central path exists and backup already exists; skipping: {}",
                    central_root.display()
                );
                return Ok(());
            }
            std::fs::rename(&central_root, &backup_root)?;
        } else {
            println!(
                "Central path exists and is not a directory; skipping: {}",
                central_root.display()
            );
            return Ok(());
        }
    }
    if !backup_root.exists() {
        std::fs::create_dir_all(&backup_root)?;
    }
    central_root
        .parent()
        .map(std::fs::create_dir_all)
        .transpose()?;
    #[cfg(unix)]
    {
        symlink(&backup_root, &central_root)?;
        println!(
            "Dotfiles backup linked: {} -> {}",
            central_root.display(),
            backup_root.display()
        );
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = central_root;
        let _ = backup_root;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "symlinks are not supported on this platform",
        ))
    }
}

fn central_root(cfg: &Config) -> Option<PathBuf> {
    let commands_parent = cfg.central_dir.parent()?;
    let skills_parent = cfg.central_skills_dir.parent()?;
    let agents_parent = cfg.central_agents_dir.parent()?;
    let rules_parent = cfg.central_rules_dir.parent()?;
    if commands_parent == skills_parent
        && commands_parent == agents_parent
        && commands_parent == rules_parent
    {
        Some(commands_parent.to_path_buf())
    } else {
        None
    }
}

#[cfg(not(any(test, coverage)))]
fn select_tools(defaults: &Config) -> io::Result<Vec<String>> {
    let items: Vec<(String, bool)> = TOOL_DEFINITIONS
        .iter()
        .map(|spec| {
            let detected = tool_detected(defaults, spec.id);
            let suffix = if detected {
                " (detected)"
            } else {
                " (not detected)"
            };
            (format!("{}{suffix}", spec.label), detected)
        })
        .collect();

    let theme = ColorfulTheme::default();
    let selections = match MultiSelect::with_theme(&theme)
        .with_prompt("Select tools to sync (Space to toggle, Enter to confirm)")
        .items_checked(&items)
        .interact_opt()
    {
        Ok(Some(indices)) => indices,
        Ok(None) => Vec::new(),
        Err(_err) => {
            let fallback: Vec<String> = TOOL_DEFINITIONS
                .iter()
                .filter(|spec| tool_detected(defaults, spec.id))
                .map(|spec| spec.id.to_string())
                .collect();
            if fallback.is_empty() {
                return Ok(TOOL_DEFINITIONS
                    .iter()
                    .map(|spec| spec.id.to_string())
                    .collect());
            }
            return Ok(fallback);
        }
    };

    let chosen: Vec<String> = selections
        .into_iter()
        .map(|idx| TOOL_DEFINITIONS[idx].id.to_string())
        .collect();
    print_selected_undetected_note(defaults, &chosen);
    Ok(chosen)
}

#[cfg(any(test, coverage))]
fn select_tools(defaults: &Config) -> io::Result<Vec<String>> {
    if let Ok(tools) = std::env::var("RELAY_TEST_TOOLS") {
        let parsed = tools
            .split(',')
            .map(|tool| tool.trim().to_ascii_lowercase())
            .filter(|tool| !tool.is_empty())
            .collect::<Vec<_>>();
        if !parsed.is_empty() {
            return Ok(parsed);
        }
    }
    Ok(defaults.enabled_tools.clone())
}

fn dialoguer_to_io(err: dialoguer::Error) -> io::Error {
    io::Error::other(err)
}

fn central_root_default(defaults: &Config) -> PathBuf {
    defaults
        .central_dir
        .parent()
        .map(|path| path.to_path_buf())
        .unwrap_or_else(|| defaults.central_dir.clone())
}

fn prompt_central_root(defaults: &Config) -> io::Result<PathBuf> {
    let default_root = central_root_default(defaults);
    let suffix = if default_root.exists() {
        ""
    } else {
        " (will be created)"
    };
    let prompt = format!(
        "Central relay directory{} (default: {})",
        suffix,
        default_root.display()
    );
    prompt_path(&prompt, Some(&default_root))
}

fn tool_base_default(path: &Path) -> PathBuf {
    path.parent()
        .map(|path| path.to_path_buf())
        .unwrap_or_else(|| path.to_path_buf())
}

fn derive_from_base(base: Option<&Path>, default: &Path, suffix: &str) -> PathBuf {
    base.map(|base| base.join(suffix))
        .unwrap_or_else(|| default.to_path_buf())
}

fn prompt_base_if_missing(
    selected: bool,
    detected: bool,
    label: &str,
    expected: &Path,
) -> io::Result<Option<PathBuf>> {
    if selected && !detected {
        let prompt = format!("{label} (expected: {})", expected.display());
        let base = prompt_path(&prompt, Some(expected))?;
        Ok(Some(base))
    } else {
        Ok(None)
    }
}

fn print_selected_undetected_note(defaults: &Config, selected: &[String]) {
    let mut notes: Vec<String> = Vec::new();
    for tool in selected {
        if tool_detected(defaults, tool) {
            continue;
        }
        if let Some(message) = tool_expected_paths(defaults, tool) {
            notes.push(message);
        }
    }
    if notes.is_empty() {
        return;
    }
    println!("Selected tools not detected yet; relay will sync when installed:");
    for note in notes {
        println!("- {note}");
    }
}

fn print_tool_detection_note(cfg: &Config) {
    let mut notes: Vec<String> = Vec::new();
    for spec in TOOL_DEFINITIONS {
        if cfg.tool_enabled(spec.id) && !tool_detected(cfg, spec.id) {
            if let Some(message) = tool_expected_paths(cfg, spec.id) {
                notes.push(message);
            }
        }
    }

    if notes.is_empty() {
        return;
    }
    println!("Note: some selected tools were not detected:");
    for note in notes {
        println!("- {note}");
    }
    println!("Relay will start syncing if these paths appear; run `relay sync` to update.");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, TOOL_CLAUDE, TOOL_CODEX, TOOL_CURSOR, TOOL_OPENCODE};
    use std::env;
    use std::fs;
    use tempfile::TempDir;

    fn set_env(key: &str, value: Option<&str>) {
        if let Some(value) = value {
            env::set_var(key, value);
        } else {
            env::remove_var(key);
        }
    }

    #[inline(never)]
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::ENV_LOCK.lock().unwrap()
    }

    fn make_config(tmp: &TempDir) -> Config {
        Config {
            enabled_tools: vec![
                TOOL_CLAUDE.to_string(),
                TOOL_CODEX.to_string(),
                TOOL_CURSOR.to_string(),
                TOOL_OPENCODE.to_string(),
            ],
            verified_versions: std::collections::HashMap::new(),
            central_dir: tmp.path().join("central/commands"),
            central_skills_dir: tmp.path().join("central/skills"),
            central_agents_dir: tmp.path().join("central/agents"),
            central_rules_dir: tmp.path().join("central/rules"),
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
    fn prompt_helpers_use_defaults() -> io::Result<()> {
        let _lock = env_lock();
        let tmp = TempDir::new()?;
        let default_path = tmp.path().join("default");
        let result = prompt_path("test", Some(&default_path))?;
        assert_eq!(result, default_path);

        let err = prompt_path("test", None).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        set_env(
            "RELAY_TEST_PROMPT_PATH",
            Some(tmp.path().join("fallback").to_string_lossy().as_ref()),
        );
        let fallback = prompt_path("test", None)?;
        assert!(fallback.ends_with("fallback"));
        set_env("RELAY_TEST_PROMPT_PATH", None);

        set_env("RELAY_TEST_CONFIRM", Some("1"));
        assert!(prompt_yes_no("test")?);
        set_env("RELAY_TEST_CONFIRM", None);
        assert!(!prompt_yes_no("test")?);
        Ok(())
    }

    #[test]
    fn prompt_path_ignores_blank_env_value() {
        let _lock = env_lock();
        set_env("RELAY_TEST_PROMPT_PATH", Some("   "));
        let err = prompt_path("test", None).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        set_env("RELAY_TEST_PROMPT_PATH", None);
    }

    #[test]
    fn prompt_path_uses_env_value() -> io::Result<()> {
        let _lock = env_lock();
        let tmp = TempDir::new()?;
        set_env(
            "RELAY_TEST_PROMPT_PATH",
            Some(tmp.path().join("env").to_string_lossy().as_ref()),
        );
        let path = prompt_path("test", None)?;
        assert!(path.ends_with("env"));
        set_env("RELAY_TEST_PROMPT_PATH", None);
        Ok(())
    }

    #[test]
    fn select_tools_uses_env_override() -> io::Result<()> {
        let _lock = env_lock();
        set_env("RELAY_TEST_TOOLS", Some("codex, claude"));
        let tmp = TempDir::new()?;
        let cfg = make_config(&tmp);
        let selected = select_tools(&cfg)?;
        assert_eq!(selected, vec!["codex".to_string(), "claude".to_string()]);
        set_env("RELAY_TEST_TOOLS", Some(" , "));
        let selected = select_tools(&cfg)?;
        assert_eq!(selected, cfg.enabled_tools);
        set_env("RELAY_TEST_TOOLS", None);
        let selected = select_tools(&cfg)?;
        assert!(!selected.is_empty());
        Ok(())
    }

    #[test]
    fn tool_detection_and_labels() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let cfg = make_config(&tmp);
        fs::create_dir_all(&cfg.claude_dir)?;
        fs::create_dir_all(&cfg.codex_skills_dir)?;
        fs::create_dir_all(&cfg.opencode_commands_dir)?;

        assert!(tool_detected(&cfg, TOOL_CLAUDE));
        assert!(tool_detected(&cfg, TOOL_CODEX));
        assert!(tool_detected(&cfg, TOOL_OPENCODE));
        assert!(!tool_detected(&cfg, TOOL_CURSOR));
        assert!(!tool_detected(&cfg, "unknown"));

        let claude_base = tool_base_default(&cfg.claude_dir);
        assert!(claude_base.ends_with("claude"));
        let codex_base = tool_base_default(&cfg.codex_dir);
        assert!(codex_base.ends_with("codex"));
        let opencode_base = tool_base_default(&cfg.opencode_commands_dir);
        assert!(opencode_base.ends_with("opencode"));

        let prompt = prompt_base_if_missing(true, false, "Claude base directory", &claude_base)?;
        assert_eq!(prompt, Some(claude_base));
        assert!(
            prompt_base_if_missing(true, true, "Claude base directory", &codex_base)?.is_none()
        );
        assert!(
            prompt_base_if_missing(false, false, "Claude base directory", &codex_base)?.is_none()
        );
        Ok(())
    }

    #[test]
    fn central_root_checks_parents() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let cfg = make_config(&tmp);
        let root = central_root(&cfg).unwrap();
        assert!(root.ends_with("central"));

        let mut cfg = cfg;
        cfg.central_rules_dir = tmp.path().join("other/rules");
        assert!(central_root(&cfg).is_none());
        Ok(())
    }

    #[test]
    fn central_root_default_and_prompt() -> io::Result<()> {
        let _lock = env_lock();
        let tmp = TempDir::new()?;
        let cfg = make_config(&tmp);
        let root = central_root_default(&cfg);
        let prompted = prompt_central_root(&cfg)?;
        assert_eq!(prompted, root);

        fs::create_dir_all(&root)?;
        let prompted = prompt_central_root(&cfg)?;
        assert_eq!(prompted, root);
        Ok(())
    }

    #[test]
    fn init_writes_config_and_notes() -> io::Result<()> {
        let _lock = env_lock();
        let tmp = TempDir::new()?;
        let home = tmp.path().join("home");
        fs::create_dir_all(&home)?;
        set_env("RELAY_HOME", Some(home.to_string_lossy().as_ref()));
        set_env("HOME", Some(home.to_string_lossy().as_ref()));
        set_env(
            "RELAY_TEST_PROMPT_PATH",
            Some(tmp.path().join("fallback").to_string_lossy().as_ref()),
        );
        let claude_commands = home.join(".claude/commands");
        fs::create_dir_all(&claude_commands)?;
        fs::write(claude_commands.join("hello.md"), "hello")?;

        init()?;

        let config_path = Config::config_path()?;
        assert!(config_path.exists());
        set_env("RELAY_HOME", None);
        set_env("HOME", None);
        set_env("RELAY_TEST_PROMPT_PATH", None);
        Ok(())
    }

    #[test]
    fn dotfiles_backup_creates_symlink() -> io::Result<()> {
        let _lock = env_lock();
        let tmp = TempDir::new()?;
        let home = tmp.path().join("home");
        fs::create_dir_all(home.join(".dotfiles"))?;
        set_env("HOME", Some(home.to_string_lossy().as_ref()));

        let mut cfg = make_config(&tmp);
        cfg.central_dir = home.join(".config/relay/commands");
        cfg.central_skills_dir = home.join(".config/relay/skills");
        cfg.central_agents_dir = home.join(".config/relay/agents");
        cfg.central_rules_dir = home.join(".config/relay/rules");
        set_env("RELAY_TEST_CONFIRM", Some("1"));

        maybe_setup_dotfiles_backup(&cfg)?;

        let backup = home.join(".dotfiles/config/relay");
        let central = home.join(".config/relay");
        assert!(fs::symlink_metadata(&central)?.file_type().is_symlink());
        let current = fs::read_link(&central)?;
        assert_eq!(current, backup);
        set_env("HOME", None);
        set_env("RELAY_TEST_CONFIRM", None);
        Ok(())
    }

    #[test]
    fn dotfiles_backup_errors_without_home() {
        let _lock = env_lock();
        set_env("RELAY_NO_HOME", Some("1"));
        let tmp = TempDir::new().expect("tmp");
        let cfg = make_config(&tmp);
        let err = maybe_setup_dotfiles_backup(&cfg).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        set_env("RELAY_NO_HOME", None);
    }

    #[test]
    fn dotfiles_backup_skips_when_central_root_missing() -> io::Result<()> {
        let _lock = env_lock();
        let tmp = TempDir::new()?;
        let home = tmp.path().join("home");
        fs::create_dir_all(home.join(".dotfiles"))?;
        set_env("RELAY_HOME", Some(home.to_string_lossy().as_ref()));
        let mut cfg = make_config(&tmp);
        cfg.central_dir = home.join(".config/relay/commands");
        cfg.central_skills_dir = home.join(".config/relay/other/skills");
        cfg.central_agents_dir = home.join(".config/relay/agents");
        cfg.central_rules_dir = home.join(".config/relay/rules");
        maybe_setup_dotfiles_backup(&cfg)?;
        set_env("RELAY_HOME", None);
        Ok(())
    }

    #[test]
    fn dotfiles_backup_replaces_mismatched_symlink() -> io::Result<()> {
        let _lock = env_lock();
        let tmp = TempDir::new()?;
        let home = tmp.path().join("home");
        fs::create_dir_all(home.join(".dotfiles"))?;
        set_env("RELAY_HOME", Some(home.to_string_lossy().as_ref()));
        let mut cfg = make_config(&tmp);
        cfg.central_dir = home.join(".config/relay/commands");
        cfg.central_skills_dir = home.join(".config/relay/skills");
        cfg.central_agents_dir = home.join(".config/relay/agents");
        cfg.central_rules_dir = home.join(".config/relay/rules");
        let backup = home.join(".dotfiles/config/relay");
        let wrong_target = home.join("other");
        fs::create_dir_all(&wrong_target)?;
        #[cfg(unix)]
        {
            let central = home.join(".config/relay");
            central.parent().map(fs::create_dir_all).transpose()?;
            std::os::unix::fs::symlink(&wrong_target, &central)?;
        }
        set_env("RELAY_TEST_CONFIRM", Some("1"));
        maybe_setup_dotfiles_backup(&cfg)?;
        #[cfg(unix)]
        {
            let current = fs::read_link(home.join(".config/relay"))?;
            assert_eq!(current, backup);
        }
        set_env("RELAY_TEST_CONFIRM", None);
        set_env("RELAY_HOME", None);
        Ok(())
    }

    #[test]
    fn dotfiles_backup_skips_when_missing() -> io::Result<()> {
        let _lock = env_lock();
        let tmp = TempDir::new()?;
        let home = tmp.path().join("home");
        fs::create_dir_all(&home)?;
        set_env("HOME", Some(home.to_string_lossy().as_ref()));
        let cfg = make_config(&tmp);
        maybe_setup_dotfiles_backup(&cfg)?;
        set_env("HOME", None);
        Ok(())
    }

    #[test]
    fn dotfiles_backup_moves_existing_central_dir() -> io::Result<()> {
        let _lock = env_lock();
        let tmp = TempDir::new()?;
        let home = tmp.path().join("home");
        fs::create_dir_all(home.join(".dotfiles"))?;
        let central = home.join(".config/relay");
        let commands = central.join("commands");
        fs::create_dir_all(&commands)?;
        fs::write(commands.join("hello.md"), "hello")?;
        set_env("HOME", Some(home.to_string_lossy().as_ref()));
        let mut cfg = make_config(&tmp);
        cfg.central_dir = home.join(".config/relay/commands");
        cfg.central_skills_dir = home.join(".config/relay/skills");
        cfg.central_agents_dir = home.join(".config/relay/agents");
        cfg.central_rules_dir = home.join(".config/relay/rules");
        set_env("RELAY_TEST_CONFIRM", Some("1"));
        maybe_setup_dotfiles_backup(&cfg)?;
        let backup = home.join(".dotfiles/config/relay");
        assert!(fs::symlink_metadata(&central)?.file_type().is_symlink());
        let current = fs::read_link(&central)?;
        assert_eq!(current, backup);
        assert!(backup.join("commands/hello.md").exists());
        set_env("RELAY_TEST_CONFIRM", None);
        set_env("HOME", None);
        Ok(())
    }

    #[test]
    fn dotfiles_backup_skips_when_existing_non_symlink() -> io::Result<()> {
        let _lock = env_lock();
        let tmp = TempDir::new()?;
        let home = tmp.path().join("home");
        let backup = home.join(".dotfiles/config/relay");
        fs::create_dir_all(&backup)?;
        let central = home.join(".config/relay");
        fs::create_dir_all(&central)?;
        set_env("HOME", Some(home.to_string_lossy().as_ref()));
        let mut cfg = make_config(&tmp);
        cfg.central_dir = home.join(".config/relay/commands");
        cfg.central_skills_dir = home.join(".config/relay/skills");
        cfg.central_agents_dir = home.join(".config/relay/agents");
        cfg.central_rules_dir = home.join(".config/relay/rules");
        set_env("RELAY_TEST_CONFIRM", Some("1"));
        maybe_setup_dotfiles_backup(&cfg)?;
        assert!(fs::metadata(&central)?.is_dir());
        set_env("RELAY_TEST_CONFIRM", None);
        set_env("HOME", None);
        Ok(())
    }

    #[test]
    fn dotfiles_backup_skips_when_symlink_matches() -> io::Result<()> {
        let _lock = env_lock();
        let tmp = TempDir::new()?;
        let home = tmp.path().join("home");
        fs::create_dir_all(home.join(".dotfiles/config/relay"))?;
        fs::create_dir_all(home.join(".config"))?;
        set_env("HOME", Some(home.to_string_lossy().as_ref()));
        let mut cfg = make_config(&tmp);
        cfg.central_dir = home.join(".config/relay/commands");
        cfg.central_skills_dir = home.join(".config/relay/skills");
        cfg.central_agents_dir = home.join(".config/relay/agents");
        cfg.central_rules_dir = home.join(".config/relay/rules");
        #[cfg(unix)]
        {
            let central = home.join(".config/relay");
            std::os::unix::fs::symlink(home.join(".dotfiles/config/relay"), &central)?;
        }

        set_env("RELAY_TEST_CONFIRM", Some("1"));
        maybe_setup_dotfiles_backup(&cfg)?;
        #[cfg(unix)]
        {
            let current = fs::read_link(home.join(".config/relay"))?;
            assert_eq!(current, home.join(".dotfiles/config/relay"));
        }
        set_env("RELAY_TEST_CONFIRM", None);
        set_env("HOME", None);
        Ok(())
    }

    #[test]
    fn init_with_subset_tools() -> io::Result<()> {
        let _lock = env_lock();
        let tmp = TempDir::new()?;
        let home = tmp.path().join("home");
        fs::create_dir_all(&home)?;
        set_env("RELAY_HOME", Some(home.to_string_lossy().as_ref()));
        set_env("HOME", Some(home.to_string_lossy().as_ref()));
        set_env("RELAY_TEST_TOOLS", Some("claude"));
        set_env(
            "RELAY_TEST_PROMPT_PATH",
            Some(tmp.path().join("fallback").to_string_lossy().as_ref()),
        );

        init()?;

        set_env("RELAY_HOME", None);
        set_env("HOME", None);
        set_env("RELAY_TEST_TOOLS", None);
        set_env("RELAY_TEST_PROMPT_PATH", None);
        Ok(())
    }

    #[test]
    fn init_without_claude() -> io::Result<()> {
        let _lock = env_lock();
        let tmp = TempDir::new()?;
        let home = tmp.path().join("home");
        fs::create_dir_all(&home)?;
        set_env("RELAY_HOME", Some(home.to_string_lossy().as_ref()));
        set_env("HOME", Some(home.to_string_lossy().as_ref()));
        set_env("RELAY_TEST_TOOLS", Some("codex"));
        set_env(
            "RELAY_TEST_PROMPT_PATH",
            Some(tmp.path().join("fallback").to_string_lossy().as_ref()),
        );

        init()?;

        set_env("RELAY_HOME", None);
        set_env("HOME", None);
        set_env("RELAY_TEST_TOOLS", None);
        set_env("RELAY_TEST_PROMPT_PATH", None);
        Ok(())
    }

    #[test]
    fn dialoguer_error_wrapped() {
        let err = dialoguer::Error::IO(io::Error::other("oops"));
        let wrapped = dialoguer_to_io(err);
        assert_eq!(wrapped.kind(), io::ErrorKind::Other);
    }

    #[test]
    fn ensure_tool_bases_creates_parents() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let mut cfg = make_config(&tmp);
        fs::create_dir_all(&cfg.claude_dir)?;
        fs::create_dir_all(&cfg.claude_skills_dir)?;
        fs::create_dir_all(&cfg.cursor_dir)?;
        fs::create_dir_all(&cfg.codex_dir)?;
        fs::create_dir_all(&cfg.codex_skills_dir)?;
        fs::create_dir_all(&cfg.opencode_commands_dir)?;
        fs::create_dir_all(&cfg.opencode_skills_dir)?;
        fs::create_dir_all(cfg.codex_rules_file.parent().unwrap())?;
        fs::create_dir_all(cfg.codex_agents_file.parent().unwrap())?;
        fs::create_dir_all(cfg.opencode_agents_file.parent().unwrap())?;

        ensure_tool_bases(&cfg)?;

        cfg.codex_rules_file = tmp.path().join("missing/default.rules");
        ensure_tool_bases(&cfg)?;
        Ok(())
    }

    #[test]
    fn print_notes_paths() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let cfg = make_config(&tmp);
        print_selected_undetected_note(&cfg, &[TOOL_CLAUDE.to_string()]);
        print_tool_detection_note(&cfg);
        fs::create_dir_all(&cfg.claude_dir)?;
        print_selected_undetected_note(&cfg, &[TOOL_CLAUDE.to_string()]);
        print_tool_detection_note(&cfg);
        Ok(())
    }

    #[test]
    fn print_notes_for_all_tools() {
        let tmp = TempDir::new().expect("tmp");
        let cfg = make_config(&tmp);
        print_selected_undetected_note(
            &cfg,
            &[
                TOOL_CODEX.to_string(),
                TOOL_OPENCODE.to_string(),
                TOOL_CURSOR.to_string(),
                "unknown".to_string(),
            ],
        );
    }

    #[test]
    fn print_tool_detection_note_empty() {
        let tmp = TempDir::new().expect("tmp");
        let mut cfg = make_config(&tmp);
        cfg.enabled_tools.clear();
        print_tool_detection_note(&cfg);
    }
}
