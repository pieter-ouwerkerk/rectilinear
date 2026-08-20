use anyhow::Result;
use colored::Colorize;

use crate::db::{Database, DateFilters, ListIssuesParams, ListOrder};

pub struct HandleListParams<'a> {
    pub team: Option<&'a str>,
    pub state: Option<&'a str>,
    pub dates: DateFilters,
    pub order: ListOrder,
    pub limit: usize,
    pub offset: usize,
    pub include_archived: bool,
    pub json: bool,
    pub workspace: &'a str,
}

pub fn handle_list(db: &Database, params: HandleListParams<'_>) -> Result<()> {
    let issues = db.list_issues(&ListIssuesParams {
        workspace_id: params.workspace,
        team_key: params.team,
        state_filter: params.state,
        label_ids: None,
        dates: params.dates,
        order: params.order,
        limit: params.limit,
        offset: params.offset,
        include_archived: params.include_archived,
    })?;

    if params.json {
        println!("{}", serde_json::to_string_pretty(&issues)?);
        return Ok(());
    }

    if issues.is_empty() {
        println!("{}", "No issues found.".dimmed());
    }
    for issue in &issues {
        let priority = match issue.priority {
            1 => "!!!".red().to_string(),
            2 => "!! ".yellow().to_string(),
            3 => "!  ".blue().to_string(),
            _ => "   ".to_string(),
        };
        println!(
            "{} {} {} [{}] {}",
            priority,
            issue.identifier.bold(),
            issue.title,
            issue.state_name,
            format!("updated {}", issue.updated_at).dimmed(),
        );
    }

    // State freshness so the reader knows what "recent" is relative to.
    for team in db.list_synced_teams(params.workspace)? {
        if params.team.is_none_or(|key| team.key.eq_ignore_ascii_case(key)) {
            println!(
                "{}",
                format!(
                    "  {} — {} issues, last synced {}",
                    team.key,
                    team.issue_count,
                    team.last_synced_at.as_deref().unwrap_or("never")
                )
                .dimmed()
            );
        }
    }
    Ok(())
}
