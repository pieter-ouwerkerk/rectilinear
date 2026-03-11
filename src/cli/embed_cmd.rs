use anyhow::Result;
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use std::io::Write;

use crate::config::Config;
use crate::db::Database;
use crate::embedding::{self, Embedder};

/// Estimate tokens for a text (chars / 4, matching chunk_text's heuristic)
fn estimate_tokens(text: &str) -> usize {
    (text.len() + 3) / 4
}

/// Get current process RSS in MB (macOS/Linux)
fn rss_mb() -> Option<u64> {
    let pid = std::process::id();
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&output.stdout);
    let kb: u64 = s.trim().parse().ok()?;
    Some(kb / 1024)
}

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

    println!("{} issues to embed", issues.len());

    let pb = ProgressBar::new(issues.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} {msg}")
            .unwrap()
            .progress_chars("█▉▊▋▌▍▎▏ "),
    );

    // Batch limits: stay well under Gemini's 100-text / token limits
    let max_texts_per_batch = 50;
    let max_tokens_per_batch: usize = 20_000;

    let debug = std::env::var("RECTILINEAR_DEBUG").is_ok();

    let mut total_chunks = 0usize;
    let mut embedded_count = 0usize;
    let mut api_calls = 0usize;

    // Batch accumulator: (issue_id, chunk_index, text)
    let mut batch: Vec<(String, usize, String)> = Vec::new();
    let mut batch_tokens: usize = 0;

    // Completed embeddings awaiting flush, grouped by issue_id
    let mut pending: std::collections::HashMap<String, Vec<(usize, String, Vec<u8>)>> =
        std::collections::HashMap::new();
    // How many chunks each issue has total
    let mut expected_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    if debug {
        eprintln!("  RSS before loop: {}MB", rss_mb().unwrap_or(0));
    }

    for (issue_num, issue) in issues.iter().enumerate() {
        if debug && issue_num % 10 == 0 {
            eprint!(
                "  chunking issue {}/{}, batch: {}/{} texts (RSS: {}MB)\n",
                issue_num + 1,
                issues.len(),
                batch.len(),
                max_texts_per_batch,
                rss_mb().unwrap_or(0)
            );
            std::io::stderr().flush().ok();
        }
        let chunks = embedding::chunk_text(
            &issue.title,
            issue.description.as_deref().unwrap_or(""),
            512,
            64,
        );
        expected_counts.insert(issue.id.clone(), chunks.len());

        for (idx, text) in chunks.into_iter().enumerate() {
            let tokens = estimate_tokens(&text);

            // If adding this chunk would exceed limits, send the batch
            if !batch.is_empty()
                && (batch.len() >= max_texts_per_batch
                    || batch_tokens + tokens > max_tokens_per_batch)
            {
                // Embed the batch
                let texts: Vec<String> = batch.iter().map(|(_, _, t)| t.clone()).collect();
                api_calls += 1;
                if debug {
                    pb.suspend(|| {
                        let rss = rss_mb().map(|m| format!(" | RSS: {}MB", m)).unwrap_or_default();
                        eprintln!(
                            "  [batch {}] {} texts, ~{}k tokens, {} pending issues{}",
                            api_calls,
                            texts.len(),
                            batch_tokens / 1000,
                            pending.len(),
                            rss,
                        );
                    });
                }
                let embeddings = embedder.embed_batch(&texts).await?;

                for ((id, ci, ct), emb) in batch.drain(..).zip(embeddings) {
                    pending
                        .entry(id)
                        .or_default()
                        .push((ci, ct, embedding::embedding_to_bytes(&emb)));
                }
                batch_tokens = 0;

                // Flush any issues where all chunks are now embedded
                let done: Vec<String> = pending
                    .keys()
                    .filter(|id| {
                        pending.get(*id).map_or(false, |c| {
                            c.len() == *expected_counts.get(*id).unwrap_or(&0)
                        })
                    })
                    .cloned()
                    .collect();
                for id in done {
                    if let Some(chunks) = pending.remove(&id) {
                        total_chunks += chunks.len();
                        db.upsert_chunks(&id, &chunks)?;
                        embedded_count += 1;
                        pb.set_position(embedded_count as u64);
                    }
                }
            }

            batch.push((issue.id.clone(), idx, text));
            batch_tokens += tokens;
        }
    }

    // Flush remaining batch
    if !batch.is_empty() {
        let texts: Vec<String> = batch.iter().map(|(_, _, t)| t.clone()).collect();
        api_calls += 1;
        if debug {
            pb.suspend(|| {
                let rss = rss_mb().map(|m| format!(" | RSS: {}MB", m)).unwrap_or_default();
                eprintln!(
                    "  [batch {} final] {} texts, ~{}k tokens{}",
                    api_calls,
                    texts.len(),
                    batch_tokens / 1000,
                    rss,
                );
            });
        }
        let embeddings = embedder.embed_batch(&texts).await?;

        for ((id, ci, ct), emb) in batch.drain(..).zip(embeddings) {
            pending
                .entry(id)
                .or_default()
                .push((ci, ct, embedding::embedding_to_bytes(&emb)));
        }
    }

    // Flush all remaining issues
    for (id, chunks) in pending.drain() {
        total_chunks += chunks.len();
        db.upsert_chunks(&id, &chunks)?;
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
