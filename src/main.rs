mod config;
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
        #[arg(long, conflicts_with = "quiet")]
        verbose: bool,
        /// Suppress all output
        #[arg(long, conflicts_with = "verbose")]
        quiet: bool,
        /// Prompt if verified tool versions differ
        #[arg(long)]
        confirm_versions: bool,
        /// Preview changes without writing files
        #[arg(long, conflicts_with = "apply")]
        plan: bool,
        /// Explicitly apply changes (default behavior)
        #[arg(long, conflicts_with = "plan")]
        apply: bool,
    },
    /// Watch folders and sync changes
    Watch {
        #[arg(long, default_value = "300")]
        debounce_ms: u64,
        /// Suppress all output
        #[arg(long)]
        quiet: bool,
        /// Prompt if verified tool versions differ
        #[arg(long)]
        confirm_versions: bool,
    },
    /// Show recent sync/watch history events
    History {
        /// Number of events to show
        #[arg(long, default_value = "20")]
        limit: usize,
    },
    /// Roll back a specific history event
    Rollback {
        /// Event id to roll back
        event_id: Option<String>,
        /// Roll back the latest event
        #[arg(long, conflicts_with = "event_id")]
        latest: bool,
        /// Skip hash safety checks
        #[arg(long)]
        force: bool,
    },
}

#[cfg(all(not(any(test, coverage)), windows))]
fn main() {
    eprintln!("relay currently targets Unix-like systems and does not support Windows yet.");
    std::process::exit(1);
}

#[cfg(all(not(any(test, coverage)), not(windows)))]
fn main() -> std::io::Result<()> {
    let cli = Cli::parse();
    logging::init(cli.debug, cli.debug_log_file.as_deref());
    logging::debug("relay start");
    match cli.command {
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
            confirm_versions,
        } => {
            logging::debug(&format!(
                "command=watch debounce_ms={debounce_ms} quiet={quiet} confirm_versions={confirm_versions}"
            ));
            let cfg = config::Config::load_or_default()?;
            let mismatch = versions::check_versions(&cfg);
            if confirm_versions && mismatch && !versions::confirm_version_mismatch()? {
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
        Commands::History { limit } => {
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

#[cfg(any(test, coverage))]
fn main() {}

#[cfg(test)]
mod tests {
    #[test]
    fn main_stub_runs() {
        super::main();
    }
}
