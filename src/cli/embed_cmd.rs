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
        if stored != embedder.dimensions() && !force {
            anyhow::bail!(
                "Embedding dimensions changed ({} -> {}). Run with --force to regenerate all embeddings.",
                stored,
                embedder.dimensions()
            );
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

    let embed_batch_size = 50;
    let mut total_chunks = 0usize;
    let mut embedded_count = 0usize;

    // Process issues one at a time: chunk, embed, flush to DB, then drop
    for issue in &issues {
        let chunks = embedding::chunk_text(
            &issue.title,
            issue.description.as_deref().unwrap_or(""),
            512,
            64,
        );

        // Embed this issue's chunks (sub-batch if needed)
        let mut all_embeddings: Vec<Vec<f32>> = Vec::with_capacity(chunks.len());
        for text_batch in chunks.chunks(embed_batch_size) {
            let texts: Vec<String> = text_batch.to_vec();
            let embeddings = embedder.embed_batch(&texts).await?;
            all_embeddings.extend(embeddings);
        }

        // Flush to DB immediately
        let chunk_data: Vec<(usize, String, Vec<u8>)> = chunks
            .into_iter()
            .zip(all_embeddings)
            .enumerate()
            .map(|(i, (text, emb))| (i, text, embedding::embedding_to_bytes(&emb)))
            .collect();

        total_chunks += chunk_data.len();
        db.upsert_chunks(&issue.id, &chunk_data)?;

        embedded_count += 1;
        pb.set_position(embedded_count as u64);
    }

    // Store dimension info
    db.set_metadata(dim_key, &embedder.dimensions().to_string())?;

    pb.finish_with_message(format!(
        "{} Embedded {} issues ({} chunks)",
        "Done!".green().bold(),
        embedded_count,
        total_chunks
    ));

    let total_embedded = db.count_embedded_issues(team_key)?;
    let total_issues = db.count_issues(team_key)?;
    println!("Embedded: {}/{} issues", total_embedded, total_issues);

    Ok(())
}
