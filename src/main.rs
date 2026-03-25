mod cli;
mod llm;
mod mcp;

// Core modules re-exported from the library crate
pub use rectilinear_core::config;
pub use rectilinear_core::db;
pub use rectilinear_core::embedding;
pub use rectilinear_core::linear;
pub use rectilinear_core::search;

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

    /// Override the active workspace
    #[arg(long, global = true)]
    workspace: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage configuration (interactive if no subcommand given)
    Config {
        #[command(subcommand)]
        action: Option<ConfigAction>,
    },
    /// Manage workspaces
    Workspace {
        #[command(subcommand)]
        action: WorkspaceAction,
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
        /// Include completed/canceled issues (for archival prioritization)
        #[arg(long)]
        include_completed: bool,
    },
    /// Mark an issue as triaged: set priority and optionally update fields
    MarkTriaged {
        /// Issue identifier (e.g., CUT-42)
        id: String,
        /// Priority (1=Urgent, 2=High, 3=Medium, 4=Low)
        #[arg(short, long)]
        priority: i32,
        /// Improved title
        #[arg(long)]
        title: Option<String>,
        /// Updated description
        #[arg(short, long)]
        description: Option<String>,
        /// Triage comment
        #[arg(long)]
        comment: Option<String>,
        /// Set state (e.g., "Done", "Cancelled", "Duplicate")
        #[arg(long)]
        state: Option<String>,
        /// Set labels (comma-separated, replaces existing)
        #[arg(short, long, value_delimiter = ',')]
        labels: Option<Vec<String>>,
        /// Set project name (or "none" to remove)
        #[arg(long)]
        project: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
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
    /// Add a new workspace
    AddWorkspace,
    /// Remove a workspace
    RemoveWorkspace {
        /// Workspace name to remove
        name: String,
    },
}

#[derive(Subcommand)]
enum WorkspaceAction {
    /// Set the active workspace
    Assume {
        /// Workspace name
        name: String,
    },
    /// List configured workspaces
    List,
    /// Show the current active workspace
    Current,
}

/// Resolve which workspace to use: CLI flag > config resolution
fn resolve_workspace(cli_flag: Option<&str>, config: &config::Config) -> Result<String> {
    if let Some(name) = cli_flag {
        // Validate workspace exists
        if !config.workspaces.contains_key(name) {
            anyhow::bail!("Workspace '{}' not found in config", name);
        }
        Ok(name.to_string())
    } else {
        config.resolve_active_workspace()
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var("RECTILINEAR_DEBUG").is_ok() {
        eprintln!(
            "rectilinear {} ({})",
            env!("CARGO_PKG_VERSION"),
            env!("GIT_HASH")
        );
    }

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
            Some(ConfigAction::AddWorkspace) => cli::config_cmd::handle_add_workspace()?,
            Some(ConfigAction::RemoveWorkspace { name }) => cli::config_cmd::handle_remove_workspace(&name)?,
            None => cli::config_cmd::handle_interactive()?,
        },
        Commands::Workspace { action } => match action {
            WorkspaceAction::Assume { name } => {
                cli::workspace_cmd::handle_assume(&name, &config)?
            }
            WorkspaceAction::List => cli::workspace_cmd::handle_list(&config)?,
            WorkspaceAction::Current => cli::workspace_cmd::handle_current(&config)?,
        },
        Commands::Serve => {
            let db = db::Database::open(&config::Config::db_path()?)?;
            mcp::serve(db, config).await?;
        }
        _ => {
            // All other commands need the database and workspace
            let db = db::Database::open(&config::Config::db_path()?)?;
            let workspace = resolve_workspace(cli.workspace.as_deref(), &config)?;

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
                        &workspace,
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
                        &workspace,
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
                        &workspace,
                    )
                    .await?;
                }
                Commands::Show { id, json, comments } => {
                    cli::show_cmd::handle_show(&db, &config, &id, json, comments, &workspace)?;
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
                        &workspace,
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
                        &workspace,
                    )
                    .await?;
                }
                Commands::Embed { team, force } => {
                    cli::embed_cmd::handle_embed(&db, &config, team.as_deref(), force, &workspace)
                        .await?;
                }
                Commands::Triage {
                    team,
                    limit,
                    no_context,
                    include_completed,
                } => {
                    cli::triage_cmd::handle_triage(
                        &db,
                        &config,
                        team.as_deref(),
                        limit,
                        no_context,
                        include_completed,
                        &workspace,
                    )
                    .await?;
                }
                Commands::MarkTriaged {
                    id,
                    priority,
                    title,
                    description,
                    comment,
                    state,
                    labels,
                    project,
                    json,
                } => {
                    cli::mark_triaged_cmd::handle_mark_triaged(
                        &db,
                        &config,
                        &id,
                        priority,
                        title.as_deref(),
                        description.as_deref(),
                        comment.as_deref(),
                        state.as_deref(),
                        labels.as_deref(),
                        project.as_deref(),
                        json,
                        &workspace,
                    )
                    .await?;
                }
                Commands::Teams => {
                    cli::teams_cmd::handle_teams(&config, &workspace).await?;
                }
                Commands::Config { .. } | Commands::Workspace { .. } | Commands::Serve => {
                    unreachable!()
                }
            }
        }
    }

    Ok(())
}
