use anyhow::Result;
use colored::Colorize;

use crate::config::Config;
use crate::db::Database;

pub fn handle_show(
    db: &Database,
    _config: &Config,
    id: &str,
    json: bool,
    comments: bool,
    _workspace: &str,
) -> Result<()> {
    let issue = db.get_issue(id)?.ok_or_else(|| {
        anyhow::anyhow!(
            "Issue '{}' not found in local database. Try syncing first.",
            id
        )
    })?;

    if json {
        let mut value = serde_json::to_value(&issue)?;
        let relations = db.get_relations_enriched(&issue.id)?;
        if !relations.is_empty() {
            value["relations"] = serde_json::to_value(&relations)?;
        }
        if comments {
            let comments = db.get_comments(&issue.id)?;
            value["comments"] = serde_json::to_value(&comments)?;
        }
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    println!("{} {}", issue.identifier.bold(), issue.title.bold());
    println!("{}", "─".repeat(60));

    println!("  {} {}", "State:".dimmed(), issue.state_name);
    println!("  {} {}", "Priority:".dimmed(), issue.priority_label());

    if let Some(ref assignee) = issue.assignee_name {
        println!("  {} {}", "Assignee:".dimmed(), assignee);
    }
    for (label, value) in membership_rows(&issue) {
        println!("  {} {}", format!("{label}:").dimmed(), value);
    }

    let labels = issue.labels();
    if !labels.is_empty() {
        println!("  {} {}", "Labels:".dimmed(), labels.join(", "));
    }

    println!("  {} {}", "Created:".dimmed(), issue.created_at);
    println!("  {} {}", "Updated:".dimmed(), issue.updated_at);
    if !issue.url.is_empty() {
        println!("  {} {}", "URL:".dimmed(), issue.url);
    }

    let relations = db.get_relations_enriched(&issue.id)?;
    if !relations.is_empty() {
        println!();
        println!("{}", "Relations:".bold());
        for rel in &relations {
            let state_suffix = if rel.issue_state.is_empty() {
                String::new()
            } else {
                format!(" [{}]", rel.issue_state)
            };
            let title_suffix = if rel.issue_title.is_empty() {
                String::new()
            } else {
                format!(" ({})", rel.issue_title)
            };
            println!(
                "  {} {} {}{}{}",
                format!("{}:", rel.relation_type).dimmed(),
                rel.issue_identifier.bold(),
                title_suffix,
                state_suffix.dimmed(),
                if rel.issue_url.is_empty() {
                    String::new()
                } else {
                    format!(" {}", rel.issue_url.dimmed())
                }
            );
        }
    }

    if let Some(ref desc) = issue.description {
        println!("\n{}", "Description:".bold());
        // Truncate very long descriptions for terminal display
        let display = if desc.len() > 2000 {
            format!(
                "{}...\n(truncated, {} chars total)",
                &desc[..2000],
                desc.len()
            )
        } else {
            desc.clone()
        };
        println!("{}", display);
    }

    if comments {
        let issue_comments = db.get_comments(&issue.id)?;
        if !issue_comments.is_empty() {
            println!("\n{} ({})", "Comments:".bold(), issue_comments.len());
            for comment in &issue_comments {
                println!(
                    "\n  {} {} {}",
                    "─".repeat(3),
                    comment.user_name.as_deref().unwrap_or("Unknown").bold(),
                    comment.created_at.dimmed()
                );
                for line in comment.body.lines() {
                    println!("  {}", line);
                }
            }
        } else {
            println!("\n{}", "No comments.".dimmed());
        }
    }

    Ok(())
}

fn membership_rows(issue: &rectilinear_core::db::Issue) -> [(&'static str, String); 3] {
    [
        (
            "Project",
            issue.project_name.clone().unwrap_or_else(|| "None".to_string()),
        ),
        (
            "Milestone",
            issue
                .project_milestone_name
                .clone()
                .unwrap_or_else(|| "None".to_string()),
        ),
        (
            "Cycle",
            issue.cycle_name.clone().unwrap_or_else(|| "None".to_string()),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use rectilinear_core::db::Issue;

    fn issue() -> Issue {
        Issue {
            id: "issue-1".into(),
            identifier: "ENG-1".into(),
            team_key: "ENG".into(),
            title: "Memberships".into(),
            description: None,
            state_name: "Todo".into(),
            state_type: "unstarted".into(),
            priority: 0,
            assignee_name: None,
            project_name: Some("API Reliability".into()),
            labels_json: "[]".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            content_hash: String::new(),
            synced_at: None,
            url: String::new(),
            branch_name: None,
            workspace_id: "default".into(),
            project_id: Some("project-1".into()),
            project_milestone_id: Some("milestone-1".into()),
            project_milestone_name: Some("Request tracing".into()),
            cycle_id: Some("cycle-1".into()),
            cycle_name: Some("Cycle 42".into()),
        }
    }

    #[test]
    fn show_memberships_include_project_milestone_and_cycle() {
        assert_eq!(
            membership_rows(&issue()),
            [
                ("Project", "API Reliability".into()),
                ("Milestone", "Request tracing".into()),
                ("Cycle", "Cycle 42".into()),
            ]
        );
    }

    #[test]
    fn show_memberships_are_explicit_when_unassigned() {
        let mut value = issue();
        value.project_name = None;
        value.project_milestone_name = None;
        value.cycle_name = None;
        assert!(membership_rows(&value)
            .into_iter()
            .all(|(_, value)| value == "None"));
    }
}
