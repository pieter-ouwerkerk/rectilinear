use anyhow::Result;
use chrono::{DateTime, Utc};
use colored::Colorize;

use crate::db::{Database, TeamSyncStatus};

pub fn handle_status(db: &Database, team: Option<&str>, json: bool, workspace: &str) -> Result<()> {
    let teams = db.sync_status(workspace, team)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&teams)?);
        return Ok(());
    }

    if teams.is_empty() {
        println!(
            "{}",
            "No hydration state found. Run `rectilinear sync --team <KEY>` first.".dimmed()
        );
        return Ok(());
    }

    for team in &teams {
        print_team(team);
    }
    Ok(())
}

fn print_team(team: &TeamSyncStatus) {
    println!(
        "{} — {} issues{}",
        team.team_key.bold(),
        team.issue_count,
        match team.last_synced_at.as_deref() {
            Some(at) => format!(", last synced {at}"),
            None => String::new(),
        }
    );
    for res in &team.resources {
        let total = res.total();
        let done = total - res.outstanding();
        let pct = if total > 0 { done * 100 / total } else { 100 };
        let mut detail = vec![format!("{done}/{total} ({pct}%)")];
        if res.retryable > 0 {
            detail.push(format!("{} awaiting retry", res.retryable));
        }
        if res.permission_denied + res.unavailable > 0 {
            detail.push(format!(
                "{} unavailable",
                res.permission_denied + res.unavailable
            ));
        }
        println!("  {:<10} {}", res.resource, detail.join(", "));
    }
    if let Some(retry_at) = team.next_retry_at.as_deref() {
        println!("  next retry {}", describe_retry(retry_at, Utc::now()));
    }
}

fn describe_retry(retry_at: &str, now: DateTime<Utc>) -> String {
    match DateTime::parse_from_rfc3339(retry_at) {
        Ok(at) => {
            let at = at.with_timezone(&Utc);
            if at <= now {
                format!("{retry_at} (due now)")
            } else {
                let mins = (at - now).num_minutes();
                format!("{retry_at} (in {}h{:02}m)", mins / 60, mins % 60)
            }
        }
        Err(_) => retry_at.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::describe_retry;
    use chrono::{TimeZone, Utc};

    #[test]
    fn describe_retry_reports_countdown() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 9, 0, 0).unwrap();
        assert_eq!(
            describe_retry("2026-08-21T10:32:00Z", now),
            "2026-08-21T10:32:00Z (in 1h32m)"
        );
        assert_eq!(
            describe_retry("2026-08-21T08:00:00Z", now),
            "2026-08-21T08:00:00Z (due now)"
        );
        assert_eq!(describe_retry("not-a-date", now), "not-a-date");
    }
}
