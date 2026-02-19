#![cfg_attr(any(test, coverage), allow(dead_code))]

use crate::config::Config;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const LAUNCHD_LABEL: &str = "dev.jdblackstar.relay.watch";
const SYSTEMD_UNIT_NAME: &str = "relay-watch.service";
const WATCH_LOG_FILE: &str = "watch.log";
const SERVICE_ENV_KEYS: [&str; 6] = [
    "RELAY_HOME",
    "CODEX_HOME",
    "CLAUDE_HOME",
    "OPENCODE_HOME",
    "CURSOR_HOME",
    "RELAY_CONFIG_DIR",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceManager {
    Launchd,
    SystemdUser,
}

impl ServiceManager {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Launchd => "launchd",
            Self::SystemdUser => "systemd-user",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    NotInstalled,
    Stopped,
    Running,
}

impl ServiceState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotInstalled => "not-installed",
            Self::Stopped => "stopped",
            Self::Running => "running",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ServicePaths {
    pub service_file: PathBuf,
    pub log_file: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ServiceStatus {
    pub manager: ServiceManager,
    pub service_name: &'static str,
    pub state: ServiceState,
    pub paths: ServicePaths,
    pub logs_hint: Option<String>,
}

#[derive(Debug, Clone)]
pub struct InstallWatchServiceOptions {
    pub debounce_ms: u64,
    pub quiet: bool,
    pub debug: bool,
    pub debug_log_file: Option<PathBuf>,
}

pub fn install_watch_service(cfg: &Config, options: &InstallWatchServiceOptions) -> io::Result<()> {
    let manager = service_manager()?;
    let paths = service_paths(cfg, manager)?;
    let relay_bin = env::current_exe()?;
    let args = watch_args(options);
    let service_env = service_env_vars();

    if let Some(parent) = paths.service_file.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Some(log_file) = paths.log_file.as_ref() {
        if let Some(parent) = log_file.parent() {
            fs::create_dir_all(parent)?;
        }
    }

    let body = match manager {
        ServiceManager::Launchd => render_launchd_plist(
            &relay_bin,
            &args,
            paths.log_file.as_ref().expect("launchd log file path"),
            &service_env,
        ),
        ServiceManager::SystemdUser => render_systemd_unit(&relay_bin, &args, &service_env),
    };
    write_atomic(&paths.service_file, body.as_bytes())?;

    if manager == ServiceManager::SystemdUser {
        systemd_daemon_reload()?;
    }

    Ok(())
}

pub fn start_watch_service(cfg: &Config) -> io::Result<()> {
    let manager = service_manager()?;
    let paths = service_paths(cfg, manager)?;
    if !paths.service_file.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "service file not found at {} (run `relay daemon install` first)",
                paths.service_file.display()
            ),
        ));
    }

    match manager {
        ServiceManager::Launchd => launchd_start(&paths),
        ServiceManager::SystemdUser => {
            systemd_daemon_reload()?;
            run_checked(
                Command::new("systemctl").args(["--user", "enable", SYSTEMD_UNIT_NAME]),
                "enable systemd service",
            )?;
            run_checked(
                Command::new("systemctl").args(["--user", "restart", SYSTEMD_UNIT_NAME]),
                "start systemd service",
            )?;
            Ok(())
        }
    }
}

pub fn stop_watch_service(cfg: &Config) -> io::Result<()> {
    let manager = service_manager()?;
    let paths = service_paths(cfg, manager)?;
    if !paths.service_file.exists() {
        return Ok(());
    }
    match manager {
        ServiceManager::Launchd => launchd_stop(),
        ServiceManager::SystemdUser => {
            let stop =
                run_capture(Command::new("systemctl").args(["--user", "stop", SYSTEMD_UNIT_NAME]))?;
            if !stop.status.success() && !looks_like_not_running(&combined_output(&stop)) {
                return Err(command_failed("stop systemd service", stop));
            }
            let disable = run_capture(Command::new("systemctl").args([
                "--user",
                "disable",
                SYSTEMD_UNIT_NAME,
            ]))?;
            if !disable.status.success() && !looks_like_not_running(&combined_output(&disable)) {
                return Err(command_failed("disable systemd service", disable));
            }
            Ok(())
        }
    }
}

pub fn restart_watch_service(cfg: &Config) -> io::Result<()> {
    let status = watch_service_status(cfg)?;
    if status.state == ServiceState::Running {
        stop_watch_service(cfg)?;
    }
    start_watch_service(cfg)
}

pub fn uninstall_watch_service(cfg: &Config) -> io::Result<()> {
    let manager = service_manager()?;
    let paths = service_paths(cfg, manager)?;

    let _ = stop_watch_service(cfg);
    if paths.service_file.exists() {
        fs::remove_file(&paths.service_file)?;
    }
    if manager == ServiceManager::SystemdUser {
        let _ = systemd_daemon_reload();
    }
    Ok(())
}

pub fn watch_service_status(cfg: &Config) -> io::Result<ServiceStatus> {
    let manager = service_manager()?;
    let paths = service_paths(cfg, manager)?;
    let state = match manager {
        ServiceManager::Launchd => launchd_status(&paths)?,
        ServiceManager::SystemdUser => systemd_status(&paths)?,
    };

    let logs_hint = match manager {
        ServiceManager::Launchd => paths
            .log_file
            .as_ref()
            .map(|path| format!("tail -f {}", path.display())),
        ServiceManager::SystemdUser => Some(format!("journalctl --user -u {SYSTEMD_UNIT_NAME} -f")),
    };

    Ok(ServiceStatus {
        manager,
        service_name: service_name(manager),
        state,
        paths,
        logs_hint,
    })
}

fn service_manager() -> io::Result<ServiceManager> {
    if cfg!(target_os = "macos") {
        return Ok(ServiceManager::Launchd);
    }
    if cfg!(target_os = "linux") {
        return Ok(ServiceManager::SystemdUser);
    }
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native daemon management is only supported on macOS and Linux",
    ))
}

fn service_name(manager: ServiceManager) -> &'static str {
    match manager {
        ServiceManager::Launchd => LAUNCHD_LABEL,
        ServiceManager::SystemdUser => SYSTEMD_UNIT_NAME,
    }
}

fn service_paths(cfg: &Config, manager: ServiceManager) -> io::Result<ServicePaths> {
    let service_file = match manager {
        ServiceManager::Launchd => launch_agents_dir()?.join(format!("{LAUNCHD_LABEL}.plist")),
        ServiceManager::SystemdUser => systemd_user_dir()?.join(SYSTEMD_UNIT_NAME),
    };
    let log_file = match manager {
        ServiceManager::Launchd => Some(runtime_dir(cfg)?.join(WATCH_LOG_FILE)),
        ServiceManager::SystemdUser => None,
    };
    Ok(ServicePaths {
        service_file,
        log_file,
    })
}

fn runtime_dir(cfg: &Config) -> io::Result<PathBuf> {
    cfg.central_dir
        .parent()
        .map(|path| path.join("runtime"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "central_dir has no parent"))
}

fn launch_agents_dir() -> io::Result<PathBuf> {
    let home = os_home_dir()?;
    Ok(home.join("Library/LaunchAgents"))
}

fn systemd_user_dir() -> io::Result<PathBuf> {
    if let Ok(path) = env::var("XDG_CONFIG_HOME") {
        if !path.is_empty() {
            return Ok(PathBuf::from(path).join("systemd/user"));
        }
    }
    let home = os_home_dir()?;
    Ok(home.join(".config/systemd/user"))
}

fn os_home_dir() -> io::Result<PathBuf> {
    dirs::home_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "could not resolve home directory"))
}

fn service_env_vars() -> BTreeMap<String, String> {
    let mut vars = BTreeMap::new();
    for key in SERVICE_ENV_KEYS {
        if let Ok(value) = env::var(key) {
            if !value.trim().is_empty() {
                vars.insert(key.to_string(), value);
            }
        }
    }
    vars
}

fn watch_args(options: &InstallWatchServiceOptions) -> Vec<String> {
    let mut args = vec![
        "watch".to_string(),
        "--debounce-ms".to_string(),
        options.debounce_ms.to_string(),
    ];
    if options.quiet {
        args.push("--quiet".to_string());
    }
    if options.debug {
        args.push("--debug".to_string());
    }
    if let Some(path) = options.debug_log_file.as_ref() {
        args.push("--debug-log-file".to_string());
        args.push(path.display().to_string());
    }
    args
}

fn render_launchd_plist(
    relay_bin: &Path,
    args: &[String],
    log_file: &Path,
    service_env: &BTreeMap<String, String>,
) -> String {
    let mut program_args = vec![relay_bin.display().to_string()];
    program_args.extend(args.iter().cloned());

    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n");
    out.push_str("<plist version=\"1.0\">\n");
    out.push_str("<dict>\n");
    out.push_str("  <key>Label</key>\n");
    out.push_str(&format!(
        "  <string>{}</string>\n",
        escape_xml(LAUNCHD_LABEL)
    ));
    out.push_str("  <key>ProgramArguments</key>\n");
    out.push_str("  <array>\n");
    for arg in program_args {
        out.push_str(&format!("    <string>{}</string>\n", escape_xml(&arg)));
    }
    out.push_str("  </array>\n");
    out.push_str("  <key>RunAtLoad</key>\n");
    out.push_str("  <true/>\n");
    out.push_str("  <key>KeepAlive</key>\n");
    out.push_str("  <true/>\n");
    out.push_str("  <key>StandardOutPath</key>\n");
    out.push_str(&format!(
        "  <string>{}</string>\n",
        escape_xml(&log_file.display().to_string())
    ));
    out.push_str("  <key>StandardErrorPath</key>\n");
    out.push_str(&format!(
        "  <string>{}</string>\n",
        escape_xml(&log_file.display().to_string())
    ));
    if !service_env.is_empty() {
        out.push_str("  <key>EnvironmentVariables</key>\n");
        out.push_str("  <dict>\n");
        for (key, value) in service_env {
            out.push_str(&format!("    <key>{}</key>\n", escape_xml(key)));
            out.push_str(&format!("    <string>{}</string>\n", escape_xml(value)));
        }
        out.push_str("  </dict>\n");
    }
    out.push_str("</dict>\n");
    out.push_str("</plist>\n");
    out
}

fn render_systemd_unit(
    relay_bin: &Path,
    args: &[String],
    service_env: &BTreeMap<String, String>,
) -> String {
    let mut exec_parts = vec![relay_bin.display().to_string()];
    exec_parts.extend(args.iter().cloned());
    let exec_start = exec_parts
        .into_iter()
        .map(|arg| format!("\"{}\"", escape_systemd(&arg)))
        .collect::<Vec<_>>()
        .join(" ");

    let mut out = String::new();
    out.push_str("[Unit]\n");
    out.push_str("Description=relay watch sync service\n");
    out.push_str("After=default.target\n\n");
    out.push_str("[Service]\n");
    out.push_str("Type=simple\n");
    out.push_str(&format!("ExecStart={exec_start}\n"));
    out.push_str("Restart=always\n");
    out.push_str("RestartSec=1\n");
    for (key, value) in service_env {
        out.push_str(&format!(
            "Environment=\"{}\"\n",
            escape_systemd(&format!("{key}={value}"))
        ));
    }
    out.push_str("\n[Install]\n");
    out.push_str("WantedBy=default.target\n");
    out
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn escape_systemd(value: &str) -> String {
    value
        .replace('%', "%%")
        .replace('$', "$$")
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

fn write_atomic(path: &Path, body: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, body)?;
    fs::rename(tmp, path)
}

fn systemd_daemon_reload() -> io::Result<()> {
    run_checked(
        Command::new("systemctl").args(["--user", "daemon-reload"]),
        "reload systemd user daemon",
    )
    .map(|_| ())
}

fn launchd_start(paths: &ServicePaths) -> io::Result<()> {
    let target = launchd_target();
    let status = run_capture(Command::new("launchctl").args(["print", &target]))?;
    if status.status.success() {
        let out = run_capture(Command::new("launchctl").args(["bootout", &target]))?;
        if !out.status.success() && !looks_like_not_running(&combined_output(&out)) {
            return Err(command_failed("stop launchd service", out));
        }
    }

    let domain = launchd_domain();
    run_checked(
        Command::new("launchctl")
            .arg("bootstrap")
            .arg(domain)
            .arg(&paths.service_file),
        "start launchd service",
    )
    .map(|_| ())
}

fn launchd_stop() -> io::Result<()> {
    let target = launchd_target();
    let out = run_capture(Command::new("launchctl").args(["bootout", &target]))?;
    if out.status.success() || looks_like_not_running(&combined_output(&out)) {
        return Ok(());
    }
    Err(command_failed("stop launchd service", out))
}

fn launchd_status(paths: &ServicePaths) -> io::Result<ServiceState> {
    if !paths.service_file.exists() {
        return Ok(ServiceState::NotInstalled);
    }
    let out = run_capture(Command::new("launchctl").args(["print", &launchd_target()]))?;
    if !out.status.success() {
        return Ok(ServiceState::Stopped);
    }
    let text = combined_output(&out);
    if text.contains("state = running") {
        Ok(ServiceState::Running)
    } else {
        Ok(ServiceState::Stopped)
    }
}

fn systemd_status(paths: &ServicePaths) -> io::Result<ServiceState> {
    if !paths.service_file.exists() {
        return Ok(ServiceState::NotInstalled);
    }
    let out =
        run_capture(Command::new("systemctl").args(["--user", "is-active", SYSTEMD_UNIT_NAME]))?;
    if out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "active" {
        Ok(ServiceState::Running)
    } else {
        Ok(ServiceState::Stopped)
    }
}

fn launchd_domain() -> String {
    format!("gui/{}", unsafe { libc::geteuid() })
}

fn launchd_target() -> String {
    format!("{}/{}", launchd_domain(), LAUNCHD_LABEL)
}

fn looks_like_not_running(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    lower.contains("not loaded")
        || lower.contains("not running")
        || lower.contains("could not find service")
        || lower.contains("no such process")
        || lower.contains("not found")
}

fn run_capture(command: &mut Command) -> io::Result<Output> {
    command.output().map_err(|err| {
        io::Error::new(
            err.kind(),
            format!("failed to execute `{command:?}`: {err}"),
        )
    })
}

fn run_checked(command: &mut Command, action: &str) -> io::Result<Output> {
    let out = run_capture(command)?;
    if out.status.success() {
        Ok(out)
    } else {
        Err(command_failed(action, out))
    }
}

fn command_failed(action: &str, output: Output) -> io::Error {
    io::Error::other(format!(
        "{action} failed: {}",
        combined_output(&output).trim()
    ))
}

fn combined_output(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::env;
    use std::ffi::OsString;
    use tempfile::TempDir;

    fn make_config(tmp: &TempDir) -> Config {
        Config {
            enabled_tools: vec!["codex".to_string()],
            verified_versions: HashMap::new(),
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
    fn launchd_plist_renders_expected_fields() {
        let mut env_vars = BTreeMap::new();
        env_vars.insert("RELAY_HOME".to_string(), "/tmp/relay".to_string());
        let body = render_launchd_plist(
            Path::new("/usr/local/bin/relay"),
            &["watch".to_string(), "--quiet".to_string()],
            Path::new("/tmp/watch.log"),
            &env_vars,
        );
        assert!(body.contains("<string>dev.jdblackstar.relay.watch</string>"));
        assert!(body.contains("<string>/usr/local/bin/relay</string>"));
        assert!(body.contains("<string>--quiet</string>"));
        assert!(body.contains("<key>EnvironmentVariables</key>"));
    }

    #[test]
    fn systemd_unit_renders_execstart() {
        let body = render_systemd_unit(
            Path::new("/usr/local/bin/relay"),
            &[
                "watch".to_string(),
                "--debounce-ms".to_string(),
                "400".to_string(),
                "--quiet".to_string(),
            ],
            &BTreeMap::new(),
        );
        assert!(body.contains(
            "ExecStart=\"/usr/local/bin/relay\" \"watch\" \"--debounce-ms\" \"400\" \"--quiet\""
        ));
        assert!(body.contains("Restart=always"));
        assert!(body.contains("WantedBy=default.target"));
    }

    #[test]
    fn systemd_unit_escapes_percent_and_dollar_for_execstart_and_environment() {
        let mut env = BTreeMap::new();
        env.insert("RELAY_HOME".to_string(), "/tmp/50%relay$home".to_string());
        let body = render_systemd_unit(
            Path::new("/usr/local/bin/relay%bin$test"),
            &[
                "watch".to_string(),
                "--debug-log-file".to_string(),
                "/tmp/relay%log$arg".to_string(),
            ],
            &env,
        );
        assert!(body.contains(
            "ExecStart=\"/usr/local/bin/relay%%bin$$test\" \"watch\" \"--debug-log-file\" \"/tmp/relay%%log$$arg\""
        ));
        assert!(body.contains("Environment=\"RELAY_HOME=/tmp/50%%relay$$home\""));
    }

    #[test]
    fn runtime_dir_uses_central_parent() -> io::Result<()> {
        let tmp = TempDir::new()?;
        let cfg = make_config(&tmp);
        assert_eq!(runtime_dir(&cfg)?, tmp.path().join("relay/runtime"));
        Ok(())
    }

    #[test]
    fn watch_args_include_requested_flags() {
        let args = watch_args(&InstallWatchServiceOptions {
            debounce_ms: 450,
            quiet: true,
            debug: true,
            debug_log_file: Some(PathBuf::from("/tmp/relay.log")),
        });
        assert_eq!(
            args,
            vec![
                "watch".to_string(),
                "--debounce-ms".to_string(),
                "450".to_string(),
                "--quiet".to_string(),
                "--debug".to_string(),
                "--debug-log-file".to_string(),
                "/tmp/relay.log".to_string()
            ]
        );
    }

    fn restore_env_var(key: &str, value: Option<OsString>) {
        if let Some(value) = value {
            env::set_var(key, value);
        } else {
            env::remove_var(key);
        }
    }

    #[test]
    fn launch_agents_dir_ignores_relay_home() -> io::Result<()> {
        let _lock = crate::ENV_LOCK.lock().unwrap();
        let prev_relay_home = env::var_os("RELAY_HOME");
        env::set_var("RELAY_HOME", "/tmp/relay-home-override");

        let expected = dirs::home_dir()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "home unavailable"))?
            .join("Library/LaunchAgents");
        let result = launch_agents_dir();

        restore_env_var("RELAY_HOME", prev_relay_home);
        assert_eq!(result?, expected);
        Ok(())
    }

    #[test]
    fn systemd_user_dir_ignores_relay_home_when_xdg_config_home_unset() -> io::Result<()> {
        let _lock = crate::ENV_LOCK.lock().unwrap();
        let prev_relay_home = env::var_os("RELAY_HOME");
        let prev_xdg_config_home = env::var_os("XDG_CONFIG_HOME");
        env::set_var("RELAY_HOME", "/tmp/relay-home-override");
        env::remove_var("XDG_CONFIG_HOME");

        let expected = dirs::home_dir()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "home unavailable"))?
            .join(".config/systemd/user");
        let result = systemd_user_dir();

        restore_env_var("RELAY_HOME", prev_relay_home);
        restore_env_var("XDG_CONFIG_HOME", prev_xdg_config_home);
        assert_eq!(result?, expected);
        Ok(())
    }
}
