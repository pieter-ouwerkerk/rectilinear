use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap};
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
{"questions": [{"id": "<short_snake_case_id>", "label": "<Short Label>", "question": "<The question>", "suggested_answer": "<your best guess>"}]}

For each question, provide a suggested_answer based on what you can infer from the issue context. The user can edit, replace, or accept these suggestions. If you're uncertain, phrase it as a guess (e.g., "Likely affects all users of..." or "Probably low frequency based on...").

You may use Markdown in the question text: **bold** for emphasis, *italics* for examples, `backticks` for code/identifiers.

Keep questions concise (one sentence each). Focus on: impact (users affected, blocking?), frequency (how often?), severity (crash vs cosmetic), business context.

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
    #[serde(default)]
    suggested_answer: Option<String>,
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
    Resize,
    QuestionsReady(Vec<Question>),
    SimilarReady(Vec<SimilarIssue>),
    ProposalReady(Result<TriageProposal>),
    ApplyDone(Result<()>),
}

#[derive(Clone)]
struct SimilarIssue {
    identifier: String,
    title: String,
    priority: &'static str,
    similarity: f32,
}

struct App<'a> {
    issues: Vec<Issue>,
    current_index: usize,
    max_issues: usize,
    phase: Phase,
    questions: Vec<Question>,
    answers: Vec<TextArea<'a>>,
    focused_field: usize,
    similar_issues: Vec<SimilarIssue>,
    similar_loading: bool,
    desc_scroll: u16,
    applied: usize,
    skipped: usize,
    status: String,
    should_quit: bool,
    show_desc: bool,
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
            similar_issues: Vec::new(),
            similar_loading: true,
            desc_scroll: 0,
            applied: 0,
            skipped: 0,
            status: "Loading questions...".into(),
            should_quit: false,
            show_desc: false,
        }
    }

    fn current_issue(&self) -> &Issue {
        &self.issues[self.current_index]
    }

    fn remaining(&self) -> usize {
        self.max_issues.saturating_sub(self.current_index + 1)
    }

    fn issue_url(&self) -> String {
        // Linear URL format: https://linear.app/issue/<identifier>
        format!(
            "https://linear.app/issue/{}",
            self.current_issue().identifier
        )
    }

    fn set_questions(&mut self, questions: Vec<Question>) {
        self.answers = questions
            .iter()
            .map(|q| {
                let mut ta = if let Some(ref suggestion) = q.suggested_answer {
                    TextArea::new(suggestion.lines().map(String::from).collect())
                } else {
                    TextArea::default()
                };
                ta.set_style(Style::default().fg(Color::White));
                ta
            })
            .collect();
        self.questions = questions;
        self.focused_field = 0;
        self.phase = Phase::Answering;
        self.update_status();
    }

    fn update_status(&mut self) {
        self.status = match &self.phase {
            Phase::Loading => "Loading questions...".into(),
            Phase::Answering if self.show_desc => {
                "↑↓=scroll description | d=close | Tab=fields | Ctrl+S=submit".into()
            }
            Phase::Answering => {
                "Tab=next | Ctrl+S=submit | d=description | Esc=skip | Ctrl+C=quit".into()
            }
            Phase::Submitting => "Generating proposal...".into(),
            Phase::Reviewing(_) => "a=accept | s=skip | q=quit".into(),
            Phase::Applying => "Applying to Linear...".into(),
        };
    }

    fn advance(&mut self) {
        self.current_index += 1;
        if self.current_index >= self.max_issues {
            self.should_quit = true;
        } else {
            self.phase = Phase::Loading;
            self.questions.clear();
            self.answers.clear();
            self.similar_issues.clear();
            self.similar_loading = true;
            self.focused_field = 0;
            self.desc_scroll = 0;
            self.show_desc = false;
            self.update_status();
        }
    }

    fn collect_qa_text(&self) -> String {
        self.questions
            .iter()
            .zip(self.answers.iter())
            .map(|(q, a)| {
                let answer = a.lines().join("\n");
                format!(
                    "{}: {}",
                    q.label,
                    if answer.is_empty() {
                        "(skipped)"
                    } else {
                        &answer
                    }
                )
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

    // Spawn event reader (keys + resize)
    let tx_evt = tx.clone();
    tokio::spawn(async move {
        loop {
            if event::poll(std::time::Duration::from_millis(50)).unwrap_or(false) {
                match event::read() {
                    Ok(Event::Key(key)) => {
                        // Deduplicate key release events (crossterm on some terminals
                        // sends both press and release for Ctrl+C)
                        if key.kind == crossterm::event::KeyEventKind::Release {
                            continue;
                        }
                        if tx_evt.send(AppEvent::Key(key)).is_err() {
                            break;
                        }
                    }
                    Ok(Event::Resize(_, _)) => {
                        tx_evt.send(AppEvent::Resize).ok();
                    }
                    _ => {}
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
                AppEvent::Resize => {
                    // Just redraw on next loop iteration
                }
                AppEvent::QuestionsReady(questions) => {
                    app.set_questions(questions);
                }
                AppEvent::SimilarReady(issues) => {
                    app.similar_issues = issues;
                    app.similar_loading = false;
                }
                AppEvent::ProposalReady(result) => match result {
                    Ok(proposal) => {
                        app.phase = Phase::Reviewing(proposal);
                        app.update_status();
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
        Phase::Answering => {
            // Description scroll mode
            if app.show_desc {
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        app.desc_scroll = app.desc_scroll.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        app.desc_scroll = app.desc_scroll.saturating_add(1);
                    }
                    KeyCode::Char('d') | KeyCode::Esc => {
                        app.show_desc = false;
                        app.update_status();
                    }
                    _ => {}
                }
                return;
            }

            match key.code {
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
                KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    app.phase = Phase::Submitting;
                    app.update_status();
                    spawn_proposal(app, llm, tx);
                }
                KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    app.phase = Phase::Submitting;
                    app.update_status();
                    spawn_proposal(app, llm, tx);
                }
                KeyCode::Char('d') if key.modifiers.is_empty() && app.answers[app.focused_field].lines().join("").is_empty() => {
                    // Only toggle description if focused textarea is empty (so 'd' can be typed normally)
                    app.show_desc = true;
                    app.desc_scroll = 0;
                    app.update_status();
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
            }
        }
        Phase::Reviewing(_) => match key.code {
            KeyCode::Char('a') | KeyCode::Enter => {
                app.phase = Phase::Applying;
                app.update_status();
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
    let messages = vec![Message::user(
        "Analyze this issue and generate clarifying questions.",
    )];

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
        tx.send(AppEvent::SimilarReady(Vec::new())).ok();
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

        let similar: Vec<SimilarIssue> = match results {
            Ok(results) => results
                .into_iter()
                .filter(|r| r.issue_id != issue_id)
                .take(3)
                .map(|r| SimilarIssue {
                    identifier: r.identifier,
                    title: r.title,
                    priority: priority_label(r.priority),
                    similarity: r.similarity.unwrap_or(0.0),
                })
                .collect(),
            Err(_) => Vec::new(),
        };

        tx.send(AppEvent::SimilarReady(similar)).ok();
    });
}

fn spawn_proposal(app: &App<'_>, llm: &LlmClient, tx: &mpsc::UnboundedSender<AppEvent>) {
    let issue_context = build_issue_context(app.current_issue());
    let qa_text = app.collect_qa_text();
    let similar = if app.similar_issues.is_empty() {
        String::new()
    } else {
        let ctx: String = app
            .similar_issues
            .iter()
            .map(|s| format!("  {} ({}): {}", s.identifier, s.priority, s.title))
            .collect::<Vec<_>>()
            .join("\n");
        format!("\nSimilar issues:\n{}", ctx)
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
    // When showing description overlay, render that instead of normal layout
    if app.show_desc {
        render_description_overlay(f, app);
        return;
    }

    // Calculate similar issues bar height
    let similar_height = if app.similar_issues.is_empty() && !app.similar_loading {
        0
    } else {
        (app.similar_issues.len() as u16 + 2).max(3) // border + at least 1 line
    };

    let outer = Layout::vertical([
        Constraint::Length(5),    // header
        Constraint::Min(8),       // questions/main
        Constraint::Length(similar_height), // similar issues bar
        Constraint::Length(1),    // status bar
    ])
    .split(f.area());

    render_header(f, outer[0], app);

    match &app.phase {
        Phase::Loading => {
            let p = Paragraph::new("Loading questions from AI...")
                .block(Block::default().borders(Borders::ALL).title(" Triage "));
            f.render_widget(p, outer[1]);
        }
        Phase::Submitting => {
            let p = Paragraph::new("Generating proposal...")
                .block(Block::default().borders(Borders::ALL).title(" Triage "));
            f.render_widget(p, outer[1]);
        }
        Phase::Applying => {
            let p = Paragraph::new("Applying changes to Linear...")
                .block(Block::default().borders(Borders::ALL).title(" Triage "));
            f.render_widget(p, outer[1]);
        }
        Phase::Answering => {
            render_questions(f, outer[1], app);
        }
        Phase::Reviewing(proposal) => {
            render_proposal(f, outer[1], app, proposal);
        }
    }

    if similar_height > 0 {
        render_similar(f, outer[2], app);
    }

    render_status(f, outer[3], app);
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

    let desc = issue.description.as_deref().unwrap_or("(no description)");
    let desc_preview: String = desc.chars().take(200).collect();
    let desc_truncated = desc.len() > 200;

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
            Span::raw("  "),
            Span::styled(
                app.issue_url(),
                Style::default().fg(Color::Blue).underlined(),
            ),
        ]),
        Line::from(vec![
            Span::styled(desc_preview, Style::default().fg(Color::DarkGray)),
            if desc_truncated {
                Span::styled(" [d=show more]", Style::default().fg(Color::Blue))
            } else {
                Span::raw("")
            },
        ]),
    ];

    let block = Block::default().borders(Borders::ALL).title(" Issue ");
    let p = Paragraph::new(text).block(block);
    f.render_widget(p, area);
}

fn render_description_overlay(f: &mut Frame, app: &mut App) {
    let identifier = app.current_issue().identifier.clone();
    let title = app.current_issue().title.clone();
    let desc = app
        .current_issue()
        .description
        .clone()
        .unwrap_or_else(|| "(no description)".into());
    let url = app.issue_url();

    let mut lines: Vec<Line> = vec![
        Line::from(vec![
            Span::styled(
                format!("{} ", identifier),
                Style::default().bold().fg(Color::Cyan),
            ),
            Span::raw(title),
        ]),
        Line::from(vec![Span::styled(
            url,
            Style::default().fg(Color::Blue).underlined(),
        )]),
        Line::from(""),
    ];
    for line in desc.lines() {
        lines.push(Line::from(Span::raw(line.to_string())));
    }

    let total_lines = lines.len() as u16;
    let visible = f.area().height.saturating_sub(3); // borders + title
    app.desc_scroll = app.desc_scroll.min(total_lines.saturating_sub(visible));

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Description (↑↓ scroll, d/Esc close) ");
    let p = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.desc_scroll, 0));
    f.render_widget(p, f.area());

    // Scrollbar
    let mut scrollbar_state =
        ScrollbarState::new(total_lines as usize).position(app.desc_scroll as usize);
    f.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight),
        f.area(),
        &mut scrollbar_state,
    );

    // Status bar at bottom
    let status_area = Rect {
        x: f.area().x,
        y: f.area().y + f.area().height.saturating_sub(1),
        width: f.area().width,
        height: 1,
    };
    let status = Paragraph::new(Span::styled(
        " ↑↓=scroll | d/Esc=close",
        Style::default().fg(Color::DarkGray),
    ));
    f.render_widget(status, status_area);
}

fn render_questions(f: &mut Frame, area: Rect, app: &mut App) {
    let block = Block::default().borders(Borders::ALL).title(" Questions ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.questions.is_empty() {
        return;
    }

    // Compute per-textarea height based on content, with fair distribution of space
    let n = app.questions.len() as u16;
    let label_lines: u16 = 2;
    let gap: u16 = 1;
    let border: u16 = 2; // top + bottom border
    let overhead_per_q = label_lines + gap;
    let available = inner.height.saturating_sub(n * overhead_per_q);

    // Content-aware heights: each textarea gets at least its line count, min 2
    let content_heights: Vec<u16> = app
        .answers
        .iter()
        .map(|ta| (ta.lines().len() as u16).max(2))
        .collect();
    let total_content: u16 = content_heights.iter().sum();

    let textarea_heights: Vec<u16> = if total_content + n * border <= available {
        // Enough room: give each textarea its content height, distribute remainder
        let remaining = available - total_content - n * border;
        let bonus = remaining / n;
        content_heights
            .iter()
            .map(|&h| h + border + bonus)
            .collect()
    } else {
        // Tight: distribute available space proportionally, min 2 + border
        content_heights
            .iter()
            .map(|&h| {
                let share = (h as u32 * available as u32 / total_content.max(1) as u32) as u16;
                share.max(2 + border)
            })
            .collect()
    };

    let constraints: Vec<Constraint> = textarea_heights
        .iter()
        .flat_map(|&h| {
            [
                Constraint::Length(label_lines),
                Constraint::Length(h),
                Constraint::Length(gap),
            ]
        })
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

        let label = Paragraph::new(Line::from(spans)).wrap(Wrap { trim: false });
        f.render_widget(label, label_area);

        let border_style = if focused {
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

    let plabel = priority_label(proposal.priority);

    let mut lines = vec![
        Line::from(vec![
            Span::styled("Priority: ", Style::default().fg(Color::DarkGray)),
            Span::styled("No priority", Style::default().fg(Color::DarkGray)),
            Span::raw(" → "),
            Span::styled(
                format!("{} ({})", plabel, proposal.priority),
                Style::default().bold().fg(Color::Green),
            ),
        ]),
        Line::from(""),
    ];

    if proposal.title != issue.title {
        lines.push(Line::from(Span::styled(
            "Title:",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(Span::styled(
            format!("  - {}", issue.title),
            Style::default().fg(Color::Red),
        )));
        lines.push(Line::from(Span::styled(
            format!("  + {}", proposal.title),
            Style::default().fg(Color::Green),
        )));
        lines.push(Line::from(""));
    }

    let current_desc = issue.description.as_deref().unwrap_or("");
    if proposal.description != current_desc {
        lines.push(Line::from(Span::styled(
            "Description:",
            Style::default().fg(Color::DarkGray),
        )));
        for line in proposal.description.lines().take(12) {
            lines.push(Line::from(Span::styled(
                format!("  {}", line),
                Style::default().fg(Color::Green),
            )));
        }
        if proposal.description.lines().count() > 12 {
            lines.push(Line::from(Span::styled(
                "  ...",
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    let block = Block::default().borders(Borders::ALL).title(" Proposal ");
    let p = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn render_similar(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Similar Issues ");

    if app.similar_loading {
        let p = Paragraph::new(Span::styled(
            " Loading...",
            Style::default().fg(Color::DarkGray),
        ))
        .block(block);
        f.render_widget(p, area);
        return;
    }

    if app.similar_issues.is_empty() {
        return;
    }

    let lines: Vec<Line> = app
        .similar_issues
        .iter()
        .map(|s| {
            Line::from(vec![
                Span::styled(
                    format!(" {} ", s.identifier),
                    Style::default().bold().fg(Color::Cyan),
                ),
                Span::styled(
                    format!("({}) ", s.priority),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(&s.title),
                Span::styled(
                    format!("  {:.0}%", s.similarity * 100.0),
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        })
        .collect();

    let p = Paragraph::new(lines).block(block);
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

fn priority_label(priority: i32) -> &'static str {
    match priority {
        1 => "Urgent",
        2 => "High",
        3 => "Medium",
        4 => "Low",
        _ => "No priority",
    }
}

/// Parse simple markdown inline formatting into ratatui Spans.
/// Supports **bold**, *italic*, and `code`.
fn parse_markdown_spans(text: &str, base_color: Color) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        let next_bold = remaining.find("**");
        let next_code = remaining.find('`');
        let next_italic = remaining.find('*').filter(|&pos| {
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
                spans.push(Span::styled(
                    remaining.to_string(),
                    Style::default().fg(base_color),
                ));
                break;
            }
            Some((pos, delim)) => {
                if pos > 0 {
                    spans.push(Span::styled(
                        remaining[..pos].to_string(),
                        Style::default().fg(base_color),
                    ));
                }
                let after_open = &remaining[pos + delim.len()..];
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
            suggested_answer: None,
        })
        .collect()
}
