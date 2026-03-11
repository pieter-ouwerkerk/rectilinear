use anyhow::Result;
use colored::Colorize;
use serde::Deserialize;
use std::io::{self, Write};

use crate::config::Config;
use crate::db::{Database, Issue};
use crate::embedding::Embedder;
use crate::linear::LinearClient;
use crate::llm::{self, LlmClient, Message};
use crate::search::{self, SearchMode};

const SYSTEM_PROMPT: &str = r#"You are a seasoned engineering project manager helping triage Linear issues. Your job is to analyze each issue and help the user determine the right priority, title, and description.

When presented with an issue, ask 2-3 focused clarifying questions to understand:
- Impact: How many users are affected? Is it blocking?
- Frequency: How often does this occur?
- Severity: Is it a crash, degraded experience, or cosmetic?

Keep your questions concise. After the user answers (or says /done), you will be asked to propose changes.

Priority levels:
1 = Urgent (production down, data loss, security)
2 = High (major feature broken, significant user impact)
3 = Medium (degraded experience, workarounds exist)
4 = Low (minor polish, nice-to-have)
"#;

const EXTRACTION_PROMPT: &str = r#"Based on the conversation above, propose the final priority, title, and description for this issue. Respond with ONLY a JSON object in this exact format, no other text:

{"priority": <1-4>, "title": "<improved title>", "description": "<improved description in markdown>"}

Rules:
- priority must be an integer 1-4
- title should be clear and specific
- description should incorporate context from the conversation
- Keep the description concise but informative"#;

#[derive(Debug, Deserialize)]
struct TriageProposal {
    priority: i32,
    title: String,
    description: String,
}

pub async fn handle_triage(
    db: &Database,
    config: &Config,
    team: Option<&str>,
    limit: Option<usize>,
    no_context: bool,
) -> Result<()> {
    let llm = LlmClient::new(config)?;
    let linear = LinearClient::new(config)?;

    let team_key = team
        .or(config.linear.default_team.as_deref())
        .ok_or_else(|| {
            anyhow::anyhow!("No team specified. Use --team or set default-team in config")
        })?;

    let issues = db.get_unprioritized_issues(Some(team_key))?;
    if issues.is_empty() {
        println!(
            "{} No unprioritized issues found in {}",
            "✓".green().bold(),
            team_key.bold()
        );
        return Ok(());
    }

    let total = if let Some(lim) = limit {
        lim.min(issues.len())
    } else {
        issues.len()
    };

    println!(
        "\nFound {} unprioritized issues in {}\n",
        total.to_string().bold(),
        team_key.bold()
    );

    // Try to create embedder for similar issue context
    let embedder = if no_context {
        None
    } else {
        Embedder::new(config).ok()
    };

    let mut applied = 0;
    let mut skipped = 0;

    for (idx, issue) in issues.iter().take(total).enumerate() {
        println!(
            "━━━ [{}/{}] {}: {} ━━━",
            idx + 1,
            total,
            issue.identifier.bold(),
            issue.title
        );
        println!(
            "  State: {} | Assignee: {} | Created: {}",
            issue.state_name.cyan(),
            issue
                .assignee_name
                .as_deref()
                .unwrap_or("Unassigned")
                .yellow(),
            &issue.created_at[..10],
        );
        if let Some(desc) = &issue.description {
            let preview: String = desc.chars().take(200).collect();
            println!("  Description: {}", preview.dimmed());
        }

        // Find similar issues for context
        let similar_context = if let Some(ref embedder) = embedder {
            build_similar_context(db, issue, embedder, config).await
        } else {
            String::new()
        };

        if !similar_context.is_empty() {
            println!("\n  {}", "Similar issues:".dimmed());
            // Print a brief version for the user
            for line in similar_context.lines().take(5) {
                if line.starts_with("  - ") {
                    println!("  {}", line.dimmed());
                }
            }
        }

        println!();

        // Build conversation
        let issue_context = format!(
            "Issue: {} - {}\nState: {}\nAssignee: {}\nCreated: {}\nDescription: {}\n{}",
            issue.identifier,
            issue.title,
            issue.state_name,
            issue.assignee_name.as_deref().unwrap_or("Unassigned"),
            &issue.created_at[..10],
            issue.description.as_deref().unwrap_or("(none)"),
            if similar_context.is_empty() {
                String::new()
            } else {
                format!("\nSimilar issues for context:\n{}", similar_context)
            }
        );

        let system = format!("{}\n\nCurrent issue context:\n{}", SYSTEM_PROMPT, issue_context);
        let mut messages: Vec<Message> = vec![Message::user(
            "Please analyze this issue and ask me clarifying questions to help determine the right priority.",
        )];

        // Initial LLM response
        match llm.generate(&messages, &system).await {
            Ok(response) => {
                println!("{} {}\n", "AI:".green().bold(), response);
                messages.push(Message::assistant(&response));
            }
            Err(e) => {
                eprintln!("{} LLM error: {}", "✗".red(), e);
                skipped += 1;
                println!();
                continue;
            }
        }

        // Interactive conversation loop
        loop {
            print!("{} ", ">".blue().bold());
            io::stdout().flush()?;

            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            let input = input.trim();

            if input.eq_ignore_ascii_case("q") || input.eq_ignore_ascii_case("quit") {
                println!("\n{} Triage session ended", "→".blue());
                print_summary(applied, skipped, total - idx);
                return Ok(());
            }

            if input.eq_ignore_ascii_case("s") || input.eq_ignore_ascii_case("skip") {
                skipped += 1;
                println!();
                break;
            }

            if input == "/done" || input.is_empty() {
                // Make extraction call
                messages.push(Message::user(EXTRACTION_PROMPT));

                match llm.generate(&messages, &system).await {
                    Ok(response) => {
                        let json_str = llm::extract_json(&response);
                        match serde_json::from_str::<TriageProposal>(json_str) {
                            Ok(proposal) => {
                                match prompt_apply(issue, &proposal, &linear, db).await {
                                    TriageAction::Applied => applied += 1,
                                    TriageAction::Skipped => skipped += 1,
                                    TriageAction::Quit => {
                                        print_summary(applied, skipped, total - idx);
                                        return Ok(());
                                    }
                                }
                            }
                            Err(e) => {
                                // Retry once
                                eprintln!(
                                    "{} Failed to parse proposal ({}), retrying...",
                                    "⚠".yellow(),
                                    e
                                );
                                messages.push(Message::assistant(&response));
                                messages.push(Message::user(
                                    "Please respond with ONLY a valid JSON object: {\"priority\": <1-4>, \"title\": \"...\", \"description\": \"...\"}",
                                ));
                                match llm.generate(&messages, &system).await {
                                    Ok(retry_response) => {
                                        let json_str = llm::extract_json(&retry_response);
                                        match serde_json::from_str::<TriageProposal>(json_str) {
                                            Ok(proposal) => {
                                                match prompt_apply(
                                                    issue, &proposal, &linear, db,
                                                )
                                                .await
                                                {
                                                    TriageAction::Applied => applied += 1,
                                                    TriageAction::Skipped => skipped += 1,
                                                    TriageAction::Quit => {
                                                        print_summary(
                                                            applied,
                                                            skipped,
                                                            total - idx,
                                                        );
                                                        return Ok(());
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                eprintln!(
                                                    "{} Could not parse proposal: {}",
                                                    "✗".red(),
                                                    e
                                                );
                                                skipped += 1;
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        eprintln!("{} LLM error on retry: {}", "✗".red(), e);
                                        skipped += 1;
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("{} LLM error: {}", "✗".red(), e);
                        skipped += 1;
                    }
                }
                println!();
                break;
            }

            // Normal conversation turn
            messages.push(Message::user(input));
            match llm.generate(&messages, &system).await {
                Ok(response) => {
                    println!("\n{} {}\n", "AI:".green().bold(), response);
                    messages.push(Message::assistant(&response));
                }
                Err(e) => {
                    eprintln!("{} LLM error: {}", "✗".red(), e);
                }
            }
        }
    }

    print_summary(applied, skipped, 0);
    Ok(())
}

enum TriageAction {
    Applied,
    Skipped,
    Quit,
}

async fn prompt_apply(
    issue: &Issue,
    proposal: &TriageProposal,
    linear: &LinearClient,
    db: &Database,
) -> TriageAction {
    let priority_label = match proposal.priority {
        1 => "Urgent",
        2 => "High",
        3 => "Medium",
        4 => "Low",
        _ => "Unknown",
    };

    println!("\n  {} Proposed changes:", "→".blue());
    println!(
        "  Priority: {} → {} ({})",
        "No priority".dimmed(),
        priority_label.bold(),
        proposal.priority
    );
    if proposal.title != issue.title {
        println!(
            "  Title:    {} → {}",
            issue.title.dimmed(),
            proposal.title.bold()
        );
    }
    let desc_changed = issue.description.as_deref().unwrap_or("") != proposal.description;
    if desc_changed {
        let preview: String = proposal.description.chars().take(100).collect();
        println!("  Description: {}", format!("{}...", preview).dimmed());
    }

    print!("\n  {} ", "[A]pply / [S]kip / [Q]uit?".bold());
    io::stdout().flush().ok();

    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    let input = input.trim().to_lowercase();

    match input.as_str() {
        "a" | "apply" | "" => {
            let title = if proposal.title != issue.title {
                Some(proposal.title.as_str())
            } else {
                None
            };
            let description = if desc_changed {
                Some(proposal.description.as_str())
            } else {
                None
            };

            match linear
                .update_issue(
                    &issue.id,
                    title,
                    description,
                    Some(proposal.priority),
                    None,
                )
                .await
            {
                Ok(()) => {
                    // Sync back
                    match linear.fetch_single_issue(&issue.id).await {
                        Ok(updated) => {
                            db.upsert_issue(&updated).ok();
                        }
                        Err(e) => {
                            eprintln!("  {} Failed to sync back: {}", "⚠".yellow(), e);
                        }
                    }
                    println!(
                        "  {} Updated {} on Linear",
                        "✓".green().bold(),
                        issue.identifier
                    );
                    TriageAction::Applied
                }
                Err(e) => {
                    eprintln!(
                        "  {} Failed to update {}: {}",
                        "✗".red(),
                        issue.identifier,
                        e
                    );
                    TriageAction::Skipped
                }
            }
        }
        "q" | "quit" => TriageAction::Quit,
        _ => TriageAction::Skipped,
    }
}

async fn build_similar_context(
    db: &Database,
    issue: &Issue,
    embedder: &Embedder,
    config: &Config,
) -> String {
    let query_text = format!(
        "{} {}",
        issue.title,
        issue.description.as_deref().unwrap_or("")
    );

    let results = search::search(
        db,
        &query_text,
        SearchMode::Vector,
        Some(&issue.team_key),
        None,
        5,
        Some(embedder),
        config.search.rrf_k,
    )
    .await;

    match results {
        Ok(results) => {
            let mut context = String::new();
            for r in results.iter().filter(|r| r.issue_id != issue.id).take(3) {
                let plabel = match r.priority {
                    1 => "Urgent",
                    2 => "High",
                    3 => "Medium",
                    4 => "Low",
                    _ => "No priority",
                };
                context.push_str(&format!(
                    "  - {} ({}): {}\n",
                    r.identifier, plabel, r.title
                ));
            }
            context
        }
        Err(_) => String::new(),
    }
}

fn print_summary(applied: usize, skipped: usize, remaining: usize) {
    println!("\n━━━ Session Summary ━━━");
    println!("  Applied: {}", applied.to_string().green().bold());
    println!("  Skipped: {}", skipped.to_string().yellow());
    if remaining > 0 {
        println!("  Remaining: {}", remaining.to_string().dimmed());
    }
}
