use anyhow::{bail, Result};
use serde_json::json;

use crate::config::Config;
use crate::db::Database;
use crate::embedding::{self, Embedder};
use crate::linear::LinearClient;

pub async fn handle_mark_triaged(
    db: &Database,
    config: &Config,
    id: &str,
    priority: i32,
    title: Option<&str>,
    description: Option<&str>,
    comment: Option<&str>,
    state: Option<&str>,
    labels: Option<&[String]>,
    project: Option<&str>,
    json_output: bool,
) -> Result<()> {
    if !(1..=4).contains(&priority) {
        bail!("Priority must be 1 (Urgent), 2 (High), 3 (Medium), or 4 (Low)");
    }

    // Resolve from local DB
    let local_issue = db
        .get_issue(id)?
        .ok_or_else(|| anyhow::anyhow!("Issue '{}' not found", id))?;

    let client = LinearClient::new(config)?;

    // Re-fetch from Linear (staleness check)
    let (issue, issue_relations) = client.fetch_single_issue(&local_issue.id).await?;
    db.upsert_issue(&issue)?;
    db.upsert_relations(&issue.id, &issue_relations)?;

    // Already triaged?
    if issue.priority != 0 {
        let result = json!({
            "identifier": issue.identifier,
            "url": issue.url,
            "status": "already_triaged",
            "current_priority": issue.priority,
            "message": format!("{} was already prioritized — skipping", issue.identifier),
        });
        if json_output {
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            eprintln!(
                "{} was already prioritized as P{} — skipping",
                issue.identifier, issue.priority
            );
        }
        return Ok(());
    }

    // Modified since queued?
    if issue.content_hash != local_issue.content_hash {
        let result = json!({
            "identifier": issue.identifier,
            "url": issue.url,
            "status": "modified_since_queued",
            "message": format!("{} was modified since last sync — review before triaging", issue.identifier),
        });
        if json_output {
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            eprintln!(
                "{} was modified since last sync — review before triaging",
                issue.identifier
            );
        }
        // Re-embed with updated content
        reembed_issue(db, config, &issue).await;
        return Ok(());
    }

    // Resolve state name -> ID
    let state_id = if let Some(state_name) = state {
        Some(client.get_state_id(&issue.team_key, state_name).await?)
    } else {
        None
    };

    // Resolve label names -> IDs
    let label_ids = if let Some(label_names) = labels {
        Some(client.get_label_ids(label_names).await?)
    } else {
        None
    };

    // Resolve project name -> ID
    let project_id = if let Some(project_name) = project {
        if project_name.eq_ignore_ascii_case("none") {
            Some(String::new())
        } else {
            Some(client.get_project_id(project_name).await?)
        }
    } else {
        None
    };

    // Preserve image references in description
    let safe_description = description.map(|new_desc| match &issue.description {
        Some(original) => crate::mcp::preserve_images(original, new_desc),
        None => new_desc.to_string(),
    });

    // Update in Linear
    client
        .update_issue(
            &issue.id,
            title,
            safe_description.as_deref(),
            Some(priority),
            state_id.as_deref(),
            label_ids.as_deref(),
            project_id.as_deref(),
        )
        .await?;

    // Add comment if provided
    if let Some(comment_text) = comment {
        client.add_comment(&issue.id, comment_text).await?;
    }

    // Sync back
    let (updated, updated_relations) = client.fetch_single_issue(&issue.id).await?;
    db.upsert_issue(&updated)?;
    db.upsert_relations(&updated.id, &updated_relations)?;

    // Re-embed if content changed
    if title.is_some() || description.is_some() {
        reembed_issue(db, config, &updated).await;
    }

    let priority_label = match priority {
        1 => "Urgent",
        2 => "High",
        3 => "Medium",
        4 => "Low",
        _ => "Unknown",
    };

    let result = json!({
        "identifier": issue.identifier,
        "url": issue.url,
        "priority": priority,
        "priority_label": priority_label,
        "title": title.unwrap_or(&issue.title),
        "status": "triaged",
    });

    if json_output {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        eprintln!(
            "✓ {} marked as P{} ({})",
            issue.identifier, priority, priority_label
        );
        if let Some(t) = title {
            eprintln!("  Title: {}", t);
        }
        if comment.is_some() {
            eprintln!("  Comment added");
        }
    }

    Ok(())
}

/// Re-chunk and re-embed a single issue. Best-effort — failures are silently ignored.
async fn reembed_issue(db: &Database, config: &Config, issue: &crate::db::Issue) {
    let Ok(embedder) = Embedder::new(config) else {
        return;
    };
    let chunks = embedding::chunk_text(
        &issue.title,
        issue.description.as_deref().unwrap_or(""),
        512,
        64,
    );
    if let Ok(embeddings) = embedder.embed_batch(&chunks).await {
        let chunk_data: Vec<(usize, String, Vec<u8>)> = chunks
            .into_iter()
            .zip(embeddings)
            .enumerate()
            .map(|(i, (text, emb))| (i, text, embedding::embedding_to_bytes(&emb)))
            .collect();
        let _ = db.upsert_chunks(&issue.id, &chunk_data);
    }
}
