use anyhow::Result;
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};

use crate::config::Config;
use crate::db::Database;
use crate::linear::LinearClient;

pub async fn handle_sync(
    db: &Database,
    config: &Config,
    team: Option<&str>,
    full: bool,
    embed: bool,
    include_archived: bool,
    workspace: &str,
) -> Result<()> {
    let api_key = config.workspace_api_key(workspace)?;
    let client = LinearClient::with_api_key(&api_key);

    let team_key = match team.or(config.workspace_default_team(workspace)?.as_deref()) {
        Some(t) => t.to_string(),
        None => {
            // No team specified — fetch available teams and let user pick
            println!("Fetching teams from Linear...");
            let teams = client.list_teams().await?;
            if teams.is_empty() {
                anyhow::bail!("No teams found in this workspace");
            }
            if teams.len() == 1 {
                let key = teams[0].key.clone();
                println!("Using team {} ({})", key.bold(), teams[0].name);
                key
            } else {
                println!("{}", "Available teams:".bold());
                for (i, t) in teams.iter().enumerate() {
                    println!("  {} {} — {}", format!("[{}]", i + 1).dimmed(), t.key.bold(), t.name);
                }
                print!("\nSelect team (1-{}): ", teams.len());
                std::io::Write::flush(&mut std::io::stdout())?;
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;
                let idx: usize = input.trim().parse::<usize>()
                    .map_err(|_| anyhow::anyhow!("Invalid selection"))?;
                if idx < 1 || idx > teams.len() {
                    anyhow::bail!("Selection out of range");
                }
                teams[idx - 1].key.clone()
            }
        }
    };

    // Ensure workspace row exists in DB
    db.upsert_workspace(workspace, None, None)?;

    let sync_type = if full { "Full" } else { "Incremental" };
    let is_first = !db.is_full_sync_done(workspace, &team_key)?;

    if is_first && !full {
        println!(
            "{} First sync for team {} — performing full sync",
            "Info:".blue().bold(),
            team_key.bold()
        );
    }

    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.green} {msg}")
            .unwrap(),
    );
    pb.set_message(format!(
        "{} sync for team {}...",
        if is_first { "Full" } else { sync_type },
        team_key
    ));
    pb.enable_steady_tick(std::time::Duration::from_millis(100));

    let do_full = full || is_first;
    let progress_cb = |total: usize| {
        pb.set_message(format!("{} issues synced", total));
    };
    let count = client
        .sync_team(db, &team_key, workspace, do_full, include_archived, Some(&progress_cb))
        .await?;

    pb.finish_with_message(format!(
        "{} Synced {} issues for team {}",
        "Done!".green().bold(),
        count,
        team_key.bold()
    ));

    let total = db.count_issues(Some(&team_key), workspace)?;
    println!(
        "Total issues in database for {}: {}",
        team_key.bold(),
        total
    );

    if count == 0 && is_first {
        eprintln!(
            "\n{} No issues found for team \"{}\". Is the team key correct?",
            "Warning:".yellow().bold(),
            team_key
        );
        eprintln!(
            "Run {} to see available team keys.",
            "rectilinear teams".bold()
        );
    }

    if embed {
        println!();
        crate::cli::embed_cmd::handle_embed(db, config, Some(&team_key), false, workspace).await?;
    }

    Ok(())
}
