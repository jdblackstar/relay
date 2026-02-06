use crate::config::resolve_home_dir;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
struct LoggerConfig {
    path: PathBuf,
}

static LOGGER: OnceLock<LoggerConfig> = OnceLock::new();

#[cfg_attr(any(test, coverage), allow(dead_code))]
pub fn init(debug_flag: bool, cli_path: Option<&Path>) {
    let env_debug = std::env::var("RELAY_DEBUG")
        .ok()
        .as_deref()
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
    if !debug_flag && !env_debug {
        return;
    }
    let path = resolve_log_path(cli_path);
    let _ = LOGGER.set(LoggerConfig { path: path.clone() });
    debug(&format!("debug logging enabled path={}", path.display()));
}

pub fn debug(message: &str) {
    let Some(config) = LOGGER.get() else {
        return;
    };
    let path = &config.path;
    if path.parent().map(fs::create_dir_all).transpose().is_err() {
        return;
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{timestamp} {message}");
    }
}

fn resolve_log_path(cli_path: Option<&Path>) -> PathBuf {
    if let Some(path) = cli_path {
        return path.to_path_buf();
    }
    if let Ok(raw) = std::env::var("RELAY_LOG_FILE") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    if let Some(home) = resolve_home_dir() {
        return home.join(".config/relay/logs/relay-debug.log");
    }
    std::env::temp_dir().join("relay-debug.log")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_log_path_prefers_cli() {
        let cli = PathBuf::from("/tmp/relay.log");
        let resolved = resolve_log_path(Some(&cli));
        assert_eq!(resolved, cli);
    }
}
