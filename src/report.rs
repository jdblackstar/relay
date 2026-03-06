use crate::sync::{SyncConflict, SyncItemKind, SyncReport};

pub fn print_sync_summary(report: &SyncReport) {
    if report.is_empty() {
        println!("sync: no changes");
        return;
    }
    println!(
        "sync: commands updated={}; skills updated={}; agents updated={}; rules updated={}",
        report.commands.updated, report.skills.updated, report.agents.updated, report.rules.updated
    );
}

pub fn print_plan_summary(report: &SyncReport) {
    if report.is_empty() {
        println!("plan: no changes");
        return;
    }
    println!(
        "plan: commands would_update={}; skills would_update={}; agents would_update={}; rules would_update={}",
        report.commands.updated, report.skills.updated, report.agents.updated, report.rules.updated
    );
}

pub fn print_conflict_summary(conflicts: &[SyncConflict]) {
    println!("conflicts: {} detected", conflicts.len());
    for conflict in conflicts {
        let kind = match conflict.kind {
            SyncItemKind::Command => "command",
            SyncItemKind::Skill => "skill",
            SyncItemKind::Agent => "agent",
            SyncItemKind::Rule => "rule",
        };
        if conflict.others.is_empty() {
            println!("  {kind} `{}`: chose `{}`", conflict.name, conflict.winner);
        } else {
            println!(
                "  {kind} `{}`: chose `{}`; also changed in `{}`",
                conflict.name,
                conflict.winner,
                conflict.others.join("`, `")
            );
        }
    }
    println!(
        "sync aborted due to conflicts; rerun without --fail-on-conflict to accept newest-wins"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync;

    #[test]
    fn print_sync_summary_variants() {
        let empty = SyncReport::default();
        print_sync_summary(&empty);
        print_plan_summary(&empty);
        let report = SyncReport {
            commands: sync::SyncStats { updated: 1 },
            skills: sync::SyncStats { updated: 2 },
            agents: sync::SyncStats { updated: 0 },
            rules: sync::SyncStats { updated: 3 },
        };
        print_sync_summary(&report);
        print_plan_summary(&report);
    }

    #[test]
    fn print_conflict_summary_smoke() {
        let conflicts = vec![
            SyncConflict {
                kind: SyncItemKind::Command,
                name: "review.md".to_string(),
                winner: "cursor",
                others: vec!["claude"],
            },
            SyncConflict {
                kind: SyncItemKind::Rule,
                name: "codex/default.rules".to_string(),
                winner: "central",
                others: Vec::new(),
            },
        ];
        print_conflict_summary(&conflicts);
    }
}
