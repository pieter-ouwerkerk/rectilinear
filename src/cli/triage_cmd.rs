use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use serde::Deserialize;
use std::io;
use std::sync::Arc;
use tokio::sync::mpsc;
use tui_textarea::TextArea;

use crate::config::Config;
use crate::db::{Database, Issue};
use crate::embedding::Embedder;
use crate::linear::LinearClient;
use crate::llm::{self, LlmClient, Message};
use crate::search::{self, SearchMode};

const QUESTIONS_PROMPT: &str = r#"You are a seasoned engineering project manager helping triage Linear issues. Analyze the issue and produce 2-4 focused clarifying questions to determine the right priority.

Respond with ONLY a JSON object in this format:
{"questions": [{"id": "<short_snake_case_id>", "label": "<Short Label>", "question": "<The question>"}]}

You may use Markdown in the question text: **bold** for emphasis, *italics* for examples, `backticks` for code/identifiers.

Focus on: impact (users affected, blocking?), frequency (how often?), severity (crash vs cosmetic), business context.

Priority levels:
1 = Urgent (production down, data loss, security)
2 = High (major feature broken, significant user impact)
3 = Medium (degraded experience, workarounds exist)
4 = Low (minor polish, nice-to-have)"#;

const EXTRACTION_PROMPT: &str = r#"Based on the issue and the user's answers, propose the final priority, title, and description. Respond with ONLY a JSON object:

{"priority": <1-4>, "title": "<improved title>", "description": "<improved description in markdown>"}

Rules:
- priority must be an integer 1-4
- title should be clear and specific
- description should incorporate context from the answers
- Keep the description concise but informative"#;

const DEFAULT_QUESTIONS: &[(&str, &str, &str)] = &[
    ("impact", "Impact", "How many users are affected? Is it blocking?"),
    ("frequency", "Frequency", "How often does this occur?"),
    (
        "severity",
        "Severity",
        "Is this a crash, degraded experience, or cosmetic?",
    ),
];

#[derive(Debug, Deserialize)]
struct QuestionsResponse {
    questions: Vec<Question>,
}

#[derive(Debug, Clone, Deserialize)]
struct Question {
    #[allow(dead_code)]
    id: String,
    label: String,
    question: String,
}

#[derive(Debug, Deserialize)]
struct TriageProposal {
    priority: i32,
    title: String,
    description: String,
}

enum Phase {
    Loading,
    Answering,
    Submitting,
    Reviewing(TriageProposal),
    Applying,
}

enum AppEvent {
    Key(KeyEvent),
    QuestionsReady(Vec<Question>),
    SimilarReady(String),
    ProposalReady(Result<TriageProposal>),
    ApplyDone(Result<()>),
}

struct App<'a> {
    issues: Vec<Issue>,
    current_index: usize,
    max_issues: usize,
    phase: Phase,
    questions: Vec<Question>,
    answers: Vec<TextArea<'a>>,
    focused_field: usize,
    similar_context: String,
    applied: usize,
    skipped: usize,
    status: String,
    should_quit: bool,
}

impl<'a> App<'a> {
    fn new(issues: Vec<Issue>, limit: Option<usize>) -> Self {
        let max_issues = limit.map(|l| l.min(issues.len())).unwrap_or(issues.len());
        Self {
            issues,
            current_index: 0,
            max_issues,
            phase: Phase::Loading,
            questions: Vec::new(),
            answers: Vec::new(),
            focused_field: 0,
            similar_context: String::new(),
            applied: 0,
            skipped: 0,
            status: "Loading questions...".into(),
            should_quit: false,
        }
    }

    fn current_issue(&self) -> &Issue {
        &self.issues[self.current_index]
    }

    fn remaining(&self) -> usize {
        self.max_issues.saturating_sub(self.current_index + 1)
    }

    fn set_questions(&mut self, questions: Vec<Question>) {
        self.answers = questions
            .iter()
            .map(|q| {
                let mut ta = TextArea::default();
                ta.set_placeholder_text(&q.question);
                ta
            })
            .collect();
        self.questions = questions;
        self.focused_field = 0;
        self.phase = Phase::Answering;
        self.status = "Tab=next field | Ctrl+Enter=submit | Esc=skip | Ctrl+C=quit".into();
    }

    fn advance(&mut self) {
        self.current_index += 1;
        if self.current_index >= self.max_issues {
            self.should_quit = true;
        } else {
            self.phase = Phase::Loading;
            self.questions.clear();
            self.answers.clear();
            self.similar_context.clear();
            self.focused_field = 0;
            self.status = "Loading questions...".into();
        }
    }

    fn collect_qa_text(&self) -> String {
        self.questions
            .iter()
            .zip(self.answers.iter())
            .map(|(q, a)| {
                let answer = a.lines().join("\n");
                format!("{}: {}", q.label, if answer.is_empty() { "(skipped)" } else { &answer })
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
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
        let total = db.count_issues(Some(team_key))?;
        if total == 0 {
            eprintln!(
                "No issues found for team \"{}\". Have you run `rectilinear sync --team {}`?",
                team_key, team_key
            );
        } else {
            eprintln!("No unprioritized issues found in {}", team_key);
        }
        return Ok(());
    }

    let embedder: Option<Arc<Embedder>> = if no_context {
        None
    } else {
        Embedder::new(config).ok().map(Arc::new)
    };

    // Install panic hook to restore terminal
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        terminal::disable_raw_mode().ok();
        crossterm::execute!(io::stdout(), LeaveAlternateScreen).ok();
        original_hook(info);
    }));

    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();

    // Spawn keyboard reader
    let tx_key = tx.clone();
    tokio::spawn(async move {
        loop {
            if event::poll(std::time::Duration::from_millis(50)).unwrap_or(false) {
                if let Ok(Event::Key(key)) = event::read() {
                    if tx_key.send(AppEvent::Key(key)).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut app = App::new(issues, limit);

    // Start first issue
    spawn_questions(&app, &llm, &tx);
    if !no_context {
        spawn_similar(&app, db, &embedder, config, &tx);
    }

    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        if let Some(evt) = rx.recv().await {
            match evt {
                AppEvent::Key(key) => {
                    handle_key(
                        &mut app, key, &llm, &linear, db, config, &embedder, no_context, &tx,
                    )
                    .await;
                }
                AppEvent::QuestionsReady(questions) => {
                    app.set_questions(questions);
                }
                AppEvent::SimilarReady(ctx) => {
                    app.similar_context = ctx;
                }
                AppEvent::ProposalReady(result) => match result {
                    Ok(proposal) => {
                        app.status = "a=accept | s=skip | q=quit".into();
                        app.phase = Phase::Reviewing(proposal);
                    }
                    Err(e) => {
                        app.status = format!("Proposal failed: {}. Esc=skip", e);
                        app.phase = Phase::Answering;
                    }
                },
                AppEvent::ApplyDone(result) => {
                    if let Err(e) = result {
                        app.status = format!("Apply failed: {}", e);
                    } else {
                        app.applied += 1;
                    }
                    app.advance();
                    if !app.should_quit {
                        spawn_questions(&app, &llm, &tx);
                        if !no_context {
                            spawn_similar(&app, db, &embedder, config, &tx);
                        }
                    }
                }
            }
        }

        if app.should_quit {
            break;
        }
    }

    // Cleanup terminal
    terminal::disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

    // Print summary to normal stdout
    println!("\n━━━ Triage Session Summary ━━━");
    println!("  Applied: {}", app.applied);
    println!("  Skipped: {}", app.skipped);
    if app.remaining() > 0 {
        println!("  Remaining: {}", app.remaining());
    }

    Ok(())
}

async fn handle_key(
    app: &mut App<'_>,
    key: KeyEvent,
    llm: &LlmClient,
    linear: &LinearClient,
    db: &Database,
    config: &Config,
    embedder: &Option<Arc<Embedder>>,
    no_context: bool,
    tx: &mpsc::UnboundedSender<AppEvent>,
) {
    // Global: Ctrl+C always quits
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.should_quit = true;
        return;
    }

    match &app.phase {
        Phase::Loading | Phase::Submitting | Phase::Applying => {
            // Ignore keys during async operations (except Ctrl+C above)
        }
        Phase::Answering => match key.code {
            KeyCode::Tab => {
                app.focused_field = (app.focused_field + 1) % app.answers.len();
            }
            KeyCode::BackTab => {
                app.focused_field = if app.focused_field == 0 {
                    app.answers.len() - 1
                } else {
                    app.focused_field - 1
                };
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.phase = Phase::Submitting;
                app.status = "Generating proposal...".into();
                spawn_proposal(app, llm, tx);
            }
            KeyCode::Esc => {
                app.skipped += 1;
                app.advance();
                if !app.should_quit {
                    spawn_questions(app, llm, tx);
                    if !no_context {
                        spawn_similar(app, db, embedder, config, tx);
                    }
                }
            }
            _ => {
                app.answers[app.focused_field].input(key);
            }
        },
        Phase::Reviewing(_) => match key.code {
            KeyCode::Char('a') | KeyCode::Enter => {
                app.phase = Phase::Applying;
                app.status = "Applying to Linear...".into();
                spawn_apply(app, linear, db, tx);
            }
            KeyCode::Char('s') | KeyCode::Esc => {
                app.skipped += 1;
                app.advance();
                if !app.should_quit {
                    spawn_questions(app, llm, tx);
                    if !no_context {
                        spawn_similar(app, db, embedder, config, tx);
                    }
                }
            }
            KeyCode::Char('q') => {
                app.should_quit = true;
            }
            _ => {}
        },
    }
}

// --- Async task spawners ---

fn spawn_questions(app: &App<'_>, llm: &LlmClient, tx: &mpsc::UnboundedSender<AppEvent>) {
    let issue_context = build_issue_context(app.current_issue());
    let system = format!("{}\n\nCurrent issue:\n{}", QUESTIONS_PROMPT, issue_context);
    let messages = vec![Message::user("Analyze this issue and generate clarifying questions.")];

    let llm = llm.clone();
    let tx = tx.clone();

    tokio::spawn(async move {
        let result = llm.generate(&messages, &system).await;

        let questions = match result {
            Ok(response) => {
                let json_str = llm::extract_json(&response);
                match serde_json::from_str::<QuestionsResponse>(json_str) {
                    Ok(qr) if !qr.questions.is_empty() => qr.questions,
                    _ => default_questions(),
                }
            }
            Err(_) => default_questions(),
        };

        tx.send(AppEvent::QuestionsReady(questions)).ok();
    });
}

fn spawn_similar(
    app: &App<'_>,
    db: &Database,
    embedder: &Option<Arc<Embedder>>,
    config: &Config,
    tx: &mpsc::UnboundedSender<AppEvent>,
) {
    let Some(embedder) = embedder.clone() else {
        return;
    };
    let issue_id = app.current_issue().id.clone();
    let query_text = format!(
        "{} {}",
        app.current_issue().title,
        app.current_issue().description.as_deref().unwrap_or("")
    );
    let team_key = app.current_issue().team_key.clone();
    let rrf_k = config.search.rrf_k;

    let db = db.clone();
    let tx = tx.clone();

    tokio::spawn(async move {
        let results = search::search(
            &db,
            &query_text,
            SearchMode::Vector,
            Some(&team_key),
            None,
            5,
            Some(&embedder),
            rrf_k,
        )
        .await;

        let ctx = match results {
            Ok(results) => {
                let mut s = String::new();
                for r in results.iter().filter(|r| r.issue_id != issue_id).take(3) {
                    let plabel = match r.priority {
                        1 => "Urgent",
                        2 => "High",
                        3 => "Medium",
                        4 => "Low",
                        _ => "No priority",
                    };
                    s.push_str(&format!("  {} ({}): {}\n", r.identifier, plabel, r.title));
                }
                s
            }
            Err(_) => String::new(),
        };

        tx.send(AppEvent::SimilarReady(ctx)).ok();
    });
}

fn spawn_proposal(
    app: &App<'_>,
    llm: &LlmClient,
    tx: &mpsc::UnboundedSender<AppEvent>,
) {
    let issue_context = build_issue_context(app.current_issue());
    let qa_text = app.collect_qa_text();
    let similar = if app.similar_context.is_empty() {
        String::new()
    } else {
        format!("\nSimilar issues:\n{}", app.similar_context)
    };

    let system = format!(
        "{}\n\nIssue context:\n{}\n\nUser's answers:\n{}{}",
        EXTRACTION_PROMPT, issue_context, qa_text, similar
    );
    let messages = vec![Message::user(
        "Based on the issue and answers above, propose the final priority, title, and description.",
    )];

    let llm = llm.clone();
    let tx = tx.clone();

    tokio::spawn(async move {
        let result = llm.generate(&messages, &system).await;

        let proposal = match result {
            Ok(response) => {
                let json_str = llm::extract_json(&response);
                serde_json::from_str::<TriageProposal>(json_str)
                    .map_err(|e| anyhow::anyhow!("Failed to parse proposal: {}", e))
            }
            Err(e) => Err(e),
        };

        tx.send(AppEvent::ProposalReady(proposal)).ok();
    });
}

fn spawn_apply(
    app: &App<'_>,
    linear: &LinearClient,
    db: &Database,
    tx: &mpsc::UnboundedSender<AppEvent>,
) {
    let Phase::Reviewing(ref proposal) = app.phase else {
        return;
    };

    let issue_id = app.current_issue().id.clone();
    let current_title = app.current_issue().title.clone();
    let current_desc = app
        .current_issue()
        .description
        .clone()
        .unwrap_or_default();

    let new_title = if proposal.title != current_title {
        Some(proposal.title.clone())
    } else {
        None
    };
    let new_desc = if proposal.description != current_desc {
        Some(proposal.description.clone())
    } else {
        None
    };
    let priority = proposal.priority;

    let linear = linear.clone();
    let db = db.clone();
    let tx = tx.clone();

    tokio::spawn(async move {
        let result = linear
            .update_issue(
                &issue_id,
                new_title.as_deref(),
                new_desc.as_deref(),
                Some(priority),
                None,
            )
            .await;

        if result.is_ok() {
            if let Ok(updated) = linear.fetch_single_issue(&issue_id).await {
                db.upsert_issue(&updated).ok();
            }
        }

        tx.send(AppEvent::ApplyDone(result)).ok();
    });
}

// --- Rendering ---

fn ui(f: &mut Frame, app: &mut App) {
    let outer = Layout::vertical([
        Constraint::Length(5),
        Constraint::Min(8),
        Constraint::Length(1),
    ])
    .split(f.area());

    render_header(f, outer[0], app);

    let body = Layout::horizontal([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(outer[1]);

    match &app.phase {
        Phase::Loading => {
            let p = Paragraph::new("Loading questions from AI...")
                .block(Block::default().borders(Borders::ALL).title(" Triage "));
            f.render_widget(p, body[0]);
        }
        Phase::Submitting => {
            let p = Paragraph::new("Generating proposal...")
                .block(Block::default().borders(Borders::ALL).title(" Triage "));
            f.render_widget(p, body[0]);
        }
        Phase::Applying => {
            let p = Paragraph::new("Applying changes to Linear...")
                .block(Block::default().borders(Borders::ALL).title(" Triage "));
            f.render_widget(p, body[0]);
        }
        Phase::Answering => {
            render_questions(f, body[0], app);
        }
        Phase::Reviewing(proposal) => {
            render_proposal(f, body[0], app, proposal);
        }
    }

    render_similar(f, body[1], app);
    render_status(f, outer[2], app);
}

fn render_header(f: &mut Frame, area: Rect, app: &App) {
    let issue = app.current_issue();
    let progress = format!(
        "[{}/{}] Applied: {} Skipped: {}",
        app.current_index + 1,
        app.max_issues,
        app.applied,
        app.skipped
    );

    let desc_preview: String = issue
        .description
        .as_deref()
        .unwrap_or("(no description)")
        .chars()
        .take(120)
        .collect();

    let text = vec![
        Line::from(vec![
            Span::styled(
                format!("{} ", issue.identifier),
                Style::default().bold().fg(Color::Cyan),
            ),
            Span::raw(&issue.title),
            Span::styled(
                format!("  {}", progress),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(vec![
            Span::styled("State: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&issue.state_name, Style::default().fg(Color::Yellow)),
            Span::raw("  "),
            Span::styled("Assignee: ", Style::default().fg(Color::DarkGray)),
            Span::raw(issue.assignee_name.as_deref().unwrap_or("Unassigned")),
            Span::raw("  "),
            Span::styled("Created: ", Style::default().fg(Color::DarkGray)),
            Span::raw(&issue.created_at[..10]),
        ]),
        Line::from(Span::styled(desc_preview, Style::default().fg(Color::DarkGray))),
    ];

    let block = Block::default().borders(Borders::ALL).title(" Issue ");
    let p = Paragraph::new(text).block(block);
    f.render_widget(p, area);
}

fn render_questions(f: &mut Frame, area: Rect, app: &mut App) {
    let block = Block::default().borders(Borders::ALL).title(" Questions ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.questions.is_empty() {
        return;
    }

    // Each question gets: 1 line label + 3 lines textarea = 4 lines, plus 1 line gap
    let constraints: Vec<Constraint> = app
        .questions
        .iter()
        .flat_map(|_| [Constraint::Length(1), Constraint::Length(3), Constraint::Length(1)])
        .collect();

    let chunks = Layout::vertical(constraints).split(inner);

    for (i, (question, textarea)) in app
        .questions
        .iter()
        .zip(app.answers.iter_mut())
        .enumerate()
    {
        let label_area = chunks[i * 3];
        let input_area = chunks[i * 3 + 1];

        let focused = i == app.focused_field;
        let base_color = if focused { Color::Cyan } else { Color::White };

        let mut spans = vec![Span::styled(
            format!("  {}: ", question.label),
            Style::default().bold().fg(base_color),
        )];
        spans.extend(parse_markdown_spans(&question.question, base_color));

        let label = Paragraph::new(Line::from(spans));
        f.render_widget(label, label_area);

        let border_style = if i == app.focused_field {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        textarea.set_block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style),
        );
        textarea.set_cursor_line_style(Style::default());

        f.render_widget(&*textarea, input_area);
    }
}

fn render_proposal(f: &mut Frame, area: Rect, app: &App, proposal: &TriageProposal) {
    let issue = app.current_issue();

    let priority_label = match proposal.priority {
        1 => "Urgent",
        2 => "High",
        3 => "Medium",
        4 => "Low",
        _ => "Unknown",
    };

    let mut lines = vec![
        Line::from(vec![
            Span::styled("Priority: ", Style::default().fg(Color::DarkGray)),
            Span::styled("No priority", Style::default().fg(Color::DarkGray)),
            Span::raw(" → "),
            Span::styled(
                format!("{} ({})", priority_label, proposal.priority),
                Style::default().bold().fg(Color::Green),
            ),
        ]),
        Line::from(""),
    ];

    if proposal.title != issue.title {
        lines.push(Line::from(vec![
            Span::styled("Title: ", Style::default().fg(Color::DarkGray)),
        ]));
        lines.push(Line::from(vec![
            Span::styled(format!("  - {}", issue.title), Style::default().fg(Color::Red)),
        ]));
        lines.push(Line::from(vec![
            Span::styled(
                format!("  + {}", proposal.title),
                Style::default().fg(Color::Green),
            ),
        ]));
        lines.push(Line::from(""));
    }

    let current_desc = issue.description.as_deref().unwrap_or("");
    if proposal.description != current_desc {
        lines.push(Line::from(vec![
            Span::styled("Description: ", Style::default().fg(Color::DarkGray)),
        ]));
        // Show first few lines of new description
        for line in proposal.description.lines().take(8) {
            lines.push(Line::from(Span::styled(
                format!("  {}", line),
                Style::default().fg(Color::Green),
            )));
        }
        if proposal.description.lines().count() > 8 {
            lines.push(Line::from(Span::styled(
                "  ...",
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    let block = Block::default().borders(Borders::ALL).title(" Proposal ");
    let p = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn render_similar(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Similar Issues ");

    let text = if app.similar_context.is_empty() {
        "Loading...".to_string()
    } else {
        app.similar_context.clone()
    };

    let p = Paragraph::new(text)
        .block(block)
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(Color::DarkGray));
    f.render_widget(p, area);
}

fn render_status(f: &mut Frame, area: Rect, app: &App) {
    let p = Paragraph::new(Span::styled(
        format!(" {}", app.status),
        Style::default().fg(Color::DarkGray),
    ));
    f.render_widget(p, area);
}

// --- Helpers ---

fn build_issue_context(issue: &Issue) -> String {
    format!(
        "Issue: {} - {}\nState: {}\nAssignee: {}\nCreated: {}\nDescription: {}",
        issue.identifier,
        issue.title,
        issue.state_name,
        issue.assignee_name.as_deref().unwrap_or("Unassigned"),
        &issue.created_at[..10],
        issue.description.as_deref().unwrap_or("(none)")
    )
}

/// Parse simple markdown inline formatting into ratatui Spans.
/// Supports **bold**, *italic*, and `code`.
fn parse_markdown_spans(text: &str, base_color: Color) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        // Find the next markdown delimiter
        let next_bold = remaining.find("**");
        let next_code = remaining.find('`');
        let next_italic = remaining.find('*').filter(|&pos| {
            // Only match single * that isn't part of **
            next_bold.map_or(true, |bp| pos != bp)
        });

        let next = [
            next_bold.map(|p| (p, "**")),
            next_code.map(|p| (p, "`")),
            next_italic.map(|p| (p, "*")),
        ]
        .into_iter()
        .flatten()
        .min_by_key(|(pos, _)| *pos);

        match next {
            None => {
                // No more delimiters
                spans.push(Span::styled(
                    remaining.to_string(),
                    Style::default().fg(base_color),
                ));
                break;
            }
            Some((pos, delim)) => {
                // Push text before delimiter
                if pos > 0 {
                    spans.push(Span::styled(
                        remaining[..pos].to_string(),
                        Style::default().fg(base_color),
                    ));
                }
                let after_open = &remaining[pos + delim.len()..];
                // Find closing delimiter
                if let Some(close_pos) = after_open.find(delim) {
                    let content = &after_open[..close_pos];
                    let style = match delim {
                        "**" => Style::default().bold().fg(base_color),
                        "`" => Style::default().fg(Color::Yellow),
                        "*" => Style::default().italic().fg(base_color),
                        _ => Style::default().fg(base_color),
                    };
                    spans.push(Span::styled(content.to_string(), style));
                    remaining = &after_open[close_pos + delim.len()..];
                } else {
                    // No closing delimiter — treat as plain text
                    spans.push(Span::styled(
                        remaining[pos..pos + delim.len()].to_string(),
                        Style::default().fg(base_color),
                    ));
                    remaining = after_open;
                }
            }
        }
    }

    spans
}

fn default_questions() -> Vec<Question> {
    DEFAULT_QUESTIONS
        .iter()
        .map(|(id, label, question)| Question {
            id: id.to_string(),
            label: label.to_string(),
            question: question.to_string(),
        })
        .collect()
}

