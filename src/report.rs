use crate::sync::SyncReport;

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
}
