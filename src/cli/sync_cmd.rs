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
) -> Result<()> {
    let client = LinearClient::new(config)?;

    let team_key = team
        .or(config.linear.default_team.as_deref())
        .ok_or_else(|| {
            anyhow::anyhow!("No team specified. Use --team or set default-team in config")
        })?;

    let sync_type = if full { "Full" } else { "Incremental" };
    let is_first = !db.is_full_sync_done(team_key)?;

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
    let count = client
        .sync_team(db, team_key, do_full, include_archived, Some(&pb))
        .await?;

    pb.finish_with_message(format!(
        "{} Synced {} issues for team {}",
        "Done!".green().bold(),
        count,
        team_key.bold()
    ));

    let total = db.count_issues(Some(team_key))?;
    println!(
        "Total issues in database for {}: {}",
        team_key.bold(),
        total
    );

    if embed {
        println!();
        crate::cli::embed_cmd::handle_embed(db, config, Some(team_key), false).await?;
    }

    Ok(())
}
