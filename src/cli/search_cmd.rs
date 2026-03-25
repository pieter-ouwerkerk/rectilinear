use anyhow::Result;
use colored::Colorize;

use crate::config::Config;
use crate::db::Database;
use crate::embedding::Embedder;
use crate::search::{self, SearchMode};

pub async fn handle_search(
    db: &Database,
    config: &Config,
    query: &str,
    team: Option<&str>,
    state: Option<&str>,
    mode: SearchMode,
    limit: usize,
    json: bool,
    _workspace: &str,
) -> Result<()> {
    let embedder = if mode != SearchMode::Fts {
        match Embedder::new(config) {
            Ok(e) => Some(e),
            Err(_) => {
                if mode == SearchMode::Vector {
                    anyhow::bail!("Vector search requires an embedding backend. Configure GEMINI_API_KEY or use --mode fts");
                }
                None // Fall back to FTS-only for hybrid
            }
        }
    } else {
        None
    };

    let team_key = team.or(config.linear.default_team.as_deref());

    let results = search::search(
        db,
        query,
        mode,
        team_key,
        state,
        limit,
        embedder.as_ref(),
        config.search.rrf_k,
    )
    .await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&results)?);
        return Ok(());
    }

    if results.is_empty() {
        println!("{}", "No results found.".dimmed());
        return Ok(());
    }

    for result in &results {
        let priority = match result.priority {
            1 => "!!!".red().to_string(),
            2 => "!! ".yellow().to_string(),
            3 => "!  ".blue().to_string(),
            _ => "   ".to_string(),
        };

        let state_colored = match result.state_name.to_lowercase().as_str() {
            s if s.contains("done") || s.contains("complete") => result.state_name.green(),
            s if s.contains("progress") || s.contains("started") => result.state_name.yellow(),
            s if s.contains("cancel") => result.state_name.red().strikethrough(),
            _ => result.state_name.normal(),
        };

        println!(
            "{} {} {} [{}] {}",
            priority,
            result.identifier.bold(),
            result.title,
            state_colored,
            format!("({:.4})", result.score).dimmed(),
        );

        if let Some(sim) = result.similarity {
            print!("   similarity: {:.2}%", sim * 100.0);
        }
        if let Some(fts_rank) = result.fts_rank {
            print!("   fts:#{}", fts_rank);
        }
        if let Some(vec_rank) = result.vector_rank {
            print!("   vec:#{}", vec_rank);
        }
        if result.similarity.is_some() || result.fts_rank.is_some() || result.vector_rank.is_some()
        {
            println!();
        }
    }

    println!("\n{} {} results", "Found".dimmed(), results.len());

    Ok(())
}

pub async fn handle_find_similar(
    db: &Database,
    config: &Config,
    text: &str,
    team: Option<&str>,
    threshold: f32,
    limit: usize,
    json: bool,
    _workspace: &str,
) -> Result<()> {
    let embedder = Embedder::new(config)?;
    let team_key = team.or(config.linear.default_team.as_deref());

    let results = search::find_duplicates(
        db,
        text,
        team_key,
        threshold,
        limit,
        &embedder,
        config.search.rrf_k,
    )
    .await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&results)?);
        return Ok(());
    }

    if results.is_empty() {
        println!("{}", "No similar issues found above threshold.".dimmed());
        return Ok(());
    }

    println!(
        "{} (threshold: {:.0}%)\n",
        "Potential duplicates:".bold(),
        threshold * 100.0
    );

    // Find the longest identifier for alignment
    let max_id_len = results
        .iter()
        .map(|r| r.identifier.len())
        .max()
        .unwrap_or(0);

    for result in &results {
        let sim_pct = result.similarity.unwrap_or(0.0) * 100.0;
        let sim_bar = "█".repeat((sim_pct / 5.0) as usize);
        let sim_color = if sim_pct >= 90.0 {
            sim_bar.red()
        } else if sim_pct >= 70.0 {
            sim_bar.yellow()
        } else {
            sim_bar.green()
        };

        println!(
            "  {:<width$} {:>5.1}% {:<20} {}",
            result.identifier.bold(),
            sim_pct,
            sim_color,
            result.title,
            width = max_id_len,
        );
    }

    Ok(())
}
