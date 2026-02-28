mod config;
mod daemon;
mod history;
mod init;
mod logging;
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
        } => {
            warn_if_not_initialized();
            let cfg = config::Config::load_or_default()?;
            let mismatch = versions::check_versions(&cfg);
            if confirm_versions && mismatch && !versions::confirm_version_mismatch()? {
                return Ok(());
            }
            let mode = if plan {
                sync::ExecutionMode::Plan
            } else {
                sync::ExecutionMode::Apply
            };
            logging::debug(&format!(
                "command=sync mode={mode:?} verbose={verbose} quiet={quiet} confirm_versions={confirm_versions}"
            ));
            let log_mode = if verbose {
                sync::LogMode::Actions
            } else {
                sync::LogMode::Quiet
            };
            let outcome = sync::sync_all_with_mode(&cfg, log_mode, mode, "sync")?;
            logging::debug(&format!(
                "sync finished commands={} skills={} agents={} rules={} history_event_id={}",
                outcome.report.commands.updated,
                outcome.report.skills.updated,
                outcome.report.agents.updated,
                outcome.report.rules.updated,
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
            warn_if_not_initialized();
            logging::debug(&format!(
                "command=watch debounce_ms={debounce_ms} quiet={quiet} daemon={daemon} confirm_versions={confirm_versions}"
            ));
            let cfg = config::Config::load_or_default()?;
            let mismatch = versions::check_versions(&cfg);
            if confirm_versions && mismatch && !versions::confirm_version_mismatch()? {
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
            let _ = sync::sync_all_with_mode(
                &cfg,
                log_mode,
                sync::ExecutionMode::Apply,
                "watch-start",
            )?;
            watch::watch(&cfg, debounce_ms, log_mode)
        }
        Commands::Status => {
            logging::debug("command=status");
            let cfg = config::Config::load_or_default()?;
            print_service_status(&cfg)
        }
        Commands::Daemon { command } => {
            warn_if_not_initialized();
            let cfg = config::Config::load_or_default()?;
            match command {
                DaemonCommand::Install {
                    debounce_ms,
                    quiet,
                    confirm_versions,
                } => {
                    logging::debug(&format!(
                        "command=daemon.install debounce_ms={debounce_ms} quiet={quiet} confirm_versions={confirm_versions}"
                    ));
                    let mismatch = versions::check_versions(&cfg);
                    if confirm_versions && mismatch && !versions::confirm_version_mismatch()? {
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
            warn_if_not_initialized();
            logging::debug(&format!("command=history limit={limit}"));
            let cfg = config::Config::load_or_default()?;
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
        Commands::Rollback {
            event_id,
            latest,
            force,
        } => {
            warn_if_not_initialized();
            logging::debug(&format!(
                "command=rollback latest={latest} force={force} event_id={}",
                event_id.as_deref().unwrap_or("none")
            ));
            let cfg = config::Config::load_or_default()?;
            let store = history::HistoryStore::from_config(&cfg)?;
            let target_event_id = if latest {
                store.latest_event_id()?.ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::NotFound, "history is empty")
                })?
            } else {
                event_id.ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "provide an event id or use --latest",
                    )
                })?
            };
            let report = store.rollback(&target_event_id, force)?;
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
    #[test]
    fn main_stub_runs() {
        super::main();
    }
}
