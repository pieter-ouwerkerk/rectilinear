use anyhow::Result;
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};

use crate::config::Config;
use crate::db::Database;
use crate::embedding::{self, Embedder};

pub async fn handle_embed(
    db: &Database,
    config: &Config,
    team: Option<&str>,
    force: bool,
) -> Result<()> {
    let embedder = Embedder::new(config)?;
    let team_key = team.or(config.linear.default_team.as_deref());

    println!(
        "{} Using {} backend ({} dimensions)",
        "Embedding:".bold(),
        embedder.backend_name(),
        embedder.dimensions()
    );

    // Check if embedding dimensions changed
    let dim_key = "embedding_dimensions";
    if let Some(stored_dim) = db.get_metadata(dim_key)? {
        let stored: usize = stored_dim.parse().unwrap_or(0);
        if stored != embedder.dimensions() {
            if !force {
                anyhow::bail!(
                    "Embedding dimensions changed ({} -> {}). Run with --force to regenerate all embeddings.",
                    stored,
                    embedder.dimensions()
                );
            }
        }
    }

    let issues = db.get_issues_needing_embedding(team_key, force)?;

    if issues.is_empty() {
        println!("{}", "All issues already have embeddings.".dimmed());
        return Ok(());
    }

    let pb = ProgressBar::new(issues.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} {msg}")
            .unwrap()
            .progress_chars("█▉▊▋▌▍▎▏ "),
    );

    // Prepare all chunks
    let mut all_texts: Vec<(usize, String, String)> = Vec::new(); // (issue_index, issue_id, chunk_text)

    for (i, issue) in issues.iter().enumerate() {
        let chunks = embedding::chunk_text(
            &issue.title,
            issue.description.as_deref().unwrap_or(""),
            512,
            64,
        );
        for chunk in chunks {
            all_texts.push((i, issue.id.clone(), chunk));
        }
    }

    // Batch embed
    let batch_size = 50;
    let mut chunk_embeddings: Vec<(String, usize, String, Vec<f32>)> = Vec::new();
    let mut chunk_counters: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    for batch in all_texts.chunks(batch_size) {
        let texts: Vec<String> = batch.iter().map(|(_, _, t)| t.clone()).collect();
        let embeddings = embedder.embed_batch(&texts).await?;

        for ((_, issue_id, text), emb) in batch.iter().zip(embeddings) {
            let idx = chunk_counters.entry(issue_id.clone()).or_insert(0);
            chunk_embeddings.push((issue_id.clone(), *idx, text.clone(), emb));
            *idx += 1;
        }

        pb.set_position(
            chunk_embeddings
                .iter()
                .map(|(id, _, _, _)| id.clone())
                .collect::<std::collections::HashSet<_>>()
                .len() as u64,
        );
    }

    // Group by issue and store
    let mut by_issue: std::collections::HashMap<String, Vec<(usize, String, Vec<u8>)>> =
        std::collections::HashMap::new();

    for (issue_id, idx, text, emb) in chunk_embeddings {
        let bytes = embedding::embedding_to_bytes(&emb);
        by_issue
            .entry(issue_id)
            .or_default()
            .push((idx, text, bytes));
    }

    for (issue_id, chunks) in &by_issue {
        db.upsert_chunks(issue_id, chunks)?;
    }

    // Store dimension info
    db.set_metadata(dim_key, &embedder.dimensions().to_string())?;

    pb.finish_with_message(format!(
        "{} Embedded {} issues ({} chunks)",
        "Done!".green().bold(),
        by_issue.len(),
        all_texts.len()
    ));

    let total_embedded = db.count_embedded_issues(team_key)?;
    let total_issues = db.count_issues(team_key)?;
    println!("Embedded: {}/{} issues", total_embedded, total_issues);

    Ok(())
}
