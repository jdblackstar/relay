use crate::config::{Config, TOOL_CLAUDE, TOOL_CODEX, TOOL_OPENCODE};
use crate::tools::tool_detected;
use console::style;

#[cfg(not(any(test, coverage)))]
use dialoguer::{theme::ColorfulTheme, Confirm};
#[cfg(not(any(test, coverage)))]
use std::process::Command;

pub fn check_versions(cfg: &Config) -> bool {
    let mut mismatch = false;
    if cfg.tool_enabled(TOOL_CODEX)
        && tool_detected(cfg, TOOL_CODEX)
        && check_command_version("codex", cfg.verified_version(TOOL_CODEX))
    {
        mismatch = true;
    }
    if cfg.tool_enabled(TOOL_CLAUDE)
        && tool_detected(cfg, TOOL_CLAUDE)
        && check_command_version("claude", cfg.verified_version(TOOL_CLAUDE))
    {
        mismatch = true;
    }
    if cfg.tool_enabled(TOOL_OPENCODE)
        && tool_detected(cfg, TOOL_OPENCODE)
        && check_command_version("opencode", cfg.verified_version(TOOL_OPENCODE))
    {
        mismatch = true;
    }
    mismatch
}

#[cfg(not(any(test, coverage)))]
fn check_command_version(bin: &str, verified: Option<&str>) -> bool {
    let output = Command::new(bin).arg("--version").output();
    match output {
        Ok(out) if out.status.success() => {
            let version = String::from_utf8_lossy(&out.stdout);
            let version = version.trim();
            if !version.is_empty() {
                let actual_token =
                    extract_version_token(version).unwrap_or_else(|| version.to_string());
                if let Some(verified) = verified {
                    let verified_token =
                        extract_version_token(verified).unwrap_or_else(|| verified.to_string());
                    if let (Some(actual_ver), Some(verified_ver)) =
                        (parse_version(&actual_token), parse_version(&verified_token))
                    {
                        let status = classify_version(actual_ver, verified_ver);
                        let colored = colorize_version(&actual_token, status);
                        println!("Detected {bin} version: {colored} (verified {verified_token})");
                        return status != VersionStatus::Ok;
                    } else {
                        println!(
                            "Detected {bin} version: {actual_token} (verified {verified_token})"
                        );
                        return actual_token != verified_token;
                    }
                } else {
                    println!("Detected {bin} version: {actual_token}");
                }
            }
        }
        _ => {
            println!("Warning: could not detect {bin} version (not installed or not on PATH).");
        }
    }
    false
}

#[cfg(any(test, coverage))]
fn check_command_version(_bin: &str, _verified: Option<&str>) -> bool {
    false
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum VersionStatus {
    Ok,
    Warn,
    Old,
}

#[derive(Clone, Copy)]
struct ParsedVersion {
    major: u64,
    minor: u64,
}

fn extract_version_token(input: &str) -> Option<String> {
    let mut start = None;
    for (idx, ch) in input.char_indices() {
        if ch.is_ascii_digit() {
            start = Some(idx);
            break;
        }
    }
    let start = start?;
    let mut end = start;
    for (offset, ch) in input[start..].char_indices() {
        if ch.is_ascii_digit() || ch == '.' {
            end = start + offset + ch.len_utf8();
        } else {
            break;
        }
    }
    Some(input[start..end].to_string())
}

fn parse_version(token: &str) -> Option<ParsedVersion> {
    let mut parts = token.split('.').filter(|part| !part.is_empty());
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts
        .next()
        .and_then(|p| p.parse::<u64>().ok())
        .unwrap_or(0);
    Some(ParsedVersion { major, minor })
}

fn classify_version(actual: ParsedVersion, verified: ParsedVersion) -> VersionStatus {
    if actual.major == verified.major {
        if actual.minor == verified.minor {
            VersionStatus::Ok
        } else {
            VersionStatus::Warn
        }
    } else {
        VersionStatus::Old
    }
}

fn colorize_version(version: &str, status: VersionStatus) -> String {
    match status {
        VersionStatus::Ok => style(version).green().to_string(),
        VersionStatus::Warn => style(version).yellow().to_string(),
        VersionStatus::Old => style(version).red().to_string(),
    }
}

#[allow(dead_code)]
pub fn confirm_version_mismatch() -> std::io::Result<bool> {
    #[cfg(not(any(test, coverage)))]
    {
        let theme = ColorfulTheme::default();
        let result = Confirm::with_theme(&theme)
            .with_prompt("Detected version mismatch vs verified versions. Proceed anyway?")
            .default(false)
            .interact();
        match result {
            Ok(value) => Ok(value),
            Err(dialoguer::Error::IO(err)) if err.kind() == std::io::ErrorKind::NotConnected => {
                Ok(false)
            }
            Err(err) => Err(std::io::Error::other(err)),
        }
    }
    #[cfg(any(test, coverage))]
    {
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, TOOL_CLAUDE, TOOL_CODEX, TOOL_CURSOR, TOOL_OPENCODE};
    use std::fs;
    use tempfile::TempDir;

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
    fn version_parsing_and_color() {
        let token = extract_version_token("codex 1.2.3").unwrap();
        assert_eq!(token, "1.2.3");
        let parsed = parse_version(&token).unwrap();
        assert_eq!(parsed.major, 1);
        assert_eq!(parsed.minor, 2);
        let status = classify_version(parsed, ParsedVersion { major: 1, minor: 2 });
        let colored = colorize_version(&token, status);
        assert!(colored.contains("1.2.3"));
        let warn_colored = colorize_version(&token, VersionStatus::Warn);
        assert!(warn_colored.contains("1.2.3"));
        let old_colored = colorize_version(&token, VersionStatus::Old);
        assert!(old_colored.contains("1.2.3"));

        assert!(extract_version_token("nope").is_none());
        assert!(parse_version("x.y").is_none());
        let token = extract_version_token("v1.2beta").unwrap();
        assert_eq!(token, "1.2");
        let status = classify_version(parsed, ParsedVersion { major: 1, minor: 3 });
        assert!(matches!(status, VersionStatus::Warn));
        let status = classify_version(parsed, ParsedVersion { major: 2, minor: 0 });
        assert!(matches!(status, VersionStatus::Old));
    }

    #[test]
    fn check_versions_runs() -> std::io::Result<()> {
        let tmp = TempDir::new()?;
        let mut cfg = make_config(&tmp);
        fs::create_dir_all(&cfg.codex_dir)?;
        fs::create_dir_all(&cfg.claude_dir)?;
        assert!(!check_versions(&cfg));
        cfg.enabled_tools.clear();
        assert!(!check_versions(&cfg));
        Ok(())
    }
}
