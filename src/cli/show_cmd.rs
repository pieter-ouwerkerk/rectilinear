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
    if let Some(ref project) = issue.project_name {
        println!("  {} {}", "Project:".dimmed(), project);
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
