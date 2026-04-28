mod blacklist;
mod config;
mod daemon;
mod history;
mod init;
mod logging;
mod process_lock;
mod report;
mod sync;
mod tools;
mod versions;
mod watch;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[cfg(test)]
use std::sync::Mutex;

#[cfg(test)]
pub(crate) static ENV_LOCK: Mutex<()> = Mutex::new(());

#[derive(Parser)]
#[command(
    name = "relay",
    version,
    about = "Minimal two-way command + skill sync for Codex/Claude/OpenCode"
)]
struct Cli {
    /// Enable detailed debug logging to file
    #[arg(long, global = true)]
    debug: bool,
    /// Override debug log path (default: ~/.config/relay/logs/relay-debug.log)
    #[arg(long, global = true)]
    debug_log_file: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Set up config and folders interactively
    Init,
    /// Sync command and skill files across tools
    Sync {
        /// Show per-action output
        #[arg(short = 'v', long, conflicts_with = "quiet")]
        verbose: bool,
        /// Suppress all output
        #[arg(short = 'q', long, conflicts_with = "verbose")]
        quiet: bool,
        /// Prompt if verified tool versions differ
        #[arg(short = 'c', long)]
        confirm_versions: bool,
        /// Preview changes without writing files
        #[arg(short = 'p', long, conflicts_with = "apply")]
        plan: bool,
        /// Explicitly apply changes (default behavior)
        #[arg(short = 'a', long, conflicts_with = "plan")]
        apply: bool,
        /// Abort without writing if sync detects conflicts
        #[arg(long)]
        fail_on_conflict: bool,
    },
    /// Watch folders and sync changes
    Watch {
        #[arg(short = 'b', long, default_value = "300")]
        debounce_ms: u64,
        /// Suppress all output
        #[arg(short = 'q', long)]
        quiet: bool,
        /// Install and run watch as a background service (launchd/systemd)
        #[arg(short = 'd', long)]
        daemon: bool,
        /// Prompt if verified tool versions differ
        #[arg(short = 'c', long)]
        confirm_versions: bool,
    },
    /// Show background service status
    Status,
    /// Manage background watch service (launchd/systemd)
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Show recent sync/watch history events
    History {
        /// Number of events to show
        #[arg(short = 'n', long, default_value = "20")]
        limit: usize,
    },
    /// Roll back a specific history event
    Rollback {
        /// Event id to roll back
        event_id: Option<String>,
        /// Roll back the latest event
        #[arg(short = 'l', long, conflicts_with = "event_id")]
        latest: bool,
        /// Skip hash safety checks
        #[arg(short = 'f', long)]
        force: bool,
    },
    /// Exclude an item from syncing to specific tools
    Blacklist {
        /// Path relative to central store (e.g. commands/review.md, skills/plan)
        path: String,
        /// Exclude from Claude
        #[arg(long)]
        claude: bool,
        /// Exclude from Codex
        #[arg(long)]
        codex: bool,
        /// Exclude from Cursor
        #[arg(long)]
        cursor: bool,
        /// Exclude from OpenCode
        #[arg(long)]
        opencode: bool,
    },
    /// Re-allow a previously blacklisted item for specific tools
    Allow {
        /// Path relative to central store (e.g. commands/review.md, skills/plan)
        path: String,
        /// Allow for Claude
        #[arg(long)]
        claude: bool,
        /// Allow for Codex
        #[arg(long)]
        codex: bool,
        /// Allow for Cursor
        #[arg(long)]
        cursor: bool,
        /// Allow for OpenCode
        #[arg(long)]
        opencode: bool,
    },
}

#[derive(Subcommand)]
enum DaemonCommand {
    /// Install or update the watch service definition
    Install {
        #[arg(short = 'b', long, default_value = "300")]
        debounce_ms: u64,
        #[arg(short = 'q', long)]
        quiet: bool,
        /// Prompt if verified tool versions differ
        #[arg(short = 'c', long)]
        confirm_versions: bool,
    },
    /// Start the installed watch service
    Start,
    /// Stop the watch service
    Stop,
    /// Restart the watch service
    Restart,
    /// Show watch service status
    Status,
    /// Stop and remove the watch service definition
    Uninstall,
}

#[cfg(all(not(any(test, coverage)), windows))]
fn main() {
    eprintln!("relay currently targets Unix-like systems and does not support Windows yet.");
    std::process::exit(1);
}

#[cfg(all(not(any(test, coverage)), not(windows)))]
fn warn_if_not_initialized() {
    match config::Config::is_initialized() {
        Ok(true) => {}
        Ok(false) => {
            eprintln!("hint: relay has not been set up yet; run `relay init` first");
            eprintln!();
        }
        Err(_) => {}
    }
}

#[cfg_attr(test, allow(dead_code))]
fn load_cfg() -> std::io::Result<config::Config> {
    #[cfg(all(not(any(test, coverage)), not(windows)))]
    warn_if_not_initialized();
    config::Config::load_or_default()
}

#[cfg_attr(test, allow(dead_code))]
fn confirm_versions_or_continue(
    cfg: &config::Config,
    confirm_versions: bool,
) -> std::io::Result<bool> {
    let mismatch = versions::check_versions(cfg);
    if mismatch && confirm_versions {
        return versions::confirm_version_mismatch();
    }
    Ok(true)
}

#[cfg_attr(test, allow(dead_code))]
fn require_tool_flags(tools: Vec<String>) -> std::io::Result<Vec<String>> {
    if !tools.is_empty() {
        return Ok(tools);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "at least one tool flag is required (--claude, --codex, --cursor, --opencode)",
    ))
}

#[cfg_attr(test, allow(dead_code))]
fn rollback_target_event_id(
    store: &history::HistoryStore,
    event_id: Option<String>,
    latest: bool,
) -> std::io::Result<String> {
    if latest {
        return store
            .latest_event_id()?
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "history is empty"));
    }
    event_id.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "provide an event id or use --latest",
        )
    })
}

#[cfg_attr(test, allow(dead_code))]
fn run_sync_command(
    cfg: &config::Config,
    log_mode: sync::LogMode,
    quiet: bool,
    mode: sync::ExecutionMode,
    fail_on_conflict: bool,
) -> std::io::Result<sync::SyncOutcome> {
    run_sync_command_with(
        log_mode,
        quiet,
        mode,
        fail_on_conflict,
        |run_log_mode, run_mode| sync::sync_all_with_mode(cfg, run_log_mode, run_mode, "sync"),
    )
}

fn run_sync_command_with<F>(
    log_mode: sync::LogMode,
    quiet: bool,
    mode: sync::ExecutionMode,
    fail_on_conflict: bool,
    mut run_sync: F,
) -> std::io::Result<sync::SyncOutcome>
where
    F: FnMut(sync::LogMode, sync::ExecutionMode) -> std::io::Result<sync::SyncOutcome>,
{
    if !fail_on_conflict {
        return run_sync(log_mode, mode);
    }

    let preflight = run_sync(sync::LogMode::Quiet, sync::ExecutionMode::Plan)?;
    if preflight.has_conflicts() {
        if !quiet {
            report::print_conflict_summary(&preflight.conflicts);
        }
        return Err(std::io::Error::other(format!(
            "sync aborted due to {} conflict{}",
            preflight.conflicts.len(),
            if preflight.conflicts.len() == 1 {
                ""
            } else {
                "s"
            }
        )));
    }

    if mode == sync::ExecutionMode::Plan {
        if log_mode == sync::LogMode::Actions {
            // Re-run the clean plan with action logging so verbose mode still prints details.
            return run_sync(log_mode, mode);
        }
        return Ok(preflight);
    }

    run_sync(log_mode, mode)
}

#[cfg_attr(test, allow(dead_code))]
fn sync_requires_process_lock(mode: sync::ExecutionMode) -> bool {
    mode == sync::ExecutionMode::Apply
}

#[cfg_attr(test, allow(dead_code))]
fn with_process_lock<T, F>(operation: &str, run: F) -> std::io::Result<T>
where
    F: FnOnce() -> std::io::Result<T>,
{
    let _lock = process_lock::ProcessLock::acquire(operation)?;
    run()
}

#[cfg(all(not(any(test, coverage)), not(windows)))]
fn main() -> std::io::Result<()> {
    let Cli {
        debug,
        debug_log_file,
        command,
    } = Cli::parse();
    logging::init(debug, debug_log_file.as_deref());
    logging::debug("relay start");
    match command {
        Commands::Init => {
            logging::debug("command=init");
            init::init()
        }
        Commands::Sync {
            verbose,
            quiet,
            confirm_versions,
            plan,
            apply: _apply,
            fail_on_conflict,
        } => {
            let cfg = load_cfg()?;
            if !confirm_versions_or_continue(&cfg, confirm_versions)? {
                return Ok(());
            }
            let mode = if plan {
                sync::ExecutionMode::Plan
            } else {
                sync::ExecutionMode::Apply
            };
            logging::debug(&format!(
                "command=sync mode={mode:?} verbose={verbose} quiet={quiet} confirm_versions={confirm_versions} fail_on_conflict={fail_on_conflict}"
            ));
            let log_mode = if verbose {
                sync::LogMode::Actions
            } else {
                sync::LogMode::Quiet
            };
            let outcome = if sync_requires_process_lock(mode) {
                with_process_lock("sync", || {
                    run_sync_command(&cfg, log_mode, quiet, mode, fail_on_conflict)
                })?
            } else {
                run_sync_command(&cfg, log_mode, quiet, mode, fail_on_conflict)?
            };
            logging::debug(&format!(
                "sync finished commands={} skills={} agents={} rules={} conflicts={} history_event_id={}",
                outcome.report.commands.updated,
                outcome.report.skills.updated,
                outcome.report.agents.updated,
                outcome.report.rules.updated,
                outcome.conflicts.len(),
                outcome.history_event_id.as_deref().unwrap_or("none")
            ));
            if !quiet {
                if mode == sync::ExecutionMode::Plan {
                    report::print_plan_summary(&outcome.report);
                } else {
                    report::print_sync_summary(&outcome.report);
                    if let Some(event_id) = outcome.history_event_id {
                        println!("history: recorded event {event_id}");
                    }
                }
            }
            Ok(())
        }
        Commands::Watch {
            debounce_ms,
            quiet,
            daemon,
            confirm_versions,
        } => {
            logging::debug(&format!(
                "command=watch debounce_ms={debounce_ms} quiet={quiet} daemon={daemon} confirm_versions={confirm_versions}"
            ));
            let cfg = load_cfg()?;
            if !confirm_versions_or_continue(&cfg, confirm_versions)? {
                return Ok(());
            }
            if daemon {
                let options = daemon::InstallWatchServiceOptions {
                    debounce_ms,
                    quiet,
                    debug,
                    debug_log_file: debug_log_file.clone(),
                };
                daemon::install_watch_service(&cfg, &options)?;
                daemon::start_watch_service(&cfg)?;
                print_service_status(&cfg)?;
                return Ok(());
            }
            let log_mode = if quiet {
                sync::LogMode::Quiet
            } else {
                sync::LogMode::Actions
            };
            let _ = with_process_lock("watch-start", || {
                sync::sync_all_with_mode(&cfg, log_mode, sync::ExecutionMode::Apply, "watch-start")
            })?;
            watch::watch(&cfg, debounce_ms, log_mode)
        }
        Commands::Status => {
            logging::debug("command=status");
            let cfg = load_cfg()?;
            print_service_status(&cfg)
        }
        Commands::Daemon { command } => {
            let cfg = load_cfg()?;
            match command {
                DaemonCommand::Install {
                    debounce_ms,
                    quiet,
                    confirm_versions,
                } => {
                    logging::debug(&format!(
                        "command=daemon.install debounce_ms={debounce_ms} quiet={quiet} confirm_versions={confirm_versions}"
                    ));
                    if !confirm_versions_or_continue(&cfg, confirm_versions)? {
                        return Ok(());
                    }
                    let options = daemon::InstallWatchServiceOptions {
                        debounce_ms,
                        quiet,
                        debug,
                        debug_log_file: debug_log_file.clone(),
                    };
                    daemon::install_watch_service(&cfg, &options)?;
                    print_service_status(&cfg)
                }
                DaemonCommand::Start => {
                    logging::debug("command=daemon.start");
                    daemon::start_watch_service(&cfg)?;
                    print_service_status(&cfg)
                }
                DaemonCommand::Stop => {
                    logging::debug("command=daemon.stop");
                    daemon::stop_watch_service(&cfg)?;
                    print_service_status(&cfg)
                }
                DaemonCommand::Restart => {
                    logging::debug("command=daemon.restart");
                    daemon::restart_watch_service(&cfg)?;
                    print_service_status(&cfg)
                }
                DaemonCommand::Status => {
                    logging::debug("command=daemon.status");
                    print_service_status(&cfg)
                }
                DaemonCommand::Uninstall => {
                    logging::debug("command=daemon.uninstall");
                    daemon::uninstall_watch_service(&cfg)?;
                    print_service_status(&cfg)
                }
            }
        }
        Commands::History { limit } => {
            logging::debug(&format!("command=history limit={limit}"));
            let cfg = load_cfg()?;
            let store = history::HistoryStore::from_config(&cfg)?;
            let events = store.list_recent(limit)?;
            if events.is_empty() {
                println!("history: no events");
                return Ok(());
            }
            for event in events {
                println!(
                    "{} ts_ms={} origin={} writes={}",
                    event.id, event.timestamp_ms, event.origin, event.writes
                );
            }
            Ok(())
        }
        Commands::Blacklist {
            path,
            claude,
            codex,
            cursor,
            opencode,
        } => {
            let tools = require_tool_flags(blacklist::collect_tool_flags(
                claude, codex, cursor, opencode,
            ))?;
            logging::debug(&format!("command=blacklist path={path} tools={tools:?}"));
            with_process_lock("blacklist", || {
                let mut cfg = load_cfg()?;
                blacklist::add_blacklist(&mut cfg, &path, &tools)
            })?;
            println!("blacklisted {path} for {}", tools.join(", "));
            Ok(())
        }
        Commands::Allow {
            path,
            claude,
            codex,
            cursor,
            opencode,
        } => {
            let tools = require_tool_flags(blacklist::collect_tool_flags(
                claude, codex, cursor, opencode,
            ))?;
            logging::debug(&format!("command=allow path={path} tools={tools:?}"));
            with_process_lock("allow", || {
                let mut cfg = load_cfg()?;
                blacklist::remove_blacklist(&mut cfg, &path, &tools)
            })?;
            println!("allowed {path} for {}", tools.join(", "));
            Ok(())
        }
        Commands::Rollback {
            event_id,
            latest,
            force,
        } => {
            logging::debug(&format!(
                "command=rollback latest={latest} force={force} event_id={}",
                event_id.as_deref().unwrap_or("none")
            ));
            let cfg = load_cfg()?;
            let report = with_process_lock("rollback", || {
                let store = history::HistoryStore::from_config(&cfg)?;
                let target_event_id = rollback_target_event_id(&store, event_id, latest)?;
                store.rollback(&target_event_id, force)
            })?;
            println!(
                "rollback: restored {} paths from {}",
                report.restored, report.target_event_id
            );
            if let Some(event_id) = report.rollback_event_id {
                println!("history: recorded event {event_id}");
            }
            Ok(())
        }
    }
}

#[cfg(all(not(any(test, coverage)), not(windows)))]
fn print_service_status(cfg: &config::Config) -> std::io::Result<()> {
    let status = daemon::watch_service_status(cfg)?;
    println!("status: manager={}", status.manager.as_str());
    println!("status: service={}", status.service_name);
    println!("status: state={}", status.state.as_str());
    println!(
        "status: service_file={}",
        status.paths.service_file.display()
    );
    if let Some(log_file) = status.paths.log_file.as_ref() {
        println!("status: log_file={}", log_file.display());
    }
    if let Some(logs_hint) = status.logs_hint.as_ref() {
        println!("status: logs={logs_hint}");
    }
    Ok(())
}

#[cfg(any(test, coverage))]
fn main() {}

#[cfg(test)]
mod tests {
    use super::{Cli, Commands};
    use crate::history::{HistoryRecorder, HistoryStore};
    use crate::sync;
    use crate::sync::test_support::{setup, write_plain};
    use clap::Parser;
    use std::io;

    #[test]
    fn main_stub_runs() {
        super::main();
    }

    #[test]
    fn cli_parses_sync_fail_on_conflict() {
        let cli = Cli::try_parse_from(["relay", "sync", "--fail-on-conflict"]).unwrap();
        match cli.command {
            Commands::Sync {
                fail_on_conflict,
                plan,
                ..
            } => {
                assert!(fail_on_conflict);
                assert!(!plan);
            }
            _ => panic!("expected sync command"),
        }
    }

    #[test]
    fn cli_parses_sync_plan_fail_on_conflict() {
        let cli = Cli::try_parse_from(["relay", "sync", "--plan", "--fail-on-conflict"]).unwrap();
        match cli.command {
            Commands::Sync {
                fail_on_conflict,
                plan,
                ..
            } => {
                assert!(fail_on_conflict);
                assert!(plan);
            }
            _ => panic!("expected sync command"),
        }
    }

    #[test]
    fn cli_rejects_conflicting_sync_flags_with_fail_on_conflict() {
        let err =
            match Cli::try_parse_from(["relay", "sync", "--plan", "--apply", "--fail-on-conflict"])
            {
                Ok(_) => panic!("expected clap parsing to fail"),
                Err(err) => err,
            };
        assert!(err.to_string().contains("--apply"));
    }

    #[test]
    fn fail_on_conflict_plan_verbose_replays_plan_with_actions_logging() {
        let mut calls = Vec::new();
        let outcome = super::run_sync_command_with(
            sync::LogMode::Actions,
            false,
            sync::ExecutionMode::Plan,
            true,
            |log_mode, mode| {
                calls.push((log_mode, mode));
                Ok(sync::SyncOutcome {
                    report: sync::SyncReport {
                        commands: sync::SyncStats { updated: 1 },
                        ..sync::SyncReport::default()
                    },
                    conflicts: Vec::new(),
                    history_event_id: None,
                })
            },
        )
        .unwrap();

        assert_eq!(
            calls,
            vec![
                (sync::LogMode::Quiet, sync::ExecutionMode::Plan),
                (sync::LogMode::Actions, sync::ExecutionMode::Plan),
            ]
        );
        assert_eq!(outcome.report.commands.updated, 1);
    }

    #[test]
    fn only_apply_sync_requires_process_lock() {
        assert!(super::sync_requires_process_lock(
            sync::ExecutionMode::Apply
        ));
        assert!(!super::sync_requires_process_lock(
            sync::ExecutionMode::Plan
        ));
    }

    #[test]
    fn require_tool_flags_accepts_non_empty_input() {
        let tools = super::require_tool_flags(vec!["codex".to_string()]).unwrap();
        assert_eq!(tools, vec!["codex".to_string()]);
    }

    #[test]
    fn require_tool_flags_rejects_empty_input() {
        let err = super::require_tool_flags(Vec::new()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn rollback_target_event_id_prefers_explicit_event_id() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let store = HistoryStore::from_config(&cfg)?;
        let event_id =
            super::rollback_target_event_id(&store, Some("manual-id".to_string()), false)?;
        assert_eq!(event_id, "manual-id");
        Ok(())
    }

    #[test]
    fn rollback_target_event_id_uses_latest_history_event() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let path = cfg.central_dir.join("test.md");
        write_plain(&path, "before")?;

        let mut recorder = HistoryRecorder::new(&cfg, "test")?;
        let before = recorder.capture_path(&path)?;
        write_plain(&path, "after")?;
        let after = recorder.capture_path(&path)?;
        recorder.record_change(&path, before, after);
        let expected = recorder.finish()?.expect("history event id");

        let store = HistoryStore::from_config(&cfg)?;
        let event_id = super::rollback_target_event_id(&store, None, true)?;
        assert_eq!(event_id, expected);
        Ok(())
    }

    #[test]
    fn rollback_target_event_id_errors_when_history_is_empty() -> io::Result<()> {
        let (_tmp, cfg) = setup()?;
        let store = HistoryStore::from_config(&cfg)?;
        let err = super::rollback_target_event_id(&store, None, true).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        Ok(())
    }
}
