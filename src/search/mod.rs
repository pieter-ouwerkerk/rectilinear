use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::db::{Database, FtsResult};
use crate::embedding::{self, Embedder};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SearchMode {
    Fts,
    Vector,
    Hybrid,
}

impl std::str::FromStr for SearchMode {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "fts" => Ok(Self::Fts),
            "vector" => Ok(Self::Vector),
            "hybrid" => Ok(Self::Hybrid),
            _ => anyhow::bail!("Invalid search mode: {}. Use fts, vector, or hybrid", s),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub issue_id: String,
    pub identifier: String,
    pub title: String,
    pub state_name: String,
    pub priority: i32,
    pub score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fts_rank: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector_rank: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similarity: Option<f32>,
}

/// Perform a search using the specified mode
pub async fn search(
    db: &Database,
    query: &str,
    mode: SearchMode,
    team_key: Option<&str>,
    state_filter: Option<&str>,
    limit: usize,
    embedder: Option<&Embedder>,
    rrf_k: u32,
) -> Result<Vec<SearchResult>> {
    let results = match mode {
        SearchMode::Fts => fts_search(db, query, limit * 2)?,
        SearchMode::Vector => {
            let embedder = embedder
                .ok_or_else(|| anyhow::anyhow!("Embedder required for vector search"))?;
            vector_search(db, query, team_key, limit * 2, embedder).await?
        }
        SearchMode::Hybrid => {
            let fts_results = fts_search(db, query, limit * 3)?;

            if let Some(embedder) = embedder {
                let vec_results =
                    vector_search(db, query, team_key, limit * 3, embedder).await?;
                reciprocal_rank_fusion(fts_results, vec_results, rrf_k, 0.3, 0.7)
            } else {
                // Fall back to FTS-only if no embedder
                fts_results
            }
        }
    };

    // Post-filter
    let results: Vec<_> = results
        .into_iter()
        .filter(|r| {
            if let Some(team) = team_key {
                // We'd need team info - for now skip team filter in post-filter
                // since FTS doesn't return team info directly
                true
            } else {
                true
            }
        })
        .filter(|r| {
            if let Some(state) = state_filter {
                r.state_name.to_lowercase().contains(&state.to_lowercase())
            } else {
                true
            }
        })
        .take(limit)
        .collect();

    Ok(results)
}

fn fts_search(db: &Database, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
    // Escape FTS5 special characters and build query
    let fts_query = build_fts_query(query);
    let fts_results = db.fts_search(&fts_query, limit)?;

    Ok(fts_results
        .into_iter()
        .enumerate()
        .map(|(rank, r)| SearchResult {
            issue_id: r.issue_id,
            identifier: r.identifier,
            title: r.title,
            state_name: r.state_name,
            priority: r.priority,
            score: -r.bm25_score, // BM25 returns negative scores, lower = better
            fts_rank: Some(rank + 1),
            vector_rank: None,
            similarity: None,
        })
        .collect())
}

async fn vector_search(
    db: &Database,
    query: &str,
    team_key: Option<&str>,
    limit: usize,
    embedder: &Embedder,
) -> Result<Vec<SearchResult>> {
    let query_embedding = embedder.embed_single(query).await?;

    let chunks = if let Some(team) = team_key {
        db.get_chunks_for_team(team)?
    } else {
        db.get_all_chunks()?
    };

    // Compute similarity for each chunk, take max per issue
    let mut issue_max_sim: HashMap<String, (f32, String)> = HashMap::new(); // issue_id -> (max_sim, identifier)

    for chunk in &chunks {
        let chunk_embedding = embedding::bytes_to_embedding(&chunk.embedding);
        let sim = embedding::cosine_similarity(&query_embedding, &chunk_embedding);

        let entry = issue_max_sim
            .entry(chunk.issue_id.clone())
            .or_insert((0.0, chunk.identifier.clone()));
        if sim > entry.0 {
            entry.0 = sim;
        }
    }

    // Sort by similarity descending
    let mut results: Vec<_> = issue_max_sim.into_iter().collect();
    results.sort_by(|a, b| b.1 .0.partial_cmp(&a.1 .0).unwrap());

    // Get issue details for top results
    let results: Vec<_> = results
        .into_iter()
        .take(limit)
        .enumerate()
        .filter_map(|(rank, (issue_id, (sim, identifier)))| {
            let issue = db.get_issue(&issue_id).ok()??;
            Some(SearchResult {
                issue_id,
                identifier: issue.identifier,
                title: issue.title,
                state_name: issue.state_name,
                priority: issue.priority,
                score: sim as f64,
                fts_rank: None,
                vector_rank: Some(rank + 1),
                similarity: Some(sim),
            })
        })
        .collect();

    Ok(results)
}

/// Reciprocal Rank Fusion combining FTS and vector results
fn reciprocal_rank_fusion(
    fts_results: Vec<SearchResult>,
    vec_results: Vec<SearchResult>,
    k: u32,
    fts_weight: f64,
    vec_weight: f64,
) -> Vec<SearchResult> {
    let mut scores: HashMap<String, (f64, SearchResult)> = HashMap::new();
    let k = k as f64;

    for (rank, result) in fts_results.into_iter().enumerate() {
        let rrf_score = fts_weight / (k + (rank + 1) as f64);
        let entry = scores
            .entry(result.issue_id.clone())
            .or_insert((0.0, result.clone()));
        entry.0 += rrf_score;
        entry.1.fts_rank = result.fts_rank;
    }

    for (rank, result) in vec_results.into_iter().enumerate() {
        let rrf_score = vec_weight / (k + (rank + 1) as f64);
        let entry = scores
            .entry(result.issue_id.clone())
            .or_insert((0.0, result.clone()));
        entry.0 += rrf_score;
        entry.1.vector_rank = result.vector_rank;
        entry.1.similarity = result.similarity;
    }

    let mut results: Vec<_> = scores
        .into_values()
        .map(|(score, mut result)| {
            result.score = score;
            result
        })
        .collect();

    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
    results
}

/// Find duplicates for a given text using vector similarity
pub async fn find_duplicates(
    db: &Database,
    text: &str,
    team_key: Option<&str>,
    threshold: f32,
    limit: usize,
    embedder: &Embedder,
    rrf_k: u32,
) -> Result<Vec<SearchResult>> {
    let mut results = search(
        db,
        text,
        SearchMode::Hybrid,
        team_key,
        None,
        limit,
        Some(embedder),
        rrf_k,
    )
    .await?;

    // For duplicate finding, also do a pure vector search and merge
    let vec_results = vector_search(db, text, team_key, limit, embedder).await?;

    // Keep results above threshold
    results.retain(|r| r.similarity.unwrap_or(0.0) >= threshold || r.score > 0.01);

    Ok(results)
}

/// Build an FTS5 query from free-text input
fn build_fts_query(input: &str) -> String {
    // Split into words, wrap each in quotes to handle special chars
    let words: Vec<_> = input
        .split_whitespace()
        .map(|w| {
            // Remove FTS5 special characters
            let clean: String = w
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
                .collect();
            if clean.is_empty() {
                None
            } else {
                Some(format!("\"{}\"", clean))
            }
        })
        .flatten()
        .collect();

    if words.is_empty() {
        "\"\"".to_string()
    } else {
        words.join(" OR ")
    }
}
