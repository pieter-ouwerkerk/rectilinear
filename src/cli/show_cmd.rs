use anyhow::Result;
use colored::Colorize;

use crate::db::Database;

pub fn handle_show(db: &Database, id: &str, json: bool, comments: bool) -> Result<()> {
    let issue = db.get_issue(id)?.ok_or_else(|| {
        anyhow::anyhow!(
            "Issue '{}' not found in local database. Try syncing first.",
            id
        )
    })?;

    if json {
        let mut value = serde_json::to_value(&issue)?;
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
