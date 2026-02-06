use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const TOOL_CLAUDE: &str = "claude";
pub const TOOL_CODEX: &str = "codex";
pub const TOOL_CURSOR: &str = "cursor";
pub const TOOL_OPENCODE: &str = "opencode";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub enabled_tools: Vec<String>,
    pub verified_versions: HashMap<String, String>,
    pub central_dir: PathBuf,
    pub central_skills_dir: PathBuf,
    pub central_agents_dir: PathBuf,
    pub central_rules_dir: PathBuf,
    pub claude_dir: PathBuf,
    pub claude_skills_dir: PathBuf,
    pub cursor_dir: PathBuf,
    pub opencode_commands_dir: PathBuf,
    pub opencode_skills_dir: PathBuf,
    pub opencode_agents_file: PathBuf,
    pub codex_dir: PathBuf,
    pub codex_skills_dir: PathBuf,
    pub codex_rules_file: PathBuf,
    pub codex_agents_file: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PartialConfig {
    pub enabled_tools: Option<Vec<String>>,
    pub verified_versions: Option<HashMap<String, String>>,
    pub central_dir: Option<PathBuf>,
    pub central_skills_dir: Option<PathBuf>,
    pub central_agents_dir: Option<PathBuf>,
    pub central_rules_dir: Option<PathBuf>,
    pub claude_dir: Option<PathBuf>,
    pub claude_skills_dir: Option<PathBuf>,
    pub cursor_dir: Option<PathBuf>,
    pub opencode_commands_dir: Option<PathBuf>,
    pub opencode_dir: Option<PathBuf>,
    pub opencode_skills_dir: Option<PathBuf>,
    pub opencode_agents_file: Option<PathBuf>,
    pub codex_dir: Option<PathBuf>,
    pub codex_skills_dir: Option<PathBuf>,
    pub codex_rules_file: Option<PathBuf>,
    pub codex_agents_file: Option<PathBuf>,
}

impl Config {
    pub fn default_paths() -> io::Result<Self> {
        let home = resolve_home_dir().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "could not resolve home directory")
        })?;
        let codex_root = resolve_tool_home(&home, "CODEX_HOME", ".codex");
        let claude_root = resolve_tool_home(&home, "CLAUDE_HOME", ".claude");
        let cursor_root = resolve_tool_home(&home, "CURSOR_HOME", ".cursor");
        let opencode_root = resolve_tool_home(&home, "OPENCODE_HOME", ".config/opencode");
        Ok(Self {
            enabled_tools: vec![
                TOOL_CLAUDE.to_string(),
                TOOL_CODEX.to_string(),
                TOOL_CURSOR.to_string(),
                TOOL_OPENCODE.to_string(),
            ],
            verified_versions: HashMap::new(),
            central_dir: home.join(".config/relay/commands"),
            central_skills_dir: home.join(".config/relay/skills"),
            central_agents_dir: home.join(".config/relay/agents"),
            central_rules_dir: home.join(".config/relay/rules"),
            claude_dir: claude_root.join("commands"),
            claude_skills_dir: claude_root.join("skills"),
            cursor_dir: cursor_root.join("commands"),
            opencode_commands_dir: opencode_root.join("command"),
            opencode_skills_dir: opencode_root.join("skill"),
            opencode_agents_file: opencode_root.join("AGENTS.md"),
            codex_dir: codex_root.join("prompts"),
            codex_skills_dir: codex_root.join("skills"),
            codex_rules_file: codex_root.join("rules/default.rules"),
            codex_agents_file: codex_root.join("AGENTS.md"),
        })
    }

    pub fn config_path() -> io::Result<PathBuf> {
        if let Some(home) = resolve_home_dir() {
            return Ok(home.join(".config/relay/config.toml"));
        }
        let config_dir = resolve_config_dir().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "could not resolve config directory",
            )
        })?;
        Ok(config_dir.join("relay/config.toml"))
    }

    fn legacy_config_path() -> io::Result<PathBuf> {
        let config_dir = resolve_config_dir().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "could not resolve config directory",
            )
        })?;
        Ok(config_dir.join("relay/config.toml"))
    }

    pub fn load_or_default() -> io::Result<Self> {
        let path = Self::config_path()?;
        let relay_home_explicit = env::var("RELAY_HOME")
            .ok()
            .map(|value| !value.is_empty())
            .unwrap_or(false);
        let read_path = if path.exists() {
            Some(path)
        } else {
            if relay_home_explicit {
                None
            } else {
                match Self::legacy_config_path() {
                    Ok(legacy) if legacy.exists() => Some(legacy),
                    _ => None,
                }
            }
        };
        if let Some(path) = read_path {
            let raw = fs::read_to_string(&path)?;
            let cfg: PartialConfig = toml::from_str(&raw)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
            let defaults = Self::default_paths()?;
            let legacy_command = cfg.opencode_dir.as_ref().and_then(|path| {
                match path.file_name().and_then(|name| name.to_str()) {
                    Some("command") | Some("commands") => Some(path.clone()),
                    _ => None,
                }
            });
            let legacy_skill = cfg.opencode_dir.as_ref().and_then(|path| {
                match path.file_name().and_then(|name| name.to_str()) {
                    Some("skill") | Some("skills") => Some(path.clone()),
                    _ => None,
                }
            });
            Ok(Self {
                enabled_tools: normalize_tools(
                    cfg.enabled_tools
                        .unwrap_or_else(|| defaults.enabled_tools.clone()),
                ),
                verified_versions: normalize_versions(
                    cfg.verified_versions
                        .unwrap_or_else(|| defaults.verified_versions.clone()),
                ),
                central_dir: cfg.central_dir.unwrap_or(defaults.central_dir),
                central_skills_dir: cfg
                    .central_skills_dir
                    .unwrap_or(defaults.central_skills_dir),
                central_agents_dir: cfg
                    .central_agents_dir
                    .unwrap_or(defaults.central_agents_dir),
                central_rules_dir: cfg.central_rules_dir.unwrap_or(defaults.central_rules_dir),
                claude_dir: cfg.claude_dir.unwrap_or(defaults.claude_dir),
                claude_skills_dir: cfg.claude_skills_dir.unwrap_or(defaults.claude_skills_dir),
                cursor_dir: cfg.cursor_dir.unwrap_or(defaults.cursor_dir),
                opencode_commands_dir: cfg
                    .opencode_commands_dir
                    .or(legacy_command)
                    .unwrap_or(defaults.opencode_commands_dir),
                opencode_skills_dir: cfg
                    .opencode_skills_dir
                    .or(legacy_skill)
                    .unwrap_or(defaults.opencode_skills_dir),
                opencode_agents_file: cfg
                    .opencode_agents_file
                    .unwrap_or(defaults.opencode_agents_file),
                codex_dir: cfg.codex_dir.unwrap_or(defaults.codex_dir),
                codex_skills_dir: cfg.codex_skills_dir.unwrap_or(defaults.codex_skills_dir),
                codex_rules_file: cfg.codex_rules_file.unwrap_or(defaults.codex_rules_file),
                codex_agents_file: cfg.codex_agents_file.unwrap_or(defaults.codex_agents_file),
            })
        } else {
            Self::default_paths()
        }
    }

    #[inline(never)]
    pub fn save(&self, path: &Path) -> io::Result<()> {
        path.parent().map(fs::create_dir_all).transpose()?;
        let serialized = toml::to_string_pretty(self).map_err(serialize_error)?;
        fs::write(path, serialized)
    }
}

fn serialize_error(err: toml::ser::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, err)
}

impl Config {
    pub fn tool_enabled(&self, tool: &str) -> bool {
        self.enabled_tools.iter().any(|name| name == tool)
    }

    pub fn verified_version(&self, tool: &str) -> Option<&str> {
        self.verified_versions
            .get(&tool.to_ascii_lowercase())
            .map(String::as_str)
    }
}

fn normalize_tools(mut tools: Vec<String>) -> Vec<String> {
    for tool in tools.iter_mut() {
        *tool = tool.to_ascii_lowercase();
    }
    tools.sort();
    tools.dedup();
    tools
}

fn normalize_versions(versions: HashMap<String, String>) -> HashMap<String, String> {
    let mut normalized = HashMap::new();
    for (key, value) in versions {
        normalized.insert(key.to_ascii_lowercase(), value);
    }
    normalized
}

pub fn expand_tilde(input: &str) -> io::Result<PathBuf> {
    if let Some(stripped) = input.strip_prefix("~/") {
        let home = resolve_home_dir().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "could not resolve home directory")
        })?;
        Ok(home.join(stripped))
    } else {
        Ok(PathBuf::from(input))
    }
}

fn resolve_tool_home(home: &Path, var: &str, default_suffix: &str) -> PathBuf {
    let value = env::var(var)
        .ok()
        .map(|raw| raw.trim().to_string())
        .filter(|raw| !raw.is_empty());
    match value.as_deref() {
        Some("~") => home.to_path_buf(),
        Some(raw) => {
            if let Some(stripped) = raw.strip_prefix("~/") {
                home.join(stripped)
            } else {
                PathBuf::from(raw)
            }
        }
        None => home.join(default_suffix),
    }
}

pub(crate) fn resolve_home_dir() -> Option<PathBuf> {
    if env::var("RELAY_NO_HOME").ok().as_deref() == Some("1") {
        return None;
    }
    if let Ok(path) = env::var("RELAY_HOME") {
        if path.is_empty() {
            return None;
        }
        return Some(PathBuf::from(path));
    }
    dirs::home_dir()
}

fn resolve_config_dir() -> Option<PathBuf> {
    if env::var("RELAY_NO_CONFIG").ok().as_deref() == Some("1") {
        return None;
    }
    if let Ok(path) = env::var("RELAY_CONFIG_DIR") {
        if path.is_empty() {
            return None;
        }
        return Some(PathBuf::from(path));
    }
    dirs::config_dir()
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn default_paths_respects_env_overrides() -> io::Result<()> {
        let _lock = env_lock();
        let tmp = TempDir::new()?;
        let home = tmp.path().join("home");
        fs::create_dir_all(&home)?;
        set_env("RELAY_HOME", Some(home.to_string_lossy().as_ref()));
        set_env("CODEX_HOME", Some("~/codex_root"));
        set_env("CLAUDE_HOME", Some("~/claude_root"));
        set_env("OPENCODE_HOME", Some("~/opencode_root"));
        set_env("CURSOR_HOME", Some("/tmp/cursor_root"));

        let cfg = Config::default_paths()?;
        assert_eq!(cfg.central_dir, home.join(".config/relay/commands"));
        assert_eq!(cfg.codex_dir, home.join("codex_root/prompts"));
        assert_eq!(cfg.codex_skills_dir, home.join("codex_root/skills"));
        assert_eq!(cfg.claude_dir, home.join("claude_root/commands"));
        assert_eq!(cfg.claude_skills_dir, home.join("claude_root/skills"));
        assert_eq!(
            cfg.opencode_commands_dir,
            home.join("opencode_root/command")
        );
        assert_eq!(cfg.opencode_skills_dir, home.join("opencode_root/skill"));
        assert_eq!(
            cfg.opencode_agents_file,
            home.join("opencode_root/AGENTS.md")
        );
        assert_eq!(cfg.cursor_dir, PathBuf::from("/tmp/cursor_root/commands"));

        set_env("RELAY_HOME", None);
        set_env("CODEX_HOME", None);
        set_env("CLAUDE_HOME", None);
        set_env("OPENCODE_HOME", None);
        set_env("CURSOR_HOME", None);
        Ok(())
    }

    #[test]
    fn config_path_uses_home_or_config_dir() -> io::Result<()> {
        let _lock = env_lock();
        let tmp = TempDir::new()?;
        let home = tmp.path().join("home");
        fs::create_dir_all(&home)?;
        set_env("RELAY_HOME", Some(home.to_string_lossy().as_ref()));
        let config_path = Config::config_path()?;
        assert_eq!(config_path, home.join(".config/relay/config.toml"));

        set_env("RELAY_HOME", None);
        set_env("RELAY_NO_HOME", Some("1"));
        set_env(
            "RELAY_CONFIG_DIR",
            Some(tmp.path().to_string_lossy().as_ref()),
        );
        let config_path = Config::config_path()?;
        assert_eq!(config_path, tmp.path().join("relay/config.toml"));

        set_env("RELAY_CONFIG_DIR", None);
        set_env("RELAY_NO_HOME", None);
        Ok(())
    }

    #[test]
    fn config_path_errors_when_missing_dirs() {
        let _lock = env_lock();
        set_env("RELAY_NO_HOME", Some("1"));
        set_env("RELAY_NO_CONFIG", Some("1"));
        let err = Config::config_path().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        set_env("RELAY_NO_HOME", None);
        set_env("RELAY_NO_CONFIG", None);
    }

    #[test]
    fn legacy_config_path_errors_when_config_missing() {
        let _lock = env_lock();
        set_env("RELAY_NO_CONFIG", Some("1"));
        let err = Config::legacy_config_path().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        set_env("RELAY_NO_CONFIG", None);
    }

    #[test]
    fn load_or_default_reads_and_merges_config() -> io::Result<()> {
        let _lock = env_lock();
        let tmp = TempDir::new()?;
        let home = tmp.path().join("home");
        fs::create_dir_all(home.join(".config/relay"))?;
        set_env("RELAY_HOME", Some(home.to_string_lossy().as_ref()));

        let config_path = Config::config_path()?;
        let config_body = r#"
enabled_tools = ["Codex", "Claude"]
central_dir = "/tmp/relay/commands"
opencode_dir = "/legacy/opencode/command"
"#;
        fs::write(&config_path, config_body)?;

        let cfg = Config::load_or_default()?;
        assert!(cfg.tool_enabled("codex"));
        assert!(cfg.tool_enabled("claude"));
        assert_eq!(cfg.central_dir, PathBuf::from("/tmp/relay/commands"));
        assert_eq!(
            cfg.opencode_commands_dir,
            PathBuf::from("/legacy/opencode/command")
        );
        set_env("RELAY_HOME", None);
        Ok(())
    }

    #[test]
    fn load_or_default_errors_on_invalid_toml() -> io::Result<()> {
        let _lock = env_lock();
        let tmp = TempDir::new()?;
        let home = tmp.path().join("home");
        fs::create_dir_all(home.join(".config/relay"))?;
        set_env("RELAY_HOME", Some(home.to_string_lossy().as_ref()));
        let config_path = Config::config_path()?;
        fs::write(&config_path, "enabled_tools = [")?;
        let err = Config::load_or_default().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        set_env("RELAY_HOME", None);
        Ok(())
    }

    #[test]
    fn expand_tilde_uses_relay_home() -> io::Result<()> {
        let _lock = env_lock();
        let tmp = TempDir::new()?;
        let home = tmp.path().join("home");
        fs::create_dir_all(&home)?;
        set_env("RELAY_HOME", Some(home.to_string_lossy().as_ref()));
        let expanded = expand_tilde("~/test")?;
        assert_eq!(expanded, home.join("test"));
        set_env("RELAY_HOME", None);
        Ok(())
    }

    #[test]
    fn normalize_tools_and_versions() {
        let tools = normalize_tools(vec!["Codex".into(), "claude".into(), "CODEX".into()]);
        assert_eq!(tools, vec!["claude".to_string(), "codex".to_string()]);
        let mut versions = HashMap::new();
        versions.insert("CoDeX".to_string(), "1.2.3".to_string());
        let normalized = normalize_versions(versions);
        assert_eq!(normalized.get("codex"), Some(&"1.2.3".to_string()));
    }

    #[test]
    fn expand_tilde_errors_without_home() {
        let _lock = env_lock();
        set_env("RELAY_NO_HOME", Some("1"));
        let err = expand_tilde("~/missing").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        set_env("RELAY_NO_HOME", None);
    }

    #[test]
    fn default_paths_error_without_home() {
        let _lock = env_lock();
        set_env("RELAY_NO_HOME", Some("1"));
        let err = Config::default_paths().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        set_env("RELAY_NO_HOME", None);
    }

    #[test]
    fn default_paths_uses_absolute_codex_home() -> io::Result<()> {
        let _lock = env_lock();
        let tmp = TempDir::new()?;
        let home = tmp.path().join("home");
        fs::create_dir_all(&home)?;
        set_env("RELAY_HOME", Some(home.to_string_lossy().as_ref()));
        set_env("CODEX_HOME", Some("/tmp/codex"));

        let cfg = Config::default_paths()?;
        assert_eq!(cfg.codex_dir, PathBuf::from("/tmp/codex/prompts"));

        set_env("RELAY_HOME", None);
        set_env("CODEX_HOME", None);
        Ok(())
    }

    #[test]
    fn config_path_uses_config_dir_when_home_empty() -> io::Result<()> {
        let _lock = env_lock();
        let tmp = TempDir::new()?;
        set_env("RELAY_HOME", Some(""));
        set_env(
            "RELAY_CONFIG_DIR",
            Some(tmp.path().to_string_lossy().as_ref()),
        );
        let path = Config::config_path()?;
        assert_eq!(path, tmp.path().join("relay/config.toml"));
        set_env("RELAY_HOME", None);
        set_env("RELAY_CONFIG_DIR", None);
        Ok(())
    }

    #[test]
    fn config_path_errors_when_config_dir_empty() {
        let _lock = env_lock();
        set_env("RELAY_NO_HOME", Some("1"));
        set_env("RELAY_CONFIG_DIR", Some(""));
        let err = Config::config_path().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        set_env("RELAY_NO_HOME", None);
        set_env("RELAY_CONFIG_DIR", None);
    }

    #[test]
    fn load_or_default_ignores_legacy_when_relay_home_is_explicit() -> io::Result<()> {
        let _lock = env_lock();
        let tmp = TempDir::new()?;
        let home = tmp.path().join("home");
        let legacy = tmp.path().join("legacy");
        fs::create_dir_all(home.join(".config/relay"))?;
        fs::create_dir_all(legacy.join("relay"))?;
        set_env("RELAY_HOME", Some(home.to_string_lossy().as_ref()));
        set_env("RELAY_CONFIG_DIR", Some(legacy.to_string_lossy().as_ref()));

        let legacy_path = legacy.join("relay/config.toml");
        fs::write(&legacy_path, "enabled_tools = [\"claude\"]")?;

        let cfg = Config::load_or_default()?;
        assert!(cfg.tool_enabled("codex"));
        assert_eq!(cfg.central_dir, home.join(".config/relay/commands"));

        set_env("RELAY_HOME", None);
        set_env("RELAY_CONFIG_DIR", None);
        Ok(())
    }

    #[test]
    fn load_or_default_defaults_when_missing_config() -> io::Result<()> {
        let _lock = env_lock();
        let tmp = TempDir::new()?;
        let home = tmp.path().join("home");
        fs::create_dir_all(&home)?;
        set_env("RELAY_HOME", Some(home.to_string_lossy().as_ref()));
        let config_dir = tmp.path().join("config");
        fs::create_dir_all(&config_dir)?;
        set_env(
            "RELAY_CONFIG_DIR",
            Some(config_dir.to_string_lossy().as_ref()),
        );

        let cfg = Config::load_or_default()?;
        assert!(cfg.central_dir.ends_with(".config/relay/commands"));

        set_env("RELAY_HOME", None);
        set_env("RELAY_CONFIG_DIR", None);
        Ok(())
    }

    #[test]
    fn load_or_default_ignores_unrecognized_opencode_dir() -> io::Result<()> {
        let _lock = env_lock();
        let tmp = TempDir::new()?;
        let home = tmp.path().join("home");
        fs::create_dir_all(home.join(".config/relay"))?;
        set_env("RELAY_HOME", Some(home.to_string_lossy().as_ref()));

        let config_path = Config::config_path()?;
        let config_body = r#"
opencode_dir = "/tmp/opencode/other"
"#;
        fs::write(&config_path, config_body)?;

        let cfg = Config::load_or_default()?;
        assert_ne!(
            cfg.opencode_commands_dir,
            PathBuf::from("/tmp/opencode/other")
        );

        set_env("RELAY_HOME", None);
        Ok(())
    }

    #[test]
    fn save_writes_config() -> io::Result<()> {
        let _lock = env_lock();
        let tmp = TempDir::new()?;
        let home = tmp.path().join("home");
        fs::create_dir_all(&home)?;
        set_env("RELAY_HOME", Some(home.to_string_lossy().as_ref()));
        let cfg = Config::default_paths()?;
        let path = tmp.path().join("relay/config.toml");
        cfg.save(&path)?;
        let contents = fs::read_to_string(path)?;
        assert!(contents.contains("central_dir"));
        set_env("RELAY_HOME", None);
        Ok(())
    }

    #[test]
    fn serialize_error_maps_to_invalid_data() {
        use serde::ser::Error as SerError;
        let err = toml::ser::Error::custom("boom");
        let io_err = serialize_error(err);
        assert_eq!(io_err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn expand_tilde_no_prefix() -> io::Result<()> {
        let _lock = env_lock();
        let path = expand_tilde("/tmp/plain")?;
        assert_eq!(path, PathBuf::from("/tmp/plain"));
        Ok(())
    }

    #[test]
    fn resolve_home_dir_falls_back_to_dirs() {
        let _lock = env_lock();
        set_env("RELAY_HOME", None);
        set_env("RELAY_NO_HOME", None);
        let resolved = resolve_home_dir();
        assert_eq!(resolved, dirs::home_dir());
    }

    #[test]
    fn resolve_config_dir_falls_back_to_dirs() {
        let _lock = env_lock();
        set_env("RELAY_CONFIG_DIR", None);
        set_env("RELAY_NO_CONFIG", None);
        let resolved = resolve_config_dir();
        assert_eq!(resolved, dirs::config_dir());
    }
}
