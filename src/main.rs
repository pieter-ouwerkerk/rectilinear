mod cli;
mod config;
mod db;
mod embedding;
mod linear;
mod llm;
mod mcp;
mod search;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "rectilinear",
    about = "Linear issue intelligence tool",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage configuration (interactive if no subcommand given)
    Config {
        #[command(subcommand)]
        action: Option<ConfigAction>,
    },
    /// Sync issues from Linear
    Sync {
        /// Team key (e.g., ENG)
        #[arg(short, long)]
        team: Option<String>,
        /// Force full sync (ignore cursor)
        #[arg(long)]
        full: bool,
        /// Generate embeddings after sync
        #[arg(long)]
        embed: bool,
        /// Include archived issues
        #[arg(long)]
        include_archived: bool,
    },
    /// Search issues
    Search {
        /// Search query
        query: String,
        /// Filter by team
        #[arg(short, long)]
        team: Option<String>,
        /// Filter by state
        #[arg(short, long)]
        state: Option<String>,
        /// Search mode: fts, vector, or hybrid
        #[arg(short, long, default_value = "hybrid")]
        mode: String,
        /// Max results
        #[arg(short, long)]
        limit: Option<usize>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Find similar issues (duplicate detection)
    Find {
        /// Text to find similar issues for
        #[arg(long)]
        similar: String,
        /// Filter by team
        #[arg(short, long)]
        team: Option<String>,
        /// Similarity threshold (0.0-1.0)
        #[arg(long, default_value = "0.7")]
        threshold: f32,
        /// Max results
        #[arg(short, long)]
        limit: Option<usize>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show issue details
    Show {
        /// Issue ID or identifier (e.g., ENG-123)
        id: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Include comments
        #[arg(long)]
        comments: bool,
    },
    /// Create a new issue in Linear
    Create {
        /// Team key (e.g., ENG)
        #[arg(short, long)]
        team: Option<String>,
        /// Issue title
        #[arg(long)]
        title: String,
        /// Issue description
        #[arg(short, long)]
        description: Option<String>,
        /// Priority (1=Urgent, 2=High, 3=Medium, 4=Low)
        #[arg(short, long)]
        priority: Option<i32>,
        /// Label names
        #[arg(short, long)]
        labels: Vec<String>,
    },
    /// Append to an existing issue
    Append {
        /// Issue ID or identifier
        id: String,
        /// Add a comment
        #[arg(long)]
        comment: Option<String>,
        /// Append to description
        #[arg(long)]
        description: Option<String>,
    },
    /// Generate embeddings for synced issues
    Embed {
        /// Filter by team
        #[arg(short, long)]
        team: Option<String>,
        /// Regenerate all embeddings
        #[arg(long)]
        force: bool,
    },
    /// Interactively triage unprioritized issues with AI
    Triage {
        /// Team key (e.g., ENG)
        #[arg(short, long)]
        team: Option<String>,
        /// Max issues to triage
        #[arg(short, long)]
        limit: Option<usize>,
        /// Skip similar-issue context
        #[arg(long)]
        no_context: bool,
    },
    /// List available Linear teams
    Teams,
    /// Start MCP server (stdio transport)
    Serve,
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Set a config value
    Set {
        /// Config key
        key: String,
        /// Config value
        value: String,
    },
    /// Get a config value
    Get {
        /// Config key
        key: String,
    },
    /// Show all config
    Show,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize logging for non-serve commands
    match &cli.command {
        Commands::Serve => {
            // MCP server uses stdio, so no logging to stdout
            tracing_subscriber::fmt()
                .with_env_filter("rectilinear=warn")
                .with_writer(std::io::stderr)
                .init();
        }
        _ => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "rectilinear=info".into()),
                )
                .with_writer(std::io::stderr)
                .without_time()
                .init();
        }
    }

    let config = config::Config::load()?;

    match cli.command {
        Commands::Config { action } => match action {
            Some(ConfigAction::Set { key, value }) => cli::config_cmd::handle_set(&key, &value)?,
            Some(ConfigAction::Get { key }) => cli::config_cmd::handle_get(&key)?,
            Some(ConfigAction::Show) => cli::config_cmd::handle_show()?,
            None => cli::config_cmd::handle_interactive()?,
        },
        Commands::Serve => {
            let db = db::Database::open(&config::Config::db_path()?)?;
            mcp::serve(db, config).await?;
        }
        _ => {
            // All other commands need the database
            let db = db::Database::open(&config::Config::db_path()?)?;

            match cli.command {
                Commands::Sync {
                    team,
                    full,
                    embed,
                    include_archived,
                } => {
                    cli::sync_cmd::handle_sync(
                        &db,
                        &config,
                        team.as_deref(),
                        full,
                        embed,
                        include_archived,
                    )
                    .await?;
                }
                Commands::Search {
                    query,
                    team,
                    state,
                    mode,
                    limit,
                    json,
                } => {
                    let mode = mode.parse()?;
                    let limit = limit.unwrap_or(config.search.default_limit);
                    cli::search_cmd::handle_search(
                        &db,
                        &config,
                        &query,
                        team.as_deref(),
                        state.as_deref(),
                        mode,
                        limit,
                        json,
                    )
                    .await?;
                }
                Commands::Find {
                    similar,
                    team,
                    threshold,
                    limit,
                    json,
                } => {
                    let limit = limit.unwrap_or(config.search.default_limit);
                    cli::search_cmd::handle_find_similar(
                        &db,
                        &config,
                        &similar,
                        team.as_deref(),
                        threshold,
                        limit,
                        json,
                    )
                    .await?;
                }
                Commands::Show { id, json, comments } => {
                    cli::show_cmd::handle_show(&db, &id, json, comments)?;
                }
                Commands::Create {
                    team,
                    title,
                    description,
                    priority,
                    labels,
                } => {
                    cli::create_cmd::handle_create(
                        &db,
                        &config,
                        team.as_deref(),
                        &title,
                        description.as_deref(),
                        priority,
                        &labels,
                    )
                    .await?;
                }
                Commands::Append {
                    id,
                    comment,
                    description,
                } => {
                    cli::append_cmd::handle_append(
                        &db,
                        &config,
                        &id,
                        comment.as_deref(),
                        description.as_deref(),
                    )
                    .await?;
                }
                Commands::Embed { team, force } => {
                    cli::embed_cmd::handle_embed(&db, &config, team.as_deref(), force).await?;
                }
                Commands::Triage {
                    team,
                    limit,
                    no_context,
                } => {
                    cli::triage_cmd::handle_triage(
                        &db,
                        &config,
                        team.as_deref(),
                        limit,
                        no_context,
                    )
                    .await?;
                }
                Commands::Teams => {
                    cli::teams_cmd::handle_teams(&config).await?;
                }
                Commands::Config { .. } | Commands::Serve => unreachable!(),
            }
        }
    }

    Ok(())
}
